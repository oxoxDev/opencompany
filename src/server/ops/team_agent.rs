//! One agent, opened: the detail read and the edit behind `GET`/`PATCH
//! {scope}/team/{agent_id}` (issue #264).
//!
//! Before this, an agent was a dead end. `GET …/team` returned a name, a role
//! and a description; nothing carried the agent's tier, its tool grants or its
//! desks, and there was no per-agent route at all. So the console could show a
//! card and offer to delete it, and that was the whole of what an operator
//! could learn or do. Worse than the missing screen: **checking what tools a
//! company actually grants an agent had no read surface anywhere**, which is
//! why a tool-grant change could not be verified from outside the process.
//!
//! ## Effective, not declared
//!
//! [`AgentToolsDto`] carries three lists rather than one, because the
//! interesting number is the one nobody could see. `requested` is what the
//! `[[agent]].tools` line asks for, `companyAllow` is the `[tools].allow`
//! ceiling it is intersected with, and `effective` is what the agent actually
//! ends up holding. An agent that requests `workspace.read` under a company
//! that allows only `composio` requests one tool and holds none, and a surface
//! that printed the request alone would report the opposite of the truth.
//!
//! `effective` is computed by
//! [`agent_effective_grants`](crate::runtime::builder::agent_effective_grants)
//! — the *same* function the harness calls when it builds the agent, not a
//! re-implementation of the rule. A second copy would eventually disagree, and
//! a tool-grant readout that disagrees with the harness is worse than none.
//!
//! ## What may be edited, and why that is the line
//!
//! The console edits what the console owns.
//!
//! An **overlay** teammate — one an operator defined through "Define an agent",
//! or the orchestrator created with `add_agent` — lives on the
//! [`CompanyRecord`], which this process writes. Its name, role and description
//! are editable here, and that is the whole of #264's "the roster is write-once
//! per member" complaint: before this, iterating on an agent's instructions
//! meant deleting it and starting over.
//!
//! A **manifest** teammate is declared in the version-controlled `company.toml`
//! — including every teammate from the global baseline, which is merged into
//! *every* company's roster. It used to be uneditable here, which meant the
//! agents a company actually ships with were the ones an operator could never
//! change: a hosted tenant has no `company.toml` to edit and nothing to
//! redeploy, so "edit it in the blueprint" was advice with no action behind it.
//!
//! It is editable now, through an
//! [`AgentOverride`](crate::ports::types::AgentOverride) layered on the record —
//! the shape #343 already used for the one field that was always editable, the
//! daily budget. Nothing rewrites `company.toml`: the blueprint keeps stating
//! what the company launched with, the overlay states what the operator has
//! since decided, and [`CompanyRecord::effective_agent`] is the single place the
//! two are resolved, so the console card and the built roster cannot disagree.
//! The merge is per field, so a field nobody edited still tracks the blueprint
//! across a redeploy.
//!
//! Removal works the same way — `DELETE …/team/{agent_id}` in
//! [`super::team`] records a tombstone rather than rewriting the blueprint —
//! and its only refusal is the company's last teammate.
//!
//! `tier` is read-only for both kinds: it has no override layer, and inventing
//! one is a policy decision rather than something to smuggle into a detail
//! view. `tools` is editable but admin-only, and can only ever *narrow* a
//! teammate within the company grant — see [`edit_agent`].
//!
//! The server states the rule rather than leaving the console to re-derive it:
//! every detail response carries an [`editable`](AgentDetailDto::editable) list,
//! and the console renders a field read-only exactly when the host says it is.
//! A console that decided this for itself would drift from what the host
//! actually accepts, and the operator would meet the disagreement as a failed
//! save.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::{self, MethodRouter};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::OpenCompanyError;
use crate::ports::store::company_write_lock;
use crate::ports::types::{AgentOverride, CompanyRecord};
use crate::runtime::builder::agent_scoped_grants;
use crate::server::error::ApiError;
use crate::server::ops::ScopedCompany;
use crate::server::ops::team::{AgentPath, daily_spend_samples, double_option};
use crate::server::users::admin::require_admin;

/// The `{scope}/team/{agent_id}` fragment: read one agent, edit one agent.
///
/// Merged into [`super::team::router`]'s existing `/team/{agent_id}` entry
/// rather than declared as its own route — axum panics on two routers claiming
/// one path, even for disjoint methods.
pub(super) fn method_router() -> MethodRouter<AppState> {
    routing::get(agent_detail).patch(edit_agent)
}

/// Which half of the roster a teammate comes from, and therefore what may be
/// done to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum AgentSource {
    /// Declared in the version-controlled `company.toml`.
    Manifest,
    /// Added at runtime by an operator or the orchestrator, stored on the
    /// company record.
    Overlay,
}

/// The fields a `PATCH` accepts for a teammate — manifest-declared or overlay
/// alike. Sent to the console so it renders the same rule the host enforces.
///
/// Seven since `feat/external-acp` met #1530 on `main`: that issue added
/// `instructions` and widened this list from overlay-only to both kinds, while
/// #1245's harness-picker follow-up added `model` and `harness`. Neither knew
/// about the other, so this is their union. It widens nothing on its own —
/// `tools`, `model` and `harness` stay admin-gated in [`edit_agent`], and
/// [`EDITABLE_FIELDS_MEMBER`] is unchanged from what #1530 left it.
const EDITABLE_FIELDS: [&str; 7] = [
    "name",
    "role",
    "description",
    "tools",
    "instructions",
    "model",
    "harness",
];

/// The subset a **non-admin** member may `PATCH` (issue #619).
///
/// `tools` is admin-only because an empty list means "the company's standard
/// grant", which makes a `tools` edit a potential *widening* — see
/// [`edit_agent`]. The list is actor-dependent for the reason the module note
/// gives: a console renders a field read-only exactly when the host says it is,
/// so offering `tools` to a member who would meet a `403` on save is precisely
/// the drift `editable` exists to remove.
const EDITABLE_FIELDS_MEMBER: [&str; 4] = ["name", "role", "description", "instructions"];

/// One agent, in full — everything #264 lists as unreachable.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AgentDetailDto {
    id: String,
    /// Absent for a manifest teammate, which is named by its role. Same rule as
    /// `GET …/team`.
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    role: String,
    /// What the agent was defined with. This is the text that frames the
    /// agent's persona for every turn it takes, which is what the issue means
    /// by "the `AGENT.md` or similar file for that agent" — the manifest
    /// already carries it, the console just never showed it after creation.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    source: AgentSource,
    /// The field names a `PATCH` will accept for this teammate. Since #1530 a
    /// manifest teammate is no longer empty — it accepts `instructions`, which
    /// write to the override record rather than `company.toml`.
    editable: Vec<&'static str>,
    /// The persona instructions **in force** for this teammate (issue #1530):
    /// an operator override when one is set, else the manifest `prompt`, else
    /// absent. This is what actually frames the agent's turns — the value the
    /// console shows in the editor.
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    /// The manifest `prompt` seed, when this teammate has one — what
    /// "Reset to blueprint" restores. Absent for an overlay teammate (no
    /// manifest row) and for a manifest agent that declares no `prompt`. The
    /// console previews/labels the reset target from this.
    #[serde(skip_serializing_if = "Option::is_none")]
    blueprint_instructions: Option<String>,
    /// Whether an operator override is currently masking the blueprint — the
    /// signal the console gates "Reset to blueprint" on, so it offers the reset
    /// only when there is something to reset to.
    instructions_overridden: bool,
    /// The declared cognition-tier hint, when the manifest sets one. An overlay
    /// teammate has none by construction.
    #[serde(skip_serializing_if = "Option::is_none")]
    tier: Option<String>,
    /// Which `[[harness]]` this teammate runs its turns on, by declared id
    /// (issue #1245's harness-picker follow-up). `None` means the harness
    /// marked `default = true` — read `GET {scope}/harnesses` for the full
    /// declared set, including which one that is.
    #[serde(skip_serializing_if = "Option::is_none")]
    harness: Option<String>,
    /// This teammate's own model override, when it has one (issue #1245's
    /// per-agent follow-up) — a manifest `[[agent]].model` line, or its
    /// overlay `OverlayAgent::model` equivalent. Unlike `tier`, both kinds
    /// can carry one. Meaningful only when the teammate resolves to an `acp`
    /// harness; the console has no way to know that from this response alone
    /// and should treat it as informational rather than validating it.
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// Whether this teammate is the company's orchestrator — resolved by the
    /// roster rule (tagged tier first, else the first declared agent), not read
    /// off `tier` alone, so an untagged roster's real orchestrator is named.
    is_orchestrator: bool,
    tools: AgentToolsDto,
    desks: Vec<AgentDeskDto>,
    inbox_enabled: bool,
    /// The cap in force, its spend, and its attribution — the same fields and
    /// the same absent-means-uncapped contract as `GET …/team`.
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_usd_daily: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spent_today_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_set_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_set_at_millis: Option<u64>,
}

/// An agent's tool grants at all three levels, so the resolution is legible
/// rather than asserted.
///
/// Built **only** through [`agent_tools`], so every surface that renders an
/// agent's tools renders the same list — see that function for why that is a
/// rule rather than a convenience.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AgentToolsDto {
    /// The globs the agent asks for. **Empty means "the company's standard
    /// grant"**, not "no tools" — an agent that lists nothing inherits the whole
    /// allow-list. The console has to say which of the two it is looking at.
    requested: Vec<String>,
    /// The company-wide `[tools].allow` ceiling.
    company_allow: Vec<String>,
    /// The ceiling contributed by the desks this agent sits on — the union of
    /// their `tools`, already narrowed by `company_allow`.
    ///
    /// **Empty means no desk narrows anything**, which is the same "empty is not
    /// nothing" trap `requested` carries: a console rendering an empty list as
    /// "this desk grants no tools" would invert the meaning. It is empty for
    /// every company that has not set a desk ceiling, which is most of them.
    desk_allow: Vec<String>,
    /// What the agent actually holds, after all three levels.
    effective: Vec<String>,
}

/// A desk this agent sits on.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AgentDeskDto {
    id: String,
    name: String,
    /// Whether this agent is the desk's lead — the first effective member, who
    /// receives a `delegate_to_desk` hand-off.
    lead: bool,
}

/// The tool globs an agent *asks* for, resolved identically for every reader.
///
/// A manifest teammate's `[[agent]].tools` line, or — for an overlay teammate —
/// its own [`OverlayAgent::tools`](crate::ports::types::OverlayAgent::tools)
/// grant (issue #661 / L5), which mirrors `harness::overlay_agent_to_manifest`.
/// An **empty** list from either source means "the company's standard grant",
/// not "no tools", so the Team tab shows the teammate's real effective grant
/// rather than the full company allow-list for every overlay member.
///
/// Its callers have already established that `agent_id` is on the roster, so a
/// miss in the manifest half can only be the overlay half.
pub(super) fn requested_grants(record: &CompanyRecord, agent_id: &str) -> Vec<String> {
    record
        .effective_agent(agent_id)
        .map(|agent| agent.tools.clone())
        .or_else(|| {
            record
                .overlay_agents
                .iter()
                .find(|agent| agent.id == agent_id)
                .map(|agent| agent.tools.clone())
        })
        .unwrap_or_default()
}

/// The **declared** cognition-tier hint for `agent_id`: the manifest
/// `[[agent]].tier` line verbatim, or `None` when the row declares none — and
/// for every overlay teammate, which has no manifest row to declare one.
///
/// Verbatim is the whole contract. This is what the company *wrote*, not a
/// resolved answer, and `None` means **undeclared** — a reader has to render
/// that as "cannot say" rather than substituting a default. Issue #643 is
/// exactly that substitution: the overview graph printed a literal `worker` for
/// every teammate, so a company declaring `tier = "orchestrator"` read back as
/// a worker on its own graph.
///
/// Sibling of [`requested_grants`] in shape and in reason: one lookup, shared
/// by the roster list and the detail read, so the two cannot answer differently
/// for the same teammate.
pub(super) fn declared_tier(record: &CompanyRecord, agent_id: &str) -> Option<String> {
    record
        .manifest
        .agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .and_then(|agent| agent.tier.clone())
}

