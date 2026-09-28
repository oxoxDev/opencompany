//! Store-agnostic bundle export and import.
//!
//! Export reads *everything* for a company through the four durable storage
//! ports ([`CompanyStore`], [`EventLog`], [`MemoryStore`], [`ContextStore`]) and
//! writes the canonical filesystem [`Bundle`](crate::store::paths::Bundle)
//! layout. Because it drives the ports rather than a backend's private files, an
//! export is *total by construction* for any backend — the fs and sqlite stores
//! produce identical bundles. Import is the exact inverse: it reads a bundle
//! directory and replays every record through the ports, so it materializes into
//! whichever backend the target ports are wired to.
//!
//! The dep-free core operates on an *unpacked bundle directory*. A single-file
//! `.tar` wrapper ([`pack_tar`]/[`unpack_tar`]) is gated behind the `export`
//! feature so the default build links no archive crate.
//!
//! `secrets/` and `keys/` are fs-only artifacts (the builder keeps them on the
//! filesystem even under a non-fs store) with no enumeration port, so they are
//! excluded from an export unless [`ExportOpts::include_secrets`] is set and a
//! source bundle directory is supplied.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::company::CompanyManifest;
use crate::error::OpenCompanyError;
use crate::ports::context::ContextStore;
use crate::ports::events::EventLog;
use crate::ports::facts::{FactRecord, FactStore};
use crate::ports::memory::MemoryStore;
use crate::ports::store::CompanyStore;
use crate::ports::types::{
    AgentOverride, BudgetOverride, CompanyEvent, CompanyId, CompanyRecord, CompressedTrace,
    ContextChunk, DeskHiveOverride, EventSeq, LedgerEntry, OverlayAgent, OverlayDesk,
    OverlayDeskMember, OverlayDeskOrder, OverlayWorkflow, PolicyOverride, StoredEvent,
    TemplateProvenance, ToolGrantsOverride,
};
use crate::store::select::MemoryScopes;

/// Canonical bundle file and directory names, matching the fs
/// [`Bundle`](crate::store::paths::Bundle) layout.
const COMPANY_TOML: &str = "company.toml";
const META_JSON: &str = "meta.json";
const EVENTS_JSONL: &str = "events.jsonl";
const LEDGER_JSONL: &str = "ledger.jsonl";
const MEMORY_DIR: &str = "memory";
const TRACES_JSONL: &str = "traces.jsonl";
const ARCHIVES_JSONL: &str = "archives.jsonl";
/// Operator facts, at the bundle ROOT — the same place the live fs bundle
/// keeps them (`paths::Bundle::facts_jsonl`), so an export stays diffable
/// against a live home and a direct reader finds them where the canonical
/// layout says. Absent from bundles written before facts joined the export;
/// `read_jsonl` treats an absent file as empty, so both directions stay
/// compatible — an old importer ignores the new file, a new importer accepts
/// an old bundle.
const FACTS_JSONL: &str = "facts.jsonl";
const CONTEXT_DIR: &str = "context";
const CONTEXT_INDEX_JSONL: &str = "index.jsonl";
const CONTEXT_BLOBS_DIR: &str = "blobs";
const SECRETS_DIR: &str = "secrets";
const KEYS_DIR: &str = "keys";

/// The four durable storage ports as trait objects, in export/import order
/// (`CompanyStore`, `EventLog`, `MemoryStore`, `ContextStore`).
pub type Ports = (
    Arc<dyn CompanyStore>,
    Arc<dyn EventLog>,
    Arc<dyn MemoryStore>,
    Arc<dyn ContextStore>,
);

/// Options controlling what an export includes.
#[derive(Clone, Debug, Default)]
pub struct ExportOpts {
    /// Include the fs-only `secrets/` and `keys/` directories. Off by default so
    /// a shared bundle never leaks the company's signing key or secrets.
    pub include_secrets: bool,
    /// The source fs bundle directory to copy `secrets/`/`keys/` from when
    /// [`Self::include_secrets`] is set. Left `None` for a non-fs source (which
    /// has no such artifacts to copy).
    pub fs_bundle: Option<PathBuf>,
}

fn io_err(path: &Path, source: std::io::Error) -> OpenCompanyError {
    OpenCompanyError::StoreIo {
        path: path.to_path_buf(),
        source,
    }
}

