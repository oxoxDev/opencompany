//! Turning a company's declared `[[harness]]` set into the engines that serve
//! it.
//!
//! One place decides, for every declared harness, whether this host can run it
//! and what runs it — so the runtime builder does not grow a second opinion
//! about which agent lands where.
//!
//! ## One pool per `built_in` harness
//!
//! Each `built_in` harness gets its own [`HarnessPool`] and its own
//! [`HarnessDeps`], differing in exactly two fields: the provider (scoped to
//! that harness's config and credential slots) and
//! [`serves`](HarnessDeps::serves), which narrows the pool to the agents bound
//! to it.
//!
//! The narrowing is what makes one-pool-per-harness affordable. Without it every
//! pool would build every agent, so a ten-agent roster across three harnesses
//! would stand up thirty live agents — each holding a model client — to use ten.
//!
//! ## What is declared but not runnable
//!
//! An `acp` harness has no engine here yet: its transports live in the desktop
//! shell and the runner lane, and neither is wired into the server build. Rather
//! than silently routing those agents somewhere else, the harness is recorded as
//! unavailable with the reason, and a turn bound to it fails saying so. Falling
//! back would be the worst outcome available — the turn would succeed, on a
//! model and a credential nobody chose.
//!
//! This applies to the **default** harness exactly as much as a named one
//! (issue #1244). It used to not: every caller built the default lane straight
//! from `HarnessDeps`/`HarnessPool` on its own, without ever asking what kind
//! the default harness actually was, so a company whose *only* declared
//! harness was `kind = "acp"` still ran on the embedded engine — a silent
//! fallback of exactly the kind this module's own doctrine forbids. Resolving
//! the default the same way as every other harness, in this one place, is what
//! closes that gap for good instead of leaving a second opinion for a future
//! caller to reintroduce.
//!
//! ## `local` acp harnesses, when a factory is wired (issue #1245)
//!
//! `transport = "local"` now has a real engine wherever the caller supplies an
//! [`AcpAgentFactory`](crate::harness::acp::run_turn::AcpAgentFactory) — the
//! desktop shell, which owns the only implementation this crate does not
//! provide itself. A server build, or a desktop build asked to run a `runner`
//! harness (its socket transport is still unwired), passes `None`/leaves it
//! `unavailable` exactly as before.

use std::collections::HashSet;
use std::sync::Arc;

use crate::company::Harness;
use crate::company::inference::{EnvDefault, HarnessScope};
use crate::harness::built_in::provider::TenantProvider;
use crate::harness::built_in::run_turn::HarnessRunTurn;
use crate::harness::built_in::{HarnessDeps, HarnessPool};
use crate::ports::SecretStore;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::delegation::RunTurn;

/// The type `build`'s `acp_agents` parameter takes. Real under `acp`
/// (`crate::harness::acp::run_turn` — the `AcpAgent`/`AcpRunTurn` types — only
/// exists there); an uninhabited placeholder otherwise, so callers built
/// under plain `openhuman` (no `acp`) still compile and simply can never pass
/// `Some`.
#[cfg(feature = "acp")]
pub type AcpFactory<'a> = &'a dyn crate::harness::acp::run_turn::AcpAgentFactory;
#[cfg(not(feature = "acp"))]
pub type AcpFactory<'a> = &'a std::convert::Infallible;

/// Why a declared harness of `kind` has no engine on this host — the one
/// message both the default-harness path and the named-harness loop use, so
/// they cannot drift into saying different things about the same gap.
fn unavailable_reason(kind: &str) -> String {
    match kind {
        "acp" => "it is an ACP harness and this build has no ACP transport wired — \
                  run it from the desktop app, or bind these agents to a `built_in` harness"
            .to_string(),
        other => format!("`{other}` is not a harness kind this build knows how to run"),
    }
}