/// The declared per-agent model override for `agent_id`, from whichever half
/// of the roster it comes from (issue #1245's per-agent follow-up).
///
/// Unlike [`declared_tier`], this checks **both** the manifest row and the
/// overlay row — `Agent::model` and `OverlayAgent::model` are siblings that
/// both exist, since a model override (unlike a tier tag) is something an
/// operator-defined teammate can carry too. `None` means undeclared, exactly
/// as `declared_tier`'s own contract: this is what the roster *wrote*, not a
/// resolved answer.
pub(super) fn declared_model(record: &CompanyRecord, agent_id: &str) -> Option<String> {
    // Through `effective_agent`, not `manifest.agents` directly: a blueprint
    // teammate's edit is stored as an overlay, and reading the raw manifest
    // row skips it. That is what made a `PATCH` here look successful and read
    // back at the old value — the write landed, this never looked at it.
    record
        .effective_agent(agent_id)
        .and_then(|agent| agent.model.clone())
        .or_else(|| {
            record
                .overlay_agents
                .iter()
                .find(|agent| agent.id == agent_id)
                .and_then(|agent| agent.model.clone())
        })
}

/// The declared harness binding for `agent_id`, from whichever half of the
/// roster it comes from (issue #1245's harness-picker follow-up).
///
/// Sibling of [`declared_model`] in shape and in reason: `Agent::harness` and
/// `OverlayAgent::harness` are the same field on both roster halves now, and
/// `None` means "the default harness", not "undeclared" — unlike
/// [`declared_tier`], every teammate resolves to *some* harness, this just
/// says whether it named one explicitly.
pub(super) fn declared_harness(record: &CompanyRecord, agent_id: &str) -> Option<String> {
    // Through `effective_agent`, not `manifest.agents` directly: a blueprint
    // teammate's edit is stored as an overlay, and reading the raw manifest
    // row skips it. That is what made a `PATCH` here look successful and read
    // back at the old value — the write landed, this never looked at it.
    record
        .effective_agent(agent_id)
        .and_then(|agent| agent.harness.clone())
        .or_else(|| {
            record
                .overlay_agents
                .iter()
                .find(|agent| agent.id == agent_id)
                .and_then(|agent| agent.harness.clone())
        })
}

/// Whether `agent_id` is this company's orchestrator.
///
/// Delegates to [`crate::company::orchestrator_id`] — the roster rule the
/// harness itself resolves the orchestrator with (the agent tagged with the
/// orchestrator tier, else the first declared agent), never a re-read of
/// [`declared_tier`].
///
/// **This is not the same question as the tier.** A company that tags nobody
/// still has an orchestrator, so an untagged first agent answers `true` here
/// while [`declared_tier`] answers `None`; and a *second* agent tagged with the
/// orchestrator tier carries that tier while answering `false` here, because the
/// rule picks one. A caller that re-derived the marker from the tier string
/// would get both of those backwards.
pub(super) fn is_orchestrator(record: &CompanyRecord, agent_id: &str) -> bool {
    crate::company::orchestrator_id(&record.effective_agents()) == Some(agent_id)
}

/// Whether `agent_id` came from the **global baseline**
/// ([`crate::globals`]) rather than from this company.
///
/// Provenance, and the one question first-run setup turns on (issue #1404).
/// `apply_globals` appends `globals/agents/*.toml` to *every* company's roster
/// whatever its manifest says, so "is the roster empty?" is answered `no` on a
/// company nobody has ever staffed — which is how the whole first-run flow came
/// to be unreachable in the shipped product. The console needs to tell the
/// baseline apart from a team, and it must not do that by hard-coding the
/// baseline's ids: the next global added would silently re-break the gate.
///
/// Read from [`Agent::global`](crate::company::Agent::global), the marker the
/// merge itself sets, so this answer moves with the baseline rather than
/// alongside it. An overlay teammate is never global — the merge only ever
/// touches the manifest roster — so an id this does not find is `false`, which
/// is also the right answer for an id that is not on the roster at all.
pub(super) fn is_global(record: &CompanyRecord, agent_id: &str) -> bool {
    record
        .manifest
        .agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .is_some_and(|agent| agent.global)
}

/// One agent's grants at all three levels — the single constructor for
/// [`AgentToolsDto`].
///
/// `effective` comes from
/// [`agent_effective_grants`](crate::runtime::builder::agent_effective_grants),
/// the same function the harness builds the agent with, for the reason the
/// module docs give. This function exists so the **roster list** and the
/// **detail read** cannot answer that question differently either (issue
/// #601): the overview graph reads the list and used to invent a tool shelf by
/// dealing slices of `[tools].allow`, so the graph and the detail card beside
/// it disagreed about the same agent. Sharing the constructor makes that
/// disagreement unrepresentable rather than merely fixed once.
/// Takes the `record` and `agent_id` rather than a pre-extracted allow-list,
/// because the desk level cannot be derived from the company grant alone — it
/// depends on which desks this teammate sits on. Passing the record is what makes
/// "forgot to apply the desk ceiling" unrepresentable at the call site rather
/// than a thing three callers each have to remember.
pub(super) fn agent_tools(record: &CompanyRecord, agent_id: &str) -> AgentToolsDto {
    let company_allow = &record.manifest.tools.allow;
    let requested = requested_grants(record, agent_id);

    // The desk ceilings this agent is under, resolved through the record's
    // *effective* desk membership so a console-seated member is scoped exactly
    // as a manifest one.
    let desk_tools = record.agent_desk_tools(agent_id);
    let desk_refs: Vec<&[String]> = desk_tools.iter().map(Vec::as_slice).collect();

    // Reported already narrowed by the company grant, so the console can render
    // the three rows as a strictly shrinking chain. A raw union could show a
    // desk "granting" something the company never allowed.
    let desk_allow = if desk_tools.iter().all(Vec::is_empty) {
        Vec::new()
    } else {
        agent_scoped_grants(company_allow, &desk_refs, &[])
    };

    AgentToolsDto {
        effective: agent_scoped_grants(company_allow, &desk_refs, &requested),
        requested,
        company_allow: company_allow.to_vec(),
        desk_allow,
    }
}

/// The `PATCH` body. Every field is optional, and an absent field is left
/// alone: this is a patch, not a replacement, so a console that renders only
/// some of an agent's fields cannot blank the rest by omission.
///
/// `description` is a **double option** so "leave it" and "clear it" stay
/// apart on the wire, the same shape and for the same reason as
/// [`SetBudget`](super::team::SetBudget)'s cap:
///
/// | body | parses as | means |
/// |---|---|---|
/// | `{}` | `None` | leave the description alone |
/// | `{"description": null}` | `Some(None)` | clear it |
/// | `{"description": "…"}` | `Some(Some(…))` | set it |
///
/// Collapsing the first two would make every partial save silently erase an
/// agent's instructions, which is the single worst thing this route could do.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EditAgent {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    description: Option<Option<String>>,
    /// The teammate's tool scope (issue #619). Absent leaves it alone; an
    /// **empty array** is the deliberate way back to the company's standard
    /// grant, which is why this is a plain `Option` and not a double option —
    /// `[]` already spells "clear it" without needing `null` to mean something
    /// different from omission.
    ///
    /// #661 made a teammate scopable at *creation* (`POST …/team` and
    /// `add_agent`). This is the half that was missing: narrowing one that
    /// already exists, without deleting and recreating it — which would orphan
    /// its workspace folder, budget row, desk memberships and inbox.
    #[serde(default)]
    tools: Option<Vec<String>>,
    /// The teammate's persona instructions (issue #1530). A **double option**,
    /// the same three-state contract as `description`:
    ///
    /// | body | parses as | means |
    /// |---|---|---|
    /// | `{}` | `None` | leave the instructions alone |
    /// | `{"instructions": null}` | `Some(None)` | clear the override → reset to blueprint |
    /// | `{"instructions": "…"}` | `Some(Some(…))` | set the override |
    ///
    /// Unlike every other field here, this is accepted for a **manifest**
    /// teammate too: it writes to the per-agent override record, not to
    /// `company.toml`, so it is legal for both kinds. A blank/whitespace string
    /// is normalized to a reset, so an override can never blank a persona.
    #[serde(default, deserialize_with = "double_option")]
    instructions: Option<Option<String>>,
    /// The teammate's own model override (issue #1245's per-agent follow-up).
    /// A double option for the same reason as `description`: absent leaves it
    /// alone, `null` clears it back to the harness's own default, and a
    /// string sets it. Admin-only, alongside `tools` — see [`edit_agent`]:
    /// a model choice carries the same "this is a cost/scope decision, not a
    /// teammate's own detail" character `tools` does, not a name or a role.
    #[serde(default, deserialize_with = "double_option")]
    model: Option<Option<String>>,
    /// Which declared `[[harness]]` this teammate runs on (issue #1245's
    /// harness-picker follow-up). Same double-option shape and the same
    /// admin gate as `model` — see [`edit_agent`]. `null` clears it back to
    /// the harness marked `default = true`; a string pins it to one of the
    /// ids `GET {scope}/harnesses` lists. Validated against that same list at
    /// write time, so a typo or a stale id from a client that cached an old
    /// harness list is a `400`, not a teammate silently orphaned from every
    /// harness's serve set.
    #[serde(default, deserialize_with = "double_option")]
    harness: Option<Option<String>>,
}

/// `GET {scope}/team/{agent_id}` — one agent, read.
async fn agent_detail(
    company: ScopedCompany,
    State(state): State<AppState>,
    headers: HeaderMap,
    crate::server::graphql::auth::MaybePeer(peer): crate::server::graphql::auth::MaybePeer,
    Path(AgentPath { agent_id }): Path<AgentPath>,
) -> Result<Json<AgentDetailDto>, ApiError> {
    // Only to decide what `editable` may claim — the read itself is open to any
    // member, unchanged. A principal this cannot resolve reads as not-admin,
    // which is fail-closed in the right direction: it under-claims what the
    // caller may edit rather than over-claiming it.
    let is_admin = is_admin_actor(&headers, &state, &company, peer).await;
    let record = company
        .runtime
        .store()
        .load(company.id())
        .await?
        .ok_or_else(|| OpenCompanyError::CompanyNotFound(company.id().to_string()))?;
    detail(&company, &record, &agent_id, is_admin).await
}