/// Bundle metadata persisted alongside the manifest. Carries the company id so an
/// import can restore the original id even when it diverges from the manifest
/// slug, plus the source-template provenance so a template-launched company keeps
/// its provenance across an export/import round-trip. The fs [`CompanyStore`]
/// reads only `lifecycle`; the extra fields are ignored there (serde skips
/// unknown fields).
#[derive(Serialize, Deserialize)]
struct BundleMeta {
    lifecycle: String,
    id: String,
    /// The operator team overlay — teammates the operator added that the
    /// version-controlled manifest does not know about. Preserved across the
    /// bundle round-trip so an export→import keeps the operator-added roster.
    /// `#[serde(default)]` loads bundles written before this field existed as an
    /// empty overlay.
    #[serde(default)]
    overlay_agents: Vec<OverlayAgent>,
    /// The operator desk-membership overlay. Preserved so operator-added desk
    /// memberships survive an export→import. `#[serde(default)]` for back-compat
    /// with older bundles.
    #[serde(default)]
    overlay_desk_members: Vec<OverlayDeskMember>,
    /// The operator per-desk member-ordering overlay. Preserved across the bundle
    /// round-trip so an export→import keeps the operator-defined desk hierarchy
    /// (and therefore the routing lead). `#[serde(default)]` loads bundles written
    /// before this field existed as an empty order.
    #[serde(default)]
    overlay_desk_order: Vec<OverlayDeskOrder>,
    /// The operator-created desk overlay. Preserved so operator-created desks
    /// survive an export→import. `#[serde(default)]` for back-compat with older
    /// bundles.
    #[serde(default)]
    overlay_desks: Vec<OverlayDesk>,
    /// The operator workflow-authoring overlay — graph bodies created from the
    /// console or the orchestrator tool, which live on the record (never in the
    /// read-only source tree). Preserved so console-created workflows survive an
    /// export→import instead of being silently dropped. `#[serde(default)]` for
    /// back-compat with older bundles.
    #[serde(default)]
    overlay_workflows: Vec<OverlayWorkflow>,
    /// The operator-set per-teammate daily spend caps (issue #343). Preserved so
    /// an export→import keeps the caps an operator set from the console, rather
    /// than silently reverting every teammate to its manifest default.
    /// `#[serde(default)]` for back-compat with older bundles.
    #[serde(default)]
    overlay_budgets: Vec<BudgetOverride>,
    /// The operator's edits of manifest-declared teammates at export time.
    /// Preserved so an export→import keeps the roster the operator shaped from
    /// the console, rather than silently reverting every blueprint teammate to
    /// the name, role, instructions and scope `company.toml` declared.
    /// `#[serde(default)]` for back-compat with older bundles.
    #[serde(default)]
    overlay_agent_edits: Vec<AgentOverride>,
    /// The move grammars installed on desks at export time. Preserved so an
    /// export→import keeps a desk deliberating under the table the operator
    /// installed rather than silently reverting to the manifest's.
    /// `#[serde(default)]` for back-compat with older bundles.
    #[serde(default)]
    overlay_desk_hive: Vec<DeskHiveOverride>,
    /// The ids of manifest teammates removed from the console at export time.
    /// Preserved so an import does not silently restore a teammate the operator
    /// retired — the blueprint still declares it, so without the tombstone it
    /// comes straight back. `#[serde(default)]` for back-compat with older
    /// bundles.
    #[serde(default)]
    overlay_retired_agents: Vec<String>,
    /// The operator's `[policy]` override at export time (issue #562).
    /// `#[serde(default)]` for back-compat with older bundles, which read as
    /// `None` — the manifest's `[policy]` decides, exactly as before.
    #[serde(default)]
    overlay_policy: Option<PolicyOverride>,
    /// The operator's console-added `[tools].allow` grants at export time
    /// (issue #1796). Preserved so an export→import does not silently revoke an
    /// integration the operator granted from a connect surface, leaving the
    /// restored company "Connected" and reaching nobody. `#[serde(default)]`
    /// for back-compat with older bundles, which read as `None`: the manifest's
    /// `[tools]` decides, exactly as before.
    ///
    /// Carries the **seed's** list beside it, not the record's materialised one
    /// — see `read_via_ports` for why the bundle's `company.toml` must not name
    /// what the console added.
    #[serde(default)]
    overlay_tool_grants: Option<ToolGrantsOverride>,
    /// The operator-set per-desk tool ceilings at export time. Preserved so an
    /// export→import does not silently widen a desk back to the company's full
    /// grant — the same class of loss `overlay_policy` above is carried to
    /// prevent, on the axis that decides capability rather than autonomy.
    /// `#[serde(default)]` for back-compat with older bundles, which read as
    /// empty: the manifest's ceilings decide, exactly as before.
    #[serde(default)]
    overlay_desk_tools: std::collections::BTreeMap<String, Vec<String>>,
    /// The workflow ids switched off at export time (issue #276). Preserved so
    /// an export→import does not silently re-arm a schedule the operator had
    /// paused — which is the one direction this bundle must never move on its
    /// own. `#[serde(default)]` for back-compat with older bundles.
    #[serde(default)]
    disabled_workflows: Vec<String>,
    /// The source-template provenance, when the exported company carried one.
    /// `#[serde(default)]` keeps older bundles written before provenance existed
    /// importing cleanly (they decode to `None` — no migration).
    #[serde(default)]
    template_provenance: Option<TemplateProvenance>,
    /// What the operator told first-run setup about their business, carried
    /// through the bundle so an export→import keeps it — Phase 2 builds
    /// workflows from these answers, and a company that lost them on a restore
    /// would be asked to describe itself twice.
    /// `#[serde(default)]` keeps older bundles importing cleanly.
    #[serde(default)]
    setup: Option<crate::company::setup::SetupAnswers>,
    /// Whether the operator had confirmed the company's display name at export
    /// time (issue #1843). Preserved so an export→import does not silently
    /// re-open a confirmation step the operator already cleared.
    /// `#[serde(default)]` keeps older bundles importing cleanly (they decode
    /// to `false`, the pre-#1843 behaviour every such bundle already had).
    #[serde(default)]
    name_confirmed: bool,
    /// Epoch-millis the activation funnel completed at export time
    /// (issue #1843). Preserved for the same reason `overlay_policy` and
    /// `disabled_workflows` above are: without this, an export→import would
    /// silently re-gate an already-activated company behind onboarding.
    /// `#[serde(default)]` keeps older bundles importing cleanly (`None`).
    #[serde(default)]
    activation_completed_at: Option<u64>,
    /// Whether the source company had ever been saved by activation-aware
    /// code at export time (PR #1875 review finding). Preserved so import
    /// does not silently stamp a legacy pre-#1843 company — one whose gate
    /// was never seen — as activation-aware, which would block
    /// `RuntimeBuilder::build`'s grandfather back-fill on the very next boot
    /// and show an established operator the fresh-company onboarding gate.
    /// `#[serde(default)]` reads a bundle written before this field existed
    /// as `false`: exactly the legacy state such a bundle actually has.
    #[serde(default)]
    activation_gate_seen: bool,
    /// The company-wide `#general` channel. Absent on bundles written before
    /// it was stored; the builder backfills it on the imported company's boot.
    #[serde(default)]
    general_channel: Option<crate::ports::types::GeneralChannel>,
}

