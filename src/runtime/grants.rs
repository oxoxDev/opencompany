//! Single-use grants: what an operator's "approve" actually buys for a tool
//! call an agent was blocked from making (issue #243).
//!
//! ## Why a grant, and not just "run the effect"
//!
//! Two different things park on the same approval queue, and they need opposite
//! treatment on approval:
//!
//! * A **native** effect — an email the runtime sends, a workflow delivery, a
//!   Medulla effect frame. The runtime built it and the runtime can perform it,
//!   so approving it means executing it, at most once, keyed by approval id.
//! * A **harness tool call** — `composio_execute`, `workspace_write`,
//!   `media_generate_image`. openhuman's `ToolPolicy` is fail-closed: it blocked
//!   the call inside the agent's turn and fed the model a refusal. The
//!   opencompany [`Effect`](crate::ports::types::Effect) projected from it is a
//!   *description* of a tool call, not something the runtime knows how to
//!   perform — its payload is the tool's arguments. Executing it would ledger a
//!   spend and route a `{channel, text}` payload if one happened to be shaped
//!   like that, and otherwise do nothing at all. The real work is the tool, and
//!   only that agent can run it.
//!
//! So approving a harness call mints a **grant**: a one-shot permission slip
//! that lets exactly one future call — same agent, same tool, byte-identical
//! arguments — through the policy that blocked it, after which it is gone. The
//! agent is then re-dispatched with an instruction to re-issue the call.
//!
//! ## Why every part of that sentence is load-bearing
//!
//! * **Single-use.** Consuming removes the grant under the same lock that
//!   matched it, so a model that re-tries the tool in a loop gets exactly one
//!   execution out of one approval. Approving once must not mean "this tool is
//!   open now".
//! * **Agent-scoped.** A grant minted for `finance` does not let `marketing`
//!   through, even for the identical call. The operator approved a specific
//!   agent's request.
//! * **Exact arguments.** Matching is `serde_json::Value` equality on the whole
//!   argument object. A model that re-issues the call with a different
//!   recipient, a larger amount, or an extra field does not match, falls
//!   through, and re-parks — which is the honest outcome, because the operator
//!   never saw those arguments. Approve-with-edit is handled by minting against
//!   the *amended* arguments, so the operator's edit is what the grant admits.
//! * **TTL.** A grant the agent never redeems expires
//!   ([`GRANT_TTL_MILLIS`]) rather than sitting live forever. An approval is
//!   consent to an action now, not a standing authorisation; without this, a
//!   grant minted today would still fire if the same call surfaced next month.
//!
//! ## The second scope: a standing grant (issue #374)
//!
//! Single-use is the right *default* and was for a while the only mode, which
//! made it the whole design. An agent reaching for the same tool a dozen times
//! produced a dozen near-identical cards, and the operator's rational escapes
//! were approving blind or switching the company to `full` — throwing the gate
//! away to stop it nagging. So there is now a second scope the operator can
//! pick: [`StandingGrant`], "this tool, for this teammate, until a deadline".
//!
//! It is a **distinct type**, not a scope enum on [`GrantedCall`], and both
//! differences are load-bearing:
//!
//! * it has **no `args` field**, so it is structurally incapable of
//!   argument-matching or of being widened into one later;
//! * its expiry is **not optional**, so it is structurally incapable of living
//!   forever. The issue forbids silent accumulation, and a type that cannot
//!   express "no expiry" cannot regress into it.
//!
//! What it never covers is decided elsewhere, once:
//! [`Effect::may_be_granted_standing`](crate::ports::types::Effect::may_be_granted_standing)
//! applied to the parked effect, which asks what the tool can **reach** rather
//! than what its name is called (issue #444). Running an arbitrary command,
//! reaching an arbitrary address and overwriting operator-owned guidance are
//! all refused, as is every Spend / Send / Sign / Publish / Hire / Identity
//! consequence and every tool nobody has classified.
//!
//! Because it has no `args` field this type admits any arguments, which is a
//! fair summary of a tool's consequence only while consequence is a property of
//! the tool name. It is not one for `composio_execute`, so the policy
//! re-classifies the live call before honouring a grant — see
//! [`ApprovalPolicy::standing_grant_allows`](crate::harness::policy::ApprovalPolicy).
//!
//! That check keeps a send out of a read's grant, but it cannot tell one
//! provider's read from another's: they are both reads, under the same tool
//! name, for the same teammate. So the grant also records **which toolkit the
//! card was about** ([`StandingGrant::scope`], issue #457). The operator
//! consented to "read from GitHub"; without the scope the grant they got was
//! "make any Composio read, anywhere" — broader than the sentence they agreed
//! to, across every account the company had ever connected.
//!
//! ## Durability
//!
//! Both sets are in-memory, but their lifecycles are journaled
//! (`ApprovalGranted` / `GrantConsumed` / `GrantExpired` for single-use;
//! `StandingGrantMinted` / `StandingGrantRevoked` / `StandingGrantExpired` for
//! standing) and replayed on boot via
//! [`RuntimeJournal::replayed_grants`](crate::runtime::journal::RuntimeJournal::replayed_grants)
//! and its standing counterpart, so a restart between "operator approved" and
//! "agent re-issued" does not silently drop the approval. Consumed, revoked and
//! expired grants are folded *out* on replay, so a restart can never resurrect
//! one that already fired or one the operator took back.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::ports::generate_id;
use crate::ports::types::{Actor, ApprovalId, EventSeq};

/// How long an unredeemed grant stays live: 15 minutes.
///
/// Sized to the gap between an operator hitting approve and the granting agent
/// finishing its re-dispatched turn — generous for a model turn, far short of
/// "still valid tomorrow". Expiry is not silent: the sweep tells the operator
/// the agent did not act, so a re-approval is an informed choice rather than a
/// mystery.
pub const GRANT_TTL_MILLIS: u64 = 15 * 60 * 1000;