/// `PATCH {scope}/team/{agent_id}` — edit a teammate.
///
/// Refuses an unknown id with a `404`. A manifest teammate's version-controlled
/// fields (`name`/`role`/`description`/`tools`) still refuse with a `409` naming
/// where the edit belongs — but since #1530 the refusal is **conditional**: an
/// `instructions`-only body is accepted for a manifest teammate too, because it
/// writes to the per-agent override record rather than `company.toml` (see the
/// check inside). `name`, `role`, `description` and `instructions` are open to
/// any signed-in member, matching `POST …/team`: defining a teammate was never
/// admin-only, so correcting one it defined is not either.
///
/// # Why `tools` is the exception (issue #619)
///
/// That reasoning covers what a teammate *is*. It does not cover what a
/// teammate may *do*, and a tool grant is the second thing — the
/// [`AdminScopedCompany`](super::AdminScopedCompany) axis: a write that settles
/// something *on behalf of* the company rather than one a member makes for
/// themselves.
///
/// The sharp edge is that **an empty `tools` list means "inherit the company's
/// standard grant"** — the widest grant the company has. So `{"tools": []}` is
/// not a small edit, it is a *widening*, and left member-open it would let any
/// signed-in member hand a deliberately-scoped teammate the company's whole
/// grant back. That is the exact inversion this field was added to prevent, and
/// `add_agent` already refuses its own version of it (a narrowing that lands
/// empty is a hard error there, never a stored empty list).
///
/// So the admin check is **conditional on the field being present**, in the
/// same shape and for the same reason as the cap on
/// [`add_member`](super::team): a member who edits a name or a role keeps
/// working exactly as before, and adding this field must not quietly take an
/// existing capability away from members.
///
/// Being conditional is also what fixes its **position**: it runs after the
/// `409`/`404` checks, so an unknown id answers `404` whether or not the body
/// carried `tools`. See the comment at the check itself.
///
/// Narrow-only-for-members was considered and rejected: it makes the scope a
/// one-way ratchet, so a teammate scoped too tightly could never be loosened by
/// anyone, and the only way back would be delete-and-recreate — which orphans
/// the workspace folder, budget row, desk memberships and inbox this route
/// exists to preserve.
async fn edit_agent(
    company: ScopedCompany,
    State(state): State<AppState>,
    headers: HeaderMap,
    crate::server::graphql::auth::MaybePeer(peer): crate::server::graphql::auth::MaybePeer,
    Path(AgentPath { agent_id }): Path<AgentPath>,
    Json(body): Json<EditAgent>,
) -> Result<Json<AgentDetailDto>, crate::server::Rejection> {
    // Serialize with every other write to `overlay_agents`, so a console edit
    // and a concurrent `add_agent` cannot clobber one another's roster.
    let write_lock = company_write_lock(company.id());
    let _lock = write_lock.lock().await;

    let mut record = company
        .runtime
        .store()
        .load(company.id())
        .await?
        .ok_or_else(|| OpenCompanyError::CompanyNotFound(company.id().to_string()))?;

    // Identity before validation, so an unknown id is a 404 rather than a
    // complaint about the shape of a body nobody could have applied anyway.
    //
    // A **manifest** teammate is edited through the override layer below rather
    // than refused: a company you have deployed is still yours to change, and
    // the blueprint is never rewritten either way.
    // Asked through `is_roster_agent`, which is the same union `detail` reads
    // back through — and, crucially, excludes a teammate the operator has
    // removed. A retired manifest id still matches `manifest.agents`, so a
    // narrower check here would store an override for a teammate that is not on
    // the roster and then answer `404` from `detail`: a failed request that
    // mutated the record on its way out.
    if !record.is_roster_agent(&agent_id) {
        return Err(ApiError(OpenCompanyError::CompanyNotFound(format!(
            "teammate {agent_id}"
        )))
        .into_response()
        .into());
    }
    let is_manifest = record.manifest.agents.iter().any(|a| a.id == agent_id);

    // Authority **after** existence, and this ordering is forced rather than
    // preferred (review of #745).
    //
    // The check is conditional on `tools`, so putting it first would make one
    // route give two answers about whether a teammate exists: `{"name": "x"}`
    // on an unknown id would 404 while `{"tools": […]}` on the same id would
    // 403. Nothing about an unrelated field should decide that, and the
    // non-`tools` path cannot be moved to match — a name edit is member-open
    // and has no authority check to run first. So this is the only order in
    // which the two paths agree.
    //
    // The usual reason to authorise first — refusing to confirm a resource
    // exists — does not apply: `GET {scope}/team/{agent_id}` is open to any
    // signed-in member and already 404s on an unknown id, so ordering 403
    // ahead of 404 here would hide nothing from the very caller it would
    // inconvenience.
    //
    // Deliberately unlike `set_budget`, which authorises first: that route is
    // admin-only in full, so admin-first is self-consistent there. This one is
    // admin-only *per field*, which is what makes the ordering load-bearing.
    if body.tools.is_some() || body.model.is_some() || body.harness.is_some() {
        require_admin(&headers, &state, &company.runtime, peer).await?;
    }

    let name = trimmed_field(body.name.as_deref(), "name").map_err(|e| e.into_response())?;
    let role = trimmed_field(body.role.as_deref(), "role").map_err(|e| e.into_response())?;
    let tools = body
        .tools
        .map(|globs| trimmed_globs(&globs))
        .transpose()
        .map_err(|e| e.into_response())?;
    // Present-and-null clears; a blank string clears too — an empty override
    // and no override mean the same thing (the harness's own default model
    // applies), and storing `Some("")` would only make the two look
    // different on the wire. Hoisted above the mutation below (unlike
    // `tools`/`name`) because the cross-field check just below needs the
    // *resulting* value, not `body.model` itself.
    let model = body
        .model
        .map(|text| text.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
    // Same double-option contract, and same reason to hoist: validated below
    // against the declared harness list before anything is written.
    let harness = body
        .harness
        .map(|text| text.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));

    // Resolved through `harness_by_id`, not against the declared list: a
    // coding CLI this build drives is bindable without any `[[harness]]`
    // naming it, and `GET {scope}/harnesses` offers exactly those ids in the
    // picker. Checking the declared list here would refuse a binding the
    // console had just offered.
    if let Some(Some(id)) = &harness
        && record.manifest.harness_by_id(id).is_none()
    {
        return Err(ApiError(OpenCompanyError::InvalidRequest(format!(
            "no harness named `{id}` is available for this company."
        )))
        .into_response()
        .into());
    }

    // A model override only means anything on an `acp` harness — the same
    // rule `CompanyManifest::validate` enforces for a manifest agent's own
    // `model`, applied here because an overlay teammate never passes through
    // that validation (it lives on the record, not the parsed manifest).
    // Resolved against the harness this edit actually leaves the teammate
    // on: the new binding when one was sent, else its current one — so
    // setting a model in the same request as switching to an ACP harness
    // is accepted, not rejected against the stale binding.
    let resulting_model = model
        .clone()
        .unwrap_or_else(|| declared_model(&record, &agent_id));
    if let Some(model_value) = &resulting_model {
        let resulting_harness_id = harness
            .clone()
            .unwrap_or_else(|| declared_harness(&record, &agent_id))
            .unwrap_or_else(|| record.manifest.default_harness_id());
        let bound = record.manifest.harness_by_id(&resulting_harness_id);
        if bound.as_ref().map(|h| h.kind.as_str()) != Some("acp") {
            return Err(ApiError(OpenCompanyError::InvalidRequest(format!(
                "`{model_value}` names a model, but this teammate's harness has no ACP \
                 transport to forward it to. Bind it to an ACP harness first, or clear \
                 the model."
            )))
            .into_response()
            .into());
        }
        // `kind = "acp"` is not sufficient: a `runner` transport is ACP and
        // still cannot carry a model, because the runner wire protocol has no
        // field for one. `CompanyManifest::validate` already refuses this
        // combination, so accepting it here let the API store a binding a
        // manifest is not allowed to declare — and one that could never take
        // effect. The wording is the validator's, so both refusals read the
        // same.
        if bound
            .as_ref()
            .and_then(|h| h.acp.as_ref())
            .map(|acp| acp.transport.as_str())
            == Some("runner")
        {
            return Err(ApiError(OpenCompanyError::InvalidRequest(format!(
                "`{model_value}` names a model, but harness `{resulting_harness_id}` uses \
                 `transport = \"runner\"`. Model overrides aren't supported for a runner \
                 yet — the runner wire protocol doesn't carry them."
            )))
            .into_response()
            .into());
        }
    }

    // Captured before the two are consumed below. `Some` means the request
    // carried the field at all — including a `null` that clears it, which
    // changes routing exactly as much as setting one does.
    let routing_changed = model.is_some() || harness.is_some();

    if is_manifest {
        // Stored as an overlay on the record, exactly like the daily-budget
        // override #343 modelled: `company.toml` keeps saying what the company
        // launched with, and this says what the operator has since decided. The
        // merge is field-wise, so a field nobody edited keeps tracking the
        // blueprint across a redeploy.
        let mut entry = crate::ports::types::AgentOverride {
            agent_id: agent_id.clone(),
            name,
            role,
            tools,
            ..Default::default()
        };
        // An empty string is the stored form of "cleared" — the write path
        // already treats a blank description and no description as one state.
        if let Some(description) = body.description {
            entry.description = Some(
                description
                    .map(|text| text.trim().to_string())
                    .unwrap_or_default(),
            );
        }
        // Issue #1245's per-agent follow-up. These were advertised as editable
        // and accepted by this handler, but the override built here carried
        // only the four fields above — so a blueprint teammate's harness or
        // model edit returned 200 and was then read back at its old value,
        // with nothing anywhere reporting the loss. Blank is the stored form
        // of "cleared", exactly as for `description`.
        if let Some(model) = model {
            entry.model = Some(model.unwrap_or_default());
        }
        if let Some(harness) = harness {
            entry.harness = Some(harness.unwrap_or_default());
        }
        record.upsert_agent_override(entry);
    } else {
        let agent = record
            .overlay_agents
            .iter_mut()
            .find(|a| a.id == agent_id)
            .expect("overlay membership was checked above");
        if let Some(name) = name {
            agent.name = name;
        }
        if let Some(role) = role {
            agent.role = role;
        }
        // Present-and-null clears; a blank string clears too, since an empty
        // description and no description frame the persona identically and
        // storing `Some("")` would only make the two look different on the wire.
        if let Some(description) = body.description {
            agent.description = description
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty());
        }
        // Issue #619: stored verbatim, exactly like a manifest `[[agent]].tools`
        // line. The company `allow` ceiling is applied at *read* time by
        // `agent_effective_grants`, so a glob the company does not cover is
        // surfaced as asked-for-but-not-granted rather than silently dropped
        // here — and this route can only ever narrow a teammate within a grant
        // the company already made.
        if let Some(tools) = tools {
            agent.tools = tools;
        }
        // Issue #1245's per-agent follow-up: already trimmed/blank-cleared
        // and cross-validated above.
        if let Some(model) = model {
            agent.model = model;
        }
        if let Some(harness) = harness {
            agent.harness = harness;
        }
    }

    // Issue #1530: the persona override, written to the record for **either**
    // kind. A `null` or blank/whitespace body normalizes to a reset — drop the
    // override so the blueprint `prompt` applies again — which is what keeps an
    // emptied edit from silently blanking a persona. A non-empty string upserts,
    // replacing any prior override so `agent_override`'s first-match read can
    // never see a stale row.
    if let Some(instructions) = body.instructions {
        match instructions
            .map(|text| crate::company::prompt::cap_persona_instructions(text.trim()))
            .filter(|text| !text.is_empty())
        {
            Some(text) => record.upsert_agent_override(AgentOverride {
                agent_id: agent_id.clone(),
                instructions: Some(text),
                ..Default::default()
            }),
            None => record.clear_agent_override(&agent_id),
        }
    }

    company.runtime.store().save(&record).await?;

    // A harness or model change needs the runtime rebuilt, not just saved.
    //
    // Lanes, router bindings and `LocalAcpAgent`'s model map are snapshots
    // `RuntimeBuilder` takes once, and `HarnessBrain::refresh_record` refreshes
    // only the record — so without this the save is durable and inert: it
    // survives, reads back correctly, and changes nothing about where turns go
    // until the process restarts. The same reasoning `inference.rs` applies to
    // a provider change, which is likewise chosen at build time.
    //
    // Only for these two fields. A name, role, tools or description edit does
    // not affect routing, and rebuilding a company for one would be a large
    // cost for no effect.
    // A let-chain rather than a nested `if`: the tuple form clippy's
    // `collapsible_if` suggests would evaluate `rebuild_company` before
    // testing the flag, rebuilding on every name edit — the exact cost this
    // guard exists to avoid.
    if routing_changed
        && let Err(error) = crate::runtime::rebuild_company(&state, company.id()).await
    {
        // Not fatal, and deliberately not a failed response: the edit *is*
        // saved and will apply on the next start. A host that cannot rebuild
        // in place (no rebuilder wired) is an ordinary configuration, not an
        // error the operator caused by editing a teammate.
        tracing::warn!(
            %error,
            agent = %agent_id,
            "saved the harness binding but could not rebuild the company runtime; \
             it applies on the next restart"
        );
    }

    // The caller either passed `require_admin` above or sent no `tools`, so
    // re-resolve rather than assume: an admin editing only a name must still
    // read back `tools` as editable.
    let is_admin = is_admin_actor(&headers, &state, &company, peer).await;
    detail(&company, &record, &agent_id, is_admin)
        .await
        .map_err(|e| e.into_response().into())
}