/// One exported context chunk: its content address, label, and body.
struct ExportedChunk {
    addr: String,
    label: String,
    body: String,
}

/// A context-index line pairing an address with its label and length. Matches the
/// fs [`ContextStore`] index shape.
#[derive(Serialize, Deserialize)]
struct IndexEntry {
    addr: String,
    label: String,
    len: usize,
}

/// Everything an export carries for one company, read through the ports.
struct BundleContents {
    id: CompanyId,
    manifest: CompanyManifest,
    lifecycle: String,
    template_provenance: Option<TemplateProvenance>,
    setup: Option<crate::company::setup::SetupAnswers>,
    ledger: Vec<LedgerEntry>,
    events: Vec<StoredEvent>,
    traces: Vec<CompressedTrace>,
    /// Traces retained in a provider's archive tier. Empty for base stores and
    /// bundles written before archive export was introduced.
    archived_traces: Vec<CompressedTrace>,
    /// Operator facts. Empty when the source served no fact port (an old
    /// bundle, or an export run without one) — never a failure.
    facts: Vec<FactRecord>,
    context: Vec<ExportedChunk>,
    /// The operator team overlay (operator-added teammates), carried through the
    /// bundle so export→import preserves the operator roster.
    overlay_agents: Vec<OverlayAgent>,
    /// The operator desk-membership overlay, carried through the bundle so
    /// export→import preserves operator-added desk memberships.
    overlay_desk_members: Vec<OverlayDeskMember>,
    /// The operator per-desk member-ordering overlay, carried through the bundle
    /// so export→import preserves the desk hierarchy (and routing lead).
    overlay_desk_order: Vec<OverlayDeskOrder>,
    /// The operator-created desk overlay, carried through the bundle so
    /// export→import preserves operator-created desks.
    overlay_desks: Vec<OverlayDesk>,
    /// The operator workflow-authoring overlay, carried through the bundle so
    /// export→import preserves console-created workflow graphs.
    overlay_workflows: Vec<OverlayWorkflow>,
    /// The operator-set per-teammate daily spend caps, carried through the
    /// bundle so export→import preserves console-set budgets (issue #343).
    overlay_budgets: Vec<BudgetOverride>,
    /// The operator's edits of manifest-declared teammates, carried through the
    /// bundle so export→import preserves a console-shaped roster.
    overlay_agent_edits: Vec<AgentOverride>,
    /// The move grammars installed on desks, carried through the bundle so
    /// export→import preserves how a desk deliberates.
    overlay_desk_hive: Vec<DeskHiveOverride>,
    /// The ids of manifest teammates the operator removed, carried through the
    /// bundle so an import does not restore them.
    overlay_retired_agents: Vec<String>,
    /// The operator's `[policy]` override, carried through the bundle so
    /// export→import preserves a console-set autonomy tier (issue #562).
    ///
    /// Without this an exported company would come back on the manifest's tier,
    /// silently re-tightening (or re-loosening) the approval gate on import —
    /// the same class of loss #343 fixed for spend caps.
    overlay_policy: Option<PolicyOverride>,
    /// The operator's console-added `[tools].allow` grants, carried through the
    /// bundle so export→import preserves an integration granted from a connect
    /// surface (rather than restoring it "Connected" and reaching nobody).
    overlay_tool_grants: Option<ToolGrantsOverride>,
    /// The operator-set per-desk tool ceilings, carried through the bundle so
    /// export→import preserves a console-narrowed department (rather than
    /// restoring it at the company's full grant).
    overlay_desk_tools: std::collections::BTreeMap<String, Vec<String>>,
    /// The workflow ids switched off, carried through the bundle so an import
    /// restores a paused workflow paused (issue #276).
    disabled_workflows: Vec<String>,
    /// Whether the operator had confirmed the company's display name
    /// (issue #1843), carried through the bundle so export→import preserves
    /// it.
    name_confirmed: bool,
    /// Epoch-millis the activation funnel completed (issue #1843), carried
    /// through the bundle so export→import does not silently re-gate an
    /// already-activated company behind onboarding.
    activation_completed_at: Option<u64>,
    /// Whether the source company had ever been saved by activation-aware
    /// code (PR #1875 review finding), carried through the bundle so import
    /// restores a legacy pre-#1843 company with its gate still unseen —
    /// otherwise `write_via_ports`'s save would stamp it seen on arrival and
    /// permanently block the grandfather back-fill for that company.
    activation_gate_seen: bool,
    /// The company-wide `#general` channel, carried so export→import keeps it.
    general_channel: Option<crate::ports::types::GeneralChannel>,
}