/// One approved-but-not-yet-redeemed tool call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GrantedCall {
    /// The approval the operator resolved to mint this grant.
    pub approval_id: ApprovalId,
    /// The roster agent allowed to redeem it. Nobody else matches.
    pub agent: String,
    /// The tool the grant admits — the parked effect's `kind`.
    pub tool: String,
    /// The exact arguments admitted. Matching is whole-value equality.
    pub args: serde_json::Value,
    /// Epoch-millis the grant was minted, for TTL expiry.
    pub at_millis: u64,
    /// The chat thread the approval was raised in (issue #379) — copied off the
    /// approval's origin when the grant is minted, so the re-dispatched turn's
    /// reply is journaled back into the conversation that asked for it.
    ///
    /// Deliberately **not** part of the redemption match: the operator approved
    /// a call, not a location, and a grant that failed to match because the
    /// turn came back on a different thread would silently re-park. It rides
    /// along purely as routing.
    ///
    /// `None` when the parked approval carried no thread (a workflow delivery,
    /// a scheduler tick) and on a grant replayed from a journal line written
    /// before this field existed. Both fall back to
    /// [`agent`](Self::agent) — today's behaviour, which is right for a DM and
    /// was never right for a desk channel.
    #[serde(default)]
    pub origin_thread: Option<String>,
    /// The thread *within* [`origin_thread`](Self::origin_thread) the approval
    /// was raised in (issue #435) — the root the raising message hangs off,
    /// copied off the approval's origin alongside the channel.
    ///
    /// Rides along on exactly the same terms as `origin_thread`, and for the
    /// same reason: **not** part of the redemption match. The operator approved
    /// a call, not a location, and a grant that failed to match because the
    /// turn came back on a different thread would silently re-park — the
    /// failure #379 called out, one axis finer. It is routing, and only
    /// routing.
    ///
    /// That is safe by construction rather than by care: [`GrantSet::consume`]
    /// matches field by field on `agent`, `tool` and `args`, so a field added
    /// here cannot join the predicate by accident.
    ///
    /// `None` when the approval was raised straight in a channel rather than
    /// inside a thread, and on a grant replayed from a line written before this
    /// field existed. Both mean the continuation answers in the channel, which
    /// is the pre-#435 behaviour.
    #[serde(default)]
    pub origin_parent: Option<EventSeq>,
    /// The task the parked call belonged to, when it was raised from a task turn
    /// (issue #796) — copied off the approval's `approval_task` join when the
    /// grant is minted.
    ///
    /// **Routing only, never part of the redemption match**, on exactly the same
    /// terms as [`origin_thread`](Self::origin_thread): the operator approved a
    /// call, not a task, and [`GrantSet::consume`] matches on `(agent, tool,
    /// args)` alone. It rides along so the re-dispatched turn can reclaim the
    /// task's held-across-park checkout ([`CheckoutLedger`] in
    /// `crate::harness::repo`) and so a denied or expired grant's tree can be
    /// swept once no live grant names the task.
    ///
    /// `None` for an approval raised outside a task (a plain operator chat) and
    /// on a grant replayed from a line written before this field existed — both
    /// mean there is no task checkout to resume, which is the pre-#796 behaviour.
    #[serde(default)]
    pub origin_task: Option<String>,
}

/// The hard ceiling on a standing grant's life: 7 days.
///
/// A request past this is a **400**, never a silent clamp. Quietly shortening a
/// duration the operator chose would leave them believing a permission is live
/// when it lapsed days earlier — the failure this issue exists to stop, in the
/// opposite direction.
pub const MAX_STANDING_GRANT_MILLIS: u64 = 7 * 24 * 60 * 60 * 1000;

/// A standing grant's own id.
///
/// Separate from [`ApprovalId`] because the two have different lifetimes: the
/// approval is resolved and gone, the grant it minted outlives it and is what
/// the operator later revokes. Keying revocation on the approval id would tie
/// the revoke route to a record that is, from the operator's side, history.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GrantId(String);

impl GrantId {
    /// Wraps an existing grant id string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Mints a fresh grant id.
    pub fn generate() -> Self {
        Self(generate_id())
    }
}

impl From<String> for GrantId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl AsRef<str> for GrantId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for GrantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What an operator's approve actually buys (issue #374).
///
/// Two options, and only two. Count-based ("the next 5 calls") was rejected: it
/// reintroduces the annoyance nondeterministically and needs a durable decrement
/// on the hot path. "For this session" was rejected because the runtime has no
/// operator-session object that an agent's work spans — it would be a fiction.
/// "Forever" is what the issue forbids.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GrantScope {
    /// Today's behaviour, byte for byte: one call, argument-exact, agent-scoped.
    /// The default, and what every caller that says nothing gets.
    #[default]
    Once,
    /// This tool, for this teammate, with any arguments, until
    /// `expires_at_millis` — an absolute epoch-millis deadline.
    Tool {
        /// Absolute epoch-millis the grant stops admitting calls.
        expires_at_millis: u64,
    },
}

/// Who may spend a standing permission (issue #1098).
///
/// Two kinds, and the second is the whole of that issue. A scheduled workflow
/// re-asked the same question on every run because a permission could only name
/// a teammate, and a graph that is simply running has none — so the case whose
/// calls were *pre-declared by an operator* was the one case standing permission
/// could not cover, while an agent choosing its arguments at run time could hold
/// one.
///
/// # Why a workflow and not one of its nodes
///
/// The operator consented to a host, and a second call to that host from the
/// same job is the same proposition they already agreed to. Keying on the node
/// would re-park it — the workflow-shaped version of slug-exactness, which
/// [`StandingGrant::scope`] records was rejected for Composio because it "would
/// re-park every new action and make the grant worth nothing".
///
/// The cost is stated rather than hidden: for the six tools that are grantable
/// with no scope at all (`file_write`, `edit`, `apply_patch`, `csv_export`,
/// `memory_store`, `publish_artifact`) nothing narrows a workflow permission, so
/// a node added inside the window inherits it. Three things bound that, and are
/// why it is the accepted trade: `shell` and `http_request` are
/// [`Standing::PerCall`](crate::policy::Standing) and never persist at all, the
/// ceiling is seven days, and a permission is per-tool — "may write files" never
/// implies "may publish". Narrowing this later means adding a node id to the
/// workflow variant, and nothing else.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GrantSubject {
    /// A roster teammate. Every permission minted before issue #1098.
    Agent(String),
    /// One authored workflow, by the stable id its file declares — not the run
    /// id, which is fresh on every firing and would make each permission die
    /// with the run that minted it.
    Workflow(String),
}

impl GrantSubject {
    /// A teammate subject from anything string-like.
    pub fn agent(id: impl Into<String>) -> Self {
        Self::Agent(id.into())
    }

    /// A workflow subject from anything string-like.
    pub fn workflow(id: impl Into<String>) -> Self {
        Self::Workflow(id.into())
    }
}

/// Who could hold a standing permission for `effect`, or `None` when nobody
/// could (issue #1098).
///
/// **The one place this is decided.** Three surfaces ask it — the card's
/// `broadly_grantable` flag, the resolve route's 400, and the mint — and all
/// three must agree or an operator is offered a control that then refuses them,
/// which is the drift issue #444 exists to prevent.
///
/// A teammate wins when there is one. A gate has no teammate but does name the
/// workflow it belongs to, and that is the subject issue #1098 added. `None` is
/// every other native effect — something the runtime performs itself, where
/// there is no tool use to hand over and approving once is the only honest
/// answer.
pub fn subject_of(effect: &crate::ports::types::Effect) -> Option<GrantSubject> {
    if let Some(agent) = effect.agent.as_deref() {
        return Some(GrantSubject::Agent(agent.to_string()));
    }
    crate::runtime::workflow_resume::gate_workflow_id(effect)
        .map(|workflow| GrantSubject::Workflow(workflow.to_string()))
}