/// Rejects a field that was sent but is blank, and trims one that was sent.
///
/// A teammate whose name is whitespace renders as an empty card with no way
/// back to it, so the refusal is a `400` rather than a stored blank.
///
/// The error is an [`ApiError`], **not** the `Response` its caller returns.
/// `clippy::result_large_err` fires on the second shape here and is right to:
/// an `axum` `Response` is 128+ bytes, so a `Result<Option<String>, Response>`
/// makes every successful call carry the footprint of the refusal it did not
/// make. The handler is exempt only because its own `Ok` variant is larger
/// still. The caller converts at the boundary, which is also what the sibling
/// refusal helpers in `team.rs` do by returning `Option<Response>`.
fn trimmed_field(value: Option<&str>, field: &str) -> Result<Option<String>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ApiError(OpenCompanyError::InvalidRequest(format!(
            "a teammate's {field} can't be empty."
        ))));
    }
    Ok(Some(trimmed.to_string()))
}

/// Trims a submitted tool-scope list, refusing a blank entry and dropping
/// duplicates (issue #619).
///
/// A blank glob is a `400` rather than a stored empty string for a sharper
/// reason than tidiness: `""` matches nothing an operator meant, so it would
/// read as a scope that grants nothing while looking like a scope that was set.
/// Duplicates are dropped rather than refused — a repeated glob is harmless and
/// the resolved grant list is de-duplicated downstream anyway.
///
/// Same `ApiError`-not-`Response` return shape as [`trimmed_field`], for the
/// reason given there.
fn trimmed_globs(globs: &[String]) -> Result<Vec<String>, ApiError> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(globs.len());
    for glob in globs {
        let trimmed = glob.trim();
        if trimmed.is_empty() {
            return Err(ApiError(OpenCompanyError::InvalidRequest(
                "a tool grant can't be empty. Send an empty list to give this teammate the company's standard grant.".to_string(),
            )));
        }
        if seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    Ok(out)
}

/// Whether the signed-in caller may administer this company — the question
/// [`EDITABLE_FIELDS`] keys off, asked without refusing.
///
/// [`require_admin`] is the enforcement path and returns a `Response` on
/// failure, which is right for a write and wrong for a read that must still
/// succeed for a member. This answers the same question through the same
/// `may_administer` predicate, so the two cannot drift.
async fn is_admin_actor(
    headers: &HeaderMap,
    state: &AppState,
    company: &ScopedCompany,
    peer: Option<std::net::SocketAddr>,
) -> bool {
    crate::server::users::routes::current_user(headers, state, company.id(), peer)
        .await
        .is_some_and(|user| user.may_administer())
}

/// Builds one agent's detail from the loaded record, or 404s when the id names
/// nobody on the roster.
async fn detail(
    company: &ScopedCompany,
    record: &CompanyRecord,
    agent_id: &str,
    is_admin: bool,
) -> Result<Json<AgentDetailDto>, ApiError> {
    // The manifest row with the operator's stored edits applied — the same
    // resolution `build_roster` performs, so the card and the running teammate
    // cannot disagree about who this is.
    let manifest_agent = record.effective_agent(agent_id);
    let overlay_agent = record.overlay_agents.iter().find(|a| a.id == agent_id);

    let (source, name, role, description) = match (manifest_agent.as_deref(), overlay_agent) {
        // A manifest agent wins an id collision, exactly as `build_roster`
        // resolves one: the version-controlled roster is authoritative.
        (Some(agent), _) => (
            AgentSource::Manifest,
            // `None` unless an operator has named this teammate: a manifest
            // `[[agent]]` is addressed by its role, and the console falls back
            // to it when there is no name.
            agent.name.clone(),
            agent.role.clone(),
            agent.description.clone(),
        ),
        // An overlay teammate has no manifest row, so `declared_tier` below
        // misses — and so does `requested_grants`: it holds the company's
        // standard grant, mirroring `harness::overlay_agent_to_manifest`.
        (None, Some(agent)) => (
            AgentSource::Overlay,
            Some(agent.name.clone()),
            agent.role.clone(),
            agent.description.clone(),
        ),
        (None, None) => {
            return Err(ApiError(OpenCompanyError::CompanyNotFound(format!(
                "teammate {agent_id}"
            ))));
        }
    };

    let cap = record.effective_budget(agent_id);
    let attribution = record.budget_override(agent_id);
    let spend_today = daily_spend_samples(company, Some(record)).await?;
    let spent = cap.and(
        spend_today
            .as_ref()
            .map(|samples| crate::metering::usd_spent_by_agent(samples, agent_id)),
    );

    let inbox_enabled = company
        .runtime
        .inbox()
        .inboxes(company.id())
        .await?
        .into_iter()
        .any(|meta| meta.key == agent_id && meta.enabled);

    // Issue #1530: the persona in force, the blueprint it would reset to, and
    // whether an override is currently masking that blueprint. `blueprint` is
    // the manifest `prompt` seed — absent for an overlay teammate, which has no
    // manifest row — so the console can preview what "Reset to blueprint"
    // restores. `overridden` is gated on an override that actually carries
    // instructions, so an empty record never reads as "overridden".
    let effective_instructions = record.effective_instructions(agent_id);
    let blueprint_instructions = record
        .manifest
        .agents
        .iter()
        .find(|a| a.id == agent_id)
        .and_then(|a| a.prompt.clone());
    let instructions_overridden = record
        .agent_override(agent_id)
        .is_some_and(|entry| entry.instructions.is_some());

    Ok(Json(AgentDetailDto {
        id: agent_id.to_string(),
        name,
        role,
        description,
        source,
        editable: match is_admin {
            true => EDITABLE_FIELDS.to_vec(),
            false => EDITABLE_FIELDS_MEMBER.to_vec(),
        },
        instructions: effective_instructions,
        blueprint_instructions,
        instructions_overridden,
        tier: declared_tier(record, agent_id),
        harness: declared_harness(record, agent_id),
        model: declared_model(record, agent_id),
        is_orchestrator: is_orchestrator(record, agent_id),
        tools: agent_tools(record, agent_id),
        desks: desks_for(record, agent_id),
        inbox_enabled,
        budget_usd_daily: cap,
        spent_today_usd: spent,
        budget_set_by: attribution.map(|entry| entry.set_by.id.clone()),
        budget_set_at_millis: attribution.map(|entry| entry.at_millis),
    }))
}

