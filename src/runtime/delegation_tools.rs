//! Brain-agnostic delegation-tool primitives (issue #176, slice 2).
//!
//! The delegation *runtime* — draining a queue and running desk-lead turns —
//! lives in [`delegation`](crate::runtime::delegation) and is harness-only
//! (it needs in-process cognition). But the delegation *tools* themselves —
//! their names, their argument schemas, and the desk-lead resolver — are
//! brain-agnostic: the hosted Medulla path advertises the same tools to the
//! remote cognition service and services them device-side.
//!
//! This module holds those brain-agnostic pieces so BOTH brains share one
//! definition:
//!
//! - the canonical tool-name constants ([`SPAWN_TASK_TOOL`],
//!   [`DELEGATE_TO_DESK_TOOL`]) — the harness `orchestrator` module re-exports
//!   them so the two paths cannot drift;
//! - [`delegation_manifest_entries`], the [`ToolManifestEntry`] catalog the
//!   hosted brain registers with Medulla;
//! - [`desk_lead`], the desk-lead resolver (moved here from the harness-only
//!   [`delegation`](crate::runtime::delegation) module so the hosted path can
//!   resolve a desk's lead without the `openhuman` feature);
//! - the argument parsers ([`SpawnTaskArgs`], [`DelegateArgs`]) the host uses
//!   to service a `spawn_task` / `delegate_to_desk` tool-call frame;
//! - the hand-off **target checks** — [`reject_desk_target`] (issue #272) and,
//!   since the recursive-delegation slice, [`reject_cycle_target`] and
//!   [`reject_out_of_allowlist_target`] (issue #176). Each returns the refusal
//!   as an `Option<String>` rather than rejecting in place, so the harness tool
//!   can turn it into a `ToolResult::error` and the hosted device-side handler
//!   into a failed tool frame, from one definition.
//!
//! What is *not* here is the enforcement of delegation **depth**. That lives on
//! the harness [`DelegationQueue`](crate::harness::orchestrator::DelegationQueue),
//! because only the harness runs a desk member's turn at all: the hosted path
//! services delegation tools solely for the orchestrator's own cycle and writes
//! a durable card, so its chain is always empty. The definitions above are
//! brain-agnostic; the recursion they guard is not.
//!
//! Compiled in every build (no feature gate): the hosted brain is in the
//! default build, and the harness path re-exports from here.

use serde_json::{Value, json};

use crate::brain::medulla::wire::ToolManifestEntry;
use crate::ports::types::{CompanyRecord, TeammateResolution};

/// The `spawn_task` tool name — open a tracked task card on the board.
pub const SPAWN_TASK_TOOL: &str = "spawn_task";
/// The `delegate_to_desk` tool name — hand work to a desk's lead member.
pub const DELEGATE_TO_DESK_TOOL: &str = "delegate_to_desk";
/// The `delegate_to_teammate` tool name — hand work to a **named teammate**
/// rather than to whoever leads their desk (issue #884).
///
/// [`DELEGATE_TO_DESK_TOOL`] always resolves to [`desk_lead`], so a desk's own
/// lead had no way to reach anybody else on its desk: handing work back to that
/// desk is refused by [`reject_cycle_target`] as self-delegation, and there was
/// no other tool. A three-person desk asked for the specialist by name got the
/// lead reading the request correctly and declining it, because declining was
/// the only move the toolbelt left.
pub const DELEGATE_TO_TEAMMATE_TOOL: &str = "delegate_to_teammate";

/// The `spawn_task` argument schema, shared by the harness tool's
/// `parameters_schema` and the hosted manifest entry so the two never drift.
pub fn spawn_task_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": { "type": "string", "description": "The task title." },
            "note": { "type": "string", "description": "An optional longer brief." },
            "assignee": { "type": "string", "description": "An optional desk/teammate id to own it." }
        },
        "required": ["title"],
        "additionalProperties": false
    })
}

/// The `delegate_to_desk` argument schema, shared by the harness tool and the
/// hosted manifest entry.
pub fn delegate_to_desk_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "desk": { "type": "string", "description": "The desk id or name to delegate to." },
            "instruction": { "type": "string", "description": "The instruction for the desk's lead member." }
        },
        "required": ["desk", "instruction"],
        "additionalProperties": false
    })
}

/// The `delegate_to_teammate` argument schema (issue #884).
///
/// `teammate` takes a **roster id**, never a display name and never prose. See
/// [`reject_teammate_target`] for why the target is a closed set resolved
/// against the record rather than anything read out of the message.
pub fn delegate_to_teammate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "teammate": { "type": "string", "description": "The roster id of the teammate to hand the work to." },
            "instruction": { "type": "string", "description": "The instruction for that teammate." }
        },
        "required": ["teammate", "instruction"],
        "additionalProperties": false
    })
}

/// The delegation tools advertised to Medulla on the hosted path: `spawn_task`
/// and `delegate_to_desk`, with the same names + schemas the harness exposes.
///
/// **`delegate_to_teammate` is deliberately absent** (issue #884). The hosted
/// brain forwards every event to a single `counterpart_agent_id` and does no
/// per-agent routing at all (`src/brain/hosted.rs`, tracked in #176), so
/// advertising a tool the device-side handler in
/// [`cycle`](crate::runtime::cycle) cannot service would turn a hand-off into a
/// failed tool frame. The harness path wires it directly instead.
///
/// Registered on top of the manifest's own `tools.allow` catalog so a hosted
/// company's orchestrator can delegate exactly as the harness one does. The
/// device services the resulting tool-call frames in
/// [`CycleHostImpl`](crate::runtime::cycle) without any local cognition — a
/// `spawn_task` opens a board card, a `delegate_to_desk` writes a durable
/// hand-off card assigned to the desk. (The *synchronous* desk-lead cognition
/// relay the harness performs in-process needs Medulla multi-agent support and
/// is tracked separately in #176 — the hosted hand-off is durable and
/// asynchronous.)
pub fn delegation_manifest_entries() -> Vec<ToolManifestEntry> {
    vec![
        ToolManifestEntry {
            name: SPAWN_TASK_TOOL.to_string(),
            description: Some(
                "Open a tracked task card on the company's board for work that should be \
followed up. Provide a `title`, an optional `note` brief, and an optional `assignee` (a desk \
or teammate id)."
                    .to_string(),
            ),
            input_schema: Some(spawn_task_schema()),
        },
        ToolManifestEntry {
            name: DELEGATE_TO_DESK_TOOL.to_string(),
            description: Some(
                "Hand work to a desk's lead member. Provide the `desk` (its id or name) and the \
`instruction` to carry out. The hand-off is tracked as a task card assigned to that desk, so \
the desk knows it was handed the work when asked directly."
                    .to_string(),
            ),
            input_schema: Some(delegate_to_desk_schema()),
        },
    ]
}