impl BundleContents {
    /// Reads the complete company state through the four durable ports.
    async fn read_via_ports(
        id: &CompanyId,
        store: Arc<dyn CompanyStore>,
        events: Arc<dyn EventLog>,
        memory: Arc<dyn MemoryStore>,
        context: Arc<dyn ContextStore>,
        facts: Option<Arc<dyn FactStore>>,
        scopes: Option<Arc<dyn MemoryScopes>>,
    ) -> Result<Self> {
        let record = store
            .load(id)
            .await?
            .ok_or_else(|| OpenCompanyError::CompanyNotFound(id.to_string()))?;
        // PR #1875 review finding: read alongside the record, not derived
        // from it — `CompanyRecord` carries no such field, only the store
        // does (see `CompanyStore::activation_gate_seen`'s doc comment).
        let activation_gate_seen = store.activation_gate_seen(id).await?;

        // Issue #358: the withdrawn half of a discussion never reaches the
        // bundle. This is the load-bearing half of that issue — hiding a
        // message on the console while the bundle keeps carrying it makes the
        // record *portable* instead of merely permanent, which is the worse
        // failure of the two.
        let events =
            scrub_redacted_discussion(events.read_from(id, EventSeq::new(0), usize::MAX).await?);
        let traces = memory.recent_traces(id, usize::MAX).await?;
        let archived_traces = match scopes {
            Some(scopes) => scopes.archived_traces(id).await?,
            None => Vec::new(),
        };
        let facts = match facts {
            Some(port) => port.list(id, None, None).await?,
            None => Vec::new(),
        };

        let metas = context.list(id, "").await?;
        let mut chunks = Vec::with_capacity(metas.len());
        for meta in metas {
            let body = context.peek(id, &meta.addr, None).await?;
            chunks.push(ExportedChunk {
                addr: meta.addr.as_ref().to_string(),
                label: meta.label,
                body,
            });
        }

        // Issue #1796: the bundle carries the **seed's** `[tools].allow`, not the
        // record's materialised one.
        //
        // `write_to_dir` serializes this manifest straight into the bundle's
        // `company.toml`, and that file BECOMES THE SEED for whatever host
        // serves the restored company. Writing the folded list there would hand
        // the next rebuild a seed that already grants `chargebee`, the carry
        // rule would correctly read that as "version control spoke" and drop the
        // override — and the console grant would have been silently promoted to
        // a manifest grant: attribution gone, and `DELETE …/tools/grants` unable
        // to reach it ever again. The override rides the bundle beside it, and
        // `restore_via_ports` re-folds, so the restored record is materialised
        // exactly as the builder would leave it.
        let mut manifest = record.manifest;
        manifest.tools.allow = crate::ports::types::seed_tool_allow(
            &manifest.tools.allow,
            record.overlay_tool_grants.as_ref(),
        );

        Ok(Self {
            id: id.clone(),
            manifest,
            lifecycle: record.lifecycle,
            template_provenance: record.template_provenance,
            setup: record.setup,
            ledger: record.ledger,
            events,
            traces,
            archived_traces,
            facts,
            context: chunks,
            overlay_agents: record.overlay_agents,
            overlay_desk_members: record.overlay_desk_members,
            overlay_desk_order: record.overlay_desk_order,
            overlay_desks: record.overlay_desks,
            overlay_workflows: record.overlay_workflows,
            overlay_budgets: record.overlay_budgets,
            overlay_agent_edits: record.overlay_agent_edits,
            overlay_desk_hive: record.overlay_desk_hive,
            overlay_retired_agents: record.overlay_retired_agents,
            overlay_policy: record.overlay_policy,
            overlay_tool_grants: record.overlay_tool_grants,
            overlay_desk_tools: record.overlay_desk_tools,
            disabled_workflows: record.disabled_workflows,
            name_confirmed: record.name_confirmed,
            activation_completed_at: record.activation_completed_at,
            activation_gate_seen,
            general_channel: Some(record.general_channel),
        })
    }

    /// Replays the complete company state through the four durable ports. Events
    /// are appended in order, so a fresh target log reproduces the original
    /// 0-based sequence numbers; context chunks re-derive their original content
    /// address from the body.
    async fn write_via_ports(
        &self,
        store: Arc<dyn CompanyStore>,
        events: Arc<dyn EventLog>,
        memory: Arc<dyn MemoryStore>,
        context: Arc<dyn ContextStore>,
        facts: Option<Arc<dyn FactStore>>,
        scopes: Option<Arc<dyn MemoryScopes>>,
    ) -> Result<()> {
        // Archived traces must remain in their recovery tier. Refuse before any
        // append-only writes when the import target cannot restore that tier.
        if !self.archived_traces.is_empty() && scopes.is_none() {
            return Err(OpenCompanyError::Store(format!(
                "bundle carries {} archived traces but the import target serves no archive tier",
                self.archived_traces.len()
            )));
        }
        // append-only, so a refusal after `store.save`/`append` would leave a
        // half-imported company whose retry duplicates history.
        if facts.is_none() && !self.facts.is_empty() {
            return Err(OpenCompanyError::Store(format!(
                "bundle carries {} operator facts but the import target serves no fact port",
                self.facts.len()
            )));
        }
        // Facts land FIRST, for the same append-only reason the refusal above
        // fires first: `upsert` is idempotent, so a failure here leaves a
        // retry-safe state — whereas a fact failure AFTER `store.save` and the
        // ledger/event appends would leave a half-import whose retry
        // duplicates history.
        if let Some(port) = &facts {
            for fact in &self.facts {
                port.upsert(&self.id, fact).await?;
            }
        }
        if let Some(scopes) = scopes {
            scopes
                .restore_archived_traces(&self.id, &self.archived_traces)
                .await?;
        }
        // The manifest + lifecycle; ledger is appended separately so the store's
        // append-only ledger stays authoritative.
        // The mirror of the strip in `read_via_ports`: the bundle holds the seed,
        // so the record written here is re-folded. Without it a restored company
        // would report its console grants as ungranted — and every reader of
        // `[tools].allow` would agree with that — until its first rebuild.
        let mut manifest = self.manifest.clone();
        manifest.tools.allow = crate::ports::types::effective_tool_allow(
            &manifest.tools.allow,
            self.overlay_tool_grants.as_ref(),
        );
        // `save_importing`, not `save`: this call is replaying a bundle's
        // prior state rather than a normal activation-aware write, so the
        // gate marker must land as `self.activation_gate_seen` — `false` for
        // a legacy pre-#1843 bundle — instead of unconditionally `true`
        // (PR #1875 review finding; see `CompanyStore::save_importing`'s doc
        // comment for the full reasoning).
        store
            .save_importing(
                &CompanyRecord {
                    overlay_agent_edits: self.overlay_agent_edits.clone(),
                    overlay_desk_hive: self.overlay_desk_hive.clone(),
                    overlay_retired_agents: self.overlay_retired_agents.clone(),
                    id: self.id.clone(),
                    manifest,
                    ledger: Vec::new(),
                    lifecycle: self.lifecycle.clone(),
                    overlay_agents: self.overlay_agents.clone(),
                    overlay_desk_members: self.overlay_desk_members.clone(),
                    overlay_desk_order: self.overlay_desk_order.clone(),
                    overlay_desks: self.overlay_desks.clone(),
                    overlay_workflows: self.overlay_workflows.clone(),
                    overlay_budgets: self.overlay_budgets.clone(),
                    overlay_policy: self.overlay_policy.clone(),
                    overlay_tool_grants: self.overlay_tool_grants.clone(),
                    overlay_desk_tools: self.overlay_desk_tools.clone(),
                    disabled_workflows: self.disabled_workflows.clone(),
                    template_provenance: self.template_provenance.clone(),
                    setup: self.setup.clone(),
                    name_confirmed: self.name_confirmed,
                    activation_completed_at: self.activation_completed_at,
                    // Bundle export/import never carries a creation timestamp
                    // through (`BundleMeta`/`BundleContents` have no
                    // `created_at_millis` field) — `None` here matches every
                    // other `CompanyRecord` this module constructs.
                    created_at_millis: None,
                    general_channel: self.general_channel.clone().unwrap_or_default(),
                },
                self.activation_gate_seen,
            )
            .await?;
        for entry in &self.ledger {
            store.append_ledger(&self.id, entry.clone()).await?;
        }
        for stored in &self.events {
            events.append(&self.id, stored.event.clone()).await?;
        }
        for trace in &self.traces {
            memory.save_trace(&self.id, trace.clone()).await?;
        }
        for chunk in &self.context {
            context
                .put(
                    &self.id,
                    ContextChunk {
                        label: chunk.label.clone(),
                        body: chunk.body.clone(),
                    },
                )
                .await?;
        }
        Ok(())
    }