/// Every desk this agent is an effective member of, manifest desks first.
///
/// Resolved through
/// [`CompanyRecord::effective_desk_members`](crate::ports::types::CompanyRecord::effective_desk_members)
/// rather than by reading the declared member lists, so an operator-added
/// membership and an operator-set lead order are both reflected — the same
/// answer the Desks page and the harness `desk_lead` resolver give.
///
/// Shared with `GET {scope}/team` (issue #601) for the same anti-drift reason
/// as [`agent_tools`]: desks are the overview graph's departments now, so the
/// roster list and this read have to agree on which desks a teammate sits on.
pub(super) fn desks_for(record: &CompanyRecord, agent_id: &str) -> Vec<AgentDeskDto> {
    let declared = record
        .manifest
        .group_chats
        .iter()
        .map(|chat| (chat.id.as_str(), chat.name.as_str()))
        .chain(
            record
                .overlay_desks
                .iter()
                .map(|desk| (desk.id.as_str(), desk.name.as_str())),
        );
    declared
        .filter_map(|(id, name)| {
            let members = record.effective_desk_members(id);
            members.iter().any(|m| m == agent_id).then(|| AgentDeskDto {
                id: id.to_string(),
                name: name.to_string(),
                lead: members.first().map(String::as_str) == Some(agent_id),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::company::CompanyManifest;
    use crate::ports::CompanyStore;
    use crate::ports::types::{CompanyId, CompanyRecord};
    use crate::runtime::RuntimeBuilder;
    use crate::server::router;
    use crate::store::FsCompanyStore;
    use crate::{AppConfig, AppState};

    /// A company whose grants actually bite: `ceo` asks for one tool the company
    /// does not allow, `writer` asks for nothing at all, and `hermit` sits on no
    /// desk. Each of those is a different arm of the resolution under test.
    const ROSTER: &str = r#"
[company]
name = "Acme"
[policy]
mode = "full"
[tools]
allow = ["workspace", "workspace.*", "composio"]

[[agent]]
id = "ceo"
role = "Chief Executive"
description = "Sets direction and delegates."
tier = "orchestrator"
tools = ["workspace.read", "email.send"]

[[agent]]
id = "writer"
role = "Writer"

[[agent]]
id = "hermit"
role = "Hermit"

[[group_chat]]
id = "content"
name = "Content desk"
members = ["writer", "ceo"]
"#;

    /// [`ROSTER`], plus a declared `[[harness]]` set (issue #1245's
    /// harness-picker follow-up): `laptop` is a `local` ACP harness and the
    /// **default**, so a fresh overlay teammate — which names no harness of
    /// its own — lands there and a model override on it is meaningful. Tests
    /// that need to exercise the harness picker itself declare a second,
    /// non-default `built_in` entry (`main`) to switch *away* from.
    const ACP_ROSTER: &str = r#"
[company]
name = "Acme"
[policy]
mode = "full"
[tools]
allow = ["workspace", "workspace.*", "composio"]

[[agent]]
id = "ceo"
role = "Chief Executive"
description = "Sets direction and delegates."
tier = "orchestrator"
tools = ["workspace.read", "email.send"]

[[harness]]
id = "main"
kind = "built_in"

[[harness]]
id = "laptop"
kind = "acp"
default = true

[harness.acp]
transport = "local"
agent = "claude"
"#;

    fn home() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("oc-agent-detail-")
            .tempdir()
            .expect("tempdir")
    }

    /// Issue #661 / L5: `requested_grants` reads a manifest agent's `tools` line,
    /// falls back to an overlay teammate's own grant, and reads an empty grant
    /// (from either source, and for an unknown id) as the standard company-wide
    /// grant — so the Team tab shows an overlay teammate's real grant instead of
    /// the full company allow-list.
    #[test]
    fn requested_grants_reads_overlay_then_manifest_then_empty() {
        use crate::ports::types::OverlayAgent;

        let manifest: CompanyManifest = toml::from_str(
            "[company]\nname = \"Acme\"\n\
             [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\ntools = [\"workspace.read\"]\n",
        )
        .unwrap();
        let mut record = CompanyRecord {
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: CompanyId::new("acme"),
            manifest,
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: Vec::new(),
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
        };
        record.overlay_agents.push(OverlayAgent {
            id: "scoped".to_string(),
            name: "Scoped".to_string(),
            role: "Researcher".to_string(),
            description: None,
            tools: vec!["docs.*".to_string()],
            model: None,
            harness: None,
        });
        record.overlay_agents.push(OverlayAgent {
            id: "standard".to_string(),
            name: "Standard".to_string(),
            role: "Generalist".to_string(),
            description: None,
            tools: Vec::new(),
            model: None,
            harness: None,
        });

        // A manifest agent's own line.
        assert_eq!(
            super::requested_grants(&record, "ceo"),
            vec!["workspace.read"]
        );
        // An overlay teammate's own grant (the L5 read side).
        assert_eq!(super::requested_grants(&record, "scoped"), vec!["docs.*"]);
        // An overlay teammate with no grant → empty (the standard grant).
        assert!(super::requested_grants(&record, "standard").is_empty());
        // An unknown id → empty, as before.
        assert!(super::requested_grants(&record, "nobody").is_empty());
    }

    async fn state_with_manifest(home: &std::path::Path, manifest_toml: &str) -> AppState {
        let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
        let store = FsCompanyStore::new(home.to_path_buf());
        let id = CompanyId::new("acme");
        store
            .save(&CompanyRecord {
                overlay_retired_agents: Vec::new(),
                overlay_agent_edits: Vec::new(),
                id: id.clone(),
                manifest: manifest.clone(),
                ledger: Vec::new(),
                lifecycle: "running".to_string(),
                overlay_agents: Vec::new(),
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
            })
            .await
            .unwrap();
        let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest)
            .with_id(id.clone())
            .build()
            .await
            .unwrap();
        let state = AppState::new(AppConfig::default());
        state.registry().insert(id, std::sync::Arc::new(runtime));
        crate::server::test_support::seed_fixed_admin(&state, "acme").await;
        state
    }

    async fn send(
        state: &AppState,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("cookie", crate::server::test_support::fixed_cookie("acme"));
        let request = match &body {
            Some(value) => builder
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(value).unwrap()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = router(state.clone()).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }

    async fn get_agent(state: &AppState, agent: &str) -> (StatusCode, Value) {
        send(state, "GET", &format!("/api/v1/company/team/{agent}"), None).await
    }

    async fn patch_agent(state: &AppState, agent: &str, body: Value) -> (StatusCode, Value) {
        send(
            state,
            "PATCH",
            &format!("/api/v1/company/team/{agent}"),
            Some(body),
        )
        .await
    }

    /// Drives the route as a specific principal. The harness signs every other
    /// request in as an admin, which is exactly why this exists: an
    /// authority check verified only as an admin passes identically against no
    /// check at all.
    async fn send_as(
        state: &AppState,
        method: &str,
        uri: &str,
        body: Option<Value>,
        cookie: String,
    ) -> (StatusCode, Value) {
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("cookie", cookie);
        let request = match &body {
            Some(value) => builder
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(value).unwrap()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = router(state.clone()).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }

    /// Adds a teammate through the console's own route and returns its id.
    async fn add_overlay(state: &AppState, name: &str, role: &str) -> String {
        let (status, created) = send(
            state,
            "POST",
            "/api/v1/company/team",
            Some(json!({"name": name, "role": role, "description": "Original."})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        created["id"].as_str().unwrap().to_string()
    }

    fn strings(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap_or_else(|| panic!("expected an array, got {value}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    // --- The read half (issue #264) -----------------------------------------

    /// The whole of what the issue calls unreachable, on the wire: tier,
    /// description, resolved tools and desk membership for a manifest teammate.
    #[tokio::test]
    async fn a_manifest_agent_opens_with_its_tier_tools_and_desks() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;

        let (status, ceo) = get_agent(&state, "ceo").await;
        assert_eq!(status, StatusCode::OK, "{ceo}");

        assert_eq!(ceo["id"], "ceo");
        assert_eq!(ceo["role"], "Chief Executive");
        assert_eq!(ceo["description"], "Sets direction and delegates.");
        assert_eq!(ceo["source"], "manifest");
        assert_eq!(ceo["tier"], "orchestrator");
        assert_eq!(ceo["isOrchestrator"], true, "{ceo}");
        assert!(
            ceo["name"].is_null(),
            "a manifest teammate is named by its role: {ceo}"
        );

        // Desk membership, with the lead flag resolved from the effective order
        // rather than from the declared list — `writer` is declared first.
        let desks = ceo["desks"].as_array().unwrap();
        assert_eq!(desks.len(), 1, "{ceo}");
        assert_eq!(desks[0]["id"], "content");
        assert_eq!(desks[0]["name"], "Content desk");
        assert_eq!(desks[0]["lead"], false, "the writer leads this desk: {ceo}");

        // A teammate on no desk says so with an empty list rather than by
        // omitting the key, so the console can render "no desks" for sure.
        let (_, hermit) = get_agent(&state, "hermit").await;
        assert_eq!(hermit["desks"].as_array().unwrap().len(), 0, "{hermit}");
        assert_eq!(hermit["isOrchestrator"], false, "{hermit}");
    }

    /// The verification gap the issue names: what an agent *asks* for and what
    /// it *holds* are different lists, and only the second one matters.
    ///
    /// `ceo` requests `email.send`, which `[tools].allow` does not cover, so it
    /// is dropped. `writer` requests nothing, which means the company's standard
    /// grant rather than no tools at all — the opposite reading, and the one a
    /// naive surface would get wrong.
    #[tokio::test]
    async fn effective_tools_are_the_intersection_not_the_request() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;

        let (_, ceo) = get_agent(&state, "ceo").await;
        assert_eq!(
            strings(&ceo["tools"]["requested"]),
            vec!["workspace.read", "email.send"],
            "{ceo}"
        );
        assert_eq!(
            strings(&ceo["tools"]["companyAllow"]),
            vec!["workspace", "workspace.*", "composio"],
            "{ceo}"
        );
        assert_eq!(
            strings(&ceo["tools"]["effective"]),
            vec!["workspace.read"],
            "a request the company never allowed is not a grant: {ceo}"
        );

        let (_, writer) = get_agent(&state, "writer").await;
        assert!(
            strings(&writer["tools"]["requested"]).is_empty(),
            "{writer}"
        );
        assert_eq!(
            strings(&writer["tools"]["effective"]),
            vec!["workspace", "workspace.*", "composio"],
            "an agent that lists no tools holds the company's whole allow-list, \
             which is the reading a surface must not invert: {writer}"
        );
    }

    /// With no desk declaring a ceiling — the shape of every company written
    /// before desks could scope tools — the desk row is empty and the effective
    /// grant is unchanged. This is the case that must not regress for anybody.
    #[tokio::test]
    async fn a_company_with_no_desk_ceilings_reports_an_empty_desk_row() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;

        for agent in ["ceo", "writer", "hermit"] {
            let (_, body) = get_agent(&state, agent).await;
            assert!(
                strings(&body["tools"]["deskAllow"]).is_empty(),
                "{agent}: {body}"
            );
        }
    }

    /// A desk ceiling narrows every member of that desk, and only that desk's
    /// members — the department scoping the feature exists for.
    #[tokio::test]
    async fn a_desk_ceiling_narrows_its_members_and_nobody_else() {
        let scoped = ROSTER.replace(
            "members = [\"writer\", \"ceo\"]",
            "members = [\"writer\", \"ceo\"]\ntools = [\"workspace.read\"]",
        );
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), &scoped).await;

        // `writer` asks for nothing, so before the desk it held the whole
        // company allow-list. The desk cuts it to one grant.
        let (_, writer) = get_agent(&state, "writer").await;
        assert_eq!(
            strings(&writer["tools"]["deskAllow"]),
            vec!["workspace.read"],
            "{writer}"
        );
        assert_eq!(
            strings(&writer["tools"]["effective"]),
            vec!["workspace.read"],
            "the desk ceiling must bite on a member that requested nothing: {writer}"
        );

        // `hermit` sits on no desk, so it is untouched and still holds the
        // company grant. A ceiling that leaked to non-members would be a scoping
        // bug invisible from the desk's own screen.
        let (_, hermit) = get_agent(&state, "hermit").await;
        assert!(
            strings(&hermit["tools"]["deskAllow"]).is_empty(),
            "{hermit}"
        );
        assert_eq!(
            strings(&hermit["tools"]["effective"]),
            vec!["workspace", "workspace.*", "composio"],
            "{hermit}"
        );
    }

    /// The three rows the console renders must shrink monotonically, or the card
    /// would show a "ceiling" that is not one.
    #[tokio::test]
    async fn a_desk_ceiling_can_never_widen_past_the_company_grant() {
        // The desk names a grant the company never allowed.
        let scoped = ROSTER.replace(
            "members = [\"writer\", \"ceo\"]",
            "members = [\"writer\", \"ceo\"]\ntools = [\"shell\", \"workspace.read\"]",
        );
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), &scoped).await;

        let (_, writer) = get_agent(&state, "writer").await;
        assert!(
            !strings(&writer["tools"]["deskAllow"]).contains(&"shell".to_string()),
            "a desk cannot grant what the company withheld: {writer}"
        );
        assert!(
            !strings(&writer["tools"]["effective"]).contains(&"shell".to_string()),
            "{writer}"
        );
    }

    /// A roster that tags nobody still has an orchestrator: the first declared
    /// agent. A console that read `tier` alone would call every teammate on such
    /// a company a worker, and be wrong about all of them.
    #[tokio::test]
    async fn an_untagged_roster_still_names_an_orchestrator() {
        let home_dir = home();
        let state = state_with_manifest(
            home_dir.path(),
            "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
             [[agent]]\nid = \"writer\"\nrole = \"Writer\"\n\
             [[agent]]\nid = \"editor\"\nrole = \"Editor\"\n",
        )
        .await;

        let (_, writer) = get_agent(&state, "writer").await;
        assert!(writer["tier"].is_null(), "{writer}");
        assert_eq!(writer["isOrchestrator"], true, "{writer}");

        let (_, editor) = get_agent(&state, "editor").await;
        assert_eq!(editor["isOrchestrator"], false, "{editor}");
    }

    /// An operator-added membership counts: the detail view resolves desks
    /// through `effective_desk_members`, not through the manifest's list.
    #[tokio::test]
    async fn an_operator_added_desk_membership_shows_on_the_agent() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;

        assert_eq!(
            get_agent(&state, "hermit").await.1["desks"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let (status, _) = send(
            &state,
            "POST",
            "/api/v1/company/desks/content/members",
            Some(json!({"agent_id": "hermit"})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, hermit) = get_agent(&state, "hermit").await;
        let desks = hermit["desks"].as_array().unwrap();
        assert_eq!(desks.len(), 1, "{hermit}");
        assert_eq!(desks[0]["id"], "content", "{hermit}");
    }

    /// An overlay teammate reads back with the company's standard grant and no
    /// tier, which is exactly what the harness builds it with.
    #[tokio::test]
    async fn an_overlay_teammate_reports_the_standard_grant() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        let (status, agent) = get_agent(&state, &jamie).await;
        assert_eq!(status, StatusCode::OK, "{agent}");
        assert_eq!(agent["source"], "overlay");
        assert_eq!(agent["name"], "Jamie");
        assert!(agent["tier"].is_null(), "{agent}");
        assert_eq!(agent["isOrchestrator"], false, "{agent}");
        assert!(strings(&agent["tools"]["requested"]).is_empty(), "{agent}");
        assert_eq!(
            strings(&agent["tools"]["effective"]),
            vec!["workspace", "workspace.*", "composio"],
            "{agent}"
        );
    }

    /// Issue #601: the roster **list** answers for tools and desks too, with
    /// the same values as the detail read.
    ///
    /// The overview graph is drawn from the list, so before this it had no way
    /// to learn either without an N+1 fetch — and invented both instead, while
    /// the detail card beside it rendered the real thing. The equality is the
    /// contract; anything less lets the two surfaces disagree again.
    #[tokio::test]
    async fn the_roster_list_carries_the_same_tools_and_desks_as_the_detail_read() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        // An overlay teammate too, so the agreement is checked on both halves
        // of the merged roster rather than only on the manifest half.
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        let (status, roster) = send(&state, "GET", "/api/v1/company/team", None).await;
        assert_eq!(status, StatusCode::OK, "{roster}");
        let rows = roster.as_array().unwrap();
        // Three manifest teammates, the overlay one, and every global the
        // fixture does not already declare an id for — this roster has its own
        // `writer`, which supersedes the baseline's rather than adding to it.
        let added = crate::globals::agents()
            .iter()
            .filter(|global| !["ceo", "writer", "hermit"].contains(&global.id.as_str()))
            .count();
        assert_eq!(rows.len(), 4 + added, "{roster}");

        for row in rows {
            let id = row["id"].as_str().unwrap();
            let (_, detail) = get_agent(&state, id).await;
            assert_eq!(
                row["tools"], detail["tools"],
                "the graph reads the list and the card reads the detail; they \
                 must not disagree about {id}"
            );
            assert_eq!(row["desks"], detail["desks"], "desks disagree for {id}");
        }

        let row_of = |id: &str| {
            rows.iter()
                .find(|row| row["id"] == id)
                .unwrap_or_else(|| panic!("{id} missing from {roster}"))
                .clone()
        };

        // …and the shared values are the *right* ones, so a shared-but-wrong
        // constructor cannot pass on agreement alone.
        let ceo = row_of("ceo");
        assert_eq!(
            strings(&ceo["tools"]["effective"]),
            vec!["workspace.read"],
            "a request the company never allowed is not a grant: {ceo}"
        );
        let writer = row_of("writer");
        assert!(
            strings(&writer["tools"]["requested"]).is_empty(),
            "{writer}"
        );
        assert_eq!(
            strings(&writer["tools"]["effective"]),
            vec!["workspace", "workspace.*", "composio"],
            "an agent that lists no tools holds the whole allow-list: {writer}"
        );
        assert_eq!(
            strings(&writer["tools"]["companyAllow"]),
            vec!["workspace", "workspace.*", "composio"],
            "the ceiling rides along, so a reader can tell an empty request \
             from an empty grant: {writer}"
        );

        // Desks, which are the graph's departments now: declared membership,
        // the lead flag off the effective order, and a stated empty list.
        let writer_desks = writer["desks"].as_array().unwrap();
        assert_eq!(writer_desks.len(), 1, "{writer}");
        assert_eq!(writer_desks[0]["id"], "content", "{writer}");
        assert_eq!(writer_desks[0]["name"], "Content desk", "{writer}");
        assert_eq!(writer_desks[0]["lead"], true, "{writer}");
        assert_eq!(ceo["desks"].as_array().unwrap()[0]["lead"], false, "{ceo}");
        assert!(
            row_of("hermit")["desks"].as_array().unwrap().is_empty(),
            "a teammate on no desk says so with an empty list rather than by \
             omitting the key: {roster}"
        );
        assert!(
            row_of(&jamie)["desks"].as_array().unwrap().is_empty(),
            "{roster}"
        );
    }

    /// An operator-added desk membership reaches the list, not just the detail
    /// read — otherwise the graph's pillars would go stale the moment somebody
    /// moved a teammate.
    #[tokio::test]
    async fn a_desk_change_shows_up_on_the_roster_list() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;

        let (status, _) = send(
            &state,
            "POST",
            "/api/v1/company/desks/content/members",
            Some(json!({"agent_id": "hermit"})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, roster) = send(&state, "GET", "/api/v1/company/team", None).await;
        let hermit = roster
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == "hermit")
            .unwrap()
            .clone();
        let desks = hermit["desks"].as_array().unwrap();
        assert_eq!(desks.len(), 1, "{hermit}");
        assert_eq!(desks[0]["id"], "content", "{hermit}");
    }

    /// A teammate created through the console reads back with the grant it
    /// actually holds, so the card the console renders from the POST response
    /// says the same thing the next list read will.
    #[tokio::test]
    async fn a_new_overlay_teammate_is_created_with_the_standard_grant() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;

        let (status, created) = send(
            &state,
            "POST",
            "/api/v1/company/team",
            Some(json!({"name": "Robin", "role": "Support"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert_eq!(
            strings(&created["tools"]["effective"]),
            vec!["workspace", "workspace.*", "composio"],
            "{created}"
        );
        assert!(
            created["desks"].as_array().unwrap().is_empty(),
            "nobody has put it on a desk yet: {created}"
        );

        let (_, detail) = get_agent(&state, created["id"].as_str().unwrap()).await;
        assert_eq!(created["tools"], detail["tools"], "{created} vs {detail}");
        assert_eq!(created["desks"], detail["desks"], "{created} vs {detail}");
    }

    // --- The edit half ------------------------------------------------------

    /// The issue's "write-once per member", gone: a console-defined teammate can
    /// be corrected, and the correction is on the host rather than in a tab.
    #[tokio::test]
    async fn an_overlay_teammate_can_be_edited_and_the_edit_persists() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        let (status, edited) = patch_agent(
            &state,
            &jamie,
            json!({"name": "Jamie R", "role": "Head of Growth", "description": "Runs paid."}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{edited}");
        assert_eq!(edited["name"], "Jamie R", "{edited}");
        assert_eq!(edited["role"], "Head of Growth", "{edited}");
        assert_eq!(edited["description"], "Runs paid.", "{edited}");

        // Read back through a fresh request, so this is the stored record and
        // not the handler's own answer.
        let (_, reread) = get_agent(&state, &jamie).await;
        assert_eq!(reread["name"], "Jamie R", "{reread}");
        assert_eq!(reread["role"], "Head of Growth", "{reread}");

        // …and the roster list agrees, so the card the operator came from is
        // updated too rather than only the panel they edited in.
        let (_, roster) = send(&state, "GET", "/api/v1/company/team", None).await;
        let row = roster
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == jamie.as_str())
            .unwrap()
            .clone();
        assert_eq!(row["name"], "Jamie R", "{row}");
        assert_eq!(row["role"], "Head of Growth", "{row}");
    }

    /// A patch leaves what it does not mention alone, and an explicit `null`
    /// clears the description. Collapsing those two would make every partial
    /// save erase an agent's instructions.
    #[tokio::test]
    async fn an_absent_field_is_left_alone_and_null_clears_the_description() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        let (status, only_role) = patch_agent(&state, &jamie, json!({"role": "Growth Lead"})).await;
        assert_eq!(status, StatusCode::OK, "{only_role}");
        assert_eq!(only_role["name"], "Jamie", "{only_role}");
        assert_eq!(
            only_role["description"], "Original.",
            "an unmentioned field survives the patch: {only_role}"
        );

        let (status, cleared) = patch_agent(&state, &jamie, json!({"description": null})).await;
        assert_eq!(status, StatusCode::OK, "{cleared}");
        assert!(
            cleared["description"].is_null(),
            "an explicit null clears it: {cleared}"
        );
        assert_eq!(cleared["role"], "Growth Lead", "{cleared}");
    }

    /// A **manifest** teammate — the shape every default and every global
    /// baseline agent has — is editable here, and the edit sticks. This is the
    /// whole point of the override layer: a hosted operator has no
    /// `company.toml` to edit and no redeploy to make, so a roster that could
    /// only be changed in the blueprint was a roster nobody could change.
    #[tokio::test]
    async fn a_manifest_teammate_can_be_edited_here() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;

        let (status, edited) = patch_agent(
            &state,
            "ceo",
            json!({"role": "Chief Vibes", "name": "Robin", "description": "Sets the beat."}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{edited}");

        let (_, ceo) = get_agent(&state, "ceo").await;
        assert_eq!(ceo["role"], "Chief Vibes", "{ceo}");
        assert_eq!(ceo["name"], "Robin", "{ceo}");
        assert_eq!(ceo["description"], "Sets the beat.", "{ceo}");
        // Still a blueprint teammate — the manifest was not rewritten, the edit
        // is an overlay on top of it.
        assert_eq!(ceo["source"], "manifest", "{ceo}");

        // A second patch merges rather than replacing: a field nobody mentioned
        // keeps the value the first edit gave it.
        let (status, again) = patch_agent(&state, "ceo", json!({"description": null})).await;
        assert_eq!(status, StatusCode::OK, "{again}");
        assert!(again["description"].is_null(), "{again}");
        assert_eq!(again["role"], "Chief Vibes", "{again}");
        assert_eq!(again["name"], "Robin", "{again}");
    }

    /// An untouched field keeps tracking the blueprint, so a redeploy that
    /// changes it is still felt. The override is per field, not a snapshot of
    /// the whole row.
    #[tokio::test]
    async fn an_unedited_field_still_comes_from_the_manifest() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;

        let (status, edited) = patch_agent(&state, "ceo", json!({"role": "Chief Vibes"})).await;
        assert_eq!(status, StatusCode::OK, "{edited}");

        let (_, ceo) = get_agent(&state, "ceo").await;
        assert_eq!(
            ceo["description"], "Sets direction and delegates.",
            "the manifest still answers for what nobody edited: {ceo}"
        );
        assert_eq!(ceo["tier"], "orchestrator", "{ceo}");
        assert_eq!(
            strings(&ceo["tools"]["requested"]),
            vec!["workspace.read", "email.send"],
            "{ceo}"
        );
    }

    /// A manifest with a blueprint `prompt`, so the persona-override tests have a
    /// seed for "Reset to blueprint" to restore.
    const PERSONA_MANIFEST: &str = r#"
[company]
name = "Acme"
[policy]
mode = "full"
[tools]
allow = ["workspace", "workspace.*"]

[[agent]]
id = "ceo"
role = "Chief Executive"
prompt = "Lead decisively."
"#;

    /// Issue #1530: a manifest teammate's persona `instructions` ARE editable —
    /// they write to the override record, not `company.toml`, so no `409` — while
    /// every other manifest field stays read-only. The response exposes the
    /// effective text, the blueprint it would reset to, and that it is overridden.
    #[tokio::test]
    async fn instructions_are_editable_on_a_manifest_teammate_without_a_409() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), PERSONA_MANIFEST).await;

        let (status, edited) = patch_agent(
            &state,
            "ceo",
            json!({"instructions": "Answer only in haiku."}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "an instructions-only edit is legal: {edited}"
        );
        assert_eq!(edited["instructions"], "Answer only in haiku.", "{edited}");
        assert_eq!(edited["instructionsOverridden"], true, "{edited}");
        assert_eq!(
            edited["blueprintInstructions"], "Lead decisively.",
            "the blueprint seed is surfaced for Reset: {edited}"
        );

        // Persisted, not just echoed by the handler.
        let (_, reread) = get_agent(&state, "ceo").await;
        assert_eq!(reread["instructions"], "Answer only in haiku.", "{reread}");
        assert_eq!(reread["instructionsOverridden"], true, "{reread}");

        // Merged behavior (main's agent-edit surface): a manifest teammate's
        // native fields are editable through the same override layer — a role
        // edit returns 200 and lands as an overlay, `company.toml` untouched —
        // and it composes with the instructions override set above.
        let (status, edited_role) =
            patch_agent(&state, "ceo", json!({"role": "Chief Vibes"})).await;
        assert_eq!(status, StatusCode::OK, "{edited_role}");
        assert_eq!(edited_role["role"], "Chief Vibes", "{edited_role}");
        assert_eq!(
            edited_role["source"], "manifest",
            "still a blueprint teammate: {edited_role}"
        );
        assert_eq!(
            edited_role["instructions"], "Answer only in haiku.",
            "the role edit leaves the instructions override intact: {edited_role}"
        );
    }

    /// Issue #1530: `instructions: null` on a manifest teammate clears the
    /// override and resets to the blueprint `prompt` — the escape hatch that
    /// keeps the override from masking version control forever.
    #[tokio::test]
    async fn null_instructions_resets_a_manifest_teammate_to_blueprint() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), PERSONA_MANIFEST).await;

        // Override, then reset.
        let (status, _) =
            patch_agent(&state, "ceo", json!({"instructions": "Custom voice."})).await;
        assert_eq!(status, StatusCode::OK);
        let (status, reset) = patch_agent(&state, "ceo", json!({"instructions": null})).await;
        assert_eq!(status, StatusCode::OK, "{reset}");
        assert_eq!(
            reset["instructions"], "Lead decisively.",
            "reset falls back to the blueprint: {reset}"
        );
        assert_eq!(
            reset["instructionsOverridden"], false,
            "no override masks the blueprint after a reset: {reset}"
        );

        // A blank string is a reset too, so an emptied editor never blanks the
        // persona.
        let (status, _) =
            patch_agent(&state, "ceo", json!({"instructions": "Custom voice."})).await;
        assert_eq!(status, StatusCode::OK);
        let (status, blanked) = patch_agent(&state, "ceo", json!({"instructions": "   "})).await;
        assert_eq!(status, StatusCode::OK, "{blanked}");
        assert_eq!(blanked["instructions"], "Lead decisively.", "{blanked}");
        assert_eq!(blanked["instructionsOverridden"], false, "{blanked}");
    }

    /// Issue #1530: an overlay teammate's persona is editable the same way. It
    /// has no manifest `prompt`, so `blueprintInstructions` is absent and a reset
    /// falls all the way to nothing.
    #[tokio::test]
    async fn instructions_are_editable_on_an_overlay_teammate() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        let (status, edited) = patch_agent(
            &state,
            &jamie,
            json!({"instructions": "Be terse and data-first."}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{edited}");
        assert_eq!(
            edited["instructions"], "Be terse and data-first.",
            "{edited}"
        );
        assert_eq!(edited["instructionsOverridden"], true, "{edited}");
        assert!(
            edited["blueprintInstructions"].is_null(),
            "an overlay teammate has no manifest seed to reset to: {edited}"
        );

        let (status, reset) = patch_agent(&state, &jamie, json!({"instructions": null})).await;
        assert_eq!(status, StatusCode::OK, "{reset}");
        assert!(
            reset["instructions"].is_null(),
            "clearing an overlay override leaves no persona text: {reset}"
        );
        assert_eq!(reset["instructionsOverridden"], false, "{reset}");
    }

    /// Review (PR #1549): an oversized `instructions` write is capped to the
    /// prompt budget rather than stored verbatim, so a single pasted
    /// "AGENT.md"-style document cannot unboundedly inflate every turn's
    /// persona prompt. The leading portion is kept and the cut is marked.
    #[tokio::test]
    async fn overlong_instructions_are_capped_at_the_write_boundary() {
        use crate::company::PROMPT_FILE_BUDGET_CHARS;

        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        let over: String = "x".repeat(PROMPT_FILE_BUDGET_CHARS + 40);
        let (status, edited) = patch_agent(&state, &jamie, json!({"instructions": over})).await;
        assert_eq!(status, StatusCode::OK, "{edited}");
        let stored = edited["instructions"].as_str().unwrap();
        assert!(
            stored.starts_with(&"x".repeat(PROMPT_FILE_BUDGET_CHARS)),
            "the leading portion is kept: {:?}",
            &stored[..64.min(stored.len())]
        );
        assert!(
            stored.contains("truncated"),
            "an overlong override is marked as cut"
        );
        assert!(
            stored.chars().count() <= PROMPT_FILE_BUDGET_CHARS + 40,
            "capped text stays bounded: {}",
            stored.chars().count()
        );
    }

    /// The console renders read-only from the host's answer, not from a rule of
    /// its own — so this list is part of the contract.
    #[tokio::test]
    async fn the_host_states_which_fields_are_editable() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        let (_, agent) = get_agent(&state, &jamie).await;
        assert_eq!(
            strings(&agent["editable"]),
            vec![
                "name",
                "role",
                "description",
                "tools",
                "instructions",
                "model",
                "harness"
            ],
            "{agent}"
        );
    }

    /// Issue #619: a teammate can be narrowed **after** it exists, not only at
    /// creation.
    ///
    /// #661 made the scope writable on `POST …/team` and through `add_agent`.
    /// This is the half that was missing — without it, correcting a teammate's
    /// grant means deleting and recreating it, which orphans its workspace
    /// folder, budget row, desk memberships and inbox.
    ///
    /// The three levels are asserted separately on purpose: `requested` proves
    /// the scope was stored, `effective` proves it reached the function the
    /// harness builds the agent with, and the untouched company `allow` proves
    /// the narrowing is per-teammate rather than a company-wide edit.
    #[tokio::test]
    async fn an_overlay_teammate_can_be_scoped_after_creation() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        let (_, before) = get_agent(&state, &jamie).await;
        assert!(
            strings(&before["tools"]["requested"]).is_empty(),
            "unscoped to begin with: {before}"
        );
        assert_eq!(
            strings(&before["tools"]["effective"]),
            vec!["workspace", "workspace.*", "composio"],
            "which resolves to everything the company allows: {before}"
        );

        let (status, scoped) = patch_agent(&state, &jamie, json!({"tools": ["workspace"]})).await;
        assert_eq!(status, StatusCode::OK, "{scoped}");
        assert_eq!(
            strings(&scoped["tools"]["requested"]),
            vec!["workspace"],
            "{scoped}"
        );
        assert_eq!(
            strings(&scoped["tools"]["effective"]),
            vec!["workspace"],
            "and it is narrower than the company grant, which is the point: {scoped}"
        );
        assert_eq!(
            strings(&scoped["tools"]["companyAllow"]),
            vec!["workspace", "workspace.*", "composio"],
            "the company ceiling is untouched — this scoped one teammate: {scoped}"
        );

        // Read back through a fresh request, so this is the stored record and
        // not the handler's own answer.
        let (_, reread) = get_agent(&state, &jamie).await;
        assert_eq!(
            strings(&reread["tools"]["requested"]),
            vec!["workspace"],
            "{reread}"
        );

        // An empty list is the deliberate way back to the standard grant, and
        // must read as "inherits everything" rather than "holds nothing".
        let (status, cleared) = patch_agent(&state, &jamie, json!({"tools": []})).await;
        assert_eq!(status, StatusCode::OK, "{cleared}");
        assert!(
            strings(&cleared["tools"]["requested"]).is_empty(),
            "{cleared}"
        );
        assert_eq!(
            strings(&cleared["tools"]["effective"]),
            vec!["workspace", "workspace.*", "composio"],
            "{cleared}"
        );
    }

    /// **The review finding (#745).** A member must not be able to widen a
    /// teammate's scope — and because an empty list means "the company's
    /// standard grant", `{"tools": []}` is the widest possible widening.
    ///
    /// This is #619's own defect reachable through the route added to fix it:
    /// `add_agent` refuses a narrowing that lands empty precisely because an
    /// empty list inherits everything, and leaving `edit_agent` member-open
    /// would have let any signed-in member undo any scoping with one call.
    ///
    /// The two-account shape is the point: the harness signs every other
    /// request in as an admin, so a check verified only as an admin passes
    /// identically against no check at all.
    #[tokio::test]
    async fn a_member_cannot_widen_a_teammates_scope() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        crate::server::test_support::seed_fixed_member(&state, "acme").await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        // Scoped by an admin.
        let (status, _) = patch_agent(&state, &jamie, json!({"tools": ["workspace"]})).await;
        assert_eq!(status, StatusCode::OK);

        let uri = format!("/api/v1/company/team/{jamie}");
        let member = || crate::server::test_support::member_cookie("acme");

        // The widening a member must not be able to perform.
        let (status, refusal) =
            send_as(&state, "PATCH", &uri, Some(json!({"tools": []})), member()).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "an empty list is the company's whole grant: {refusal}"
        );

        // …and neither may a member set a different scope at all.
        let (status, _) = send_as(
            &state,
            "PATCH",
            &uri,
            Some(json!({"tools": ["composio"]})),
            member(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // Nothing was written by either attempt.
        let (_, unchanged) = get_agent(&state, &jamie).await;
        assert_eq!(
            strings(&unchanged["tools"]["requested"]),
            vec!["workspace"],
            "the scope an admin set must survive both refusals: {unchanged}"
        );
    }

    /// Issue #1245's per-agent follow-up: an admin can set and clear a
    /// teammate's own model override, and a member meets the same `403` this
    /// module already enforces for `tools` — the two fields share the
    /// "cost/scope decision" character `edit_agent`'s own docs give for why
    /// `tools` is admin-only.
    ///
    /// `ACP_ROSTER`, not `ROSTER`: the fresh overlay teammate lands on
    /// whichever harness is `default = true`, and a model only means
    /// anything there when that harness is `acp` — see the cross-field
    /// rejection test below for the `built_in` case this deliberately avoids.
    #[tokio::test]
    async fn an_admin_can_set_and_clear_a_teammates_model_override() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ACP_ROSTER).await;
        crate::server::test_support::seed_fixed_member(&state, "acme").await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        // Undeclared until set.
        let (_, before) = get_agent(&state, &jamie).await;
        assert!(before["model"].is_null(), "{before}");

        // A member may not set one.
        let (status, refusal) = send_as(
            &state,
            "PATCH",
            &format!("/api/v1/company/team/{jamie}"),
            Some(json!({"model": "claude-opus-4-5"})),
            crate::server::test_support::member_cookie("acme"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refusal}");

        // An admin may.
        let (status, set) = patch_agent(&state, &jamie, json!({"model": "claude-opus-4-5"})).await;
        assert_eq!(status, StatusCode::OK, "{set}");
        assert_eq!(set["model"], "claude-opus-4-5");

        let (_, reread) = get_agent(&state, &jamie).await;
        assert_eq!(reread["model"], "claude-opus-4-5", "{reread}");

        // `null` clears it back to the harness's own default.
        let (status, cleared) = patch_agent(&state, &jamie, json!({"model": null})).await;
        assert_eq!(status, StatusCode::OK, "{cleared}");
        assert!(cleared["model"].is_null(), "{cleared}");
    }

    /// Issue #1245's harness-picker follow-up: a model override is refused
    /// outright when the teammate's harness (here, the implicit `built_in`
    /// default `ROSTER` never overrides) has no ACP transport to forward it
    /// to — the overlay-write mirror of `CompanyManifest::validate`'s
    /// identical rule for a manifest agent's own `model`.
    #[tokio::test]
    async fn a_model_override_is_refused_off_an_acp_harness() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        let (status, refusal) =
            patch_agent(&state, &jamie, json!({"model": "claude-opus-4-5"})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{refusal}");

        let (_, unchanged) = get_agent(&state, &jamie).await;
        assert!(unchanged["model"].is_null(), "{unchanged}");
    }

    /// Issue #1245's harness-picker follow-up: a teammate's harness binding
    /// is admin-only (same gate as `model`/`tools`), validated against the
    /// company's own declared set, and clears back to the default with
    /// `null` — the same three behaviours the model test above proves for
    /// `model`, on the sibling field.
    #[tokio::test]
    async fn an_admin_can_pin_and_clear_a_teammates_harness() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ACP_ROSTER).await;
        crate::server::test_support::seed_fixed_member(&state, "acme").await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        // Undeclared until set — this teammate is on the default (`laptop`)
        // implicitly, not by naming it.
        let (_, before) = get_agent(&state, &jamie).await;
        assert!(before["harness"].is_null(), "{before}");

        // A member may not set one.
        let (status, refusal) = send_as(
            &state,
            "PATCH",
            &format!("/api/v1/company/team/{jamie}"),
            Some(json!({"harness": "main"})),
            crate::server::test_support::member_cookie("acme"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refusal}");

        // An unknown id is refused, not silently accepted into a binding
        // that would orphan the teammate from every harness's serve set.
        let (status, refusal) =
            patch_agent(&state, &jamie, json!({"harness": "does-not-exist"})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{refusal}");

        // An admin may pin it to a declared harness.
        let (status, set) = patch_agent(&state, &jamie, json!({"harness": "main"})).await;
        assert_eq!(status, StatusCode::OK, "{set}");
        assert_eq!(set["harness"], "main");

        let (_, reread) = get_agent(&state, &jamie).await;
        assert_eq!(reread["harness"], "main", "{reread}");

        // `null` clears it back to the declared default.
        let (status, cleared) = patch_agent(&state, &jamie, json!({"harness": null})).await;
        assert_eq!(status, StatusCode::OK, "{cleared}");
        assert!(cleared["harness"].is_null(), "{cleared}");
    }

    /// Issue #1245's harness-picker follow-up: switching a teammate onto an
    /// ACP harness and setting its model happen in the same `PATCH` in the
    /// console's own edit flow, so the cross-field check has to validate
    /// against the *new* binding, not the stale one — this is the case that
    /// would wrongly 400 if it read `declared_harness` unconditionally
    /// instead of preferring the harness this same request also sent.
    #[tokio::test]
    async fn harness_and_model_can_be_set_together_against_the_new_binding() {
        let home_dir = home();
        // `main` (`built_in`) is default here — the opposite of `ACP_ROSTER`
        // — so a model alone would be refused, and only succeeds because
        // this request also moves the teammate onto `laptop` in the same call.
        const TOML: &str = r#"
[company]
name = "Acme"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[harness]]
id = "main"
kind = "built_in"
default = true

[[harness]]
id = "laptop"
kind = "acp"

[harness.acp]
transport = "local"
agent = "claude"
"#;
        let state = state_with_manifest(home_dir.path(), TOML).await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        let (status, set) = patch_agent(
            &state,
            &jamie,
            json!({"harness": "laptop", "model": "claude-opus-4-5"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{set}");
        assert_eq!(set["harness"], "laptop");
        assert_eq!(set["model"], "claude-opus-4-5");
    }

    /// The same edit against a **manifest** teammate, which is the common case
    /// and the one that silently did nothing.
    ///
    /// Both fields were advertised in `editable` and accepted with a 200, but
    /// the override written for a blueprint agent carried only name, role,
    /// tools and description — so the values were dropped on the floor and the
    /// next read returned the blueprint's. Nothing surfaced the loss: the
    /// response body echoed the request, so it looked saved.
    ///
    /// Asserted through a fresh `GET` rather than the `PATCH` response,
    /// because echoing the request back is precisely what made the bug
    /// invisible.
    #[tokio::test]
    async fn harness_and_model_persist_for_a_manifest_teammate() {
        let home_dir = home();
        const TOML: &str = r#"
[company]
name = "Acme"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[harness]]
id = "main"
kind = "built_in"
default = true

[[harness]]
id = "laptop"
kind = "acp"

[harness.acp]
transport = "local"
agent = "claude"
"#;
        let state = state_with_manifest(home_dir.path(), TOML).await;

        let (status, set) = patch_agent(
            &state,
            "ceo",
            json!({"harness": "laptop", "model": "claude-opus-4-5"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{set}");

        let (_, reread) = get_agent(&state, "ceo").await;
        assert_eq!(reread["harness"], "laptop", "{reread}");
        assert_eq!(reread["model"], "claude-opus-4-5", "{reread}");

        // And clearing returns it to the blueprint rather than sticking.
        let (status, cleared) =
            patch_agent(&state, "ceo", json!({"harness": null, "model": null})).await;
        assert_eq!(status, StatusCode::OK, "{cleared}");
        let (_, after) = get_agent(&state, "ceo").await;
        assert!(after["harness"].is_null(), "{after}");
        assert!(after["model"].is_null(), "{after}");
    }

    /// Resetting instructions must not take the harness and model with it.
    ///
    /// `clear_agent_override` drops an override row once nothing is left in
    /// it, and its retention predicate named only the fields that existed when
    /// it was written — so for a teammate whose row held instructions plus a
    /// harness, clearing the first deleted the row and silently reverted the
    /// second to the blueprint.
    #[tokio::test]
    async fn clearing_instructions_leaves_the_harness_binding_alone() {
        let home_dir = home();
        const TOML: &str = r#"
[company]
name = "Acme"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[harness]]
id = "main"
kind = "built_in"
default = true

[[harness]]
id = "laptop"
kind = "acp"

[harness.acp]
transport = "local"
agent = "claude"
"#;
        let state = state_with_manifest(home_dir.path(), TOML).await;

        let (status, _) = patch_agent(
            &state,
            "ceo",
            json!({"harness": "laptop", "instructions": "Be brief."}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = patch_agent(&state, "ceo", json!({"instructions": null})).await;
        assert_eq!(status, StatusCode::OK);

        let (_, after) = get_agent(&state, "ceo").await;
        assert_eq!(
            after["harness"], "laptop",
            "clearing one override field must not discard the others: {after}"
        );
    }

    /// A `runner` harness is `kind = "acp"` and still cannot carry a model —
    /// its wire protocol has no field for one. `CompanyManifest::validate`
    /// already refuses the combination, so accepting it here let the API store
    /// a binding a manifest may not declare and that could never take effect.
    #[tokio::test]
    async fn a_model_is_refused_on_a_runner_bound_harness() {
        let home_dir = home();
        const TOML: &str = r#"
[company]
name = "Acme"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[harness]]
id = "main"
kind = "built_in"
default = true

[[harness]]
id = "shared"
kind = "acp"

[harness.acp]
transport = "runner"
runner = "build-box"
agent = "claude"
"#;
        let state = state_with_manifest(home_dir.path(), TOML).await;

        let (status, refused) = patch_agent(
            &state,
            "ceo",
            json!({"harness": "shared", "model": "claude-opus-4-5"}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
        assert!(
            refused.to_string().contains("runner"),
            "the refusal names the reason: {refused}"
        );
    }

    /// **Review of #745.** An unknown id answers the same way whether or not
    /// the body carries `tools`.
    ///
    /// The invariant, stated independently of which ordering is "right": one
    /// route must not give two answers about whether a teammate exists,
    /// decided by an unrelated field. Putting the conditional admin check
    /// before the existence lookup did exactly that — `{"name": "x"}` on an
    /// unknown id returned `404` while `{"tools": […]}` on the same id
    /// returned `403`.
    ///
    /// Driven as a **member**, because that is the only actor for whom the two
    /// orderings differ: an admin passes the check either way and would see
    /// `404` regardless, so a test written as an admin would pass against the
    /// broken ordering too.
    #[tokio::test]
    async fn an_unknown_teammate_is_a_404_whether_or_not_tools_are_sent() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        crate::server::test_support::seed_fixed_member(&state, "acme").await;

        let uri = "/api/v1/company/team/nobody";
        let member = || crate::server::test_support::member_cookie("acme");

        let (without_tools, _) = send_as(
            &state,
            "PATCH",
            uri,
            Some(json!({"role": "Ghost"})),
            member(),
        )
        .await;
        let (with_tools, _) = send_as(
            &state,
            "PATCH",
            uri,
            Some(json!({"tools": ["workspace"]})),
            member(),
        )
        .await;

        assert_eq!(
            with_tools, without_tools,
            "an unrelated field must not change whether a teammate is reported \
             as existing"
        );
        assert_eq!(
            with_tools,
            StatusCode::NOT_FOUND,
            "and the shared answer is 404: existence is already readable by any \
             member through GET, so 403-first would hide nothing"
        );
    }

    /// The conditional check must not take an existing capability away: a
    /// member editing a name or a role keeps working exactly as before, which
    /// is the same rule `POST …/team` applies to its budget cap.
    #[tokio::test]
    async fn a_member_may_still_edit_a_teammates_name_and_role() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        crate::server::test_support::seed_fixed_member(&state, "acme").await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        let (status, edited) = send_as(
            &state,
            "PATCH",
            &format!("/api/v1/company/team/{jamie}"),
            Some(json!({"name": "Jamie R", "role": "Head of Growth"})),
            crate::server::test_support::member_cookie("acme"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{edited}");
        assert_eq!(edited["name"], "Jamie R", "{edited}");
        assert_eq!(edited["role"], "Head of Growth", "{edited}");
    }

    /// `editable` is the host stating the rule so the console does not
    /// re-derive it. It therefore has to answer per **actor**, or a member is
    /// offered a `tools` field whose save is a `403` — the drift this list
    /// exists to remove.
    #[tokio::test]
    async fn editable_names_tools_only_for_an_admin() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        crate::server::test_support::seed_fixed_member(&state, "acme").await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        let (_, as_admin) = get_agent(&state, &jamie).await;
        assert_eq!(
            strings(&as_admin["editable"]),
            vec![
                "name",
                "role",
                "description",
                "tools",
                "instructions",
                "model",
                "harness"
            ],
            "{as_admin}"
        );

        let (_, as_member) = send_as(
            &state,
            "GET",
            &format!("/api/v1/company/team/{jamie}"),
            None,
            crate::server::test_support::member_cookie("acme"),
        )
        .await;
        assert_eq!(
            strings(&as_member["editable"]),
            vec!["name", "role", "description", "instructions"],
            "a member is not offered a field they cannot save: {as_member}"
        );
    }

    /// A blank glob is refused rather than stored: `""` matches nothing an
    /// operator meant, so it would read as a scope that grants nothing while
    /// looking like a scope that was set.
    #[tokio::test]
    async fn a_blank_tool_glob_is_refused() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        let (status, refusal) =
            patch_agent(&state, &jamie, json!({"tools": ["workspace", "  "]})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{refusal}");

        let (_, unchanged) = get_agent(&state, &jamie).await;
        assert!(
            strings(&unchanged["tools"]["requested"]).is_empty(),
            "and nothing was written: {unchanged}"
        );
    }

    /// A manifest teammate's tool line is editable too, and lands under the
    /// same ceiling every other grant does: the request is stored verbatim and
    /// intersected with `[tools].allow` at read time, so this can narrow a
    /// teammate within the company grant and never past it.
    #[tokio::test]
    async fn a_manifest_teammates_tools_can_be_narrowed_here() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;

        let (status, edited) = patch_agent(&state, "ceo", json!({"tools": ["workspace"]})).await;
        assert_eq!(status, StatusCode::OK, "{edited}");

        let (_, ceo) = get_agent(&state, "ceo").await;
        assert_eq!(
            strings(&ceo["tools"]["requested"]),
            vec!["workspace"],
            "{ceo}"
        );
        assert_eq!(
            strings(&ceo["tools"]["effective"]),
            vec!["workspace"],
            "{ceo}"
        );
    }

    /// A blank name would render a card with no way back to it, so it is a
    /// refusal rather than a stored blank. Whitespace is trimmed, not accepted.
    #[tokio::test]
    async fn a_blank_name_or_role_is_refused() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;
        let jamie = add_overlay(&state, "Jamie", "Growth").await;

        for body in [json!({"name": "   "}), json!({"role": ""})] {
            let (status, refusal) = patch_agent(&state, &jamie, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body} → {refusal}");
        }

        let (status, trimmed) = patch_agent(&state, &jamie, json!({"name": "  Jamie R  "})).await;
        assert_eq!(status, StatusCode::OK, "{trimmed}");
        assert_eq!(trimmed["name"], "Jamie R", "{trimmed}");
    }

    /// A teammate the operator has **removed** is an id that names nobody, and
    /// the refusal has to land before anything is written.
    ///
    /// A retired manifest id still matches `manifest.agents`, so the obvious
    /// existence check passes and the handler stores an override — for a
    /// teammate `detail` then answers `404` for. That is a failed request that
    /// mutated the record on its way out, and it leaves an edit waiting to be
    /// applied to whoever next takes that id: the id is a slug of the display
    /// name, so a later teammate can inherit a rename nobody made for it.
    #[tokio::test]
    async fn a_removed_teammate_is_not_found_and_no_edit_is_stored() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;

        // Remove `writer` through the route an operator would use, leaving the
        // blueprint that declares it untouched.
        let (status, _) = send(&state, "DELETE", "/api/v1/company/team/writer", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = get_agent(&state, "writer").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "a removed teammate is gone");

        let (status, _) = patch_agent(&state, "writer", json!({"role": "Ghost Writer"})).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // The record is the assertion that matters: the refusal must be the end
        // of the request, not a `404` rendered over a write that already landed.
        let record = state
            .registry()
            .get(&CompanyId::new("acme"))
            .unwrap()
            .store()
            .load(&CompanyId::new("acme"))
            .await
            .unwrap()
            .unwrap();
        assert!(
            record.agent_override("writer").is_none(),
            "the refused edit was stored anyway: {:?}",
            record.overlay_agent_edits
        );
    }

    /// An id that names nobody is a `404` on both verbs, rather than a detail
    /// view of a teammate that does not exist or a write that lands nowhere.
    #[tokio::test]
    async fn an_unknown_teammate_is_not_found() {
        let home_dir = home();
        let state = state_with_manifest(home_dir.path(), ROSTER).await;

        let (status, _) = get_agent(&state, "nobody").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = patch_agent(&state, "nobody", json!({"role": "Ghost"})).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