/// Resolves one `kind = "acp"` harness to an engine, or records why it has
/// none. Shared by the default-harness resolution and the named-harness loop
/// so the two cannot describe the same gap differently.
#[cfg(feature = "acp")]
fn resolve_acp_engine(
    harness: &Harness,
    acp_agents: Option<AcpFactory<'_>>,
    workspace_root: &std::path::Path,
) -> std::result::Result<Arc<dyn RunTurn>, String> {
    // Validation guarantees `acp` is `Some` and `transport` is one of
    // `ACP_TRANSPORTS` on every harness that reaches here — this crate's own
    // `CompanyManifest::validate`, not a caller-supplied invariant.
    let acp = harness
        .acp
        .as_ref()
        .ok_or_else(|| unavailable_reason("acp"))?;

    if acp.transport != "local" {
        // `runner` (a remote socket dispatch) has no engine on any build yet —
        // a materially different, larger piece of work than the local
        // subprocess case, and out of scope here.
        return Err(
            "it uses `transport = \"runner\"` and this build has no runner transport wired yet"
                .to_string(),
        );
    }

    let factory = acp_agents.ok_or_else(|| unavailable_reason("acp"))?;
    let agent_id = acp.agent.as_deref().unwrap_or_default();
    factory
        .build(agent_id, acp.model.as_deref(), workspace_root)
        .map(|agent| {
            Arc::new(crate::harness::acp::run_turn::AcpRunTurn::new(agent)) as Arc<dyn RunTurn>
        })
        .map_err(|error| format!("`{agent_id}` could not be started: {error}"))
}

/// The `openhuman`-without-`acp` build: unconditionally unavailable, exactly
/// as every `acp` harness was before issue #1245 — `acp_agents` can only ever
/// be `None` here (its type is uninhabited), so there is nothing to build.
#[cfg(not(feature = "acp"))]
fn resolve_acp_engine(
    _harness: &Harness,
    _acp_agents: Option<AcpFactory<'_>>,
    _workspace_root: &std::path::Path,
) -> std::result::Result<Arc<dyn RunTurn>, String> {
    Err(unavailable_reason("acp"))
}

/// The engines a company's declared harnesses resolve to on this host.
pub struct Lanes {
    /// Agents the **default** harness serves, when the company declares more
    /// than one. `None` means the whole roster — the single-harness case.
    pub default_serves: Option<HashSet<String>>,
    /// The engine for the default harness itself, when this host can run it.
    ///
    /// `None` if and only if the default harness's id has a matching entry in
    /// `unavailable` — callers must not substitute another engine in that
    /// case; see the module docs.
    pub default_engine: Option<Arc<dyn RunTurn>>,
    /// Every lane beyond the default: its harness id and the engine serving it.
    pub lanes: Vec<(String, Arc<dyn RunTurn>)>,
    /// Declared harnesses this host cannot run, and why. Includes the default
    /// harness's own id when `default_engine` is `None`.
    pub unavailable: Vec<(String, String)>,
}

/// Which agents are bound to `harness_id`, given the company's default.
fn agents_on(record: &CompanyRecord, harness_id: &str, default_harness: &str) -> HashSet<String> {
    let mut ids: HashSet<String> = record
        .manifest
        .agents
        .iter()
        .filter(|a| a.harness.as_deref().unwrap_or(default_harness) == harness_id)
        .map(|a| a.id.clone())
        .collect();
    // A console-created (overlay) teammate has no manifest row and therefore no
    // harness binding — `overlay_agent_to_manifest` hardcodes `harness: None` —
    // so every overlay runs on the default harness. Fold them into the default
    // lane's serve set, or a multi-harness company would build them on no pool
    // at all: the default pool's `serves` would exclude them, no other lane
    // claims them, and the roster would silently drop a teammate the console
    // is still showing.
    if harness_id == default_harness {
        ids.extend(record.overlay_agents.iter().map(|a| a.id.clone()));
    }
    ids
}

/// Builds the lanes for `record`, given the shared pool and deps the
/// **default** harness runs on when it is runnable at all.
///
/// `default_serves` is `None` — "the whole roster, no narrowing" — for a
/// company that declares no `[[harness]]` (or declares exactly one): the
/// byte-identical single-pool path every existing company takes. That stays
/// true regardless of whether the default harness turns out to be runnable;
/// what changed (issue #1244) is that `default_engine`/`unavailable` are now
/// always resolved too, instead of every caller resolving the default
/// separately (and inconsistently) on its own.
pub fn build(
    record: &CompanyRecord,
    pool: Arc<HarnessPool>,
    base: &HarnessDeps,
    secrets: Arc<dyn SecretStore>,
    env_default: Option<EnvDefault>,
    acp_agents: Option<AcpFactory<'_>>,
) -> Lanes {
    let declared = record.manifest.effective_harnesses();
    let default_harness = record.manifest.default_harness();
    let default_harness_id = default_harness.id.clone();

    let mut lanes = Vec::new();
    let mut unavailable = Vec::new();

    let default_engine = match default_harness.kind.as_str() {
        // The base deps already resolved the default's own `[harness.inference]`
        // precedence (`default_harness_inference`) before this was called —
        // wrap them in the caller's shared pool exactly as it always did.
        "built_in" => {
            Some(Arc::new(HarnessRunTurn::new(pool, Arc::new(base.clone()))) as Arc<dyn RunTurn>)
        }
        "acp" => match resolve_acp_engine(&default_harness, acp_agents, &base.workspace_root) {
            Ok(engine) => Some(engine),
            Err(reason) => {
                unavailable.push((default_harness_id.clone(), reason));
                None
            }
        },
        kind => {
            unavailable.push((default_harness_id.clone(), unavailable_reason(kind)));
            None
        }
    };

    for harness in declared.iter().filter(|h| h.id != default_harness_id) {
        match harness.kind.as_str() {
            "built_in" => lanes.push((
                harness.id.clone(),
                built_in_lane(
                    record,
                    base,
                    &secrets,
                    env_default.clone(),
                    harness,
                    &default_harness_id,
                ),
            )),
            "acp" => match resolve_acp_engine(harness, acp_agents, &base.workspace_root) {
                Ok(engine) => lanes.push((harness.id.clone(), engine)),
                Err(reason) => unavailable.push((harness.id.clone(), reason)),
            },
            kind => unavailable.push((harness.id.clone(), unavailable_reason(kind))),
        }
    }

    let default_serves = if declared.len() <= 1 {
        None
    } else {
        Some(agents_on(record, &default_harness_id, &default_harness_id))
    };

    Lanes {
        default_serves,
        default_engine,
        lanes,
        unavailable,
    }
}