    /// Writes the canonical fs bundle layout under `dest`.
    async fn write_to_dir(&self, dest: &Path) -> Result<()> {
        create_dir(dest).await?;

        let toml_src = toml::to_string(&self.manifest)
            .map_err(|e| OpenCompanyError::Store(format!("cannot serialize manifest: {e}")))?;
        write_file(&dest.join(COMPANY_TOML), toml_src.as_bytes()).await?;

        let meta = BundleMeta {
            lifecycle: self.lifecycle.clone(),
            id: self.id.as_ref().to_string(),
            overlay_agents: self.overlay_agents.clone(),
            overlay_desk_members: self.overlay_desk_members.clone(),
            overlay_desk_order: self.overlay_desk_order.clone(),
            overlay_desks: self.overlay_desks.clone(),
            overlay_workflows: self.overlay_workflows.clone(),
            overlay_budgets: self.overlay_budgets.clone(),
            overlay_agent_edits: self.overlay_agent_edits.clone(),
            overlay_desk_hive: self.overlay_desk_hive.clone(),
            overlay_retired_agents: self.overlay_retired_agents.clone(),
            overlay_policy: self.overlay_policy.clone(),
            overlay_tool_grants: self.overlay_tool_grants.clone(),
            overlay_desk_tools: self.overlay_desk_tools.clone(),
            disabled_workflows: self.disabled_workflows.clone(),
            template_provenance: self.template_provenance.clone(),
            setup: self.setup.clone(),
            name_confirmed: self.name_confirmed,
            activation_completed_at: self.activation_completed_at,
            activation_gate_seen: self.activation_gate_seen,
            general_channel: self.general_channel.clone(),
        };
        write_file(
            &dest.join(META_JSON),
            serde_json::to_string(&meta)?.as_bytes(),
        )
        .await?;

        write_file(&dest.join(LEDGER_JSONL), jsonl(&self.ledger)?.as_bytes()).await?;
        write_file(&dest.join(EVENTS_JSONL), jsonl(&self.events)?.as_bytes()).await?;

        let memory_dir = dest.join(MEMORY_DIR);
        create_dir(&memory_dir).await?;
        write_file(
            &memory_dir.join(TRACES_JSONL),
            jsonl(&self.traces)?.as_bytes(),
        )
        .await?;
        if !self.archived_traces.is_empty() {
            write_file(
                &memory_dir.join(ARCHIVES_JSONL),
                jsonl(&self.archived_traces)?.as_bytes(),
            )
            .await?;
        } else {
            match tokio::fs::remove_file(memory_dir.join(ARCHIVES_JSONL)).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(OpenCompanyError::Store(format!(
                        "cannot remove a stale archive file from the bundle: {e}"
                    )));
                }
            }
        }
        // Only when there are any: an empty file would make every new export
        // differ from an old host's byte-for-byte for no information. At the
        // bundle root, matching `paths::Bundle::facts_jsonl`. A factless
        // export must also REMOVE a stale file a previous export left in the
        // same directory — otherwise a later import resurrects facts that are
        // absent from the selected source.
        if !self.facts.is_empty() {
            write_file(&dest.join(FACTS_JSONL), jsonl(&self.facts)?.as_bytes()).await?;
        } else {
            match tokio::fs::remove_file(dest.join(FACTS_JSONL)).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(OpenCompanyError::Store(format!(
                        "cannot remove a stale facts file from the bundle: {e}"
                    )));
                }
            }
        }

        let context_dir = dest.join(CONTEXT_DIR);
        let blobs_dir = context_dir.join(CONTEXT_BLOBS_DIR);
        create_dir(&blobs_dir).await?;
        let index: Vec<IndexEntry> = self
            .context
            .iter()
            .map(|c| IndexEntry {
                addr: c.addr.clone(),
                label: c.label.clone(),
                len: c.body.len(),
            })
            .collect();
        write_file(
            &context_dir.join(CONTEXT_INDEX_JSONL),
            jsonl(&index)?.as_bytes(),
        )
        .await?;
        for chunk in &self.context {
            write_file(&blobs_dir.join(&chunk.addr), chunk.body.as_bytes()).await?;
        }
        Ok(())
    }

    /// Reads a bundle directory (the inverse of [`Self::write_to_dir`]).
    async fn read_from_dir(src: &Path) -> Result<Self> {
        let toml_path = src.join(COMPANY_TOML);
        let toml_src = read_to_string(&toml_path).await?;
        let manifest: CompanyManifest = toml::from_str(&toml_src)
            .map_err(|e| OpenCompanyError::Store(format!("invalid {COMPANY_TOML}: {e}")))?;

        let meta: BundleMeta = serde_json::from_str(&read_to_string(&src.join(META_JSON)).await?)?;

        // A bundle is the one place `overlay_budgets` arrives from outside this
        // process, so it is the one place the "at most one override per teammate"
        // invariant can be violated by data we did not write. Refuse rather than
        // resolve: `CompanyRecord::effective_budget` reads the first match, so
        // importing two rows for one teammate would apply whichever the bundle
        // happened to serialize first — possibly the obsolete one, possibly the
        // looser one, and with somebody else's name on the attribution. A bundle
        // that disagrees with itself about a spend cap has no right answer to
        // pick, and picking silently is how a revoked allowance comes back.
        if let Some(agent_id) = BudgetOverride::duplicate_agent_id(&meta.overlay_budgets) {
            return Err(OpenCompanyError::Store(format!(
                "invalid {META_JSON}: {} carries more than one budget override for teammate \
                 '{agent_id}'; at most one is allowed",
                meta.id
            )));
        }
        // The roster edits carry the same invariant for the same reason, and are
        // checked in the same breath: `CompanyRecord::agent_override` also reads
        // the first match, so two rows for one teammate would apply whichever the
        // bundle happened to serialize first — restoring a name the operator
        // changed, or a tool grant they narrowed, with nothing to say which row
        // won. Both refusals fire before any port is written, so a rejected
        // bundle leaves the target untouched.
        if let Some(agent_id) = AgentOverride::duplicate_agent_id(&meta.overlay_agent_edits) {
            return Err(OpenCompanyError::Store(format!(
                "invalid {META_JSON}: {} carries more than one edit for teammate \
                 '{agent_id}'; at most one is allowed",
                meta.id
            )));
        }

        let ledger = read_jsonl::<LedgerEntry>(&src.join(LEDGER_JSONL)).await?;
        // Scrubbed on the way IN as well as on the way out (issue #358), which
        // is not belt-and-braces: a bundle written by a host that predates this
        // carries the withdrawn text beside its tombstone, and importing it
        // as-is would write that text into a fresh journal — the resurrection
        // the issue names, arriving through the one door the exporter cannot
        // guard.
        let events =
            scrub_redacted_discussion(read_jsonl::<StoredEvent>(&src.join(EVENTS_JSONL)).await?);
        let traces =
            read_jsonl::<CompressedTrace>(&src.join(MEMORY_DIR).join(TRACES_JSONL)).await?;
        let archived_traces =
            read_jsonl::<CompressedTrace>(&src.join(MEMORY_DIR).join(ARCHIVES_JSONL)).await?;
        // Absent on bundles that predate facts-in-the-bundle: empty, not an error.
        let facts = read_jsonl::<FactRecord>(&src.join(FACTS_JSONL)).await?;

        let context_dir = src.join(CONTEXT_DIR);
        let index = read_jsonl::<IndexEntry>(&context_dir.join(CONTEXT_INDEX_JSONL)).await?;
        let blobs_dir = context_dir.join(CONTEXT_BLOBS_DIR);
        let mut context = Vec::with_capacity(index.len());
        for entry in index {
            let body = read_to_string(&blobs_dir.join(&entry.addr)).await?;
            context.push(ExportedChunk {
                addr: entry.addr,
                label: entry.label,
                body,
            });
        }

        Ok(Self {
            id: CompanyId::new(meta.id),
            manifest,
            lifecycle: meta.lifecycle,
            template_provenance: meta.template_provenance,
            setup: meta.setup,
            ledger,
            events,
            traces,
            archived_traces,
            facts,
            context,
            overlay_agents: meta.overlay_agents,
            overlay_desk_members: meta.overlay_desk_members,
            overlay_desk_order: meta.overlay_desk_order,
            overlay_desks: meta.overlay_desks,
            overlay_workflows: meta.overlay_workflows,
            overlay_budgets: meta.overlay_budgets,
            overlay_agent_edits: meta.overlay_agent_edits,
            overlay_desk_hive: meta.overlay_desk_hive,
            overlay_retired_agents: meta.overlay_retired_agents,
            overlay_policy: meta.overlay_policy,
            overlay_tool_grants: meta.overlay_tool_grants,
            overlay_desk_tools: meta.overlay_desk_tools,
            disabled_workflows: meta.disabled_workflows,
            name_confirmed: meta.name_confirmed,
            activation_completed_at: meta.activation_completed_at,
            activation_gate_seen: meta.activation_gate_seen,
            general_channel: meta.general_channel,
        })
    }
}