/// A standing permission: one tool, one subject, until a deadline (issues #374,
/// #1098).
///
/// Deliberately **not** a variant of [`GrantedCall`]. See the module docs: no
/// `args` and a non-optional expiry are the two properties that make this type
/// unable to become the thing the issue warns about.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StandingGrant {
    /// This grant's id — what a revoke addresses.
    pub id: GrantId,
    /// The roster agent allowed to redeem it. Nobody else matches.
    ///
    /// Empty on a workflow permission, which names its subject in
    /// [`workflow`](Self::workflow) instead. Read
    /// [`subject`](Self::subject) rather than this field — matching on a bare
    /// agent string would let an empty one collide.
    #[serde(default)]
    pub agent: String,
    /// The authored workflow allowed to redeem it (issue #1098).
    ///
    /// `None` on every teammate permission, and on any journal line written
    /// before this field existed — both of which are agent permissions and
    /// resolve through [`agent`](Self::agent), so a replayed line reproduces
    /// today's behaviour exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// The tool it admits, with any arguments.
    pub tool: String,
    /// Who granted it. Journaled so "who opened this up" is answerable later.
    pub granted_by: Actor,
    /// The approval whose resolution minted it — the provenance the brain's
    /// re-dispatch joins on, and the audit link back to the card the operator
    /// was looking at when they decided.
    pub approval_id: ApprovalId,
    /// Epoch-millis it was minted.
    pub at_millis: u64,
    /// Absolute epoch-millis it stops admitting calls. Not an optional, and not
    /// a duration: an absolute deadline survives a restart without arithmetic,
    /// and a required field cannot be omitted into immortality.
    pub expires_at_millis: u64,
    /// The chat thread the approval was raised in (issue #379), carried for the
    /// same reason [`GrantedCall::origin_thread`] is: so the re-dispatched
    /// turn's reply is journaled back into the conversation that asked for it.
    ///
    /// **Routing only, never part of the match** — matching is `(agent, tool,
    /// unexpired)` and nothing else. A desk channel and a direct message to that
    /// channel's lead are answered by the same teammate, so without this the
    /// continuation of a channel's request would land in the lead's private
    /// line: the operator approves in one place and the work resumes in another
    /// they are not looking at.
    ///
    /// `None` for an approval with no conversation behind it, and on a grant
    /// replayed from a line written before this field existed. Both fall back to
    /// [`agent`](Self::agent), which is right for a DM.
    #[serde(default)]
    pub origin_thread: Option<String>,
    /// The thread *within* [`origin_thread`](Self::origin_thread) the approval
    /// was raised in (issue #435), carried on exactly the terms
    /// [`GrantedCall::origin_parent`] documents.
    ///
    /// **Routing only, never part of the match** — the match here is `(agent,
    /// tool, unexpired)`, and a standing grant deliberately admits any
    /// arguments; adding a location to it would make a broad permission
    /// silently narrow.
    ///
    /// `None` when the approval was raised straight in a channel, and on a
    /// grant replayed from a line written before this field existed — both
    /// answer in the channel, as before.
    #[serde(default)]
    pub origin_parent: Option<EventSeq>,
    /// The task the parked call belonged to, when raised from a task turn
    /// (issue #796), carried on exactly the terms
    /// [`GrantedCall::origin_task`] documents: routing and cleanup only, never
    /// part of the `(agent, tool, unexpired)` match. A standing grant can resume
    /// a task's checkout across repeated parks, so it carries the same link.
    ///
    /// `None` for an approval raised outside a task, and on a grant replayed
    /// from a line written before this field existed.
    #[serde(default)]
    pub origin_task: Option<String>,
    /// The slice of [`tool`](Self::tool) this grant is confined to, when the
    /// tool's name is not the whole of what it can do (issue #457).
    ///
    /// `None` for every tool whose name already describes its consequence, and
    /// for those it is not a missing value but the correct one: nothing about
    /// `file_write` needs narrowing, and matching on `(agent, tool, unexpired)`
    /// says exactly what the operator agreed to.
    ///
    /// `Some(toolkit)` for `composio_execute`, the one tool that carries every
    /// action of every connected provider under a single name. The card the
    /// operator read said "read from GitHub"; without this field the grant it
    /// minted said "make any Composio read, anywhere", because every Composio
    /// read matched the same `(agent, "composio_execute")` pair. Minted from the
    /// parked effect's own payload, so what is recorded is what was shown.
    ///
    /// Scoped by **toolkit and not by action slug**, deliberately: the operator
    /// consented to a provider, so a *different* GitHub read has to keep
    /// passing. Slug-exact would re-park every new action and make the grant
    /// worth nothing.
    ///
    /// `Some(origin)` — a `scheme://host[:port]` URL origin — for `web_fetch`
    /// since issues #673/#739, on the same terms and from the same function.
    /// **This is a second kind of value in the same string**, and a reader that
    /// assumes the toolkit kind is wrong about it: the console spelled
    /// `https://docs.rs` out with the toolkit speller and rendered
    /// `Https://docs.rs` for three releases (issue #785). Anything that
    /// *displays* a scope has to tell the two apart; anything that *matches* one
    /// must not care, because [`admits_scope`](Self::admits_scope) is exact
    /// string equality over whichever kind was minted.
    ///
    /// `None` also on a grant replayed from a journal line written before this
    /// field existed, where it means "unscoped" and reproduces the old
    /// behaviour exactly — see [`admits_scope`](Self::admits_scope).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl StandingGrant {
    /// Who may spend this permission (issue #1098).
    ///
    /// The one place the two subject fields are reconciled, so no caller has to
    /// decide what an empty [`agent`](Self::agent) means. A line carrying
    /// [`workflow`](Self::workflow) is a workflow permission; everything else —
    /// including every line written before that field existed — is an agent
    /// permission, which is what makes replay a no-op.
    pub fn subject(&self) -> GrantSubject {
        match self.workflow.as_deref() {
            Some(workflow) => GrantSubject::Workflow(workflow.to_string()),
            None => GrantSubject::Agent(self.agent.clone()),
        }
    }

    /// Whether this grant admits a call whose live scope is `scope` (issue
    /// #457).
    ///
    /// Three cases, and the two edges are the ones that matter:
    ///
    /// * **This grant has no scope** — it admits anything its `(agent, tool)`
    ///   match already admitted. That is what makes a journal line written
    ///   before the field existed replay into today's behaviour rather than
    ///   into a grant that silently stopped working.
    /// * **Scopes are equal** — the ordinary hit. A second GitHub read against a
    ///   GitHub-scoped grant.
    /// * **This grant is scoped and the live call has no scope** — refused. An
    ///   action the catalogue cannot place might belong to any provider, so
    ///   admitting it on a GitHub grant would be a guess in the permissive
    ///   direction. It falls through and parks instead, which is the same answer
    ///   this codebase gives an unrecognised action everywhere else: unknown is
    ///   a send.
    ///
    /// # Dormant, deliberately (issue #610)
    ///
    /// No current tier routes a Composio call through this: since #559 a
    /// catalogue read is allowed by the tier before the grant checks, and under
    /// `readonly` the #243 brake denies above them. The scope this spends is
    /// still minted, and the reasoning for keeping both halves is recorded once,
    /// at [`standing_scope_of`](crate::policy::consequence::standing_scope_of) —
    /// read it before concluding this is unused.
    pub fn admits_scope(&self, scope: Option<&str>) -> bool {
        match self.scope.as_deref() {
            None => true,
            Some(mine) => scope == Some(mine),
        }
    }

    /// Whether this grant still admits calls at `now_millis`.
    ///
    /// Strictly `<`, so the deadline instant itself is already past. Checked at
    /// redemption under the grant lock as well as swept periodically — the sweep
    /// is housekeeping and an operator notice, never the enforcement.
    pub fn is_live_at(&self, now_millis: u64) -> bool {
        now_millis < self.expires_at_millis
    }
}

