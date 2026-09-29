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

/// TinyHiveMind's active roster snapshot for one routing/dispatch decision.
pub fn tinyhivemind_roster(record: &CompanyRecord) -> Vec<tinyhivemind_core::roster::RosterMember> {
    record
        .effective_agents()
        .into_iter()
        .filter(|agent| !record.is_retired(&agent.id))
        .map(|agent| tinyhivemind_core::roster::RosterMember {
            id: agent.id,
            name: agent.name,
        })
        .collect()
}

pub struct TinyHiveDeskSnapshots {
    declared: Vec<tinyhivemind_core::desk::Desk>,
    added: Vec<tinyhivemind_core::desk::Desk>,
    member_additions: Vec<tinyhivemind_core::desk::DeskMember>,
    orders: Vec<tinyhivemind_core::desk::DeskOrder>,
    retired: Vec<String>,
}

impl TinyHiveDeskSnapshots {
    pub fn set(&self) -> tinyhivemind_core::desk::DeskSet<'_> {
        tinyhivemind_core::desk::DeskSet::new(
            &self.declared,
            &self.added,
            &self.member_additions,
            &self.orders,
            &self.retired,
        )
    }
}

pub fn tinyhivemind_desks(record: &CompanyRecord) -> TinyHiveDeskSnapshots {
    use tinyhivemind_core::desk::{Desk, DeskMember, DeskOrder, ResponderMode};
    TinyHiveDeskSnapshots {
        declared: record
            .manifest
            .group_chats
            .iter()
            .map(|chat| Desk {
                id: chat.id.clone(),
                name: chat.name.clone(),
                description: chat.description.clone(),
                members: chat.members.clone(),
                responder_mode: ResponderMode::Lead,
            })
            .collect(),
        added: record
            .overlay_desks
            .iter()
            .map(|desk| Desk {
                id: desk.id.clone(),
                name: desk.name.clone(),
                description: desk.description.clone(),
                members: desk.members.clone(),
                responder_mode: match desk.responder {
                    crate::ports::types::ResponderMode::Lead => ResponderMode::Lead,
                    crate::ports::types::ResponderMode::Auto => ResponderMode::Auto,
                },
            })
            .collect(),
        member_additions: record
            .overlay_desk_members
            .iter()
            .map(|member| DeskMember {
                desk_id: member.desk_id.clone(),
                agent_id: member.agent_id.clone(),
            })
            .collect(),
        orders: record
            .overlay_desk_order
            .iter()
            .map(|order| DeskOrder {
                desk_id: order.desk_id.clone(),
                ordered: order.ordered.clone(),
            })
            .collect(),
        retired: record.overlay_retired_agents.clone(),
    }
}

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
///
/// An [`Auto`](crate::ports::types::ResponderMode::Auto) channel (issue #1835)
/// answers `None` **by definition, not by accident**: no lead exists there, so
/// every lead-derived surface — the org chart's crown, the members-pane badge,
/// a `delegate_to_desk` hand-off — stays honest without knowing the mode. The
/// deterministic "who answers here" question is [`desk_default_responder`],
/// which ignores the mode on purpose.
pub fn desk_lead(record: &CompanyRecord, desk: &str) -> Option<String> {
    let desk_id = record.resolve_desk_id(desk)?;
    if !record.desk_responder_mode(&desk_id).is_lead() {
        return None;
    }
    record
        .effective_desk_members(&desk_id)
        .into_iter()
        .find(|m| record.is_roster_agent(m))
}

/// The desk's first effective roster member, **whatever its responder mode**
/// (issue #1835).
///
/// For a [`Lead`](crate::ports::types::ResponderMode::Lead) desk this is the
/// lead, byte-for-byte what [`desk_lead`] answers. For an `Auto` channel it is
/// the deterministic fallback: the answer wherever per-message selection
/// cannot run — the default build (the selector compiles only under the
/// harness feature), the cycle's small-talk fast path, or a selection failure.
/// One function, so "the fallback" cannot drift from "the pre-selector
/// behaviour".
pub fn desk_default_responder(record: &CompanyRecord, desk: &str) -> Option<String> {
    let desk_id = record.resolve_desk_id(desk)?;
    record
        .effective_desk_members(&desk_id)
        .into_iter()
        .find(|m| record.is_roster_agent(m))
}