/// The lead member of a desk: the first effective member (manifest ∪ overlay)
/// that is a real roster teammate. `None` when no desk matches or none of its
/// members are on the roster.
///
/// Resolves the desk key (id or case-insensitive name) against both manifest
/// and operator-created overlay desks, then reads the same **effective**
/// membership the REST `list_desks` handler uses
/// ([`CompanyRecord::effective_desk_members`]), so routing and the console
/// cannot drift. An overlay-added lead is reachable on a desk the manifest left
/// empty.
///
/// Lifted here from the harness-only delegation runtime so the hosted path can
/// resolve a desk lead without the `openhuman` feature.
pub fn desk_lead(record: &CompanyRecord, desk: &str) -> Option<String> {
    let desk_id = record.resolve_desk_id(desk)?;
    record
        .effective_desk_members(&desk_id)
        .into_iter()
        .find(|m| record.is_roster_agent(m))
}

/// How many desk ids a rejection message names before eliding the rest, so the
/// message stays short enough to be useful on a company with many desks.
const LISTED_DESKS: usize = 12;

/// Every desk id the company actually has: the manifest `[[group_chat]]` desks
/// in declaration order, then any operator-created overlay desks, deduplicated.
///
/// The **id** is what [`delegate_to_desk`](DELEGATE_TO_DESK_TOOL) takes, so this
/// is the set a delegation target is grounded against. Reads the same two
/// sources [`CompanyRecord::resolve_desk_id`] searches, so "what ids exist" and
/// "does this id resolve" cannot disagree.
pub fn desk_ids(record: &CompanyRecord) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for chat in &record.manifest.group_chats {
        if !ids.contains(&chat.id) {
            ids.push(chat.id.clone());
        }
    }
    for desk in &record.overlay_desks {
        if !ids.contains(&desk.id) {
            ids.push(desk.id.clone());
        }
    }
    ids
}

/// Why a `delegate_to_desk` target cannot be delivered, phrased for the model
/// that called the tool — or `None` when `key` names a desk that can actually
/// take the work (issue #272).
///
/// Two refusals, deliberately distinct so the caller can act on them:
///
/// * **Unknown desk** — `key` resolves to no desk at all. This is the invented
///   target: the observed failure was a hand-off to `writer`, which is a
///   *teammate*, not a desk. The message names the company's real desk ids so
///   the model can retry in the same turn, and when the key does name a
///   teammate it says so and points at the desk that teammate leads.
/// * **Leadless desk** — the desk is real but no member of it is on the roster,
///   so no turn can ever run for it. Delegating there is always a no-op.
///
/// Returning the reason as a string (rather than rejecting inside the tool)
/// keeps this brain-agnostic: the harness tool turns it into a
/// `ToolResult::error` and the hosted device-side handler turns it into a failed
/// tool frame, from one definition.
pub fn reject_desk_target(record: &CompanyRecord, key: &str) -> Option<String> {
    let Some(desk_id) = record.resolve_desk_id(key) else {
        return Some(unknown_desk_message(record, key));
    };
    if desk_lead(record, &desk_id).is_some() {
        return None;
    }
    let with_leads = desk_list(
        desk_ids(record)
            .into_iter()
            .filter(|id| desk_lead(record, id).is_some())
            .collect(),
    );
    Some(match with_leads {
        Some(list) => format!(
            "The \"{desk_id}\" desk has no member on the roster, so nothing can be handed to it. \
Desks that can take work: {list}."
        ),
        None => format!(
            "The \"{desk_id}\" desk has no member on the roster, so nothing can be handed to it, \
and no other desk has a lead either. Answer directly instead of delegating."
        ),
    })
}

/// The refusal for a delegation target that matches no desk (issue #272).
///
/// Public because the hosted path refuses **only** this case: there, a hand-off
/// is a durable board card assigned to the desk, so a real desk with no lead yet
/// is still visible work rather than a silent drop. The harness path — where a
/// hand-off is a live turn that a leadless desk can never run — goes through
/// [`reject_desk_target`], which covers both.
pub fn unknown_desk_message(record: &CompanyRecord, key: &str) -> String {
    let Some(list) = desk_list(desk_ids(record)) else {
        return format!(
            "There is no \"{key}\" desk — this company has no desks at all, so `delegate_to_desk` \
cannot be used. Answer directly instead."
        );
    };
    // A teammate's id is the most common invented target (the orchestrator
    // reaches for the person it has in mind rather than the desk they sit on),
    // so name the mistake instead of only listing the alternatives.
    let Some(agent) = record.resolve_teammate_key(key).agent() else {
        return format!(
            "There is no \"{key}\" desk. Valid desk ids: {list}. Call `delegate_to_desk` again \
with one of those ids."
        );
    };
    match desk_of_member(record, &agent) {
        Some(desk) => format!(
            "There is no \"{key}\" desk — \"{key}\" is a teammate, not a desk. The desk they are \
on is \"{desk}\". Valid desk ids: {list}."
        ),
        None => format!(
            "There is no \"{key}\" desk — \"{key}\" is a teammate, not a desk, and is on no desk. \
Valid desk ids: {list}."
        ),
    }
}