/// The live grant set: a cheap shared handle, the same pattern as
/// [`ApprovalRequestQueue`](crate::harness::policy::ApprovalRequestQueue).
///
/// Cloning shares the state, so the policy installed on every roster agent, the
/// cycle runner that mints, and the sweep that expires all see one set.
#[derive(Clone, Default)]
pub struct GrantSet {
    inner: Arc<Mutex<GrantState>>,
}

#[derive(Default)]
struct GrantState {
    live: HashMap<ApprovalId, GrantedCall>,
    /// Grants consumed since the last [`GrantSet::drain_consumed`].
    ///
    /// Consumption happens deep inside a `ToolPolicy::check`, which is sync and
    /// has no journal handle, so the record cannot be written there. The id is
    /// buffered instead and the cycle runner drains it after the cycle it
    /// belongs to. A crash between consuming and draining loses the
    /// `GrantConsumed` record, which on replay re-arms a grant whose tool
    /// already ran: the `ApprovalGranted` that minted it survives, the
    /// redemption does not, and [`GrantSet::consume`] will admit the identical
    /// call a second time with **no** new approval card — until the grant's own
    /// [`GRANT_TTL_MILLIS`] retires it.
    ///
    /// This is a duplication window, not the safe direction, and it is stated
    /// that way because it used to be recorded here as the opposite. What issue
    /// #392 could close from the journal's side it did: `GrantConsumed` is
    /// host-durable, so a record that reached the append is on stable storage
    /// before the append returns. This buffer is the half that remains — closing
    /// it means recording the redemption where it happens, which needs a journal
    /// handle at the `ToolPolicy::check` seam.
    consumed: Vec<ApprovalId>,
    /// Standing grants (issue #374), keyed by their own id.
    ///
    /// A second map rather than a second variant in `live`: the two are matched
    /// on different keys (approval id vs grant id), matched by different
    /// predicates (argument-exact vs tool-and-agent), and have opposite
    /// redemption semantics (remove vs leave). Fusing them would put a branch on
    /// every operation of both.
    standing: HashMap<GrantId, StandingGrant>,
    /// Work units with an approval **parked but not yet resolved** (issue #796),
    /// keyed by the approval id so each resolution clears exactly its own entry.
    ///
    /// A parked approval mints no grant until the operator decides it, so between
    /// the park and the decision neither `live` nor `standing` names its task.
    /// Without this map [`any_for_task`](GrantSet::any_for_task) would read
    /// `false` in that window and an unrelated turn's
    /// [`sweep_orphans`](crate::harness::repo::CheckoutLedger::sweep_orphans)
    /// would delete the checkout the parked step is holding for its own resume —
    /// the very deadlock #796 exists to prevent, reopened one turn upstream.
    /// Filled when the effect parks, emptied when it resolves, is denied, or
    /// expires. In-memory only, like the checkouts it guards: a restart boot-
    /// sweeps every checkout, so there is nothing left for a rehydrated mark to
    /// protect.
    pending: HashMap<ApprovalId, String>,
}

impl GrantSet {
    /// Mints a grant.
    pub fn grant(&self, call: GrantedCall) {
        self.inner
            .lock()
            .expect("grant set poisoned")
            .live
            .insert(call.approval_id.clone(), call);
    }

    /// Redeems a grant for `(agent, tool, args)`, removing it.
    ///
    /// The match and the removal happen under one lock, so two concurrent turns
    /// racing the same grant cannot both be admitted — exactly one gets the
    /// `Some`. Returns `None` when nothing matches, which is the fall-through
    /// that re-parks.
    pub fn consume(
        &self,
        agent: &str,
        tool: &str,
        args: &serde_json::Value,
    ) -> Option<GrantedCall> {
        let mut state = self.inner.lock().expect("grant set poisoned");
        let id = state
            .live
            .iter()
            .find(|(_, g)| g.agent == agent && g.tool == tool && &g.args == args)
            .map(|(id, _)| id.clone())?;
        let call = state.live.remove(&id)?;
        state.consumed.push(id);
        Some(call)
    }

    /// Reads a live grant by approval id without redeeming it.
    ///
    /// This is how the brain recovers the arguments to tell the agent to
    /// re-issue: the grant must still be live when the instruction is written,
    /// and must still be live when the agent's tool call reaches the policy.
    pub fn peek(&self, id: &ApprovalId) -> Option<GrantedCall> {
        self.inner
            .lock()
            .expect("grant set poisoned")
            .live
            .get(id)
            .cloned()
    }

    /// Removes every grant minted more than `ttl_millis` before `now_millis`,
    /// returning them so the caller can journal and announce each expiry.
    pub fn sweep(&self, now_millis: u64, ttl_millis: u64) -> Vec<GrantedCall> {
        let mut state = self.inner.lock().expect("grant set poisoned");
        let expired: Vec<ApprovalId> = state
            .live
            .iter()
            .filter(|(_, g)| now_millis.saturating_sub(g.at_millis) >= ttl_millis)
            .map(|(id, _)| id.clone())
            .collect();
        expired
            .into_iter()
            .filter_map(|id| state.live.remove(&id))
            .collect()
    }

    /// Seeds the live set from a journal replay (boot recovery).
    pub fn rehydrate(&self, calls: impl IntoIterator<Item = GrantedCall>) {
        let mut state = self.inner.lock().expect("grant set poisoned");
        for call in calls {
            state.live.insert(call.approval_id.clone(), call);
        }
    }

    /// Takes the ids consumed since the last drain, so they can be journaled.
    pub fn drain_consumed(&self) -> Vec<ApprovalId> {
        std::mem::take(&mut self.inner.lock().expect("grant set poisoned").consumed)
    }

    /// How many grants are live (tests / observability).
    pub fn live_count(&self) -> usize {
        self.inner.lock().expect("grant set poisoned").live.len()
    }

    /// Records that a work unit has an approval **parked and awaiting a
    /// decision** (issue #796), so [`any_for_task`](Self::any_for_task) treats it
    /// as live until the approval resolves.
    ///
    /// Keyed by the approval id, computed the same way the mint side derives a
    /// grant's [`GrantedCall::origin_task`], so the pending mark and the grant it
    /// eventually becomes name one unit. A task parking a second approval simply
    /// adds a second entry naming the same task; each clears independently.
    pub fn mark_pending(&self, approval_id: &ApprovalId, task: String) {
        self.inner
            .lock()
            .expect("grant set poisoned")
            .pending
            .insert(approval_id.clone(), task);
    }

    /// Drops the pending-approval mark for `approval_id` (issue #796) — whether
    /// it was approved (a grant now names the task), denied, or expired (nothing
    /// does, so its checkout is now sweepable). A no-op for an id that never
    /// parked a task-scoped effect.
    pub fn clear_pending(&self, approval_id: &ApprovalId) {
        self.inner
            .lock()
            .expect("grant set poisoned")
            .pending
            .remove(approval_id);
    }