/// Replaces the text of every discussion post a later tombstone withdrew
/// (issue #358).
///
/// ## Why the bundle is where this matters most
///
/// A withdrawal that only affected the console would leave the message in
/// `events.jsonl`, and the bundle is the copy that *leaves the instance* — it
/// is handed to support, restored onto a laptop, committed to a repository. So
/// a redaction that stops at the read fold does not make a pasted credential
/// less exposed; it makes it exposed somewhere nobody is looking.
///
/// ## What it does
///
/// Walks the log once, collecting the `(task_id, seq)` pairs named by
/// [`CompanyEvent::TaskDiscussionRedacted`], then rewrites the `text` of each
/// post they name to
/// [`REDACTED_DISCUSSION_TEXT`](crate::ports::tasks::REDACTED_DISCUSSION_TEXT).
/// Two passes rather than one because a tombstone always follows its post, so a
/// single forward pass would have already written the post out.
///
/// **The tombstone itself is kept.** Dropping it would leave the imported
/// company with a post whose text is a placeholder and no record of why, and
/// the fold would show it as an ordinary message reading "This message was
/// removed." — a sentence nobody wrote. Carried through, the imported thread
/// says the same thing the exporting one did, with the same attribution.
///
/// Every other event passes through untouched, including posts with no
/// tombstone: this is a substitution, not a filter, so the log's shape,
/// ordering and sequence numbering are exactly what they were.
fn scrub_redacted_discussion(events: Vec<StoredEvent>) -> Vec<StoredEvent> {
    use std::collections::HashSet;

    let withdrawn: HashSet<(String, u64)> = events
        .iter()
        .filter_map(|stored| match &stored.event {
            CompanyEvent::TaskDiscussionRedacted { task_id, seq, .. } => {
                Some((task_id.clone(), *seq))
            }
            _ => None,
        })
        .collect();
    if withdrawn.is_empty() {
        return events;
    }

    events
        .into_iter()
        .map(|mut stored| {
            if let CompanyEvent::TaskDiscussionPosted { task_id, text, .. } = &mut stored.event
                && withdrawn.contains(&(task_id.clone(), stored.seq.value()))
            {
                *text = crate::ports::tasks::REDACTED_DISCUSSION_TEXT.to_string();
            }
            stored
        })
        .collect()
}