/// Which roster teammate answers a message addressed to `chat` — or `None`
/// when the key names neither a desk nor a teammate.
///
/// The four arms, in order, are the ones the harness brain's `responder_for`
/// has always tried: a desk key resolves to its
/// [`desk_lead`], a bare roster agent id answers as itself, and the console's
/// `dm:<teammate-id>` thread key is unwrapped and tried both ways (issue #982,
/// step 3) — last, so it can only claim a key that resolves to nothing today.
///
/// **Returns `None` rather than falling back to the orchestrator**, because the
/// two callers want different things from a miss: the harness brain logs a
/// warning and answers as the orchestrator it resolved when it was built, while
/// the cycle's small-talk fast path (issue #1725) resolves the orchestrator off
/// the record in hand. Folding the fallback in here would make one of those
/// wrong.
///
/// Lifted out of the harness brain so the fast path — which is brain-agnostic
/// and runs before any brain — attributes its reply to the same teammate the
/// turn it replaced would have been answered by.
///
/// #general answers to nobody here, so both callers resolve their own
/// orchestrator for it.
pub fn chat_responder(record: &CompanyRecord, chat: &str) -> Option<String> {
    // The desk arm asks [`desk_default_responder`], not [`desk_lead`]: for a
    // lead desk the two are identical, and for an `Auto` channel (issue #1835)
    // — where `desk_lead` is `None` by definition — this stays the
    // deterministic answer. The per-message selection that may override it is
    // the harness brain's own rung, deliberately not folded in here: this seam
    // is sync, record-only, and shared with the small-talk fast path, which
    // must not pay an inference call to attribute a greeting.
    let direct = |key: &str| {
        desk_default_responder(record, key).or_else(|| record.resolve_roster_agent_id(key))
    };
    if let Some(responder) = desk_default_responder(record, chat) {
        return Some(responder);
    }
    if chat == crate::ports::general_channel::GENERAL_CHANNEL_ID {
        return None;
    }
    if let Some(agent) = record.resolve_roster_agent_id(chat) {
        return Some(agent);
    }
    // A `dm:` key names a **teammate**, so the roster is asked first when the
    // prefix was used. Unwrapping straight into the desk-first `direct`
    // resolver let a desk capture it: a blueprint may declare both a teammate
    // and a desk with id `main` — manifest validation does not forbid the
    // collision — and the prefixed address exists precisely to reach that
    // teammate, so handing it to the desk's lead answers the wrong party in
    // the one case the prefix was added for (issue #1743). The desk arm stays
    // as the fallback, so `dm:<desk>` still resolves a desk that no teammate
    // shares an id with, exactly as before.
    crate::runtime::assignee::dm_key(chat)
        .and_then(|key| record.resolve_roster_agent_id(key).or_else(|| direct(key)))
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
        if !ids.contains(&desk.id) && !crate::ports::general_channel::is_general_spelling(&desk.id)
        {
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
    // An `Auto` channel (issue #1835) is refused with its own reason, before
    // the leadless-desk arm below can claim it: that arm's wording — "has no
    // member on the roster" — would be a lie about a channel full of members.
    if let Some(reason) = reject_auto_channel_target(record, &desk_id) {
        return Some(reason);
    }
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

/// Why an [`Auto`](crate::ports::types::ResponderMode::Auto) channel cannot
/// take a `delegate_to_desk` hand-off (issue #1835) — or `None` when
/// `desk_id` is an ordinary lead desk.
///
/// A hand-off to a desk is a hand-off to **its lead**, and an auto channel has
/// none by design: its answerer is chosen per message. So the refusal says
/// that, rather than reusing the leadless-desk wording ("has no member on the
/// roster"), which would be a lie about a channel full of members.
///
/// **Public because both delegation paths must refuse identically.** The
/// harness tool reaches it through [`reject_desk_target`]; the hosted
/// device-side handler (`runtime::cycle`) calls it directly, because that path
/// deliberately does *not* refuse an ordinary leadless desk — there a hand-off
/// is a durable card on the board, visible whether or not anyone leads the
/// desk yet. An auto channel is the one case it must still refuse, and codex
/// caught the two paths disagreeing: the hosted one accepted the channel and
/// wrote a card noting "no lead member on the roster yet", which is both false
/// and permanently so.
///
/// Takes a **resolved** desk id, as `desk_responder_mode` does.
pub fn reject_auto_channel_target(record: &CompanyRecord, desk_id: &str) -> Option<String> {
    if record.desk_responder_mode(desk_id).is_lead() {
        return None;
    }
    // `desk_lead` is `None` for an auto channel by definition, so this list
    // excludes them without a second mode check.
    let with_leads = desk_list(
        desk_ids(record)
            .into_iter()
            .filter(|id| desk_lead(record, id).is_some())
            .collect(),
    );
    Some(match with_leads {
        Some(list) => format!(
            "The \"{desk_id}\" channel has no lead — who answers there is picked per \
message, not by rank — so `delegate_to_desk` has no one to hand this to. Desks that can \
take work: {list}."
        ),
        None => format!(
            "The \"{desk_id}\" channel has no lead — who answers there is picked per \
message, not by rank — so `delegate_to_desk` has no one to hand this to, and no other \
desk has a lead either. Answer directly instead of delegating."
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
        // Peers on the desk itself — the people the refused hand-off was
        // aimed at — not the whole roster an unrestricted reach spans.
        let peers = agent_list(desk_peers(record, delegator));
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

/// Everybody on a desk with `caller`, in desk-membership order, deduplicated,
/// never the caller — the desk-peer arm of [`teammate_targets`] on its own.
pub fn desk_peers(record: &CompanyRecord, caller: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for desk in desks_of_member(record, caller) {
        for member in record.effective_desk_members(&desk) {
            if member != caller
                && record.is_roster_agent(&member)
                && !ids.iter().any(|held| held == &member)
            {
                ids.push(member);
            }
        }
    }
    ids
}

/// Whether a [`delegates_to`](crate::company::Agent::delegates_to) list places
/// no bound on where its holder may hand work: **empty**, or carrying the
/// [`DELEGATES_TO_WILDCARD`](crate::company::DELEGATES_TO_WILDCARD).
///
/// Empty is unrestricted on the same convention as an omitted `tools` grant or
/// an omitted `ledgers` list — the manifest says nothing, so nothing is
/// narrowed. Only a list that names desks narrows. This is what makes a
/// teammate able to reach the rest of its company without an operator
/// enumerating the company in every agent's manifest entry.
pub fn reach_is_unrestricted(allowed: &[String]) -> bool {
    allowed.is_empty()
        || allowed
            .iter()
            .any(|entry| entry.trim() == crate::company::DELEGATES_TO_WILDCARD)
}

/// The teammates `caller` may hand work to with [`DELEGATE_TO_TEAMMATE_TOOL`]
/// (issue #884). Never the caller itself.
///
/// With an unrestricted `allowed` (see [`reach_is_unrestricted`]) — the
/// ordinary case, an agent whose manifest entry says nothing — that is
/// **everybody on the roster**: desk-mates first, then everybody else in
/// roster order. A teammate is not locked out of the company because nobody
/// wrote it a list, and the orchestrator and a desk-less specialist are
/// reachable like anyone else. What bounds a chain is the depth cap and the
/// cycle guard at the tool boundary, not the reach.
///
/// With a list that names desks, the reach is everybody on a desk with the
/// caller, plus everybody on a desk the list permits. The desk-peer arm is the
/// one #884 added and the one that closes D1: a desk's lead can reach the
/// specialist sitting beside it without going back through the orchestrator.
/// The allowlist arm is a re-reading of the #176 desk permission at teammate
/// granularity — a member allowed to hand work to a desk is allowed to hand it
/// to somebody on that desk.
///
/// Order is desk-peers first, then the rest, each in the desk's own membership
/// order, deduplicated — so a refusal (and the team brief) lists the nearest
/// options first.
pub fn teammate_targets(record: &CompanyRecord, caller: &str, allowed: &[String]) -> Vec<String> {
    let mut ids: Vec<String> = desk_peers(record, caller);
    let push = |ids: &mut Vec<String>, id: &str| {
        if id != caller && record.is_roster_agent(id) && !ids.iter().any(|held| held == id) {
            ids.push(id.to_string());
        }
    };
    if reach_is_unrestricted(allowed) {
        for id in roster_agent_ids(record) {
            push(&mut ids, &id);
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
pub(crate) fn desks_of_member(record: &CompanyRecord, member: &str) -> Vec<String> {
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
/// An **empty** `allowed` is unrestricted, exactly like the wildcard — see
/// [`reach_is_unrestricted`]. It used to mean "the tool was not wired at all",
/// back when only a member that opted in with `delegates_to` carried the tool;
/// now every roster agent does, and an omitted list is the ordinary case.
///
/// [`DELEGATES_TO_WILDCARD`]: crate::company::DELEGATES_TO_WILDCARD
pub fn reject_out_of_allowlist_target(
    record: &CompanyRecord,
    allowed: &[String],
    key: &str,
) -> Option<String> {
    if reach_is_unrestricted(allowed) {
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
///
/// Naming one example desk is enough for this function's informational-message
/// callers (`unknown_desk_message`) — the caller only needs *a* desk to point
/// at. It is the wrong call for a value that gets **persisted**: see
/// [`sole_desk_of_member`] for that case (issue #1882 review).
pub(crate) fn desk_of_member(record: &CompanyRecord, member: &str) -> Option<String> {
    desks_of_member(record, member).into_iter().next()
}

/// The desk `member` sits on, but only when unambiguous — `member` belongs to
/// exactly one desk. `None` both when `member` is on no desk and when it is on
/// two or more.
///
/// `pub(crate)`: the issue #1862 prerequisite's default-owner lever —
/// `apply_workflow_proposal` (`server/ops/tasks.rs`) fills a proposal's
/// omitted `owner_desk` from the assignee's desk. Unlike [`desk_of_member`]'s
/// informational-message use, that default gets written to disk, so picking
/// the first desk in manifest declaration order for a teammate who sits on
/// several would silently misrepresent ownership (issue #1882 review) —
/// leaving `owner_desk` unset is the same permissive "best-effort" stance the
/// caller already takes toward an assignee with no desk at all.
pub(crate) fn sole_desk_of_member(record: &CompanyRecord, member: &str) -> Option<String> {
    let mut desks = desks_of_member(record, member).into_iter();
    let first = desks.next()?;
    if desks.next().is_some() {
        return None;
    }
    Some(first)
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
#[path = "delegation_tools_tests_core.rs"]
mod tests_core;
#[cfg(test)]
#[path = "delegation_tools_tests_part2.rs"]
mod tests_part2;
#[cfg(test)]
#[path = "delegation_tools_tests_recursive_delegation_target_ch.rs"]
mod tests_recursive_delegation_target_ch;