/// Why a **desk member's** `delegate_to_desk` call would close a loop rather
/// than make progress — or `None` when the target is a step forward (issue
/// #176).
///
/// Two shapes, both of which the depth cap alone would let run to its bound
/// while producing nothing:
///
/// * **Back up the chain** — the target desk is already executing somewhere
///   above this turn (`chain` is the scope chain, outermost first). A→B→A is the
///   loop the issue names; the desk that handed this work out cannot be the desk
///   it is handed back to.
/// * **Back to itself** — the target desk's lead *is* the member calling. Its
///   own turn is the one running; handing work to itself would re-enter it a
///   level deeper to do what it is already doing.
///
/// Identity here is the **resolved desk id**, deliberately, on both sides. That
/// is what `delegate_to_desk` already validates and what the chain records, and
/// an agent-keyed visited set would false-positive on the perfectly ordinary
/// company where one strong teammate leads two desks — refusing a real hand-off
/// to a second, different desk. The self-lead arm is the one place an *agent*
/// identity is compared, and only against the immediate caller.
///
/// The depth cap, not this, is the real runaway bound; this is what keeps a
/// bounded chain from spending its whole budget going in a circle, and what
/// keeps the guard honest if the cap is ever raised.
pub fn reject_cycle_target(
    record: &CompanyRecord,
    chain: &[String],
    key: &str,
    delegator: &str,
) -> Option<String> {
    // An unresolvable key is `reject_desk_target`'s refusal to give, not this
    // one's: saying "that would loop" about a desk that does not exist would
    // send the model looking for a cycle it cannot find.
    let desk_id = record.resolve_desk_id(key)?;
    if chain.iter().any(|scoped| scoped == &desk_id) {
        let trail = chain_trail(chain);
        return Some(format!(
            "The \"{desk_id}\" desk is already working on this — it is where the work came from \
({trail}). Handing it back would loop. Do the part you can do yourself, or hand it to a desk \
that is not already on that list."
        ));
    }
    if desk_lead(record, &desk_id).as_deref() == Some(delegator) {
        // Issue #884: this refusal used to dead-end at "do it in this turn
        // instead". That was the only advice available — there was no tool for
        // handing a slice to a peer — and it is what made a desk lead decline a
        // request addressed to a specialist sitting beside it. It now names the
        // tool that does exist, the in-turn teaching shape #272 and #176 use
        // everywhere else on this seam.
        let peers = agent_list(teammate_targets(record, delegator, &[]));
        return Some(match peers {
            Some(list) => format!(
                "You lead the \"{desk_id}\" desk, so handing this to it would hand it back to \
yourself. To give a slice to somebody specific on it, call `{DELEGATE_TO_TEAMMATE_TOOL}` with one \
of: {list}. Otherwise do it in this turn, or hand it to a different desk."
            ),
            None => format!(
                "You lead the \"{desk_id}\" desk, so handing this to it would hand it back to \
yourself, and nobody else is on it. Do it in this turn instead, or hand it to a different desk."
            ),
        });
    }
    None
}

/// The scope-chain entry a **teammate** hand-off pushes (issue #884).
///
/// Namespaced with a prefix a desk id cannot carry, because one chain now holds
/// two kinds of identity: [`enter_scope`] records a resolved *desk* id for a
/// [`DELEGATE_TO_DESK_TOOL`] hand-off and a resolved *agent* id for a
/// [`DELEGATE_TO_TEAMMATE_TOOL`] one, and the two guards must not read each
/// other's entries. Without the prefix, a company with a desk and a teammate
/// sharing an id would have one refuse the other's perfectly ordinary hand-off.
///
/// The prefix also keeps the desk guard honest in the direction that matters
/// most here: a lead handing a slice to a peer on **its own desk** is the whole
/// point of #884, so that hand-off must not read as "the desk you came from is
/// already on the chain".
///
/// [`enter_scope`]: crate::harness::orchestrator::DelegationQueue::enter_scope
pub fn teammate_scope_key(agent_id: &str) -> String {
    format!("{TEAMMATE_SCOPE_PREFIX}{agent_id}")
}

/// The prefix [`teammate_scope_key`] stamps. Not a legal desk id — desk ids come
/// from a manifest `[[group_chat]].id` or an operator-created overlay desk, and
/// neither mints a colon.
const TEAMMATE_SCOPE_PREFIX: &str = "agent:";