/// One `built_in` lane: its own pool, over deps carrying its own provider and
/// narrowed to the agents bound to it.
fn built_in_lane(
    record: &CompanyRecord,
    base: &HarnessDeps,
    secrets: &Arc<dyn SecretStore>,
    env_default: Option<EnvDefault>,
    harness: &Harness,
    default_harness: &str,
) -> Arc<dyn RunTurn> {
    // Its own `[harness.inference]`, else the company-level `[inference]` — the
    // caller cannot pick, because only the harness knows whether it declared
    // one.
    let manifest_inference = harness
        .inference
        .clone()
        .unwrap_or_else(|| record.manifest.inference.clone());

    let provider = Arc::new(
        TenantProvider::new(
            record.id.clone(),
            secrets.clone(),
            manifest_inference,
            env_default,
        )
        .with_scope(HarnessScope::named(&harness.id)),
    );

    let mut deps = base.clone();
    deps.provider = provider;
    deps.serves = Some(agents_on(record, &harness.id, default_harness));

    Arc::new(HarnessRunTurn::new(
        Arc::new(HarnessPool::new()),
        Arc::new(deps),
    ))
}

/// The company id a lane set was built for. Exposed so a caller can assert it
/// wired the lanes it thinks it did.
pub fn company_of(record: &CompanyRecord) -> &CompanyId {
    &record.id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::types::OverlayAgent;

    /// A two-harness company with a console-created overlay teammate.
    fn record() -> CompanyRecord {
        let manifest: crate::company::CompanyManifest = toml::from_str(
            r#"
[company]
name = "Acme"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[agent]]
id = "researcher"
role = "Researcher"
harness = "deep"

[[harness]]
id = "embedded"
kind = "built_in"
default = true

[[harness]]
id = "deep"
kind = "built_in"
"#,
        )
        .expect("valid manifest");
        CompanyRecord {
            id: CompanyId::new("acme"),
            manifest,
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: vec![OverlayAgent {
                id: "writer".into(),
                name: "Writer".into(),
                role: "Content Writer".into(),
                description: None,
                tools: Vec::new(),
            }],
            overlay_desk_members: Vec::new(),
            overlay_desk_order: Vec::new(),
            overlay_desks: Vec::new(),
            overlay_workflows: Vec::new(),
            overlay_budgets: Vec::new(),
            overlay_policy: None,
            overlay_desk_tools: Default::default(),
            disabled_workflows: Vec::new(),
            template_provenance: None,
            setup: None,
        }
    }

    /// The default lane serves the whole default-bound roster **including**
    /// every overlay teammate, whose only harness is the default.
    #[test]
    fn the_default_lane_serves_every_overlay_agent() {
        let rec = record();
        let default = agents_on(&rec, "embedded", "embedded");
        assert!(default.contains("ceo"));
        assert!(!default.contains("researcher"), "bound to the deep lane");
        assert!(
            default.contains("writer"),
            "a console-created teammate runs on the default harness"
        );

        // And the named lane must not claim it — the overlay is nobody's but
        // the default's.
        let deep = agents_on(&rec, "deep", "embedded");
        assert!(deep.contains("researcher"));
        assert!(!deep.contains("writer"));
        assert!(!deep.contains("ceo"));
    }
}