    /// Whether a live grant, a standing grant, **or a still-parked approval**
    /// names `task` as its origin (issue #796).
    ///
    /// The harness asks this to decide whether a task's checkout held across an
    /// approval park is still awaiting a resume or has been orphaned by a denied
    /// or expired approval, so
    /// [`CheckoutLedger::sweep_orphans`](crate::harness::repo::CheckoutLedger::sweep_orphans)
    /// can reclaim the disk. Three states keep it live: a live grant names it (an
    /// approved step waiting to be re-issued), a standing grant names it, or an
    /// approval it parked is **still pending** — that last case mints no grant
    /// yet, so without `pending` the checkout would be swept in the window
    /// between the park and the operator's decision. A spent grant is already
    /// removed from every map, so this reads `false` the moment the resume is
    /// under way — which is safe because the resuming turn has reclaimed the tree
    /// onto its turn-scoped list by then.
    pub fn any_for_task(&self, task: &str) -> bool {
        let state = self.inner.lock().expect("grant set poisoned");
        let names_task = |t: &Option<String>| t.as_deref() == Some(task);
        state.live.values().any(|g| names_task(&g.origin_task))
            || state.standing.values().any(|g| names_task(&g.origin_task))
            || state.pending.values().any(|t| t == task)
    }

    // -----------------------------------------------------------------------
    // Standing grants (issue #374)
    // -----------------------------------------------------------------------

    /// Arms a standing grant.
    pub fn grant_standing(&self, grant: StandingGrant) {
        self.inner
            .lock()
            .expect("grant set poisoned")
            .standing
            .insert(grant.id.clone(), grant);
    }

    /// Matches an unexpired standing grant for `(subject, tool, scope)`
    /// **without** removing it — that is the whole difference from
    /// [`consume`](Self::consume).
    ///
    /// `scope` is the *live* call's scope, computed from its arguments by
    /// [`standing_scope_of`](crate::policy::consequence::standing_scope_of) and
    /// compared against what the grant recorded at mint time (issue #457). It is
    /// `None` for every tool whose name is the whole of what it can do, and for
    /// a Composio action the catalogue cannot place — the second of which a
    /// scoped grant refuses, per
    /// [`StandingGrant::admits_scope`](StandingGrant::admits_scope).
    ///
    /// Expiry is enforced *here*, under the same lock as the match, rather than
    /// being left to the sweep. The sweep runs on the scheduler's maintenance
    /// tick; between two ticks a lapsed grant would otherwise keep admitting
    /// calls, and "until 5pm" has to mean 5pm rather than "until the next tick
    /// after 5pm".
    ///
    /// Deterministic when several grants could match — the same agent and tool
    /// granted twice — by taking the one that expires **last**: the operator's
    /// most recent intent is the more permissive one they are living with, and
    /// picking arbitrarily out of a `HashMap` would make redemption depend on
    /// hash order.
    /// `subject` rather than a bare agent string (issue #1098): a workflow
    /// permission carries an empty [`StandingGrant::agent`], so a `&str`
    /// parameter would let one match on emptiness. The enum makes the two kinds
    /// of subject impossible to confuse at the call site.
    pub fn match_standing(
        &self,
        subject: &GrantSubject,
        tool: &str,
        scope: Option<&str>,
        now_millis: u64,
    ) -> Option<StandingGrant> {
        let state = self.inner.lock().expect("grant set poisoned");
        state
            .standing
            .values()
            .filter(|g| {
                &g.subject() == subject
                    && g.tool == tool
                    && g.admits_scope(scope)
                    && g.is_live_at(now_millis)
            })
            .max_by_key(|g| g.expires_at_millis)
            .cloned()
    }

    /// Revokes a standing grant, returning it when there was one.
    ///
    /// `None` means it was already gone — revoked by another browser tab, or
    /// swept — which the route reports as a 404 rather than pretending to have
    /// done something.
    pub fn revoke_standing(&self, id: &GrantId) -> Option<StandingGrant> {
        self.inner
            .lock()
            .expect("grant set poisoned")
            .standing
            .remove(id)
    }

    /// Reads a standing grant by the approval that minted it.
    ///
    /// The brain's re-dispatch needs this: it is handed an approval id and has
    /// to find the permission that resolution created, whichever scope it was.
    pub fn peek_standing_by_approval(&self, approval_id: &ApprovalId) -> Option<StandingGrant> {
        self.inner
            .lock()
            .expect("grant set poisoned")
            .standing
            .values()
            .find(|g| &g.approval_id == approval_id)
            .cloned()
    }