/// Exports `id`'s complete state through the ports into an unpacked bundle
/// directory at `dest`.
///
/// Total by construction: every port is drained (`read_from(0, MAX)`,
/// `recent_traces(MAX)`, `list("")` + `peek`), so an export never depends on a
/// backend's private on-disk shape. When [`ExportOpts::include_secrets`] is set
/// and [`ExportOpts::fs_bundle`] points at the source fs bundle, the fs-only
/// `secrets/` and `keys/` directories are copied verbatim.
// Eight arguments is over clippy's default ceiling, taken knowingly: five of
// them are the durable ports, and folding them into a struct is a wider
// refactor than this addition warrants (the repo carries the same allow at
// its other port-heavy seams).
#[allow(clippy::too_many_arguments)]
pub async fn export_bundle(
    id: &CompanyId,
    dest: &Path,
    store: Arc<dyn CompanyStore>,
    events: Arc<dyn EventLog>,
    memory: Arc<dyn MemoryStore>,
    context: Arc<dyn ContextStore>,
    facts: Option<Arc<dyn FactStore>>,
    opts: ExportOpts,
) -> Result<()> {
    export_bundle_with_scopes(id, dest, store, events, memory, context, facts, None, opts).await
}

/// Exports a bundle while preserving an optional provider archive tier.
#[allow(clippy::too_many_arguments)]
pub async fn export_bundle_with_scopes(
    id: &CompanyId,
    dest: &Path,
    store: Arc<dyn CompanyStore>,
    events: Arc<dyn EventLog>,
    memory: Arc<dyn MemoryStore>,
    context: Arc<dyn ContextStore>,
    facts: Option<Arc<dyn FactStore>>,
    scopes: Option<Arc<dyn MemoryScopes>>,
    opts: ExportOpts,
) -> Result<()> {
    let contents =
        BundleContents::read_via_ports(id, store, events, memory, context, facts, scopes).await?;
    contents.write_to_dir(dest).await?;

    if opts.include_secrets
        && let Some(src_bundle) = &opts.fs_bundle
    {
        for sub in [SECRETS_DIR, KEYS_DIR] {
            copy_dir(&src_bundle.join(sub), &dest.join(sub)).await?;
        }
    }
    Ok(())
}

/// Imports a bundle directory at `src` through the target ports, returning the
/// restored company id.
///
/// The inverse of [`export_bundle`] for the port-driven records: the manifest,
/// lifecycle, ledger, events, traces, and context are replayed through the
/// supplied ports. `secrets/`/`keys/` are fs artifacts restored separately via
/// [`restore_fs_artifacts`].
pub async fn import_bundle(
    src: &Path,
    store: Arc<dyn CompanyStore>,
    events: Arc<dyn EventLog>,
    memory: Arc<dyn MemoryStore>,
    context: Arc<dyn ContextStore>,
    facts: Option<Arc<dyn FactStore>>,
) -> Result<CompanyId> {
    import_bundle_with_scopes(src, store, events, memory, context, facts, None).await
}

/// Imports a bundle while restoring its optional provider archive tier.
pub async fn import_bundle_with_scopes(
    src: &Path,
    store: Arc<dyn CompanyStore>,
    events: Arc<dyn EventLog>,
    memory: Arc<dyn MemoryStore>,
    context: Arc<dyn ContextStore>,
    facts: Option<Arc<dyn FactStore>>,
    scopes: Option<Arc<dyn MemoryScopes>>,
) -> Result<CompanyId> {
    let contents = BundleContents::read_from_dir(src).await?;
    let id = contents.id.clone();
    contents
        .write_via_ports(store, events, memory, context, facts, scopes)
        .await?;
    Ok(id)
}