/// Renders a scope chain for a refusal, unwrapping [`teammate_scope_key`]'s
/// namespace so the model reads names rather than the encoding.
fn chain_trail(chain: &[String]) -> String {
    chain
        .iter()
        .map(|scoped| {
            scoped
                .strip_prefix(TEAMMATE_SCOPE_PREFIX)
                .unwrap_or(scoped)
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(" → ")
}

/// Why a `delegate_to_teammate` target cannot be delivered, phrased for the
/// model that called the tool — or `None` when `key` names a teammate this
/// caller may actually hand work to (issue #884).
///
/// **Closed-set validation, never free text.** The target is resolved against
/// the company record and checked against a set derived from it; nothing is read
/// out of the message. That is the deliberate half of #884: a `Name:` prefix
/// parser would let a pasted email beginning "SEO Specialist:" decide whose tool
/// grants and whose budget execute the turn, is undefined for a message naming
/// two people, and breaks in every language the company does not write its
/// personas in. Routing stays deterministic; reading the prose stays with the
/// model, which already does it correctly.
///
/// Three refusals, deliberately distinct:
///
/// * **Not a teammate** — `key` resolves to no roster agent. When it names a
///   *desk* the message says so and points at [`DELEGATE_TO_DESK_TOOL`], the
///   mirror of [`unknown_desk_message`]'s teammate arm.
/// * **More than one teammate** — `key` is a display name two operator-added
///   teammates answer to. Real, but not routable; the refusal names the
///   colliding ids so the model can pick one (issue #1162).
/// * **Yourself** — the target is the caller. Its turn is the one running.
/// * **Out of reach** — a real teammate the caller may not hand work to: not on
///   a desk with them, and not on any desk their
///   [`delegates_to`](crate::company::Agent::delegates_to) allowlist permits.
///
/// `caller` is `None` for the **orchestrator's** copy of the tool, which is
/// unrestricted exactly as its `delegate_to_desk` is: grounding applies, the
/// allowlist and the self-check do not.
pub fn reject_teammate_target(
    record: &CompanyRecord,
    caller: Option<&str>,
    allowed: &[String],
    key: &str,
) -> Option<String> {
    let target = match record.resolve_teammate_key(key) {
        TeammateResolution::Agent(id) => id,
        TeammateResolution::Unknown => {
            return Some(unknown_teammate_message(record, caller, allowed, key));
        }
        TeammateResolution::Ambiguous(ids) => {
            return Some(ambiguous_teammate_message(key, ids));
        }
    };
    let caller = caller?;
    if target == caller {
        return Some(format!(
            "You are \"{caller}\" — handing this to yourself would re-enter the turn you are \
already running. Do it in this turn instead, or hand it to somebody else."
        ));
    }
    let reachable = teammate_targets(record, caller, allowed);
    if reachable.contains(&target) {
        return None;
    }
    Some(match agent_list(reachable) {
        Some(list) => format!(
            "You may not hand work to \"{target}\": they are not on a desk with you, and no desk \
you may reach has them on it. The teammates you can hand work to are: {list}. Call \
`{DELEGATE_TO_TEAMMATE_TOOL}` again with one of those, or do the work yourself."
        ),
        None => format!(
            "You may not hand work to \"{target}\", and there is no other teammate you can hand \
work to either. Do the work yourself, or say what you cannot do."
        ),
    })
}

/// The refusal for a `delegate_to_teammate` target that names no roster teammate
/// (issue #884) — the mirror of [`unknown_desk_message`].
fn unknown_teammate_message(
    record: &CompanyRecord,
    caller: Option<&str>,
    allowed: &[String],
    key: &str,
) -> String {
    let reachable = match caller {
        Some(caller) => teammate_targets(record, caller, allowed),
        None => roster_agent_ids(record),
    };
    // A desk id is the most likely wrong target here, exactly as a teammate id
    // is the most likely wrong target for `delegate_to_desk`.
    if record.resolve_desk_id(key).is_some() {
        return format!(
            "\"{key}\" is a desk, not a teammate. Hand work to a desk with \
`{DELEGATE_TO_DESK_TOOL}`, or name somebody on the roster here instead."
        );
    }
    match agent_list(reachable) {
        Some(list) => format!(
            "There is no \"{key}\" on this company's roster. The teammates you can hand work to \
are: {list}. Call `{DELEGATE_TO_TEAMMATE_TOOL}` again with one of those ids."
        ),
        None => format!(
            "There is no \"{key}\" on this company's roster, and there is nobody you can hand \
work to. Do the work yourself, or say what you cannot do."
        ),
    }
}

/// The refusal for a `delegate_to_teammate` target that names **more than one**
/// teammate (issue #1162).
///
/// Grounding a display name gave this case somewhere to happen: two
/// operator-added teammates may carry the same name, and their ids are what
/// tell them apart. Taking the first would be the misrouting
/// [`CompanyRecord::overlay_agent_ids_by_name`] exists to end, and the plain
/// "there is no such teammate" message would be a lie about a teammate that
/// demonstrably exists — so the collision is named, with the ids to retry with.
fn ambiguous_teammate_message(key: &str, ids: Vec<String>) -> String {
    let list = agent_list(ids).unwrap_or_else(|| "-".to_string());
    format!(
        "\"{key}\" is the name of more than one teammate here, so it does not say who to hand \
this to. Call `{DELEGATE_TO_TEAMMATE_TOOL}` again with one of their ids: {list}."
    )
}

/// Why a **teammate** hand-off would close a loop rather than make progress — or
/// `None` when the target is a step forward (issue #884).
///
/// The agent-keyed companion to [`reject_cycle_target`], reading the same chain
/// through [`teammate_scope_key`]'s namespace. A→B→A is the shape: `B` cannot
/// hand back to the teammate whose turn it is running inside.
///
/// Identity is the **resolved roster id** on both sides, so a hand-off written
/// with a different capitalisation is still the same cycle. An unresolvable key
/// is [`reject_teammate_target`]'s refusal to give, not this one's.
pub fn reject_teammate_cycle_target(
    record: &CompanyRecord,
    chain: &[String],
    key: &str,
) -> Option<String> {
    // The same id-then-name resolve the refusal above grounds with (#1162): a
    // hand-off written as a display name has to reach this guard as the id it
    // means, or A→B→A spelled with a name would slip past the chain check.
    let target = record.resolve_teammate_key(key).agent()?;
    let scoped = teammate_scope_key(&target);
    if !chain.iter().any(|entry| entry == &scoped) {
        return None;
    }
    let trail = chain_trail(chain);
    Some(format!(
        "\"{target}\" is already working on this — it is where the work came from ({trail}). \
Handing it back would loop. Do the part you can do yourself, or hand it to somebody who is not \
already on that list."
    ))
}

/// Every roster teammate id the company has: manifest agents in declaration
/// order, then operator-added overlay teammates, deduplicated.
///
/// The teammate-side counterpart of [`desk_ids`], and read for the same reason —
/// "who exists" and "does this key resolve" must come from one source.
pub fn roster_agent_ids(record: &CompanyRecord) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for id in record
        .manifest
        .agents
        .iter()
        .map(|a| &a.id)
        .chain(record.overlay_agents.iter().map(|a| &a.id))
    {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    ids
}

/// The teammates `caller` may hand work to with [`DELEGATE_TO_TEAMMATE_TOOL`]
/// (issue #884): everybody on a desk with them, plus everybody on a desk their
/// `allowed` ([`delegates_to`](crate::company::Agent::delegates_to)) list
/// permits. Never the caller itself.
///
/// The desk-peer arm is the one #884 adds and the one that closes D1: a desk's
/// lead can now reach the specialist sitting beside it without going back
/// through the orchestrator. The allowlist arm is a re-reading of the existing
/// #176 permission rather than a second one — a member allowed to hand work to a
/// desk is allowed to hand it to somebody on that desk — so opting in to
/// `delegates_to` grants exactly what it already granted, at teammate
/// granularity.
///
/// Order is desk-peers first, then allowlisted desks, each in the desk's own
/// membership order, deduplicated — so a refusal lists the nearest options
/// first.
pub fn teammate_targets(record: &CompanyRecord, caller: &str, allowed: &[String]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let push = |ids: &mut Vec<String>, id: &str| {
        if id != caller && record.is_roster_agent(id) && !ids.iter().any(|held| held == id) {
            ids.push(id.to_string());
        }
    };
    for desk in desks_of_member(record, caller) {
        for member in record.effective_desk_members(&desk) {
            push(&mut ids, &member);
        }
    }
    if allowed
        .iter()
        .any(|entry| entry.trim() == crate::company::DELEGATES_TO_WILDCARD)
    {
        // `"*"` is documented as "every desk the company has"
        // ([`DELEGATES_TO_WILDCARD`](crate::company::DELEGATES_TO_WILDCARD)),
        // not "every roster agent" — the orchestrator and any agent sitting on
        // no desk are outside a desk-based grant even when it is the widest
        // one, so this must walk `desk_ids` + `effective_desk_members` exactly
        // as the allowlisted-desk loop below does, not `roster_agent_ids`.
        for desk in desk_ids(record) {
            for member in record.effective_desk_members(&desk) {
                push(&mut ids, &member);
            }
        }
        return ids;
    }
    for desk in allowed
        .iter()
        .filter_map(|entry| record.resolve_desk_id(entry.trim()))
    {
        for member in record.effective_desk_members(&desk) {
            push(&mut ids, &member);
        }
    }
    ids
}

/// Every desk `member` sits on, in [`desk_ids`] order.
fn desks_of_member(record: &CompanyRecord, member: &str) -> Vec<String> {
    desk_ids(record)
        .into_iter()
        .filter(|id| {
            record
                .effective_desk_members(id)
                .iter()
                .any(|m| m == member)
        })
        .collect()
}

/// Renders a teammate-id list for a message, on the same terms as [`desk_list`].
fn agent_list(ids: Vec<String>) -> Option<String> {
    desk_list(ids)
}

/// Why a **desk member's** `delegate_to_desk` target is outside what its
/// manifest entry permits — or `None` when the member may reach it (issue #176).
///
/// `allowed` is the member's
/// [`delegates_to`](crate::company::Agent::delegates_to) list, whose entries are
/// desk ids or names; [`DELEGATES_TO_WILDCARD`] admits every desk. Both sides
/// are resolved to desk ids before comparison, so an allowlist written with
/// display names and a call made with an id agree.
///
/// The refusal **names the desks the member may reach**, because the model has
/// no other way to learn its own allowlist: the tool schema is shared with the
/// orchestrator's unrestricted copy, and a bare "not allowed" costs a turn per
/// guess. Retryable in the same turn, unlike the depth and no-drain refusals.
///
/// Never reached with an empty `allowed`: an empty allowlist means the tool was
/// not wired at all. It still fails closed if one ever arrives — nothing
/// resolves, so everything is refused.
///
/// [`DELEGATES_TO_WILDCARD`]: crate::company::DELEGATES_TO_WILDCARD
pub fn reject_out_of_allowlist_target(
    record: &CompanyRecord,
    allowed: &[String],
    key: &str,
) -> Option<String> {
    if allowed
        .iter()
        .any(|entry| entry.trim() == crate::company::DELEGATES_TO_WILDCARD)
    {
        return None;
    }
    // As above: an unresolvable key belongs to `reject_desk_target`.
    let desk_id = record.resolve_desk_id(key)?;
    let permitted: Vec<String> = allowed
        .iter()
        .filter_map(|entry| record.resolve_desk_id(entry.trim()))
        .collect();
    if permitted.contains(&desk_id) {
        return None;
    }
    Some(match desk_list(permitted) {
        Some(list) => format!(
            "You may not hand work to the \"{desk_id}\" desk. The desks you can hand work to are: \
{list}. Call `delegate_to_desk` again with one of those, or do the work yourself."
        ),
        None => format!(
            "You may not hand work to the \"{desk_id}\" desk, and there is no other desk you can \
hand work to either. Do the work yourself, or say what you cannot do."
        ),
    })
}

/// The first desk `member` is on, so an invented teammate-as-desk target can be
/// redirected at the desk that teammate actually sits on.
fn desk_of_member(record: &CompanyRecord, member: &str) -> Option<String> {
    desks_of_member(record, member).into_iter().next()
}

/// Renders a desk-id list for a message, capped at [`LISTED_DESKS`] with the
/// remainder counted. `None` when there are no ids to list.
fn desk_list(ids: Vec<String>) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    let shown = ids.len().min(LISTED_DESKS);
    let mut list = ids[..shown].join(", ");
    if ids.len() > shown {
        list.push_str(&format!(" (+{} more)", ids.len() - shown));
    }
    Some(list)
}

/// Parsed `spawn_task` arguments: a required title, an optional brief note, and
/// an optional assignee. Blank strings are treated as absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnTaskArgs {
    /// The task title (required, non-blank).
    pub title: String,
    /// An optional longer brief.
    pub note: Option<String>,
    /// An optional desk/teammate id to own the card.
    pub assignee: Option<String>,
}