    /// Every live standing grant, newest first — what the console lists.
    pub fn standing(&self) -> Vec<StandingGrant> {
        let state = self.inner.lock().expect("grant set poisoned");
        let mut out: Vec<StandingGrant> = state.standing.values().cloned().collect();
        out.sort_by(|a, b| b.at_millis.cmp(&a.at_millis).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Removes every standing grant whose deadline has passed, returning them so
    /// the caller can journal each expiry.
    pub fn sweep_standing(&self, now_millis: u64) -> Vec<StandingGrant> {
        let mut state = self.inner.lock().expect("grant set poisoned");
        let expired: Vec<GrantId> = state
            .standing
            .values()
            .filter(|g| !g.is_live_at(now_millis))
            .map(|g| g.id.clone())
            .collect();
        expired
            .into_iter()
            .filter_map(|id| state.standing.remove(&id))
            .collect()
    }

    /// Seeds the standing map from a journal replay (boot recovery).
    pub fn rehydrate_standing(&self, grants: impl IntoIterator<Item = StandingGrant>) {
        let mut state = self.inner.lock().expect("grant set poisoned");
        for grant in grants {
            state.standing.insert(grant.id.clone(), grant);
        }
    }

    /// How many standing grants are live (tests / observability).
    pub fn standing_count(&self) -> usize {
        self.inner
            .lock()
            .expect("grant set poisoned")
            .standing
            .len()
    }
}

impl std::fmt::Debug for GrantSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrantSet")
            .field("live", &self.live_count())
            .field("standing", &self.standing_count())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn call(id: &str, agent: &str, tool: &str, args: serde_json::Value) -> GrantedCall {
        GrantedCall {
            approval_id: ApprovalId::new(id),
            agent: agent.to_string(),
            tool: tool.to_string(),
            args,
            at_millis: 1_000,
            origin_thread: None,
            origin_parent: None,
            origin_task: None,
        }
    }

    /// Issue #435, guarding #379's decision: a grant's origin is **routing, not
    /// identity**. Neither the channel nor the thread within it may join the
    /// redemption match.
    ///
    /// The failure this prevents is silent and expensive. If location were part
    /// of the match, a re-dispatched turn that came back anywhere other than
    /// where it started would simply fail to find its grant and re-park — the
    /// operator approves, the agent asks again, and nothing anywhere says why.
    /// `origin_parent` makes that mistake newly reachable by adding a second,
    /// finer location to get wrong, so it is pinned here rather than left to
    /// the comment on the field.
    #[test]
    fn a_grants_origin_is_routing_and_never_part_of_the_match() {
        let args = serde_json::json!({ "to": "a@b.test" });

        // Minted inside a thread; redeemed by a turn that knows only the call.
        // `consume` is not even given a location to compare against — that is
        // the shape of the guarantee.
        let set = GrantSet::default();
        set.grant(GrantedCall {
            origin_thread: Some("desk-finance".to_string()),
            origin_parent: Some(EventSeq::new(7)),
            origin_task: None,
            ..call("a1", "finance", "composio_execute", args.clone())
        });
        let redeemed = set
            .consume("finance", "composio_execute", &args)
            .expect("a thread-rooted grant is redeemed by the matching call");
        assert_eq!(
            redeemed.origin_parent,
            Some(EventSeq::new(7)),
            "and the location rides along on the consumed grant, for routing",
        );
        assert_eq!(redeemed.origin_thread, Some("desk-finance".to_string()));

        // Two grants differing *only* in origin are the same call as far as
        // matching is concerned: the first still redeems, so nothing about the
        // location narrowed it.
        for origin in [
            (None, None),
            (Some("desk-finance".to_string()), None),
            (Some("agent-cfo".to_string()), Some(EventSeq::new(9))),
        ] {
            let set = GrantSet::default();
            set.grant(GrantedCall {
                origin_thread: origin.0,
                origin_parent: origin.1,
                origin_task: None,
                ..call("a1", "finance", "composio_execute", args.clone())
            });
            assert!(
                set.consume("finance", "composio_execute", &args).is_some(),
                "the operator approved a call, not a location",
            );
        }
    }

    #[test]
    fn a_grant_is_redeemed_exactly_once() {
        let set = GrantSet::default();
        let args = serde_json::json!({ "to": "a@b.test" });
        set.grant(call("a1", "finance", "composio_execute", args.clone()));

        assert!(set.consume("finance", "composio_execute", &args).is_some());
        assert!(
            set.consume("finance", "composio_execute", &args).is_none(),
            "one approval buys one call, not an open door"
        );
        assert_eq!(set.live_count(), 0);
        assert_eq!(set.drain_consumed().len(), 1);
        assert!(set.drain_consumed().is_empty(), "the drain is a take");
    }

    #[test]
    fn a_grant_is_scoped_to_its_agent_and_its_exact_arguments() {
        let set = GrantSet::default();
        let args = serde_json::json!({ "to": "a@b.test", "body": "hi" });
        set.grant(call("a1", "finance", "composio_execute", args.clone()));

        // Another agent making the identical call is not who was approved.
        assert!(
            set.consume("marketing", "composio_execute", &args)
                .is_none()
        );
        // A different tool is not what was approved.
        assert!(set.consume("finance", "workspace_write", &args).is_none());
        // Different arguments are not what the operator saw.
        assert!(
            set.consume(
                "finance",
                "composio_execute",
                &serde_json::json!({ "to": "someone@else.test", "body": "hi" })
            )
            .is_none()
        );
        // An extra key is a different call too — matching is whole-value.
        assert!(
            set.consume(
                "finance",
                "composio_execute",
                &serde_json::json!({ "to": "a@b.test", "body": "hi", "cc": "x@y.test" })
            )
            .is_none()
        );
        // ...and none of those near-misses burned the grant.
        assert!(set.consume("finance", "composio_execute", &args).is_some());
    }

    #[test]
    fn peek_reads_without_redeeming() {
        let set = GrantSet::default();
        let args = serde_json::json!({ "q": 1 });
        set.grant(call("a1", "finance", "web_fetch", args.clone()));

        let seen = set.peek(&ApprovalId::new("a1")).expect("grant is live");
        assert_eq!(seen.tool, "web_fetch");
        assert_eq!(set.live_count(), 1, "peeking must not consume");
        assert!(set.peek(&ApprovalId::new("nope")).is_none());
    }

    /// Issue #796: the window between a park and the operator's decision. A
    /// parked approval mints no grant, so `any_for_task` would read `false`
    /// without the pending mark — and an unrelated turn's checkout sweep would
    /// then delete the parked step's tree. The mark keeps the task live until the
    /// approval settles, and two approvals on one task clear independently.
    #[test]
    fn a_still_parked_approval_keeps_its_task_alive() {
        let set = GrantSet::default();
        assert!(!set.any_for_task("t-1"), "nothing names the task yet");

        // A parked approval: no grant, but the task is marked pending.
        set.mark_pending(&ApprovalId::new("a1"), "t-1".to_string());
        assert!(
            set.any_for_task("t-1"),
            "a pending approval must keep the task live"
        );

        // A second approval parks on the same task.
        set.mark_pending(&ApprovalId::new("a2"), "t-1".to_string());
        // Settling the first still leaves the second holding the task.
        set.clear_pending(&ApprovalId::new("a1"));
        assert!(
            set.any_for_task("t-1"),
            "the second pending approval still names the task"
        );
        // Settling the last (denied or expired, so no grant follows) drops it.
        set.clear_pending(&ApprovalId::new("a2"));
        assert!(
            !set.any_for_task("t-1"),
            "no pending approval and no grant leaves nothing to keep the task alive"
        );
    }

    #[test]
    fn sweep_expires_only_grants_past_the_ttl() {
        let set = GrantSet::default();
        set.grant(call("old", "finance", "t", serde_json::json!({})));
        let mut fresh = call("new", "finance", "t2", serde_json::json!({}));
        fresh.at_millis = 900_000;
        set.grant(fresh);

        let expired = set.sweep(1_000 + GRANT_TTL_MILLIS, GRANT_TTL_MILLIS);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].approval_id, ApprovalId::new("old"));
        assert_eq!(set.live_count(), 1, "the fresh grant survives");
    }

    #[test]
    fn rehydrate_seeds_the_live_set() {
        let set = GrantSet::default();
        set.rehydrate([
            call("a1", "finance", "t", serde_json::json!({})),
            call("a2", "legal", "t", serde_json::json!({})),
        ]);
        assert_eq!(set.live_count(), 2);
        assert!(set.peek(&ApprovalId::new("a2")).is_some());
    }

    /// Concurrent redemption of one grant: exactly one caller wins.
    ///
    /// The match and the removal are one critical section precisely so this
    /// cannot double-fire. A read-then-remove would let both threads see the
    /// grant and both proceed, turning one approval into two executions of a
    /// tool the operator approved once.
    #[test]
    fn two_threads_racing_one_grant_yield_exactly_one_winner() {
        let set = GrantSet::default();
        let args = serde_json::json!({ "amount_usd": 40.0 });
        set.grant(call("a1", "finance", "pay_invoice", args.clone()));

        let barrier = Arc::new(std::sync::Barrier::new(8));
        let winners = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let set = set.clone();
                let args = args.clone();
                let barrier = Arc::clone(&barrier);
                let winners = Arc::clone(&winners);
                std::thread::spawn(move || {
                    barrier.wait();
                    if set.consume("finance", "pay_invoice", &args).is_some() {
                        winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread");
        }

        assert_eq!(winners.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(set.live_count(), 0);
        assert_eq!(set.drain_consumed().len(), 1, "one consumption journaled");
    }

    // -----------------------------------------------------------------------
    // Standing grants (issue #374)
    // -----------------------------------------------------------------------

    fn operator() -> Actor {
        Actor {
            kind: crate::ports::types::ActorKind::User,
            id: "user-1".to_string(),
        }
    }

    fn standing(id: &str, agent: &str, tool: &str, expires_at_millis: u64) -> StandingGrant {
        StandingGrant {
            id: GrantId::new(id),
            agent: agent.to_string(),
            workflow: None,
            tool: tool.to_string(),
            granted_by: operator(),
            approval_id: ApprovalId::new(format!("approval-{id}")),
            at_millis: 1_000,
            expires_at_millis,
            origin_thread: None,
            origin_parent: None,
            origin_task: None,
            scope: None,
        }
    }

    /// A permission held by a workflow rather than a teammate (issue #1098).
    fn standing_workflow(
        id: &str,
        workflow: &str,
        tool: &str,
        scope: Option<&str>,
        expires_at_millis: u64,
    ) -> StandingGrant {
        StandingGrant {
            agent: String::new(),
            workflow: Some(workflow.to_string()),
            scope: scope.map(str::to_string),
            ..standing(id, "", tool, expires_at_millis)
        }
    }

    /// The two subject fields reconcile in exactly one place, and a line with no
    /// `workflow` is an agent permission — which is what makes every journal line
    /// written before issue #1098 replay unchanged.
    #[test]
    fn subject_reads_a_workflow_line_as_a_workflow_and_everything_else_as_an_agent() {
        assert_eq!(
            standing("g1", "maya", "web_fetch", 9_999).subject(),
            GrantSubject::agent("maya")
        );
        assert_eq!(
            standing_workflow("g2", "sports_blog", "web_fetch", None, 9_999).subject(),
            GrantSubject::workflow("sports_blog")
        );
    }

    /// A journal line written before the field existed carries no `workflow` key
    /// at all. It must deserialize, and it must replay as the agent permission it
    /// was — not as a workflow one keyed on an empty string.
    #[test]
    fn a_pre_1098_journal_line_replays_as_an_agent_permission() {
        let line = r#"{
            "id": "g-old",
            "agent": "maya",
            "tool": "web_fetch",
            "granted_by": { "kind": "user", "id": "user-1" },
            "approval_id": "approval-old",
            "at_millis": 1000,
            "expires_at_millis": 9999
        }"#;
        let replayed: StandingGrant =
            serde_json::from_str(line).expect("a pre-#1098 line still deserializes");
        assert_eq!(replayed.workflow, None);
        assert_eq!(replayed.subject(), GrantSubject::agent("maya"));

        let set = GrantSet::default();
        set.grant_standing(replayed);
        assert!(
            set.match_standing(&GrantSubject::agent("maya"), "web_fetch", None, 2_000)
                .is_some(),
            "a replayed line must still admit the calls it always admitted"
        );
    }

    /// The two subjects are separate namespaces. A workflow named like a teammate
    /// must not spend that teammate's permission, in either direction.
    #[test]
    fn an_agent_and_a_workflow_of_the_same_name_do_not_share_a_permission() {
        let set = GrantSet::default();
        set.grant_standing(standing("g1", "digest", "web_fetch", 9_999));

        assert!(
            set.match_standing(&GrantSubject::workflow("digest"), "web_fetch", None, 2_000)
                .is_none(),
            "a workflow must not spend a teammate's permission"
        );

        let set = GrantSet::default();
        set.grant_standing(standing_workflow("g2", "digest", "web_fetch", None, 9_999));
        assert!(
            set.match_standing(&GrantSubject::agent("digest"), "web_fetch", None, 2_000)
                .is_none(),
            "a teammate must not spend a workflow's permission"
        );
    }

    /// The scope machinery is subject-agnostic: a workflow permission narrows by
    /// host on exactly the terms a teammate's does.
    #[test]
    fn a_workflow_permission_is_narrowed_by_its_host_like_any_other() {
        let set = GrantSet::default();
        set.grant_standing(standing_workflow(
            "g1",
            "sports_blog",
            "web_fetch",
            Some("https://www.bbc.co.uk"),
            9_999,
        ));
        let subject = GrantSubject::workflow("sports_blog");

        assert!(
            set.match_standing(&subject, "web_fetch", Some("https://www.bbc.co.uk"), 2_000)
                .is_some(),
            "the host it was granted for keeps passing"
        );
        assert!(
            set.match_standing(&subject, "web_fetch", Some("https://www.espn.com"), 2_000)
                .is_none(),
            "a repointed host re-parks — scope equality is the invalidation"
        );
        assert!(
            set.match_standing(&subject, "web_fetch", None, 2_000)
                .is_none(),
            "a call whose host cannot be read is refused by a scoped permission"
        );
    }

    /// A grant confined to one Composio toolkit (issue #457).
    fn scoped(
        id: &str,
        agent: &str,
        tool: &str,
        scope: &str,
        expires_at_millis: u64,
    ) -> StandingGrant {
        StandingGrant {
            scope: Some(scope.to_string()),
            ..standing(id, agent, tool, expires_at_millis)
        }
    }

    /// The whole point of the scope: the tool stops asking, whatever the
    /// arguments, until the deadline.
    #[test]
    fn a_standing_grant_admits_varying_arguments_until_it_expires() {
        let set = GrantSet::default();
        set.grant_standing(standing("g1", "ops", "shell", 10_000));

        assert!(
            set.match_standing(&GrantSubject::agent("ops"), "shell", None, 2_000)
                .is_some()
        );
        // Matching does not depend on arguments at all — there are none to
        // depend on. Two different calls, same admission.
        assert!(
            set.match_standing(&GrantSubject::agent("ops"), "shell", None, 9_999)
                .is_some()
        );
        assert_eq!(
            set.standing_count(),
            1,
            "redeeming a standing grant must not remove it — that is single-use's job"
        );

        // The deadline instant itself is already past.
        assert!(
            set.match_standing(&GrantSubject::agent("ops"), "shell", None, 10_000)
                .is_none()
        );
        assert!(
            set.match_standing(&GrantSubject::agent("ops"), "shell", None, 10_001)
                .is_none()
        );
    }

    #[test]
    fn a_standing_grant_is_scoped_to_its_agent_and_its_tool() {
        let set = GrantSet::default();
        set.grant_standing(standing("g1", "ops", "shell", 10_000));

        assert!(
            set.match_standing(&GrantSubject::agent("marketing"), "shell", None, 2_000)
                .is_none(),
            "another teammate is not who the operator granted"
        );
        assert!(
            set.match_standing(&GrantSubject::agent("ops"), "workspace_write", None, 2_000)
                .is_none(),
            "another tool is not what the operator granted"
        );
    }

    /// Issue #457, the discrimination that matters. A grant minted from a
    /// GitHub read admits another GitHub read — the operator consented to a
    /// provider, not to one action — and refuses a Gmail read. Both are
    /// catalogue reads, so nothing upstream of this predicate tells them apart:
    /// same agent, same tool, same grantable verdict.
    #[test]
    fn a_scoped_grant_admits_its_own_provider_and_refuses_another() {
        let set = GrantSet::default();
        set.grant_standing(scoped("g1", "ops", "composio_execute", "github", 10_000));

        assert!(
            set.match_standing(
                &GrantSubject::agent("ops"),
                "composio_execute",
                Some("github"),
                2_000
            )
            .is_some(),
            "a second GitHub read is the sentence the operator agreed to"
        );
        assert!(
            set.match_standing(
                &GrantSubject::agent("ops"),
                "composio_execute",
                Some("gmail"),
                2_000
            )
            .is_none(),
            "'read from GitHub' is not consent to read a mailbox"
        );
    }

    /// A call the catalogue cannot place has no scope, and a scoped grant must
    /// not admit it: it could belong to any provider, and guessing would guess
    /// permissively. It falls through and parks, which is what this codebase
    /// does with every unrecognised action.
    #[test]
    fn a_scoped_grant_refuses_a_call_with_no_scope_at_all() {
        let set = GrantSet::default();
        set.grant_standing(scoped("g1", "ops", "composio_execute", "github", 10_000));

        assert!(
            set.match_standing(&GrantSubject::agent("ops"), "composio_execute", None, 2_000)
                .is_none()
        );
    }

    /// **Replay compatibility (issue #457).** A `StandingGrantMinted` line
    /// written before the scope field existed deserializes with `scope: None`,
    /// and an unscoped grant behaves exactly as it did before this change —
    /// admitting the tool whatever the live scope is. Without this the upgrade
    /// would silently void every permission an operator granted before it.
    #[test]
    fn a_grant_journaled_before_scopes_existed_replays_and_behaves_as_before() {
        // The old wire shape, verbatim: no `scope` key anywhere.
        let line = serde_json::json!({
            "id": "g-old",
            "agent": "ops",
            "tool": "composio_execute",
            "granted_by": { "kind": "user", "id": "user-1" },
            "approval_id": "approval-g-old",
            "at_millis": 1_000,
            "expires_at_millis": 10_000,
        });
        let replayed: StandingGrant =
            serde_json::from_value(line).expect("an old line still deserializes");
        assert_eq!(replayed.scope, None, "absent means unscoped, not broken");

        let set = GrantSet::default();
        set.rehydrate_standing([replayed]);

        // Unscoped: it admits the tool exactly as it did before scopes existed,
        // whatever the live call resolves to.
        for live in [Some("github"), Some("gmail"), None] {
            assert!(
                set.match_standing(&GrantSubject::agent("ops"), "composio_execute", live, 2_000)
                    .is_some(),
                "an unscoped grant must keep admitting: {live:?}"
            );
        }
        // …and the boundaries it always had still hold.
        assert!(
            set.match_standing(
                &GrantSubject::agent("marketing"),
                "composio_execute",
                Some("github"),
                2_000
            )
            .is_none()
        );
        assert!(
            set.match_standing(
                &GrantSubject::agent("ops"),
                "composio_execute",
                Some("github"),
                10_000
            )
            .is_none()
        );
    }

    /// A scoped grant round-trips, and an unscoped one still writes the old
    /// shape — so a journal read by an older build is unchanged.
    #[test]
    fn the_scope_round_trips_and_is_omitted_when_absent() {
        let unscoped = serde_json::to_value(standing("g1", "ops", "shell", 10_000)).expect("json");
        assert!(
            unscoped.get("scope").is_none(),
            "an unscoped grant writes the pre-#457 line: {unscoped}"
        );

        let grant = scoped("g2", "ops", "composio_execute", "github", 10_000);
        let round: StandingGrant =
            serde_json::from_value(serde_json::to_value(&grant).expect("json"))
                .expect("round trip");
        assert_eq!(round, grant);
    }

    #[test]
    fn revoking_a_standing_grant_stops_it_matching() {
        let set = GrantSet::default();
        set.grant_standing(standing("g1", "ops", "shell", 10_000));
        assert!(
            set.match_standing(&GrantSubject::agent("ops"), "shell", None, 2_000)
                .is_some()
        );

        let revoked = set.revoke_standing(&GrantId::new("g1")).expect("was live");
        assert_eq!(revoked.tool, "shell");
        assert!(
            set.match_standing(&GrantSubject::agent("ops"), "shell", None, 2_000)
                .is_none()
        );
        assert_eq!(set.standing_count(), 0);
        assert!(
            set.revoke_standing(&GrantId::new("g1")).is_none(),
            "revoking twice reports nothing to revoke rather than pretending"
        );
    }

    /// A single-use grant must burn even when a standing grant would also have
    /// admitted the call.
    ///
    /// The ordering is enforced by the policy arm, but the primitives have to
    /// make it expressible: `consume` is what removes, and a standing match
    /// never touches the single-use set. If a standing match ran first, the
    /// operator's one-off approval would sit live until its TTL and then be
    /// announced as "the agent never acted" — a lie about work that ran.
    #[test]
    fn a_single_use_grant_still_burns_while_a_standing_grant_is_live() {
        let set = GrantSet::default();
        let args = serde_json::json!({ "cmd": "ls" });
        set.grant(call("a1", "ops", "shell", args.clone()));
        set.grant_standing(standing("g1", "ops", "shell", 10_000));

        assert!(set.consume("ops", "shell", &args).is_some());
        assert_eq!(set.live_count(), 0, "the single-use grant burned");
        assert_eq!(set.standing_count(), 1, "the standing grant is untouched");
        assert_eq!(set.drain_consumed().len(), 1);
    }

    #[test]
    fn sweep_standing_removes_only_lapsed_grants() {
        let set = GrantSet::default();
        set.grant_standing(standing("old", "ops", "shell", 5_000));
        set.grant_standing(standing("new", "ops", "workspace_write", 50_000));

        let expired = set.sweep_standing(10_000);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].id, GrantId::new("old"));
        assert_eq!(set.standing_count(), 1);
        assert!(
            set.match_standing(&GrantSubject::agent("ops"), "workspace_write", None, 10_000)
                .is_some()
        );
    }

    #[test]
    fn the_longest_lived_match_wins_so_redemption_is_not_hash_order() {
        let set = GrantSet::default();
        set.grant_standing(standing("short", "ops", "shell", 5_000));
        set.grant_standing(standing("long", "ops", "shell", 50_000));

        let matched = set
            .match_standing(&GrantSubject::agent("ops"), "shell", None, 1_000)
            .expect("matches");
        assert_eq!(matched.id, GrantId::new("long"));
    }

    #[test]
    fn standing_grants_rehydrate_and_are_findable_by_their_approval() {
        let set = GrantSet::default();
        set.rehydrate_standing([
            standing("g1", "ops", "shell", 10_000),
            standing("g2", "legal", "workspace_write", 10_000),
        ]);
        assert_eq!(set.standing_count(), 2);

        let found = set
            .peek_standing_by_approval(&ApprovalId::new("approval-g2"))
            .expect("provenance is queryable");
        assert_eq!(found.agent, "legal");
        assert!(
            set.peek_standing_by_approval(&ApprovalId::new("nope"))
                .is_none()
        );

        // Newest first, and `at_millis` ties break on id so the list is stable.
        let listed = set.standing();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, GrantId::new("g1"));
    }
}