/// Copies the fs-only `secrets/` and `keys/` directories from an imported bundle
/// at `src` into the live fs bundle directory `dest_bundle_dir`, if present.
///
/// A no-op for subdirectories the bundle did not carry (the common case, since
/// they are excluded from exports by default).
pub async fn restore_fs_artifacts(src: &Path, dest_bundle_dir: &Path) -> Result<()> {
    for sub in [SECRETS_DIR, KEYS_DIR] {
        let from = src.join(sub);
        if tokio::fs::metadata(&from).await.is_ok() {
            copy_dir(&from, &dest_bundle_dir.join(sub)).await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Directory helpers
// ---------------------------------------------------------------------------

async fn create_dir(dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| io_err(dir, e))
}

async fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    tokio::fs::write(path, bytes)
        .await
        .map_err(|e| io_err(path, e))
}

async fn read_to_string(path: &Path) -> Result<String> {
    tokio::fs::read_to_string(path)
        .await
        .map_err(|e| io_err(path, e))
}

/// Serializes a slice as newline-delimited JSON (one value per line).
fn jsonl<T: Serialize>(items: &[T]) -> Result<String> {
    let mut out = String::new();
    for item in items {
        out.push_str(&serde_json::to_string(item)?);
        out.push('\n');
    }
    Ok(out)
}

/// Parses every non-empty JSONL line of `path`, skipping an absent file.
///
/// **Import stays strict, by decision** (issue #387). The boot path now tolerates
/// a damaged ledger line — see
/// [`read_jsonl_lenient`](crate::store::fs::read_jsonl_lenient) — and this reader
/// deliberately does not follow it. The two are not the same situation:
///
/// * Boot has no alternative. The bundle is the company's only copy, refusing to
///   read it strands the tenant, and skipping keeps the bytes on disk for repair.
/// * Import does have one. The bundle being read is an *incoming* archive whose
///   source still exists, and refusing it costs nothing but a retry with a good
///   bundle. Half-importing instead would mint a company whose ledger silently
///   disagrees with the archive it claims to be, with no record of what was
///   dropped — an inconsistency that outlives the damaged file.
///
/// So a corrupt archive fails the import outright. That is the correct answer
/// here, and it should not be "fixed" to match the boot path.
async fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let contents = match tokio::fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(path, e)),
    };
    let mut out = Vec::new();
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}

/// Recursively copies `from` into `to`. A no-op when `from` does not exist.
async fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    if tokio::fs::metadata(from).await.is_err() {
        return Ok(());
    }
    create_dir(to).await?;
    let mut entries = tokio::fs::read_dir(from)
        .await
        .map_err(|e| io_err(from, e))?;
    while let Some(entry) = entries.next_entry().await.map_err(|e| io_err(from, e))? {
        let path = entry.path();
        let dest = to.join(entry.file_name());
        let file_type = entry.file_type().await.map_err(|e| io_err(&path, e))?;
        if file_type.is_dir() {
            Box::pin(copy_dir(&path, &dest)).await?;
        } else {
            tokio::fs::copy(&path, &dest)
                .await
                .map_err(|e| io_err(&path, e))?;
        }
    }
    Ok(())
}

/// Locates the bundle root under `dir`: `dir` itself when it holds a
/// `company.toml`, else the single immediate subdirectory that does (as produced
/// by [`pack_tar`], which nests the bundle under a top-level slug directory).
pub fn find_bundle_root(dir: &Path) -> Result<PathBuf> {
    if dir.join(COMPANY_TOML).is_file() {
        return Ok(dir.to_path_buf());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| io_err(dir, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| io_err(dir, e))?;
        let path = entry.path();
        if path.join(COMPANY_TOML).is_file() {
            return Ok(path);
        }
    }
    Err(OpenCompanyError::Store(format!(
        "no {COMPANY_TOML} found under {}",
        dir.display()
    )))
}

// ---------------------------------------------------------------------------
// Tar wrapper (feature `export`)
// ---------------------------------------------------------------------------

/// Packs an unpacked bundle directory into a single `.tar` at `out`.
///
/// The bundle is nested under a top-level directory named after `bundle_dir`, so
/// [`unpack_tar`] followed by [`find_bundle_root`] recovers it unambiguously.
#[cfg(feature = "export")]
pub fn pack_tar(bundle_dir: &Path, out: &Path) -> Result<()> {
    let file = std::fs::File::create(out).map_err(|e| io_err(out, e))?;
    let mut builder = tar::Builder::new(file);
    let top = bundle_dir
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_else(|| std::ffi::OsString::from("bundle"));
    builder
        .append_dir_all(&top, bundle_dir)
        .map_err(|e| io_err(bundle_dir, e))?;
    builder.finish().map_err(|e| io_err(out, e))?;
    Ok(())
}

/// Unpacks a `.tar` produced by [`pack_tar`] into `dest`.
#[cfg(feature = "export")]
pub fn unpack_tar(tar_path: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest).map_err(|e| io_err(dest, e))?;
    let file = std::fs::File::open(tar_path).map_err(|e| io_err(tar_path, e))?;
    let mut archive = tar::Archive::new(file);
    archive.unpack(dest).map_err(|e| io_err(dest, e))?;
    Ok(())
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "export_budget_tests.rs"]
mod tests_budget;
#[cfg(test)]
#[path = "export_bundle_tests.rs"]
mod tests_bundle;