impl SpawnTaskArgs {
    /// Parses `spawn_task` args, returning `None` when `title` is missing or
    /// blank (the one hard requirement).
    pub fn parse(args: &Value) -> Option<Self> {
        let title = trimmed_str(args, "title")?;
        Some(Self {
            title,
            note: trimmed_str(args, "note"),
            assignee: trimmed_str(args, "assignee"),
        })
    }
}

/// Parsed `delegate_to_desk` arguments: the target desk and the instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegateArgs {
    /// The desk id or name to hand the work to.
    pub desk: String,
    /// The instruction the desk should carry out.
    pub instruction: String,
}

impl DelegateArgs {
    /// Parses `delegate_to_desk` args, returning `None` when either `desk` or
    /// `instruction` is missing or blank.
    pub fn parse(args: &Value) -> Option<Self> {
        Some(Self {
            desk: trimmed_str(args, "desk")?,
            instruction: trimmed_str(args, "instruction")?,
        })
    }
}

/// Reads `key` as a string, trims it, and returns it only when non-empty.
fn trimmed_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn manifest_entries_cover_both_delegation_tools() {
        let entries = delegation_manifest_entries();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&SPAWN_TASK_TOOL), "got {names:?}");
        assert!(names.contains(&DELEGATE_TO_DESK_TOOL), "got {names:?}");
        // Every entry carries a schema so Medulla can shape the call.
        assert!(
            entries
                .iter()
                .all(|e| e.input_schema.is_some() && e.description.is_some())
        );
    }

    #[test]
    fn spawn_task_args_require_a_nonblank_title() {
        assert_eq!(SpawnTaskArgs::parse(&json!({})), None);
        assert_eq!(SpawnTaskArgs::parse(&json!({ "title": "  " })), None);
        let parsed = SpawnTaskArgs::parse(&json!({
            "title": "  Ship it ",
            "note": " brief ",
            "assignee": " eng "
        }))
        .expect("valid");
        assert_eq!(parsed.title, "Ship it");
        assert_eq!(parsed.note.as_deref(), Some("brief"));
        assert_eq!(parsed.assignee.as_deref(), Some("eng"));
        // Blank optionals collapse to None.
        let bare = SpawnTaskArgs::parse(&json!({ "title": "x", "note": "", "assignee": "" }))
            .expect("valid");
        assert_eq!(bare.note, None);
        assert_eq!(bare.assignee, None);
    }

    /// The company shape issue #272 was observed on: real desks, plus a
    /// teammate (`writer`) the orchestrator mistook for one.
    fn record() -> CompanyRecord {
        let manifest = toml::from_str(
            r#"
[company]
name = "Acme"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[agent]]
id = "writer"
role = "Writer"

[[group_chat]]
id = "engineering"
name = "Engineering desk"
members = ["ceo"]

[[group_chat]]
id = "content"
name = "Content desk"
members = ["writer"]

[[group_chat]]
id = "legal"
name = "Legal desk"
members = ["counsel"]
"#,
        )
        .expect("valid manifest");
        CompanyRecord {
            id: crate::ports::types::CompanyId::new("acme"),
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
        }
    }

    #[test]
    fn desk_ids_lists_manifest_desks_in_declaration_order() {
        assert_eq!(desk_ids(&record()), ["engineering", "content", "legal"]);
    }

    #[test]
    fn a_real_desk_with_a_lead_is_not_rejected() {
        assert_eq!(reject_desk_target(&record(), "content"), None);
        // The id-or-name key `resolve_desk_id` already accepts still works.
        assert_eq!(reject_desk_target(&record(), "Content desk"), None);
    }

    /// The observed bug: `writer` is a teammate, not a desk. The refusal must
    /// name the real desk ids so the model can retry in the same turn, and say
    /// which desk that teammate is actually on.
    #[test]
    fn an_invented_desk_is_rejected_with_the_valid_set() {
        let message = reject_desk_target(&record(), "writer").expect("rejected");
        assert!(message.contains("engineering"), "{message}");
        assert!(message.contains("content"), "{message}");
        assert!(message.contains("legal"), "{message}");
        assert!(
            message.contains("teammate"),
            "a teammate-as-desk target must be named as such: {message}"
        );
        // A key that is neither a desk nor a teammate still gets the valid set.
        let message = reject_desk_target(&record(), "growth").expect("rejected");
        assert!(message.contains("engineering"), "{message}");
        assert!(!message.contains("teammate"), "{message}");
    }

    /// A desk that exists but has nobody on the roster can never run a turn, so
    /// it is refused too — with the desks that CAN take work.
    #[test]
    fn a_desk_with_no_roster_lead_is_rejected() {
        let message = reject_desk_target(&record(), "legal").expect("rejected");
        assert!(message.contains("no member on the roster"), "{message}");
        assert!(
            message.contains("Desks that can take work: engineering, content."),
            "only desks with a lead may be offered as alternatives: {message}"
        );
    }

    /// A company with no desks at all cannot delegate; say so rather than
    /// offering an empty list.
    #[test]
    fn a_company_with_no_desks_says_so() {
        let mut record = record();
        record.manifest.group_chats.clear();
        let message = reject_desk_target(&record, "content").expect("rejected");
        assert!(message.contains("no desks at all"), "{message}");
    }

    #[test]
    fn the_desk_list_is_capped_and_counts_the_remainder() {
        let ids: Vec<String> = (0..LISTED_DESKS + 3).map(|i| format!("d{i}")).collect();
        let list = desk_list(ids).expect("non-empty");
        assert!(list.ends_with("(+3 more)"), "{list}");
        assert_eq!(desk_list(Vec::new()), None);
    }

    // --- Recursive-delegation target checks (issue #176) -------------------

    /// The A→B→A loop the issue names: a desk already on the chain cannot be
    /// handed the work back.
    #[test]
    fn a_desk_already_on_the_chain_is_refused_as_a_cycle() {
        let record = record();
        let chain = vec!["engineering".to_string()];
        let message =
            reject_cycle_target(&record, &chain, "engineering", "writer").expect("rejected");
        assert!(message.contains("engineering"), "{message}");
        assert!(message.contains("loop"), "{message}");
        // A desk that is NOT on the chain is a step forward.
        assert_eq!(reject_cycle_target(&record, &chain, "content", "ceo"), None);
        // The id-or-name key `resolve_desk_id` accepts is checked identically —
        // a cycle written with the display name is still a cycle.
        assert!(reject_cycle_target(&record, &chain, "Engineering desk", "writer").is_some());
    }

    /// Handing work to the desk you lead is handing it to yourself.
    #[test]
    fn a_desk_the_caller_leads_is_refused_as_self_delegation() {
        let record = record();
        // `writer` leads `content`.
        let message = reject_cycle_target(&record, &[], "content", "writer").expect("rejected");
        assert!(message.contains("yourself"), "{message}");
        // Somebody who does NOT lead it may hand work to it.
        assert_eq!(reject_cycle_target(&record, &[], "content", "ceo"), None);
    }

    // --- Teammate hand-off (issue #884) ------------------------------------

    /// The one-member-per-desk shape, reachable from a test that has shadowed
    /// [`record`] with a binding of its own.
    fn solo_record() -> CompanyRecord {
        record()
    }

    /// The company shape #884 D1 was observed on: one desk with THREE members,
    /// so its lead has peers it could not previously reach, plus a second desk
    /// nobody on the first sits on.
    fn desk_record() -> CompanyRecord {
        let manifest = toml::from_str(
            r#"
[company]
name = "Acme"

[[agent]]
id = "chief"
role = "Chief of Staff"
tier = "orchestrator"

[[agent]]
id = "brand_strategist"
role = "Brand Strategist"

[[agent]]
id = "seo_specialist"
role = "SEO Specialist"

[[agent]]
id = "copywriter"
role = "Copywriter"

[[agent]]
id = "analyst"
role = "Analyst"

[[group_chat]]
id = "strategy"
name = "Strategy desk"
members = ["brand_strategist", "seo_specialist", "copywriter"]

[[group_chat]]
id = "data"
name = "Data desk"
members = ["analyst"]
"#,
        )
        .expect("valid manifest");
        CompanyRecord {
            manifest,
            ..record()
        }
    }

    /// D1 itself: the lead of a three-person desk may hand a slice to either
    /// peer on it, with no allowlist and no orchestrator round-trip.
    #[test]
    fn a_desk_lead_may_hand_work_to_a_peer_on_its_own_desk() {
        let record = desk_record();
        assert_eq!(
            teammate_targets(&record, "brand_strategist", &[]),
            ["seo_specialist", "copywriter"],
            "own-desk peers, in the desk's own order, never the caller"
        );
        assert_eq!(
            reject_teammate_target(&record, Some("brand_strategist"), &[], "seo_specialist"),
            None
        );
        // The key is resolved, not matched: a typed capital is the same person.
        assert_eq!(
            reject_teammate_target(&record, Some("brand_strategist"), &[], "SEO_Specialist"),
            None
        );
    }

    /// A teammate on neither the caller's desks nor an allowlisted one is
    /// refused — and the refusal names who the caller CAN reach, because the
    /// model has no other way to learn its own closed set.
    #[test]
    fn a_teammate_out_of_reach_is_refused_with_the_reachable_set() {
        let record = desk_record();
        let message = reject_teammate_target(&record, Some("brand_strategist"), &[], "analyst")
            .expect("rejected");
        assert!(message.contains("analyst"), "{message}");
        assert!(message.contains("seo_specialist"), "{message}");
        assert!(message.contains(DELEGATE_TO_TEAMMATE_TOOL), "{message}");

        // …and the #176 allowlist admits that desk's members, at teammate
        // granularity: permission to hand work to a desk is permission to hand
        // it to somebody on it.
        let allowed = vec!["data".to_string()];
        assert_eq!(
            reject_teammate_target(&record, Some("brand_strategist"), &allowed, "analyst"),
            None
        );
        // `"*"` admits every desk's members — `analyst` is on `data` — exactly
        // as it does for desks. It does NOT admit the whole roster; see
        // `the_wildcard_teammate_reach_is_desk_members_not_the_whole_roster`
        // for the orchestrator/no-desk case that distinguishes the two.
        assert_eq!(
            reject_teammate_target(&record, Some("brand_strategist"), &["*".into()], "analyst"),
            None
        );
    }

    /// `"*"` is documented as "every desk the company has"
    /// ([`DELEGATES_TO_WILDCARD`]), not "every roster agent" — the orchestrator
    /// (`chief`, on no desk) must stay unreachable through even the widest
    /// grant, exactly as a wildcard `delegate_to_desk` allowlist cannot reach a
    /// non-desk id.
    #[test]
    fn the_wildcard_teammate_reach_is_desk_members_not_the_whole_roster() {
        let record = desk_record();
        let targets = teammate_targets(&record, "brand_strategist", &["*".to_string()]);
        assert!(
            !targets.contains(&"chief".to_string()),
            "the orchestrator sits on no desk and must not be reachable through a wildcard \
             desk-based grant: {targets:?}"
        );
        assert_eq!(
            targets,
            ["seo_specialist", "copywriter", "analyst"],
            "wildcard must resolve to every desk's members, not the raw roster: {targets:?}"
        );
        // The refusal path agrees: the orchestrator is not a valid target even
        // though the caller holds the widest possible grant.
        assert!(
            reject_teammate_target(&record, Some("brand_strategist"), &["*".into()], "chief")
                .is_some()
        );
    }

    /// Handing work to yourself re-enters the turn already running.
    #[test]
    fn a_teammate_hand_off_to_yourself_is_refused() {
        let record = desk_record();
        let message =
            reject_teammate_target(&record, Some("seo_specialist"), &[], "seo_specialist")
                .expect("rejected");
        assert!(message.contains("yourself"), "{message}");
    }

    /// A key that is nobody is refused, and a key that names a **desk** is
    /// redirected at the tool that takes desks — the mirror of the
    /// teammate-as-desk arm `delegate_to_desk` already has.
    #[test]
    fn an_unknown_teammate_target_is_grounded() {
        let record = desk_record();
        let message = reject_teammate_target(&record, Some("brand_strategist"), &[], "nobody")
            .expect("rejected");
        assert!(message.contains("no \"nobody\""), "{message}");
        assert!(message.contains("copywriter"), "{message}");

        let message = reject_teammate_target(&record, Some("brand_strategist"), &[], "data")
            .expect("rejected");
        assert!(message.contains(DELEGATE_TO_DESK_TOOL), "{message}");
        assert!(message.contains("is a desk, not a teammate"), "{message}");
    }

    /// The orchestrator's copy is unrestricted — every roster teammate, no
    /// allowlist — exactly as its `delegate_to_desk` is. Grounding still applies.
    #[test]
    fn the_orchestrators_teammate_target_set_is_the_whole_roster() {
        let record = desk_record();
        for target in ["analyst", "seo_specialist", "copywriter"] {
            assert_eq!(reject_teammate_target(&record, None, &[], target), None);
        }
        let message = reject_teammate_target(&record, None, &[], "nobody").expect("rejected");
        assert!(
            message.contains("analyst"),
            "the roster is listed: {message}"
        );
        assert_eq!(
            roster_agent_ids(&record),
            [
                "chief",
                "brand_strategist",
                "seo_specialist",
                "copywriter",
                "analyst"
            ]
        );
    }

    /// A→B→A: `seo_specialist` cannot hand the work back to the
    /// `brand_strategist` whose turn it is running inside.
    #[test]
    fn a_teammate_already_on_the_chain_is_refused_as_a_cycle() {
        let record = desk_record();
        let chain = vec![teammate_scope_key("brand_strategist")];
        let message =
            reject_teammate_cycle_target(&record, &chain, "brand_strategist").expect("rejected");
        assert!(message.contains("loop"), "{message}");
        assert!(
            message.contains("brand_strategist") && !message.contains("agent:"),
            "the trail must read as names, not as the chain's encoding: {message}"
        );
        // A different peer is a step forward.
        assert_eq!(
            reject_teammate_cycle_target(&record, &chain, "copywriter"),
            None
        );
        // Case is folded on both sides — a cycle spelled differently is a cycle.
        assert!(reject_teammate_cycle_target(&record, &chain, "Brand_Strategist").is_some());
        // An unresolvable key belongs to the grounding check, not to this one.
        assert_eq!(
            reject_teammate_cycle_target(&record, &chain, "nobody"),
            None
        );
    }

    /// The two chains share one vector and must not read each other's entries.
    ///
    /// The direction that matters: a lead reached through a `delegate_to_desk`
    /// hand-off has its own desk on the chain, and handing a slice to a peer on
    /// that desk is precisely what #884 exists to allow. If the teammate guard
    /// read desk entries it would refuse the feature's own headline case.
    #[test]
    fn the_desk_and_teammate_cycle_guards_do_not_read_each_others_entries() {
        let record = desk_record();
        let desk_chain = vec!["strategy".to_string()];
        assert_eq!(
            reject_teammate_cycle_target(&record, &desk_chain, "seo_specialist"),
            None,
            "a peer on the desk the chain came through is still reachable"
        );
        let agent_chain = vec![teammate_scope_key("brand_strategist")];
        assert_eq!(
            reject_cycle_target(&record, &agent_chain, "data", "chief"),
            None,
            "an agent entry is not a desk entry"
        );
    }

    /// Issue #884: the self-desk refusal is a dead end no longer — it names the
    /// tool that can actually deliver the work, and who it can be delivered to.
    #[test]
    fn the_self_desk_refusal_names_the_teammate_tool() {
        let record = desk_record();
        let message =
            reject_cycle_target(&record, &[], "strategy", "brand_strategist").expect("rejected");
        assert!(message.contains("yourself"), "{message}");
        assert!(message.contains(DELEGATE_TO_TEAMMATE_TOOL), "{message}");
        assert!(
            message.contains("seo_specialist") && message.contains("copywriter"),
            "the peers it can reach are named: {message}"
        );

        // A lead with nobody else on its desk gets the old advice, not an empty
        // list and a tool that would refuse every target.
        let message =
            reject_cycle_target(&solo_record(), &[], "content", "writer").expect("rejected");
        assert!(!message.contains(DELEGATE_TO_TEAMMATE_TOOL), "{message}");
        assert!(message.contains("nobody else is on it"), "{message}");
    }

    #[test]
    fn the_teammate_schema_takes_a_roster_id_and_an_instruction() {
        let schema = delegate_to_teammate_schema();
        assert_eq!(schema["required"], json!(["teammate", "instruction"]));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    /// The hosted manifest deliberately does NOT advertise the teammate tool —
    /// that path has no per-agent routing to service it (issue #176).
    #[test]
    fn the_hosted_manifest_does_not_advertise_the_teammate_tool() {
        let names: Vec<String> = delegation_manifest_entries()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(
            !names.contains(&DELEGATE_TO_TEAMMATE_TOOL.to_string()),
            "{names:?}"
        );
    }

    /// An unresolvable key belongs to `reject_desk_target`, not to either of
    /// the #176 checks — otherwise an invented desk would be reported as a
    /// cycle or an allowlist miss and the model would go looking for the wrong
    /// mistake.
    #[test]
    fn an_unknown_desk_is_left_to_the_grounding_check() {
        let record = record();
        assert_eq!(reject_cycle_target(&record, &[], "nowhere", "ceo"), None);
        assert_eq!(
            reject_out_of_allowlist_target(&record, &["content".into()], "nowhere"),
            None
        );
        // …and it IS refused by the check that owns it.
        assert!(reject_desk_target(&record, "nowhere").is_some());
    }

    /// The allowlist refusal must name what the member CAN reach: the model has
    /// no other way to learn its own `delegates_to`.
    #[test]
    fn an_out_of_allowlist_desk_is_refused_with_the_permitted_set() {
        let record = record();
        let allowed = vec!["content".to_string()];
        let message =
            reject_out_of_allowlist_target(&record, &allowed, "engineering").expect("rejected");
        assert!(message.contains("engineering"), "{message}");
        assert!(message.contains("content"), "{message}");
        // The permitted desk itself passes, by id and by display name.
        assert_eq!(
            reject_out_of_allowlist_target(&record, &allowed, "content"),
            None
        );
        assert_eq!(
            reject_out_of_allowlist_target(&record, &allowed, "Content desk"),
            None
        );
        // …and an allowlist written with display names admits the id.
        let by_name = vec!["Content desk".to_string()];
        assert_eq!(
            reject_out_of_allowlist_target(&record, &by_name, "content"),
            None
        );
    }

    /// `"*"` admits every desk; an empty allowlist admits none (fail-closed —
    /// though the tool is never wired in that state).
    #[test]
    fn the_wildcard_admits_every_desk_and_an_empty_allowlist_admits_none() {
        let record = record();
        let wildcard = vec!["*".to_string()];
        for desk in ["engineering", "content", "legal"] {
            assert_eq!(
                reject_out_of_allowlist_target(&record, &wildcard, desk),
                None,
                "`*` must admit {desk}"
            );
        }
        let message = reject_out_of_allowlist_target(&record, &[], "content").expect("rejected");
        assert!(
            message.contains("no other desk"),
            "an empty allowlist must say so rather than offer an empty list: {message}"
        );
    }

    #[test]
    fn delegate_args_require_desk_and_instruction() {
        assert_eq!(DelegateArgs::parse(&json!({ "desk": "eng" })), None);
        assert_eq!(
            DelegateArgs::parse(&json!({ "instruction": "do it" })),
            None
        );
        let parsed = DelegateArgs::parse(&json!({
            "desk": " engineering ",
            "instruction": " build the thing "
        }))
        .expect("valid");
        assert_eq!(parsed.desk, "engineering");
        assert_eq!(parsed.instruction, "build the thing");
    }
}
