//! The runtime journal: durable at-most-once effect execution and the
//! persistent approval queue.
//!
//! The journal is distinct from the [`EventLog`](crate::ports::EventLog).
//! [`CompanyEvent`](crate::ports::CompanyEvent) is a closed, binding enum with
//! no marker variants, so effect-execution and approval-parking markers cannot
//! ride the event log. They live here instead, in a per-company `journal.jsonl`
//! that boot replay reads back to rebuild in-flight state.
//!
//! Two guarantees:
//!
//! * **At-most-once effects.** Before a side effect runs, its idempotency key is
//!   committed to the journal. On recovery the committed key is skipped, so a
//!   crash after the commit but before the side effect drops the effect (at
//!   most once) rather than repeating it.
//! * **Durable approvals.** Parked effects are journaled and rehydrated on boot,
//!   so an approval survives a restart with its original [`ApprovalId`].
//!
//! Both guarantees are only as durable as what the records are written to, which
//! is why the sink is a port ([`JournalStore`], issue #726) rather than a file
//! path. On a hosted mongodb tenant the container's `/data` is ephemeral scratch,
//! so a journal pinned to the filesystem there lost every committed key and every
//! parked approval on container replacement. Everything semantic — the record
//! enum, replay, corrupt-line recovery — lives here and is backend-agnostic; the
//! store below it only keeps opaque lines in order.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex as TokioMutex;

use crate::Result;
use crate::ports::blockers::BlockerResolution;
use crate::ports::journal::{Durability, JournalStore};
use crate::ports::types::{Actor, ApprovalId, CompanyId, Effect, EventSeq, StartedBy};
use crate::runtime::grants::{ApprovalContinuation, GrantId, GrantedCall, StandingGrant};
pub use crate::runtime::types::TaskLink;
use crate::store::fs::FsJournalStore;

/// Why a parked approval was retired without an operator deciding it
/// (issue #971).
///
/// Retirement has one implementation — [`CompanyRuntime::retire_approval`] —
/// and this says which rule invoked it. Recorded rather than inferred: the
/// journal is the audit trail for a default-deny, and "the deadline passed" and
/// any future automatic retirement are different things to have happened to
/// someone's request, however identical the resulting queue looks.
///
/// The enum exists rather than a bool because the reasons keep arriving — the
/// next one already known is an approval retired because a newer identical
/// request superseded it — and a `superseded: bool` beside a `reason` would be
/// two fields describing one fact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpiryReason {
    /// It sat unresolved past its `[policy].approval_ttl_hours` deadline.
    #[default]
    Ttl,
    /// The card it was parked for could not be written, so the blocker was
    /// withdrawn rather than left pointing at a card nobody paused
    /// (issue #1861).
    ///
    /// A blocker and its card are two writes to two stores, and the planning
    /// pass parks first so the queue can never promise a release for a column
    /// that never changed. That leaves the mirror-image gap when the second
    /// write fails: a live, journaled blocker against a card still sitting in
    /// Planning — which the TTL sweep cannot repair either, because
    /// `return_expired_blocker_card` only moves cards already in `paused`. This
    /// is the compensating retirement, recorded under its own name because "we
    /// could not write the card" and "the deadline passed" are different things
    /// to have happened to an operator's queue.
    CardUnwritable,
}

/// The pre-#1862 fallback for a [`JournalRecord::BlockedNodeStashed`] line
/// written before that record carried `started_by` at all — never a live
/// choice, only what a legacy row's `#[serde(default)]` decodes to.
fn default_started_by_operator() -> StartedBy {
    StartedBy::Operator
}

/// One durable journal record.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "record")]
enum JournalRecord {
    /// A side effect committed to run under this idempotency key.
    EffectExecuted {
        /// The effect's idempotency key.
        key: String,
        /// What the key committed (issue #351).
        ///
        /// The key alone answers "has this run?" and nothing else, which is all
        /// the at-most-once guarantee needs and not nearly enough to tell an
        /// operator what a previous attempt already did. Absent on records
        /// written before #351 — those replay as an executed key with no
        /// description, exactly as they behaved before, and set
        /// [`State::undescribed_executed`] so the console can say so instead of
        /// implying the gap is an all-clear.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect: Option<ExecutedEffect>,
    },
    /// An effect parked for operator approval.
    ApprovalParked {
        /// The parked approval's id.
        id: ApprovalId,
        /// The parked effect.
        effect: Effect,
        /// Epoch-millis the effect was parked.
        at_millis: u64,
        /// Which board task this effect was parked for (issue #333).
        ///
        /// This is the correlation key that makes a task's Approvals tab
        /// possible. Before it, an approval carried nothing tying it to a card,
        /// so the only join available was "did this resolve while that task was
        /// running" — a time window, which a second task worked in the same
        /// window silently absorbs.
        ///
        /// **Always written from #333 onward**, as either
        /// [`TaskLink::Task`] or [`TaskLink::Unlinked`] — never omitted. That is
        /// the whole point of the enum over a bare `Option<String>`: "parked for
        /// no card" and "parked by a host that did not record cards" are
        /// different facts, and only the second may fall back to the run window.
        /// Collapsing them sent every workflow delivery, chat turn and scheduler
        /// tick to whatever card happened to be running.
        ///
        /// `None` therefore means exactly one thing: a journal line written
        /// before this field existed. `#[serde(default)]` is what lets those
        /// replay instead of failing to parse.
        #[serde(default)]
        task: Option<TaskLink>,
        /// Which **chat thread** produced the parking cycle (issue #379) — the
        /// desk id for a channel, the roster agent id for a direct message.
        ///
        /// The correlation key that lets an approval be raised in the
        /// conversation that asked for it. [`Effect::agent`] cannot do that job:
        /// a desk channel and a direct message to that desk's lead resolve to
        /// the same agent id, so placing the card by asker would raise a
        /// channel's request inside the lead's private DM.
        ///
        /// A plain `Option<String>` rather than a [`TaskLink`]-style enum,
        /// because nothing downstream falls back to a heuristic when it is
        /// absent: an approval with no thread matches no channel filter and
        /// stays Approvals-page-only, which is exactly today's behaviour. So
        /// "parked by a host that did not record threads" and "parked by a turn
        /// with no conversation behind it" need not be told apart — both mean
        /// "no channel owns this", and both are correct.
        ///
        /// `#[serde(default)]` is what lets a pre-#379 line replay.
        #[serde(
            default,
            deserialize_with = "crate::ports::general_channel::deserialize_general_chat_opt"
        )]
        thread: Option<String>,
        /// Which **thread within that channel** produced the parking cycle
        /// (issue #435) — the root the raising message hangs off, as that
        /// root's own [`EventSeq`].
        ///
        /// A separate field rather than a widening of `thread`, because the two
        /// answer different questions and both are needed: `thread` says which
        /// channel, this says where inside it. Overloading `thread` would have
        /// silently changed the meaning of every existing reader of it — see
        /// [`ApprovalOrigin::parent`] for the whole argument.
        ///
        /// `None` for a park raised straight into a channel rather than inside
        /// a thread, which is also every line written before this field
        /// existed. Both mean the same thing downstream and correctly so: the
        /// channel is the answer, which is exactly the pre-#435 behaviour.
        ///
        /// `#[serde(default)]` is what lets a pre-#435 line replay.
        #[serde(default)]
        parent: Option<EventSeq>,
        /// Which **cycle** parked it (issue #469) — the turn key.
        ///
        /// The three keys above all answer "what is this approval about". This
        /// one answers "what is waiting on it", and only it can: a single turn
        /// can park several calls, and each of the others is either shared by
        /// turns that are not blocked on each other (a thread hosts many turns)
        /// or absent for the case that matters most (a chat turn has no card and
        /// no run).
        ///
        /// Without it, resolving four sign-offs from one turn re-ran that turn
        /// four times, because nothing could say the four belonged together.
        /// With it, the runtime holds the continuation until the last of a
        /// turn's approvals is decided and then runs it once — see
        /// [`ContinuationQueue`](crate::runtime::continuation::ContinuationQueue).
        ///
        /// `None` means a line written before this field existed, and falls back
        /// to the pre-#469 behaviour of continuing that approval on its own.
        /// `#[serde(default)]` is what lets those lines replay.
        #[serde(default)]
        cycle: Option<String>,
    },
    /// A parked approval that has since been resolved (approved or denied).
    ApprovalResolved {
        /// The resolved approval's id.
        id: ApprovalId,
    },
    /// A parked approval that expired to a default-deny with no operator action.
    ApprovalExpired {
        /// The expired approval's id.
        id: ApprovalId,
        /// Epoch-millis the expiry was recorded.
        at_millis: u64,
        /// Why it was retired (issue #971).
        ///
        /// `#[serde(default)]` is what lets every line written before this
        /// field existed replay: they were all TTL expiries, which is exactly
        /// what [`ExpiryReason::Ttl`] means, so the default is the truth about
        /// them rather than a placeholder.
        #[serde(default)]
        reason: ExpiryReason,
    },
    /// An operator pushed a parked approval's deadline out to a fresh full TTL
    /// window (issue #1805).
    ///
    /// The durable half of the extend lever. It carries the new anchor rather
    /// than an offset, because the anchor is exactly what the sweeper and the
    /// projected deadline both read (`ParkedApproval::deadline_anchor_millis`),
    /// so replay re-applies the move by rehydrating the gate from it — an
    /// extension survives a redeploy instead of reverting to the original park
    /// instant. Deliberately separate from `ApprovalParked::at_millis`, which
    /// dates the PAYLOAD (issue #1024) and must not shift when a deadline does.
    ApprovalExtended {
        /// The extended approval's id.
        id: ApprovalId,
        /// Epoch-millis the TTL window was re-anchored to (the extension time).
        at_millis: u64,
        /// Who extended it.
        by: Actor,
    },
    /// A parked approval the operator approved with an amended effect payload.
    ///
    /// Audit-only: the queue removal is recorded by the paired
    /// [`ApprovalResolved`](JournalRecord::ApprovalResolved). The original
    /// effect stays recoverable from the earlier
    /// [`ApprovalParked`](JournalRecord::ApprovalParked), so the immutable log
    /// shows both what was requested and what the operator approved.
    ApprovalAmended {
        /// The amended approval's id.
        id: ApprovalId,
        /// The operator-amended effect that was executed.
        amended_effect: Effect,
        /// Epoch-millis the amendment was recorded.
        at_millis: u64,
    },
    /// A single-use grant minted because the operator approved a tool call an
    /// agent had been blocked from making (issue #243).
    ///
    /// This is the durable audit line for "the operator said yes to *this*
    /// call": it carries the agent, the tool, and the exact arguments admitted,
    /// which is more than the event log's
    /// [`ApprovalResolved`](crate::ports::CompanyEvent::ApprovalResolved) can
    /// hold. Written *before* the grant reaches the live set, so a crash between
    /// the two re-arms it on replay rather than losing the operator's decision.
    ApprovalGranted {
        /// The grant, whole.
        grant: GrantedCall,
    },
    /// A single-use grant committed to one follow-up turn.
    /// Written before that turn starts so recovery cannot re-arm authority
    /// whose tool call may already have run.
    GrantDispatched {
        /// The grant committed to the turn.
        id: ApprovalId,
        /// Epoch-millis the dispatch was committed.
        at_millis: u64,
    },
    /// A follow-up turn owed after an agent explicitly asked the operator a
    /// question. Unlike `ApprovalGranted`, this carries either verdict and
    /// conveys no authority to execute a tool call.
    ApprovalContinuationQueued {
        /// The verdict and routing context, whole.
        continuation: ApprovalContinuation,
    },
    /// An explicit decision follow-up is committed to one dispatch attempt.
    /// Written before the agent turn starts so recovery never repeats external
    /// actions from a continuation that may already have partially run.
    ApprovalContinuationDispatched {
        /// The approval whose follow-up was claimed.
        id: ApprovalId,
        /// Epoch-millis the dispatch was committed.
        at_millis: u64,
    },
    /// An explicit approval continuation was delivered to its requesting agent.
    ApprovalContinuationConsumed {
        /// The approval whose follow-up completed.
        id: ApprovalId,
    },
    /// An explicit approval continuation expired before it could be delivered.
    ApprovalContinuationExpired {
        /// The approval whose follow-up expired.
        id: ApprovalId,
        /// Epoch-millis the expiry was recorded.
        at_millis: u64,
    },
    /// An operator's answer to a parked blocker, banked before the detached
    /// resume spawns so a restart mid-resume replays it (issue #1863). It
    /// conveys no authority to execute anything — a blocker's effect is inert —
    /// only which of the four things the operator asked for, and their words.
    BlockerResolved {
        /// The blocker approval the answer settles.
        id: ApprovalId,
        /// The verdict and answer, whole.
        resolution: BlockerResolution,
    },
    /// A parked blocker's answer was re-entered into the stopped step, so it no
    /// longer needs re-arming on the next boot.
    BlockerResumed {
        /// The blocker approval whose answer was consumed by the resume.
        id: ApprovalId,
    },
    /// A grant redeemed by its agent — the tool ran.
    GrantConsumed {
        /// The consumed grant's approval id.
        id: ApprovalId,
        /// What the redeemed grant actually did (issue #351).
        ///
        /// An approved *agent tool call* never reaches
        /// [`EffectExecuted`](Self::EffectExecuted): it is settled by minting a
        /// grant, and the tool then runs inside the agent's next turn. This
        /// record is therefore the only line in the journal that means "an
        /// operator-approved `composio_execute` payment fired", and without a
        /// description on it the retry dialog would open naming the native
        /// email beside it and nothing else — a confirmation understating what
        /// already happened.
        ///
        /// Absent on records written before this field existed; those replay as
        /// a consumed grant with no description, the same additive contract
        /// [`EffectExecuted`](Self::EffectExecuted) has.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect: Option<ExecutedEffect>,
    },
    /// A grant that expired unredeemed past [`GRANT_TTL_MILLIS`](crate::runtime::grants::GRANT_TTL_MILLIS).
    GrantExpired {
        /// The expired grant's approval id.
        id: ApprovalId,
        /// Epoch-millis the expiry was recorded.
        at_millis: u64,
    },
    /// A **standing** grant minted because the operator chose the broader scope
    /// on an approval: this tool, for this teammate, until a deadline (#374).
    ///
    /// Carries the grant whole, like
    /// [`ApprovalGranted`](Self::ApprovalGranted), because this line is the only
    /// durable answer to "who opened this tool up, when, off which card, and
    /// until when". `StandingGrant::granted_by` is the operator's real identity,
    /// not the placeholder the resolve route used to hardcode.
    ///
    /// Written *before* the grant reaches the live set, the same crash direction
    /// `ApprovalGranted` takes.
    StandingGrantMinted {
        /// The standing grant, whole.
        grant: StandingGrant,
    },
    /// A standing grant the operator took back (#374).
    ///
    /// Takes effect on the **next** policy check — an already-admitted call is
    /// not aborted, because there is no abort lever inside an agent's turn and
    /// killing one mid-call is the lifecycle anti-pattern this codebase avoids
    /// elsewhere. The next check finds nothing and re-parks.
    StandingGrantRevoked {
        /// The revoked grant's id.
        id: GrantId,
        /// Who revoked it.
        by: Actor,
        /// Epoch-millis the revocation was recorded.
        at_millis: u64,
    },
    /// A standing grant that reached its deadline (#374).
    StandingGrantExpired {
        /// The expired grant's id.
        id: GrantId,
        /// Epoch-millis the expiry was recorded.
        at_millis: u64,
    },
    /// A cycle began (issue #390).
    ///
    /// Written **before the per-company serial lock is taken**, which is the
    /// whole point of the record — see
    /// [`open_cycles`](RuntimeJournal::open_cycles) for why after the lock would
    /// miss the case this exists for.
    CycleStarted {
        /// The cycle's id — the same `cycle_id`
        /// [`ApprovalParked::cycle`](JournalRecord::ApprovalParked) already
        /// correlates approvals on. No second identifier is introduced, for the
        /// reason `run_supervisor` gives for reusing `run_id`.
        cycle_id: String,
        /// Epoch-millis the cycle started.
        at_millis: u64,
        /// A short, stable label for what kicked the cycle off, so an operator
        /// reading an open bracket can tell a stuck approval continuation from
        /// a stuck chat turn without joining anything.
        trigger: String,
    },
    /// A cycle ended, for any reason (issue #390).
    CycleFinished {
        /// The cycle this closes.
        cycle_id: String,
        /// Epoch-millis the cycle ended.
        at_millis: u64,
        /// `None` when the cycle completed; the failure otherwise.
        ///
        /// A cycle that returned `Err` and one the host never finished are both
        /// failures an operator may need to retry, but they are different facts
        /// and the read side must not merge them — a boot sweep writes
        /// [`INTERRUPTED_BY_HOST_RESTART`] here, and a real failure writes what
        /// actually went wrong.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The two facts a blocked agent node's continuation needs — its workflow id
    /// and the paused run's trigger input — stashed durably at park time so an
    /// approval can re-dispatch the run **after a restart** (issue #1816,
    /// Stage 2).
    ///
    /// The runtime's in-memory
    /// [`BlockedNodeQueue`](crate::runtime::blocked_nodes::BlockedNodeQueue) is
    /// the fast path; this record is what the builder re-arms it from at boot
    /// (see [`blocked_stashes`](RuntimeJournal::blocked_stashes)). Written from
    /// the one place — the runner's block-settle — that holds the workflow id,
    /// the trigger input and the blocked-node list together, exactly where the
    /// in-memory stash is armed. The parked tool-call effect itself carries no
    /// workflow lineage, which is why this is a dedicated record rather than a
    /// widening of [`ApprovalParked`](Self::ApprovalParked).
    BlockedNodeStashed {
        /// The per-(run, node) turn key its parked calls also armed the
        /// [`ContinuationQueue`](crate::runtime::continuation::ContinuationQueue)
        /// under, so a released batch and this stash name the same block.
        turn: String,
        /// The workflow whose run blocked, to load the graph for the re-run.
        workflow_id: String,
        /// The paused run's own trigger input, replayed unchanged — the grant the
        /// approve minted is what lets the identical gated call pass on the re-run.
        input: Value,
        /// The blocked run's own attribution (issue #1862 prerequisite), carried
        /// so a restart between park and approve rehydrates the real trigger
        /// instead of degrading every stash to [`StartedBy::Operator`] — see
        /// [`BlockedNodeQueue::rearm`](crate::runtime::blocked_nodes::BlockedNodeQueue::rearm).
        /// `#[serde(default)]` so a record written before this field existed
        /// still replays: it degrades to `Operator`, the same fallback the
        /// pre-#1862 code path always used.
        #[serde(default = "default_started_by_operator")]
        started_by: StartedBy,
        /// The tinyflows checkpoint lineage this block resumes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        /// The graph's [`content_fingerprint`](crate::company::WorkflowFile::content_fingerprint)
        /// at park time, the blocked-node counterpart to a parked gate's
        /// `PAYLOAD_WORKFLOW_FINGERPRINT` — so a restart rehydrates the same
        /// refusal `spawn_blocked_node_continuation` applies to the in-memory
        /// stash when the graph was edited while this block sat pending.
        /// `#[serde(default)]` so a record written before this field existed
        /// still replays: it degrades to `None`, which
        /// `graph_unchanged_since_park`-style checks already treat as "nothing
        /// to compare against".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_fingerprint: Option<String>,
        /// Epoch-millis the block was stashed.
        at_millis: u64,
    },
    /// A blocked-node stash whose run has been re-dispatched (or whose block was
    /// wholly refused): the paired terminator for
    /// [`BlockedNodeStashed`](Self::BlockedNodeStashed), so a resolved block does
    /// not rehydrate a duplicate continuation on the next boot (issue #1816).
    BlockedNodeReleased {
        /// The turn key whose stash this drops.
        turn: String,
    },
    /// At least one of a blocked agent node's parked calls has been approved,
    /// banked durably the moment that decision lands (issue #1816).
    ///
    /// [`ContinuationQueue`](crate::runtime::continuation::ContinuationQueue)'s
    /// batch only carries the verdicts a single process happened to hold in
    /// memory when the turn's last decision released it — a restart between
    /// two decisions on the same node drops the earlier ones from that batch
    /// exactly as [`WorkflowGateQueue`](crate::runtime::workflow_gates::WorkflowGateQueue)'s
    /// own docs describe. That queue can afford the loss: the workflow graph
    /// replay re-parks whatever the batch forgot, so the operator is asked
    /// again rather than never. A blocked agent node has no such re-park —
    /// [`resume_blocked_agent_node`](crate::company::runtime::CompanyRuntime::resume_blocked_agent_node)
    /// either spawns the continuation or does not, once — so losing an earlier
    /// approval from the batch would silently strand a grant the operator
    /// already minted: an approved tool call that runs to redeem it never
    /// executes and nothing re-asks. This record is the fact the batch cannot
    /// carry, kept durable on its own so a restart mid-decision cannot erase
    /// it. Written from the same place [`ContinuationQueue::decide`] is told
    /// about the verdict, not deferred to release time, so it survives a
    /// restart that lands on any decision but the last.
    BlockedNodeApproved {
        /// The turn key whose node had at least one call approved.
        turn: String,
    },
    /// A blocked agent node's continuation is about to be launched (issue
    /// #1825), written by
    /// [`spawn_blocked_node_continuation`](crate::runtime::workflow_resume::spawn_blocked_node_continuation)
    /// itself, immediately after the run is admitted
    /// ([`RunSupervisor::begin`](crate::runtime::RunSupervisor::begin)) and
    /// immediately before its detached task is actually launched — and,
    /// either way, well before
    /// [`resume_blocked_agent_node`](crate::company::runtime::CompanyRuntime::resume_blocked_agent_node)
    /// retires the stash via [`BlockedNodeReleased`](Self::BlockedNodeReleased).
    ///
    /// That retirement is the pair of facts (`blocked_stashes` and
    /// `blocked_node_approvals`) that would otherwise tell a restart "this
    /// stash is ready to dispatch" — exactly as true after a real dispatch
    /// whose release-write failed as it is before any dispatch at all. Without
    /// a marker recorded on the success side of admission,
    /// `reconcile_stranded_blocked_nodes` cannot distinguish the two and would
    /// re-spawn a continuation that already ran, potentially repeating token
    /// spend or unprotected upstream work a second time. This record is that
    /// marker.
    ///
    /// **Ordering, precisely.** The write sits between admission and launch
    /// rather than after the whole spawn call returns (as the first cut of
    /// this fix had it) because the launched task is detached — its caller
    /// never awaits it — so a marker written only after the *call* returns
    /// races the entire run, however long it takes, not a moment's gap. Between
    /// admission and launch there is no further `.await`, so the crash window
    /// this leaves is the width of this write's own append landing, nothing
    /// more: a crash there can still leave the two out of sync (a marker with
    /// nothing yet launched to justify it, which strands the turn rather than
    /// duplicating it — the opposite, and cheaper, failure), but a crash
    /// after the write cannot land inside a run this record does not already
    /// know about.
    BlockedNodeDispatched {
        /// The turn key whose node's continuation has been spawned.
        turn: String,
    },
}

impl JournalRecord {
    /// Which failure this record must survive.
    ///
    /// Host durability is bought for exactly the records whose loss would make
    /// the runtime **repeat an external action**, and for nothing else. The
    /// frequency asymmetry is the whole argument, and it runs the helpful way:
    /// the dangerous records are rare and the frequent records are harmless.
    /// [`EffectExecuted`](Self::EffectExecuted) is written at human-approval
    /// scale, immediately in front of a network call that costs 100ms-2s, so a
    /// flush ahead of it is invisible; [`CycleStarted`](Self::CycleStarted) is
    /// written on the front edge of *every* cycle, before the per-company serial
    /// lock, and losing it costs an observability bracket. A blanket flush would
    /// tax the hottest cosmetic record in the journal to protect the rarest
    /// dangerous one.
    ///
    /// The match is **wildcard-free on purpose, and must stay that way**: a new
    /// record kind is a compile error until its author has decided which failure
    /// it must survive. That decision — not the two flushes — is what #392
    /// delivers. Same reasoning as [`TaskLink`] being an enum rather than a bare
    /// `Option`: the type refuses to let a decision be skipped by default.
    fn durability(&self) -> Durability {
        match self {
            // Written *before* the side effect runs (`execute_effect_once`), so
            // losing it makes the next boot re-fire the effect mechanically —
            // the single duplication this journal exists to prevent.
            Self::EffectExecuted { .. } => Durability::Host,
            // Losing it silently un-revokes: the grant replays live on the next
            // boot and keeps admitting calls until its own deadline, undoing an
            // operator's withdrawal of authority. An operator action, so rare
            // enough that the flush costs nothing measurable.
            Self::StandingGrantRevoked { .. } => Durability::Host,
            // Losing it re-arms a grant whose tool already ran. Replay keeps the
            // `ApprovalGranted` that minted it and drops the redemption, so the
            // grant returns to the live set and `GrantSet::consume` will admit
            // the identical call again — no card, no operator, until the grant's
            // own TTL. That is a repeated external action under an authority the
            // operator spent once, which is the criterion above.
            //
            // The flush does not close the window on its own, and is not claimed
            // to: redemption happens inside a sync `ToolPolicy::check` with no
            // journal handle, so the id is buffered and written at cycle end
            // (`CompanyCycle::run`), and a crash inside *that* gap loses the
            // record before any append is reached. Flushing removes the part
            // this file controls — the record that was written but only
            // page-cached. Narrowing a duplication window is worth one flush on
            // a record written at operator-decision scale; the batching is the
            // remaining half and is not this issue's to close.
            Self::GrantConsumed { .. } => Durability::Host,
            // Losing a park loses the *question*: the approval vanishes and the
            // agent parks it again on its next attempt. Nothing external fired.
            //
            // **Except for a workflow gate, which has no next attempt (issue
            // #1145).** The re-park reasoning above is a property of the
            // *caller*, not of the record, and it was generalised to a caller
            // that has none. A chat turn re-enters its gate and mints a new
            // approval, so the cost of the loss is one extra question — the
            // tolerance is exactly right there, and that is where the volume is.
            // A workflow run does not: `workflow_resume` turns on the fact that
            // "resume is a re-run, because a paused run is settled" — the engine
            // returned, the future completed, and nothing is holding a
            // continuation. So the parked effect is not a record *of* the
            // continuation, it **is** the continuation, carrying the whole
            // trigger input, and `WorkflowGateQueue::rearm` rebuilds the live
            // gate set at recovery from exactly these still-parked lines. Lose
            // the line and the run keeps a durable `pending_approvals` naming a
            // question that exists nowhere: no card to decide, no re-park
            // coming, and the whole downstream of that pipeline held behind it.
            //
            // Not a new guarantee so much as the one this crate already claims
            // in two other places and did not deliver — `workflow_resume`'s
            // "restart durability … a host that dies between the park and the
            // approval loses nothing", and `CompanyCycle::park`'s "survives a
            // restart with its original `ApprovalId`". Both hold for a *process*
            // restart and fail for the host death the first one names, because
            // `Process` is page-cache-resident by definition. The flush is the
            // same trade `GrantConsumed` accepted four arms up: one flush on a
            // record written at operator-decision scale.
            //
            // The card for an agent node's gated tool call used to stay
            // `Process` here on the theory that the continuation it strands is
            // durable a different way (issue #1816): the two facts the
            // continuation needs are written at park time as a dedicated
            // host-durable `BlockedNodeStashed` record and re-armed into
            // `BlockedNodeQueue` at boot, so a restart between park and approve
            // re-dispatches the run from that record — once the operator has
            // decided.
            //
            // "Once the operator has decided" is exactly what a lost card takes
            // away, and nothing gives it back. `BlockedNodeQueue::rearm`
            // restores the stash, but a stash with no matching card is not a
            // pending decision: it is invisible to the operator (`pending()`
            // and `parked_turns()` both replay from this same record, so a
            // lost line is a lost row on both) and invisible to
            // `reconcile_stranded_blocked_nodes`, which only resumes a turn
            // already durably marked in `blocked_node_approvals` — a set this
            // park's own loss keeps empty, because nobody ever got the chance
            // to approve it. A restart between the park and the decision does
            // not strand the *continuation* (#1816 covers that); it strands the
            // *question*, permanently, with no re-park coming — the identical
            // failure the workflow-gate arm below exists to close, one caller
            // down. So this park needs that arm's durability for that arm's
            // reason, bought the same way: human-approval scale, one flush per
            // blocked node.
            //
            // Keyed on `run_id.is_some()` rather than the effect kind, because
            // a blocked-node park's kind is the tool name itself and varies per
            // call — there is no fixed tag to match the way
            // `WORKFLOW_APPROVE_KIND` is one. `run_id` already carries the
            // distinction that matters: `ApprovalRequestQueue::stamp_run` stamps
            // it with the task-attempt id at the dispatch boundary for exactly a
            // workflow node's own gated call, and deliberately leaves a chat
            // turn's own park — which DOES re-park on its next attempt — at
            // `None`. `gate_effect` stamps the workflow gate's own park with a
            // run id too, so `is_some()` alone already covers it; the explicit
            // kind check is kept so that arm's own reasoning stays legible
            // without depending on this one.
            Self::ApprovalParked { effect, .. }
                if effect.kind == crate::runtime::WORKFLOW_APPROVE_KIND
                    || effect.run_id.is_some() =>
            {
                Durability::Host
            }
            Self::ApprovalParked { .. } => Durability::Process,
            // Bookkeeping after the decision. A ghost approval that is approved
            // a second time cannot duplicate the effect, because the effect's
            // own commit is host-durable and `is_executed` skips it.
            Self::ApprovalResolved { .. } => Durability::Process,
            // Recomputed: the parked record carries the deadline, so replay
            // re-expires it.
            Self::ApprovalExpired { .. } => Durability::Process,
            // An operator's extension (issue #1805). `Process`, like the park it
            // moves: losing it on host death reverts the deadline to the original
            // window — the approval simply expires on its first schedule, the same
            // "one default-deny sooner" tolerance a lost park has. A redeploy
            // (process restart) keeps the page-cached record and replays the move,
            // which is the case the lever has to survive.
            Self::ApprovalExtended { .. } => Durability::Process,
            // Audit-only. The queue removal rides on the paired
            // `ApprovalResolved`, and the original effect stays recoverable from
            // the earlier `ApprovalParked`.
            Self::ApprovalAmended { .. } => Durability::Process,
            // Written *before* the grant goes live, so losing it forgets a YES:
            // the agent is blocked again and the operator is re-asked. The safe
            // direction — the cost of the loss is an extra question, never an
            // extra call.
            Self::ApprovalGranted { .. } => Durability::Process,
            Self::GrantDispatched { .. } => Durability::Host,
            // Conversation continuations carry no execution authority. Losing
            // a queued one means the agent misses a verdict; losing a terminal
            // line can repeat a model follow-up, but cannot repeat an effect.
            // Losing the dispatch claim can replay an entire model turn whose
            // earlier tool call already left the company. Host durability buys
            // at-most-once dispatch; a crash after the claim but before the turn
            // takes the safe at-most-once direction and may drop the follow-up.
            // Losing the queue record after `ApprovalResolved` survived leaves
            // a decided request with no card and no follow-up to recover.
            Self::ApprovalContinuationQueued { .. }
            | Self::ApprovalContinuationDispatched { .. } => Durability::Host,
            Self::ApprovalContinuationConsumed { .. }
            | Self::ApprovalContinuationExpired { .. } => Durability::Process,
            // The same direction as `ApprovalContinuationQueued`: losing a
            // blocker's answer after `ApprovalResolved` survived would leave a
            // decided blocker with nothing to re-enter the stopped step. The
            // paired `BlockerResumed` only clears a re-armed answer, so a lost
            // clear replays as "still armed" — re-resuming, the safe direction.
            Self::BlockerResolved { .. } => Durability::Host,
            Self::BlockerResumed { .. } => Durability::Process,
            // The same direction as `ApprovalGranted`, one scope wider.
            Self::StandingGrantMinted { .. } => Durability::Process,
            // Deadline arithmetic rather than state: `replayed_standing_grants`
            // takes `now_millis` and re-expires anything past its deadline on
            // the next boot regardless of whether the record survived.
            Self::GrantExpired { .. } => Durability::Process,
            Self::StandingGrantExpired { .. } => Durability::Process,
            // Observability brackets, and the highest-volume records here.
            // Losing either half reads as an interrupted cycle — which, after a
            // host crash, it was.
            Self::CycleStarted { .. } => Durability::Process,
            Self::CycleFinished { .. } => Durability::Process,
            // Issue #1816: the whole point of the record is to outlive the
            // process — and, on a hosted tenant whose journal store is the
            // shared database, the container. `Process` would leave it
            // page-cache-resident and lost with the pod, which is precisely the
            // failure that stranded parked tasks on the ~90-min staging cron.
            // Written at human-approval scale (one per blocked node), so the
            // flush is invisible — the same trade the workflow-gate park makes
            // for its own continuation facts one arm up.
            Self::BlockedNodeStashed { .. } => Durability::Host,
            // The terminator must be at least as durable as the record it
            // retires: if the stash survived a crash but its release did not,
            // the next boot would rehydrate a stash whose run already
            // re-dispatched and could double-spawn under a boot sweep. Host, to
            // match `BlockedNodeStashed`.
            Self::BlockedNodeReleased { .. } => Durability::Host,
            // Protects against exactly the failure `BlockedNodeStashed` does —
            // the fact this exists to carry is only needed across the same
            // restart window, and a `Process`-tier write could be lost to the
            // same pod-roll that motivated the stash's own Host tier, silently
            // reopening the gap this record closes.
            Self::BlockedNodeApproved { .. } => Durability::Host,
            // Issue #1825: the same tier as `BlockedNodeStashed` for the same
            // reason — this is the fact that makes a `BlockedNodeReleased`
            // write failure safe rather than a double-dispatch, so a
            // `Process`-tier write that a pod-roll could still drop would
            // reopen exactly the gap it exists to close.
            Self::BlockedNodeDispatched { .. } => Durability::Host,
        }
    }
}

/// A parked approval awaiting resolution.
#[derive(Clone, Debug)]
pub struct PendingApproval {
    /// The approval's id.
    pub id: ApprovalId,
    /// The parked effect.
    pub effect: Effect,
    /// Epoch-millis the effect was parked.
    pub at_millis: u64,
    /// Epoch-millis this approval's deadline is measured from (issue #1805) —
    /// `at_millis` for a fresh park, the extension time once an operator has
    /// extended it. The projected `expires_at_millis` is this plus the gate's
    /// TTL, so a card's deadline reflects an extension. Distinct from
    /// `at_millis` (payload age, issue #1024) on purpose.
    pub deadline_anchor_millis: u64,
    /// Which board task this approval was parked for (issue #333). `None` only
    /// for a journal line written before the link existed — see [`TaskLink`].
    pub task: Option<TaskLink>,
    /// The chat thread that produced the parking cycle (issue #379) — a desk id
    /// for a channel, a roster agent id for a direct message.
    ///
    /// `None` for a pre-#379 journal line *and* for every park with no
    /// conversation behind it (a workflow delivery, a scheduler tick, a cycle
    /// whose triggers were ambiguous). Both are the same fact downstream: no
    /// channel owns this approval, so it is shown on the Approvals page only.
    pub thread: Option<String>,
    /// The turn that parked it (issue #469), carried out to the read side by
    /// issue #842 so the console can ask about a turn's gated calls **once**.
    ///
    /// Not a new fact and deliberately not a new record: `ApprovalParked`
    /// already journals the parking cycle, because #469 needed to know which
    /// approvals one turn is blocked on in order to continue it exactly once.
    /// #842 is the same grouping seen from the operator's side — a turn that
    /// reached three sites parked three calls, and being asked three times is
    /// the same fact told badly. Projecting the key it already had is the whole
    /// of the mechanism; each park stays its own record, its own decision and
    /// its own host-scoped grant.
    ///
    /// `None` for a pre-#469 journal line and for every park raised outside a
    /// cycle (a workflow node, a scheduler tick): `park_and_journal` in
    /// `workflows::delivery` passes no turn key, because a run holds no
    /// continuation for one to belong to. Both read downstream as "belongs to
    /// no batch", which renders exactly as it did before this field existed:
    /// one card, decided on its own.
    pub batch: Option<String>,
}

/// What an approval *was*, retained for the whole life of the journal — after
/// it resolves, expires, or is amended away (issue #333, over #305's index).
///
/// The parked effect itself is dropped from the queue on resolution, and
/// [`CompanyEvent::ApprovalResolved`](crate::ports::CompanyEvent::ApprovalResolved)
/// carries only an id, a verdict and an actor. So without this index a resolved
/// approval is unreadable: the read side cannot say what was approved, when it
/// parked, or which task it belonged to.
///
/// **Entries are never removed, and the map is unbounded.** It has the same
/// append-only lifetime as the journal file it is replayed from: one resident
/// entry per approval ever parked, for the life of the process, growing
/// without a ceiling. #333 widens each entry from a `u64` to a `u64` plus two
/// `String`s (the effect kind and, when linked, the task id). No rotation
/// exists today, so `load` rebuilding this from every `ApprovalParked` line is
/// the only path — and it is the correct one. If journal rotation ever lands,
/// this index is the first thing that has to survive it, because a rotated-away
/// park line silently turns its approval unreadable.
#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalOrigin {
    /// Epoch-millis the effect was parked.
    pub at_millis: u64,
    /// The parked effect's dotted kind, e.g. `payment.send`.
    pub kind: String,
    /// Which board task the parking cycle was dispatched for. `None` only for a
    /// pre-#333 journal line — see [`TaskLink`].
    ///
    /// The **card-level** key, and the fallback one: it cannot say which of a
    /// card's attempts parked the approval. See [`run_id`](Self::run_id).
    pub task: Option<TaskLink>,
    /// The attempt this approval was parked under
    /// ([`Effect::run_id`](crate::ports::types::Effect::run_id), issue #242),
    /// copied off the effect at park time so the read side need not re-open it.
    ///
    /// The **attempt-level** key, and the authoritative one where present: a
    /// [`RunRecord`](crate::ports::runs::RunRecord) names its card, so a run id
    /// resolves to a task, while a task id can never resolve to a run. #183
    /// settled that repeat trips through review are normal, so two attempts on
    /// one card is the expected case — and only this key tells them apart.
    ///
    /// `None` by design for every park with no attempt behind it: a chat turn,
    /// a workflow delivery, a scheduler tick, and the hosted brain's own gate.
    /// That is why it cannot be the only key — see [`task`](Self::task).
    pub run_id: Option<String>,
    /// The chat thread the parking cycle answered (issue #379).
    ///
    /// The **conversation-level** key, and orthogonal to the two above: a chat
    /// turn has a thread and no card, a dispatched card has a card and no
    /// thread, and a desk turn triggered from a channel has both. Retained here
    /// (not only on the live queue) so a *resolved* approval's origin thread is
    /// still recoverable — which is what lets a follow-up cycle's own re-park
    /// stay in the channel the first sign-off was asked in.
    pub thread: Option<String>,
    /// The **thread within** that conversation the parking cycle answered
    /// (issue #435): the root message the raising message hangs off, as that
    /// root's own [`EventSeq`].
    ///
    /// Strictly finer-grained than [`thread`](Self::thread), never a substitute
    /// for it. `thread` names the channel a continuation is delivered to; this
    /// names where inside that channel it is threaded. A continuation needs
    /// both, and a `parent` without a `thread` is meaningless — a sequence
    /// number with no channel to resolve it against.
    ///
    /// **Why not widen `thread`.** `thread` is misleadingly named: it has
    /// always held a *channel* id (a desk id, or a roster agent id for a DM),
    /// and every reader of it — the approvals feed's channel filter, the
    /// continuation's `chat_id`, the grant's routing — depends on that. Making
    /// it mean "thread" would have changed all of them at once, silently, and
    /// the compiler could not have caught a single one because the type is
    /// unchanged. A new field makes the addition additive by construction: the
    /// no-thread path is not merely preserved, it is untouched.
    ///
    /// **The root, not the raising message.** The console folds a transcript
    /// one level deep — a reply whose parent is itself a reply renders nowhere
    /// (`buildTimeline` in `frontend/src/views/chat/model.ts`, pinned by the
    /// timeline unit test "renders a grandchild nowhere: the fold is exactly
    /// one level deep" in `frontend/test/unit/chat-timeline.test.ts`). That
    /// test exists for this decision: without it, growing a second fold level
    /// in the console would make the choice below unnecessary and nothing would
    /// say so — the routing would survive as an unexplained convention.
    ///
    /// So a continuation parented to the raising *message* would vanish
    /// precisely when that message is itself a thread reply, which is the case
    /// this issue exists to fix. Parenting to the root is also what the chat
    /// route already does for an ordinary answer — "the answer joins the thread
    /// its question was asked in, rather than opening one under the question"
    /// (issue #364, `crate::server::operator`) — so this is that established
    /// rule applied to the continuation, not a second convention. It is stable
    /// under an edit of the raising message for the same reason.
    ///
    /// `None` for a park with no thread behind it — a message posted straight
    /// into a channel, a workflow delivery, a scheduler tick — and for every
    /// line written before this field existed. All of them mean "the channel is
    /// the answer", which is the pre-#435 behaviour, unchanged.
    pub parent: Option<EventSeq>,
    /// The **turn** that parked it: the id of the parking cycle (issue #469).
    ///
    /// The key that groups the several approvals one turn can raise, so the
    /// turn is continued once — after the last of them is decided — instead of
    /// once per decision. `None` for a pre-#469 journal line, which continues
    /// on its own exactly as it used to.
    pub cycle: Option<String>,
}

/// Where an approval was raised: the channel, and the thread inside it
/// (issue #435).
///
/// The pair a continuation needs in order to land back where it was asked for.
/// Returned as one value by
/// [`approval_conversation`](Journal::approval_conversation) so the two can
/// never be read from different approvals; see that method for why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApprovalConversation {
    /// The channel — see [`ApprovalOrigin::thread`], whose name this mirrors
    /// and whose channel-not-thread meaning it keeps.
    pub thread: Option<String>,
    /// The thread root within that channel — see [`ApprovalOrigin::parent`].
    ///
    /// Only meaningful alongside `thread`. A `parent` with no `thread` cannot
    /// arise from a park — both are stamped from one cycle, and a cycle with no
    /// channel has no thread either — and would not be resolvable if it did.
    pub parent: Option<EventSeq>,
}

/// One approval currently waiting in the in-memory queue.
#[derive(Clone, Debug)]
struct ParkedApproval {
    effect: Effect,
    at_millis: u64,
    /// Epoch-millis this approval's TTL window is measured from (issue #1805).
    ///
    /// Starts equal to `at_millis` — a fresh park's deadline is `at_millis +
    /// ttl` — and moves to the extension time when an operator extends it. Held
    /// separately from `at_millis` because that one dates the PAYLOAD (issue
    /// #1024) and a deadline extension must not make the content look fresher.
    /// This is the anchor the gate is rehydrated from at boot, so both the live
    /// sweeper and the projected deadline stay in step across a redeploy.
    deadline_anchor_millis: u64,
    /// `None` only for a journal line written before #333.
    task: Option<TaskLink>,
    /// The chat thread that parked it (issue #379); `None` when no conversation
    /// produced it, or on a pre-#379 line.
    thread: Option<String>,
    /// The turn that parked it (issue #469); `None` on a pre-#469 line. Held on
    /// the live entry, not only in `origins`, because recovery has to re-arm the
    /// continuation queue from exactly the approvals that are *still* waiting.
    cycle: Option<String>,
}

/// A side effect that was **committed to run** (issue #351): what it was, which
/// board task it was run for, and whether it is one that cannot be taken back.
///
/// "Committed", not "completed", and the distinction is deliberate. The record
/// is written *before* the side effect is performed — that ordering is what
/// makes effects at-most-once — and a failed or interrupted perform leaves it
/// standing. So an entry means "this was committed, and the runtime will never
/// run it again", which is exactly the fact a retry warning needs: the operator
/// has to assume it happened, because nothing else will ever finish it and
/// nothing will re-attempt it. It does **not** mean the effect is known to have
/// completed. Operator-facing wording is qualified to match
/// (`RetryButton`, `frontend/src/views/TaskDetailView.tsx`).
///
/// Recorded alongside the idempotency key so a retry can say what the previous
/// attempt already did. Deliberately **not** the whole [`Effect`]: `payload`
/// carries recipients, message bodies and arguments, and this record is read
/// back out onto an operator's screen through the task-detail route, which
/// scrubs by construction. The classification facts are kept; the contents are
/// not.
///
/// `irreversible` is decided **at execution time**, by the gate that was in
/// force then (`ManifestApprovalGate::is_irreversible`), rather than re-derived
/// on read. A company that later raises its auto-approve cap does not get to
/// retroactively decide that the payment it made last week was routine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutedEffect {
    /// The dotted effect kind, e.g. `payment.send`. The console maps it to
    /// plain language; it is never shown raw.
    pub kind: String,
    /// The USD amount involved, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_usd: Option<f64>,
    /// The board task this effect was executed for, when a card was behind it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Epoch-millis the effect was committed.
    pub at_millis: u64,
    /// Whether the supervised taxonomy calls this one irreversible.
    pub irreversible: bool,
}

/// A journal line [`load`](RuntimeJournal::load) could not replay (issue #386).
///
/// Deliberately carries **no line content**. The journal holds effect payloads —
/// recipients, message bodies, arguments — and a corruption report exists to be
/// logged and read by an operator, which is the one place [`ExecutedEffect`]
/// goes to some trouble to keep those out of. The line number locates it in the
/// file, the byte length separates a merged pair (long) from a truncated tail
/// (short), and the parse error names the column without quoting it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorruptLine {
    /// The record's 1-based position in what the [`JournalStore`] read back.
    ///
    /// For the filesystem backend that is the line's number in `journal.jsonl`,
    /// unchanged — the fs store returns every `\n`-separated segment, blanks
    /// included, so a blank line does not shift the count. For a database
    /// backend there is no file to open, and the number locates the record in
    /// append order.
    pub line: usize,
    /// The line's length in bytes.
    pub bytes: usize,
    /// What the parse rejected.
    pub message: String,
}

/// A cycle that journaled a start and no finish (issue #390).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCycle {
    /// The cycle's id.
    pub cycle_id: String,
    /// Epoch-millis it started.
    pub at_millis: u64,
    /// What kicked it off — see [`JournalRecord::CycleStarted::trigger`].
    pub trigger: String,
}

/// The error stamped on a cycle the host never got to finish (issue #390).
///
/// Phrased as a host fact rather than an agent fault, exactly as
/// [`INTERRUPTED_BY_RESTART`](crate::runtime::workflow_outcome::INTERRUPTED_BY_RESTART)
/// is for a workflow run: nothing about the turn went wrong, the process holding
/// it went away. An operator reading this should retry the approval, not go
/// looking at their agent.
pub const INTERRUPTED_BY_HOST_RESTART: &str = concat!(
    "this cycle was interrupted by a host restart and never finished; ",
    "if it was an approval's follow-up, re-approving is a safe no-op that ",
    "mints no second grant"
);

/// In-memory state rebuilt from (and kept in sync with) `journal.jsonl`.
#[derive(Default)]
struct State {
    executed: HashSet<String>,
    /// Cycles that started and have not finished (issue #390).
    ///
    /// A start inserts, a finish removes; whatever is left when replay ends
    /// either is running right now or died with a previous host. Telling those
    /// two apart is not this map's job — it is the boot sweep's, and the sweep
    /// is half the requirement rather than a follow-up. Without it every crashed
    /// cycle reads as in-flight forever, which is worse than the log line this
    /// replaces because it looks like live work.
    open_cycles: HashMap<String, OpenCycle>,
    /// Lines the last replay could not read — see [`CorruptLine`].
    corrupt: Vec<CorruptLine>,
    /// Every irreversible effect that ran for a board task, indexed by that
    /// task and oldest first within it (issue #351).
    ///
    /// Append-only for the same reason [`executed`](Self::executed) is: an
    /// effect that fired stays fired, and a retry warning that forgot half the
    /// history would be worse than none. One small record per effect, with no
    /// payload — see [`ExecutedEffect`].
    ///
    /// Indexed rather than a flat list because the read side is a per-task
    /// lookup on every Task Detail GET, and a linear scan of every effect a
    /// company ever executed is not flat for a long-lived one. Reversible
    /// effects and effects with no card behind them are dropped on the way in:
    /// nothing reads them, and the only thing keeping them would grow is
    /// memory.
    irreversible_by_task: HashMap<String, Vec<ExecutedEffect>>,
    /// Whether replay saw an executed key it cannot describe (issue #351).
    ///
    /// True when a pre-#351 `EffectExecuted` line is read back: the key proves
    /// something ran, and the record carries no way to say what. The retry
    /// dialog's "nothing irreversible here" is only honest when this is false,
    /// so the console is told and confirms regardless — see
    /// [`has_undescribed_history`](RuntimeJournal::has_undescribed_history).
    undescribed_executed: bool,
    parked: HashMap<ApprovalId, ParkedApproval>,
    /// The effect each approval was parked with, **payload scrubbed**, retained
    /// after the approval leaves [`parked`](Self::parked) (issue #351).
    ///
    /// Approving a harness tool call mints a grant rather than executing, so
    /// the only description of what the operator said yes to lives on the park
    /// record. This is what the grant-consumption path reads back to classify
    /// and name it once the tool has actually run. Overwritten by an
    /// approve-with-edit, because the grant is minted against the amended
    /// arguments and the amount the operator approved is the one to report.
    ///
    /// The payload is replaced with `Null` on the way in. Classification reads
    /// only the kind, group, amount and counterparty flags, and this map
    /// outlives the queue entry — retaining recipients and message bodies for
    /// the life of the process to answer a question that never asks for them
    /// would be the one leak [`ExecutedEffect`] exists to avoid.
    approval_effects: HashMap<ApprovalId, Effect>,
    /// What each approval was when it parked, retained after it leaves `parked`.
    ///
    /// This is what makes waiting time readable (issue #305) and what links a
    /// resolved approval back to its board task (issue #333). Both facts are
    /// journal-only — [`CompanyEvent::ApprovalResolved`](crate::ports::CompanyEvent::ApprovalResolved)
    /// carries the resolution but neither the park time nor the task — so they
    /// are recoverable only by joining the two on [`ApprovalId`]. See
    /// [`ApprovalOrigin`] for why entries are never removed.
    origins: HashMap<ApprovalId, ApprovalOrigin>,
    /// Grants minted and not yet consumed or expired (issue #243).
    ///
    /// Unlike [`origins`](Self::origins) this one IS removed from on
    /// the terminal records: a replayed grant is handed straight back to the
    /// live [`GrantSet`](crate::runtime::grants::GrantSet), so keeping a
    /// consumed or expired entry here would re-arm a tool call that already ran
    /// (or that the operator was already told had lapsed) on every restart.
    grants: HashMap<ApprovalId, GrantedCall>,
    /// Explicit approval follow-ups still owed after replay. Kept separate from
    /// grants because a denial is a continuation, never executable authority.
    approval_continuations: HashMap<ApprovalId, ApprovalContinuation>,
    /// Blocker answers armed but not yet re-entered after replay (issue #1863).
    /// Kept separate from [`approval_continuations`](Self::approval_continuations)
    /// for the same reason that map is kept from `grants`: a blocker answer is
    /// not executable authority. A [`BlockerResumed`](JournalRecord::BlockerResumed)
    /// removes an entry once the stopped step has re-entered.
    blocker_resolutions: HashMap<ApprovalId, BlockerResolution>,
    /// Standing grants minted and not yet revoked or expired (issue #374).
    ///
    /// Removed from on both terminal records for the same reason as
    /// [`grants`](Self::grants): a replayed entry is handed straight back to the
    /// live set, so retaining a revoked one would hand back a permission the
    /// operator explicitly took away — on every restart, silently.
    standing_grants: HashMap<GrantId, StandingGrant>,
    /// Blocked agent-node continuation facts still awaiting re-dispatch, keyed by
    /// the per-(run, node) turn key (issue #1816, Stage 2).
    ///
    /// A [`BlockedNodeStashed`](JournalRecord::BlockedNodeStashed) inserts, its
    /// paired [`BlockedNodeReleased`](JournalRecord::BlockedNodeReleased) removes
    /// — the same start-inserts / terminator-removes shape
    /// [`grants`](Self::grants) uses, and for the same reason: a replayed entry is
    /// handed straight back to the live
    /// [`BlockedNodeQueue`](crate::runtime::blocked_nodes::BlockedNodeQueue) at
    /// boot, so retaining a released one would rehydrate a run that already
    /// re-dispatched.
    blocked_stashes: HashMap<String, BlockedStash>,
    /// Blocked agent-node turns with at least one approved decision banked so
    /// far, keyed the same as [`blocked_stashes`](Self::blocked_stashes)
    /// (issue #1816).
    ///
    /// Inserted by [`BlockedNodeApproved`](JournalRecord::BlockedNodeApproved)
    /// and removed by its stash's paired
    /// [`BlockedNodeReleased`](JournalRecord::BlockedNodeReleased) — the release
    /// that retires the stash also retires whatever this set knows about it,
    /// so a turn never lingers here past the continuation it describes.
    blocked_node_approvals: HashSet<String>,
    /// Blocked agent-node turns whose continuation has already been spawned
    /// once, keyed the same as [`blocked_stashes`](Self::blocked_stashes)
    /// (issue #1825).
    ///
    /// Inserted by
    /// [`BlockedNodeDispatched`](JournalRecord::BlockedNodeDispatched), the
    /// moment [`resume_blocked_agent_node`](crate::company::runtime::CompanyRuntime::resume_blocked_agent_node)'s
    /// spawn attempt actually succeeds — before that call goes on to retire the
    /// stash via [`BlockedNodeReleased`](JournalRecord::BlockedNodeReleased),
    /// which is the write `retire_blocked_stash` treats as best-effort. If that
    /// later write fails, `blocked_stashes` and `blocked_node_approvals` both
    /// survive a restart exactly as if nothing had been dispatched, and without
    /// this set `reconcile_stranded_blocked_nodes` cannot tell that apart from
    /// the genuine stranded case — it would re-spawn a continuation that
    /// already ran.
    ///
    /// # Deliberately *not* retired by `BlockedNodeReleased` (finding `3877914597`)
    ///
    /// Unlike [`blocked_node_approvals`](Self::blocked_node_approvals), a turn
    /// entered here is permanent for the life of the process and every future
    /// replay — the same shape [`executed`](Self::executed) already uses, for
    /// the same reason. A workflow-gate blocked-node card's own
    /// [`ApprovalParked`](JournalRecord::ApprovalParked) is `Durability::Host`,
    /// but [`ApprovalResolved`](JournalRecord::ApprovalResolved) is always
    /// `Durability::Process`: a host crash can lose only the resolution and
    /// leave that card durably reopened as a "ghost" *after* its continuation
    /// already ran to completion and its own `BlockedNodeReleased` already
    /// landed. `resume_blocked_agent_node`'s guard against a ghost decision
    /// (issue #1825, finding `3877718169`) reads only this set, so a version
    /// that cleared the turn out of it on release (as this one used to) made
    /// that guard read `false` for exactly the case it exists to catch — the
    /// ghost then fell through to the "no stash on this host" branch, which
    /// tells the operator to re-run the workflow by hand, manually repeating
    /// the very side effect the guard exists to prevent automatically. One
    /// leaked turn key per completed blocked node is the accepted cost of
    /// closing that, the same trade `executed` already makes.
    blocked_node_dispatched: HashSet<String>,
}

/// One blocked agent node's durable continuation facts (issue #1816).
#[derive(Clone, Debug)]
struct BlockedStash {
    workflow_id: String,
    input: Value,
    /// The blocked run's own attribution (issue #1862 prerequisite), carried
    /// so [`blocked_stashes`](RuntimeJournal::blocked_stashes) can hand
    /// [`BlockedNodeQueue::rearm`](crate::runtime::blocked_nodes::BlockedNodeQueue::rearm)
    /// the real trigger instead of a hardcoded `Operator` default.
    started_by: StartedBy,
    thread_id: Option<String>,
    /// The graph's content fingerprint at park time, mirrored from
    /// [`JournalRecord::BlockedNodeStashed`]'s own field.
    workflow_fingerprint: Option<String>,
    /// Whether this stash's `BlockedNodeStashed` append has actually landed
    /// (issue #1825, P1 — found by chatgpt-codex-connector).
    ///
    /// Set on insert by [`replay`](RuntimeJournal::replay), since a record it
    /// folds is durable by construction. Starts `false` for a live insert made
    /// by [`record_blocked_node_stashed`](RuntimeJournal::record_blocked_node_stashed)
    /// ahead of its own append, and flips to `true` only once that append
    /// actually returns `Ok`. The settle-time fallback call reads this — not
    /// mere presence in the map — to decide whether there is still an append
    /// worth retrying.
    durable: bool,
}

impl State {
    /// Files an executed effect under the card it ran for, keeping only what
    /// the retry warning reads (issue #351).
    ///
    /// Two drops, both deliberate: a reversible effect is never named, and an
    /// effect with no card behind it belongs to no dialog. Retaining either
    /// would grow one map per company for a lookup that filters them straight
    /// back out.
    fn index_executed(&mut self, effect: ExecutedEffect) {
        if !effect.irreversible {
            return;
        }
        let Some(task_id) = effect.task_id.clone() else {
            return;
        };
        self.irreversible_by_task
            .entry(task_id)
            .or_default()
            .push(effect);
    }

    /// Retains an approval's effect for later description, without its payload.
    fn retain_approval_effect(&mut self, id: &ApprovalId, effect: &Effect) {
        self.approval_effects.insert(
            id.clone(),
            Effect {
                payload: serde_json::Value::Null,
                ..effect.clone()
            },
        );
    }
}

/// A per-company append-only journal backing at-most-once effects and the
/// durable approval queue.
///
/// One process should own a given company's journal, but [`append`](Self::append)
/// no longer depends on that for integrity (issue #386). The filesystem store
/// writes every record whole — terminator included — in a single `O_APPEND`
/// write that has reached the kernel before the call returns, so a concurrent
/// writer can land a record before or after but never inside one, and it
/// serialises writers within the process on a per-path lock. A database backend
/// gets the same property more cheaply: a row or document insert is atomic, and
/// its sequence comes from the server, so two live hosts interleave without
/// collision.
///
/// Writers through *one* `RuntimeJournal` additionally serialise on
/// [`write_lock`](Self::write_lock), which keeps records in call order — so a
/// park cannot be replayed after the resolution that drains it. That lock is
/// held across the store call, which is what keeps a backend's sequence
/// allocation in call order too.
/// The company id a [`file-pinned`](RuntimeJournal::new) journal reports.
///
/// The store behind that constructor addresses one named file and never looks at
/// the id, so this is what shows up in a log line rather than a key anything
/// resolves. Named instead of empty so a stray appearance in a trace is
/// self-explaining.
const FILE_PINNED_COMPANY: &str = "<file-pinned journal>";

pub struct RuntimeJournal {
    store: Arc<dyn JournalStore>,
    company: CompanyId,
    state: StdMutex<State>,
    write_lock: TokioMutex<()>,
}

impl RuntimeJournal {
    /// Opens the journal for `company` over `store`, without loading it.
    ///
    /// Call [`load`](Self::load) to replay an existing journal into memory.
    pub fn with_store(store: Arc<dyn JournalStore>, company: CompanyId) -> Self {
        Self {
            store,
            company,
            state: StdMutex::new(State::default()),
            write_lock: TokioMutex::new(()),
        }
    }

    /// Opens (or prepares) a filesystem journal at `path` without loading it.
    ///
    /// The convenience constructor over [`with_store`](Self::with_store) for the
    /// case where a caller has a file rather than a backend — every test in the
    /// crate, and nothing in production, which resolves its store from the
    /// selected backend in `RuntimeBuilder`.
    ///
    /// The store is pinned to the named file and ignores the company id, so the
    /// id here is a label rather than a key. Two journals over one path still
    /// share an append lock: the key is the absolutised path, so a relative and
    /// an absolute spelling of one file match; a symlinked or `..`-laden
    /// spelling still does not, and falls back on the atomic write for its
    /// safety.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_store(
            Arc::new(FsJournalStore::at_file(path)),
            CompanyId::new(FILE_PINNED_COMPANY),
        )
    }

    /// Replays the on-disk journal into memory, reconstructing the executed-key
    /// set and the parked-approval queue. Idempotent.
    ///
    /// **A damaged line does not fail the load** (issue #386). It is skipped,
    /// logged against the file and line number, and reported through
    /// [`corruption`](Self::corruption) for the caller to act on. Before this,
    /// one bad line returned `Err` from here and took the whole company's boot
    /// with it — turning the loss of a single record into the loss of every
    /// record after it, plus the tenant. An operator cannot repair a journal
    /// through a console that will not start.
    ///
    /// The skip is genuinely lossy and the safety argument is not symmetric: a
    /// dropped `ApprovalResolved` leaves an approval parked, which a person can
    /// still deny, while a dropped `EffectExecuted` un-commits a key and lets an
    /// effect run twice. That is why [`replay_line`] recovers a merged line in
    /// full rather than skipping it — the historical corruption this issue is
    /// about is exactly the recoverable kind, and skipping it is the outcome
    /// worth working to avoid.
    pub async fn load(&self) -> Result<()> {
        // The store hands back lines, never bytes, and decodes lossily on the
        // way: a torn write can split a multi-byte codepoint, and a whole-file
        // UTF-8 decode would fail the entire load on that one bad byte — failing
        // the boot for exactly the damage this function exists to survive.
        // Per-line decoding keeps a single mangled line on the `CorruptLine`
        // path with the rest of the journal intact.
        let lines = self.store.read_journal(&self.company).await?;

        let mut state = State::default();
        for (index, line) in lines.iter().enumerate() {
            let line = line.as_str();
            if line.trim().is_empty() {
                continue;
            }
            let records = match replay_line(line) {
                Ok(records) => records,
                Err(message) => {
                    let corrupt = CorruptLine {
                        line: index + 1,
                        bytes: line.len(),
                        message,
                    };
                    tracing::error!(
                        company = %self.company,
                        line = corrupt.line,
                        bytes = corrupt.bytes,
                        error = %corrupt.message,
                        "journal line could not be replayed; skipping it and continuing",
                    );
                    state.corrupt.push(corrupt);
                    continue;
                }
            };
            if records.len() > 1 {
                // Recovered, not lost — so not a `CorruptLine`. Still worth
                // saying out loud: the journal carries damage from a host that
                // predates the write fix, and a reader looking at it by hand
                // should know why one line holds several records.
                tracing::warn!(
                    company = %self.company,
                    line = index + 1,
                    records = records.len(),
                    "journal line holds several records with no separator; \
                     replaying all of them",
                );
            }
            for record in records {
                Self::replay(&mut state, record);
            }
        }
        *self.state.lock().expect("journal state poisoned") = state;
        Ok(())
    }

    /// Lines the last [`load`](Self::load) could not replay, in file order.
    ///
    /// Empty is the only healthy answer. A non-empty one means the company is
    /// running on an incomplete history and something above has to say so.
    pub fn corruption(&self) -> Vec<CorruptLine> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .corrupt
            .clone()
    }

    /// Folds one replayed record into the rebuilt state.
    fn replay(state: &mut State, record: JournalRecord) {
        match record {
            JournalRecord::EffectExecuted { key, effect } => {
                state.executed.insert(key);
                // Absent on a pre-#351 line: the key still replays, the
                // description simply does not exist to replay. Flag it, so
                // the console says "there is earlier activity I cannot
                // describe" rather than showing an all-clear.
                match effect {
                    Some(effect) => state.index_executed(effect),
                    None => state.undescribed_executed = true,
                }
            }
            JournalRecord::ApprovalParked {
                id,
                effect,
                at_millis,
                task,
                thread,
                parent,
                cycle,
            } => {
                state.retain_approval_effect(&id, &effect);
                state.origins.insert(
                    id.clone(),
                    ApprovalOrigin {
                        at_millis,
                        kind: effect.kind.clone(),
                        task: task.clone(),
                        run_id: effect.run_id.clone(),
                        thread: thread.clone(),
                        parent,
                        cycle: cycle.clone(),
                    },
                );
                state.parked.insert(
                    id,
                    ParkedApproval {
                        effect,
                        at_millis,
                        // Reset each replay to the park instant, then moved by
                        // any later `ApprovalExtended` line below — the log order
                        // is what makes the last extension win (issue #1805).
                        deadline_anchor_millis: at_millis,
                        task,
                        thread,
                        cycle,
                    },
                );
            }
            JournalRecord::ApprovalResolved { id } => {
                state.parked.remove(&id);
            }
            JournalRecord::ApprovalExpired { id, .. } => {
                state.parked.remove(&id);
            }
            // Issue #1805: re-anchor the deadline window. A no-op for an id no
            // longer parked (already resolved/expired earlier in the log), which
            // is correct — an extension of something since decided moves nothing.
            JournalRecord::ApprovalExtended { id, at_millis, .. } => {
                if let Some(parked) = state.parked.get_mut(&id) {
                    parked.deadline_anchor_millis = at_millis;
                }
            }
            // Audit-only for the queue: the paired `ApprovalResolved`
            // handles removal. The amended effect does supersede the parked
            // one for description, because it is the amended arguments the
            // grant was minted against.
            JournalRecord::ApprovalAmended {
                id, amended_effect, ..
            } => {
                state.retain_approval_effect(&id, &amended_effect);
            }
            JournalRecord::ApprovalGranted { grant } => {
                state.grants.insert(grant.approval_id.clone(), grant);
            }
            JournalRecord::GrantDispatched { id, .. } => {
                state.grants.remove(&id);
            }
            JournalRecord::ApprovalContinuationQueued { continuation } => {
                state
                    .approval_continuations
                    .insert(continuation.call.approval_id.clone(), continuation);
            }
            JournalRecord::ApprovalContinuationDispatched { id, .. }
            | JournalRecord::ApprovalContinuationConsumed { id }
            | JournalRecord::ApprovalContinuationExpired { id, .. } => {
                state.approval_continuations.remove(&id);
            }
            JournalRecord::BlockerResolved { id, resolution } => {
                state.blocker_resolutions.insert(id, resolution);
            }
            JournalRecord::BlockerResumed { id } => {
                state.blocker_resolutions.remove(&id);
            }
            JournalRecord::GrantConsumed { id, effect } => {
                state.grants.remove(&id);
                // Absent only on a line written before the grant path was
                // described; same additive contract as `EffectExecuted`.
                if let Some(effect) = effect {
                    state.index_executed(effect);
                }
            }
            JournalRecord::GrantExpired { id, .. } => {
                state.grants.remove(&id);
            }
            JournalRecord::StandingGrantMinted { grant } => {
                state.standing_grants.insert(grant.id.clone(), grant);
            }
            JournalRecord::StandingGrantRevoked { id, .. } => {
                state.standing_grants.remove(&id);
            }
            JournalRecord::StandingGrantExpired { id, .. } => {
                state.standing_grants.remove(&id);
            }
            // Issue #390: start inserts, finish removes. A finish for a cycle
            // this journal never started removes nothing, which is right rather
            // than a gap — a pre-#390 line has no start to be matched against,
            // so no such cycle can be sitting in the map.
            JournalRecord::CycleStarted {
                cycle_id,
                at_millis,
                trigger,
            } => {
                state.open_cycles.insert(
                    cycle_id.clone(),
                    OpenCycle {
                        cycle_id,
                        at_millis,
                        trigger,
                    },
                );
            }
            JournalRecord::CycleFinished { cycle_id, .. } => {
                state.open_cycles.remove(&cycle_id);
            }
            // Issue #1816: start inserts, terminator removes — the same shape as
            // grants. A `BlockedNodeReleased` for a turn this journal never
            // stashed removes nothing, which is correct: a pre-#1816 line has no
            // stash to retire, so none can be sitting in the map.
            JournalRecord::BlockedNodeStashed {
                turn,
                workflow_id,
                input,
                started_by,
                thread_id,
                workflow_fingerprint,
                ..
            } => {
                state.blocked_stashes.insert(
                    turn,
                    BlockedStash {
                        workflow_id,
                        input,
                        started_by,
                        thread_id,
                        workflow_fingerprint,
                        // A record `replay` folds is durable by construction —
                        // it was read back from the journal it describes.
                        durable: true,
                    },
                );
            }
            JournalRecord::BlockedNodeReleased { turn } => {
                state.blocked_stashes.remove(&turn);
                state.blocked_node_approvals.remove(&turn);
                // `blocked_node_dispatched` is deliberately NOT retired here —
                // see that field's own doc comment (finding `3877914597`). A
                // ghost decision can still reach this turn after this very
                // release replays, and the guard it feeds needs the tombstone
                // to still be standing when it does.
            }
            JournalRecord::BlockedNodeApproved { turn } => {
                state.blocked_node_approvals.insert(turn);
            }
            JournalRecord::BlockedNodeDispatched { turn } => {
                state.blocked_node_dispatched.insert(turn);
            }
        }
    }

    /// Opens a cycle's bracket (issue #390).
    ///
    /// # Called before the serial lock, deliberately
    ///
    /// The issue's body asked for this "as the follow-up cycle takes the serial
    /// lock". That placement cannot see the failure the issue exists for. The
    /// per-company serial lock is held for a **whole** cycle, so a continuation
    /// spawned behind a busy company waits on it for an unbounded time — and
    /// every way an operator ends up with "I approved, it said `recorded: true`,
    /// nothing happened" is on the near side of that lock:
    ///
    /// * the host dies after `tokio::spawn` but before the task is first polled;
    /// * the host dies while the task is queued on the lock;
    /// * the spawned task panics before the cycle body runs.
    ///
    /// Bracketing after the lock would report every one of those as though the
    /// cycle had never been asked for, which is the state of the world today.
    ///
    /// # The window this still does not cover
    ///
    /// A host that dies between the **durable verdict** and `tokio::spawn`
    /// writes no start at all, so nothing — not this bracket, not the sweep —
    /// can see it, and the operator is exactly as blind as before. Closing that
    /// needs a record written when the verdict is settled (an "owed
    /// continuation"), which is a different feature from a cycle bracket and is
    /// deliberately not built here. Named rather than left to be discovered, in
    /// the register of `run_supervisor`'s two known gaps.
    ///
    /// # Ordering
    ///
    /// Appends serialise on [`JOURNAL_WRITE_LOCKS`], **not** on the cycle's
    /// serial lock, so brackets from concurrent cycles interleave in the file.
    /// That is harmless for [`open_cycles`](Self::open_cycles), which folds by
    /// id rather than by position — but the journal stops reading as one
    /// sequential story by hand, and anyone doing that should know why.
    pub async fn record_cycle_started(&self, cycle_id: &str, trigger: &str) -> Result<()> {
        let at_millis = crate::ports::now_millis();
        self.state
            .lock()
            .expect("journal state poisoned")
            .open_cycles
            .insert(
                cycle_id.to_string(),
                OpenCycle {
                    cycle_id: cycle_id.to_string(),
                    at_millis,
                    trigger: trigger.to_string(),
                },
            );
        self.append(&JournalRecord::CycleStarted {
            cycle_id: cycle_id.to_string(),
            at_millis,
            trigger: trigger.to_string(),
        })
        .await
    }

    /// Closes a cycle's bracket (issue #390). `error` is `None` on success.
    ///
    /// A **panicking** cycle task journals nothing here — it unwinds past this
    /// call — so it reads as open until the next boot sweep settles it. That is
    /// the same exposure `run_supervisor` documents for a panicking workflow
    /// run, and the same remedy covers both.
    pub async fn record_cycle_finished(&self, cycle_id: &str, error: Option<String>) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .open_cycles
            .remove(cycle_id);
        self.append(&JournalRecord::CycleFinished {
            cycle_id: cycle_id.to_string(),
            at_millis: crate::ports::now_millis(),
            error,
        })
        .await
    }

    /// Cycles that started and never finished, oldest first (issue #390).
    ///
    /// Only honest because [`sweep_interrupted_cycles`](Self::sweep_interrupted_cycles)
    /// settles the strays at boot. Without it this would report every cycle any
    /// dead host ever started as in-flight forever.
    pub fn open_cycles(&self) -> Vec<OpenCycle> {
        let mut open: Vec<OpenCycle> = self
            .state
            .lock()
            .expect("journal state poisoned")
            .open_cycles
            .values()
            .cloned()
            .collect();
        // Sorted so the surface is deterministic; a `HashMap` order would make
        // two reads of an unchanged journal disagree for no reason.
        open.sort_by(|a, b| {
            a.at_millis
                .cmp(&b.at_millis)
                .then(a.cycle_id.cmp(&b.cycle_id))
        });
        open
    }

    /// Settles every cycle left open by a previous host process, returning how
    /// many were closed (issue #390).
    ///
    /// # Why an unterminated start is provably dead at boot
    ///
    /// The same argument
    /// [`sweep_interrupted_runs`](crate::runtime::sweep_interrupted_runs) rests
    /// on: a cycle journals its start before it does anything, every cycle is
    /// driven inside this process, and one process owns this journal. So at
    /// boot, before any entry point can have started a cycle, an unmatched start
    /// cannot belong to a live one — there are no live ones. No timeout
    /// heuristic is needed.
    ///
    /// # It must NOT run on a rebuild
    ///
    /// That argument holds at boot and is false the moment a company has been
    /// serving. A cycle survives a live runtime swap
    /// ([`rebuild_company`](crate::runtime::rebuild_company)), so sweeping
    /// mid-life would stamp "interrupted by a host restart" on a cycle still
    /// running — and its real finish would then land after the synthetic one,
    /// leaving two contradictory outcomes for one cycle id. The caller gates on
    /// the handover being absent; see the call site in the runtime builder.
    ///
    /// Best-effort: an append failure is logged and swallowed, because
    /// record-keeping must never stop a company from booting.
    pub async fn sweep_interrupted_cycles(&self) -> usize {
        let open = self.open_cycles();
        let mut settled = 0;
        for cycle in open {
            tracing::info!(
                company = %self.company,
                cycle = %cycle.cycle_id,
                trigger = %cycle.trigger,
                started_at = cycle.at_millis,
                "settling a cycle left open by a previous host process"
            );
            match self
                .record_cycle_finished(
                    &cycle.cycle_id,
                    Some(INTERRUPTED_BY_HOST_RESTART.to_string()),
                )
                .await
            {
                Ok(()) => settled += 1,
                Err(err) => tracing::warn!(
                    company = %self.company,
                    cycle = %cycle.cycle_id,
                    %err,
                    "could not settle an interrupted cycle; it stays open in the journal"
                ),
            }
        }
        settled
    }

    /// Whether an effect under `key` was already committed.
    pub fn is_executed(&self, key: &str) -> bool {
        self.state
            .lock()
            .expect("journal state poisoned")
            .executed
            .contains(key)
    }

    /// Commits an effect key to the journal before its side effect runs,
    /// alongside a description of what the key is about to do (issue #351).
    ///
    /// A no-op (returns `Ok`) if the key is already committed — which is also
    /// what keeps the executed-effect list free of duplicates: the second
    /// commit under a key never reaches the append.
    ///
    /// **A failed append releases the key again.** The in-memory set is a mirror
    /// of what the file holds, and holding a key the append refused makes it lie
    /// in the one direction that is silent: `execute_effect_once` aborts before
    /// `perform_effect` on the error, so nothing external fired — but a later
    /// attempt under the same key would then find the key present, take the
    /// `Ok(())` early return, and skip the effect *reporting success*. The
    /// effect would never run and no caller would ever hear that. Releasing the
    /// key makes the retry a real retry.
    ///
    /// This does not weaken at-most-once, because the two are on opposite sides
    /// of the side effect. The guarantee is about a crash *after* a commit that
    /// succeeded; this is a commit that failed, before which nothing ran. The
    /// worst case is the uncertain one — the write reached the file and only the
    /// flush failed — and it still cannot duplicate: the retry appends a second
    /// line for the key (replay dedupes, `executed` is a set) and runs the
    /// effect exactly once, and a crash before the retry replays the first line
    /// and skips the effect entirely. Every path is one execution or none.
    ///
    /// Contrast [`record_grant_consumed`](Self::record_grant_consumed), which
    /// deliberately does *not* roll back: its tool has already run by the time
    /// the record is written, so keeping the grant spent in memory is the safe
    /// direction and restoring it would re-arm a grant that was redeemed.
    pub async fn record_executed(&self, key: &str, effect: ExecutedEffect) -> Result<()> {
        {
            let mut state = self.state.lock().expect("journal state poisoned");
            if !state.executed.insert(key.to_string()) {
                return Ok(());
            }
        }
        let appended = self
            .append(&JournalRecord::EffectExecuted {
                key: key.to_string(),
                effect: Some(effect.clone()),
            })
            .await;
        let mut state = self.state.lock().expect("journal state poisoned");
        match appended {
            // Indexed only once the commit is on the file, so the retry warnings
            // built from it describe effects the journal actually committed.
            Ok(()) => state.index_executed(effect),
            Err(_) => {
                state.executed.remove(key);
            }
        }
        appended
    }

    /// Records a newly parked approval and which board task it belongs to
    /// (issue #333).
    ///
    /// `task` is deliberately **not** an `Option`: every caller must say which
    /// it is, [`TaskLink::Task`] or [`TaskLink::Unlinked`], so that a missing
    /// link can only ever mean "written before #333". A caller with an
    /// `Option<&str>` in hand converts with [`TaskLink::from_task_id`].
    ///
    /// `thread` **is** an `Option`, and deliberately so (issue #379): unlike the
    /// task link, nothing downstream distinguishes "no conversation produced
    /// this" from "this host does not record conversations". Both mean no
    /// channel owns the approval, and both correctly leave it on the Approvals
    /// page alone.
    ///
    /// `cycle` is the parking turn (issue #469), and is what lets the runtime
    /// continue a turn once rather than once per approval it raised. `Option`
    /// on the same terms as `thread`: absent means "this host did not record a
    /// turn", which falls back to continuing the approval on its own.
    ///
    /// `conversation` carries the channel **and** the thread root inside it as
    /// one value (issue #435), rather than as two adjacent parameters. Both of
    /// its fields are `Option` on the terms above, and its `parent` is only ever
    /// meaningful alongside its `thread` — see [`ApprovalOrigin::parent`]. They
    /// travel together for the same reason
    /// [`approval_conversation`](Self::approval_conversation) returns them
    /// together: two same-shaped `Option`s side by side in a call are trivially
    /// transposable by a caller and the compiler would not notice, and a park is
    /// the one place a wrong pairing would be written down durably. The
    /// `ApprovalConversation` this hands back on the read side is the same type,
    /// so a continuation round-trips one value instead of re-assembling two.
    pub async fn record_parked(
        &self,
        id: &ApprovalId,
        effect: &Effect,
        at_millis: u64,
        task: TaskLink,
        conversation: ApprovalConversation,
        cycle: Option<String>,
    ) -> Result<()> {
        let ApprovalConversation { thread, parent } = conversation;
        {
            let mut state = self.state.lock().expect("journal state poisoned");
            state.origins.insert(
                id.clone(),
                ApprovalOrigin {
                    at_millis,
                    kind: effect.kind.clone(),
                    task: Some(task.clone()),
                    run_id: effect.run_id.clone(),
                    thread: thread.clone(),
                    parent,
                    cycle: cycle.clone(),
                },
            );
            state.parked.insert(
                id.clone(),
                ParkedApproval {
                    effect: effect.clone(),
                    at_millis,
                    // A fresh park's deadline runs from when it was parked.
                    deadline_anchor_millis: at_millis,
                    task: Some(task.clone()),
                    thread: thread.clone(),
                    cycle: cycle.clone(),
                },
            );
            state.retain_approval_effect(id, effect);
        }
        self.append(&JournalRecord::ApprovalParked {
            id: id.clone(),
            effect: effect.clone(),
            at_millis,
            task: Some(task),
            thread,
            parent,
            cycle,
        })
        .await
    }

    /// The turn key of every approval **still parked**, one entry per approval
    /// (issue #469).
    ///
    /// Read once, at recovery, to re-arm the
    /// [`ContinuationQueue`](crate::runtime::continuation::ContinuationQueue):
    /// a restart in the middle of a partly-decided turn must come back still
    /// knowing that turn is blocked, or its continuation would either fire early
    /// or never fire at all. Approvals with no turn key (pre-#469 lines) are
    /// omitted — they continue on their own and are never gated.
    pub fn parked_turns(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .parked
            .values()
            .filter_map(|p| p.cycle.clone())
            .collect()
    }

    /// Every blocked agent-node stash still awaiting re-dispatch, as
    /// `(turn, workflow_id, input, started_by)` (issue #1816, Stage 2; the
    /// `started_by` field added for issue #1862's prerequisite).
    ///
    /// The builder folds this at boot into the live
    /// [`BlockedNodeQueue`](crate::runtime::blocked_nodes::BlockedNodeQueue)
    /// (via [`rearm`](crate::runtime::blocked_nodes::BlockedNodeQueue::rearm)) so
    /// an approval landing after a restart finds the run to continue, the way
    /// [`pending`](Self::pending) feeds the gate queue's re-arm. Only stashes
    /// whose paired [`BlockedNodeReleased`](JournalRecord::BlockedNodeReleased)
    /// has not replayed are returned — a re-dispatched run does not come back.
    pub fn blocked_stashes(&self) -> Vec<crate::runtime::blocked_nodes::BlockedStashRow> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .blocked_stashes
            .iter()
            .map(|(turn, stash)| {
                (
                    turn.clone(),
                    stash.workflow_id.clone(),
                    stash.input.clone(),
                    stash.started_by.clone(),
                    stash.thread_id.clone(),
                    stash.workflow_fingerprint.clone(),
                )
            })
            .collect()
    }

    /// Stashes a blocked agent node's continuation facts durably (issue #1816).
    ///
    /// Called from `HarnessAgentRunner::park_gated_calls` at park time, in
    /// lockstep with the in-memory
    /// [`BlockedNodeQueue::arm`](crate::runtime::blocked_nodes::BlockedNodeQueue::arm)
    /// (issue #1825, P1 second follow-up) — and again, redundantly, from the
    /// runner's block-settle pass, which called this first and alone until
    /// that follow-up. Both calls for one node carry the same `(turn,
    /// workflow_id, input)`, so once the append has actually landed the
    /// second call is a first-write-wins no-op rather than a second source of
    /// truth — the same tier `arm` itself skips at for the identical reason,
    /// now applied one durability level up so the settle pass's fallback call
    /// does not double every blocked node's `BlockedNodeStashed` write
    /// forever. Best-effort at the call site either way: a failed durable
    /// write leaves the in-memory stash serving the common (no-restart) case,
    /// exactly as a failed gate journal leaves its live queue in place.
    ///
    /// # Retrying a failed first append (issue #1825, P1 — found by
    /// chatgpt-codex-connector)
    ///
    /// The in-memory insert below lands before the append that durably backs
    /// it, so a transient failure on the park-time call still leaves `turn` in
    /// `blocked_stashes` — otherwise a resolve landing before the settle-time
    /// fallback would find no stash to release even though the in-memory arm
    /// (this call's sibling) says the node is blocked. But that same
    /// in-memory presence used to be read as "already durable": the
    /// settle-time fallback's call would see the entry, assume its own append
    /// was the redundant second write, and return without ever appending —
    /// so the durable record was never retried, and a restart landing before
    /// the run re-dispatches rehydrates nothing for an approval card that is
    /// still sitting there, clickable. [`BlockedStash::durable`] is what closes
    /// that: it is only set once an append for this stash has actually
    /// returned `Ok`, so a call that finds the turn present but not yet
    /// durable retries the append instead of skipping it.
    pub async fn record_blocked_node_stashed(
        &self,
        turn: &str,
        workflow_id: &str,
        input: &Value,
        started_by: &StartedBy,
    ) -> Result<()> {
        self.record_blocked_node_stashed_checkpointed(
            turn,
            workflow_id,
            input,
            started_by,
            None,
            None,
        )
        .await
    }

    pub async fn record_blocked_node_stashed_checkpointed(
        &self,
        turn: &str,
        workflow_id: &str,
        input: &Value,
        started_by: &StartedBy,
        thread_id: Option<&str>,
        workflow_fingerprint: Option<&str>,
    ) -> Result<()> {
        {
            let mut state = self.state.lock().expect("journal state poisoned");
            match state.blocked_stashes.get(turn) {
                Some(existing) if existing.durable => {
                    // Already durably recorded — either this run's own
                    // park-time write already landed and the settle pass is
                    // the redundant call, or a retry of this same call raced
                    // itself. Either way the facts are identical (one node
                    // parks under one turn), so a second durable append would
                    // only double the flush for no new information.
                    return Ok(());
                }
                Some(_) => {
                    // In memory, but its first durable append never landed —
                    // fall through and retry below instead of returning early
                    // and leaving the in-memory state misrepresent durability
                    // forever.
                }
                None => {
                    state.blocked_stashes.insert(
                        turn.to_string(),
                        BlockedStash {
                            workflow_id: workflow_id.to_string(),
                            input: input.clone(),
                            started_by: started_by.clone(),
                            thread_id: thread_id.map(str::to_string),
                            workflow_fingerprint: workflow_fingerprint.map(str::to_string),
                            durable: false,
                        },
                    );
                }
            }
        }
        self.append(&JournalRecord::BlockedNodeStashed {
            turn: turn.to_string(),
            workflow_id: workflow_id.to_string(),
            input: input.clone(),
            started_by: started_by.clone(),
            thread_id: thread_id.map(str::to_string),
            workflow_fingerprint: workflow_fingerprint.map(str::to_string),
            at_millis: crate::ports::now_millis(),
        })
        .await?;
        // Reached only once the append actually landed. A concurrent release
        // (the turn resolved and retired between the block above and here)
        // leaves nothing for `and_modify` to touch — correctly: there is no
        // stash left to mark durable, and none should be resurrected here.
        self.state
            .lock()
            .expect("journal state poisoned")
            .blocked_stashes
            .entry(turn.to_string())
            .and_modify(|stash| stash.durable = true);
        Ok(())
    }

    /// Retires a blocked-node stash once its run has re-dispatched (or its block
    /// was wholly refused), so a later boot does not rehydrate it (issue #1816).
    ///
    /// Also retires the turn from `blocked_node_approvals`, mirroring what
    /// replaying this same record does in [`replay`](Self::replay) — the
    /// doc-stated invariant on that field is that a turn never lingers there
    /// past the continuation it describes. Without this, a live release left
    /// the turn banked in that set for the rest of the process's life: a
    /// long-running tenant would accumulate one stale key per completed
    /// block, invisible until the next full reload replayed the same record
    /// correctly.
    ///
    /// `blocked_node_dispatched` is the one exception — deliberately left
    /// standing here, matching [`replay`](Self::replay)'s own fold. See that
    /// field's doc comment (finding `3877914597`) for why a live release
    /// clearing its own dispatch tombstone reopens the exact ghost-redispatch
    /// gap issue #1825 exists to close.
    pub async fn record_blocked_node_released(&self, turn: &str) -> Result<()> {
        {
            let mut state = self.state.lock().expect("journal state poisoned");
            state.blocked_stashes.remove(turn);
            state.blocked_node_approvals.remove(turn);
        }
        self.append(&JournalRecord::BlockedNodeReleased {
            turn: turn.to_string(),
        })
        .await
    }

    /// Every blocked-node turn durably known to have at least one approved
    /// decision banked (issue #1816).
    ///
    /// The builder folds this at boot into the live
    /// [`BlockedNodeQueue`](crate::runtime::blocked_nodes::BlockedNodeQueue)
    /// (via [`mark_approved`](crate::runtime::blocked_nodes::BlockedNodeQueue::mark_approved),
    /// once per turn) alongside [`blocked_stashes`](Self::blocked_stashes), so a
    /// restart that landed between an approval and the node's last decision
    /// still knows that approval happened when the last one lands.
    pub fn blocked_node_approvals(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .blocked_node_approvals
            .iter()
            .cloned()
            .collect()
    }

    /// Records durably that a blocked agent node's turn has at least one
    /// approved decision, the moment that decision lands (issue #1816).
    ///
    /// Called beside [`ContinuationQueue::decide`](crate::runtime::continuation::ContinuationQueue::decide),
    /// not deferred to the turn's release — the whole point is to survive a
    /// restart that lands on a decision that is not the turn's last, which is
    /// exactly the window release-time bookkeeping cannot cover. Idempotent by
    /// construction (a set insert), so a node whose second call is also
    /// approved writes this again harmlessly.
    pub async fn record_blocked_node_approved(&self, turn: &str) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .blocked_node_approvals
            .insert(turn.to_string());
        self.append(&JournalRecord::BlockedNodeApproved {
            turn: turn.to_string(),
        })
        .await
    }

    /// Every blocked-node turn durably known to have already had its
    /// continuation spawned once (issue #1825).
    ///
    /// [`CompanyRuntime::reconcile_stranded_blocked_nodes`](crate::company::runtime::CompanyRuntime::reconcile_stranded_blocked_nodes)
    /// checks this before re-spawning an approved-but-still-rehydrated stash:
    /// without it, a boot cannot tell "never dispatched" apart from "dispatched,
    /// but its `BlockedNodeReleased` write failed" — both leave the same
    /// `blocked_stashes` + `blocked_node_approvals` pair behind. See
    /// [`BlockedNodeDispatched`](JournalRecord::BlockedNodeDispatched) for the
    /// full reasoning.
    pub fn blocked_node_dispatched(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .blocked_node_dispatched
            .iter()
            .cloned()
            .collect()
    }

    /// Whether `turn`'s continuation has already been durably marked
    /// dispatched (issue #1825, finding `3877718169`).
    ///
    /// A single-lookup twin of [`blocked_node_dispatched`](Self::blocked_node_dispatched),
    /// for callers that only need one turn's membership rather than the whole
    /// set — [`CompanyRuntime::resume_blocked_agent_node`](crate::company::runtime::CompanyRuntime::resume_blocked_agent_node)'s
    /// own guard checks this on every live decision reaching a blocked node,
    /// not once per boot the way [`CompanyRuntime::reconcile_stranded_blocked_nodes`](crate::company::runtime::CompanyRuntime::reconcile_stranded_blocked_nodes)
    /// does, so cloning the full set on every call would be waste for no
    /// reason a `HashSet::contains` doesn't already avoid.
    pub fn is_blocked_node_dispatched(&self, turn: &str) -> bool {
        self.state
            .lock()
            .expect("journal state poisoned")
            .blocked_node_dispatched
            .contains(turn)
    }

    /// Records durably that a blocked agent node's continuation has been
    /// spawned, the moment the spawn attempt actually succeeds (issue #1825).
    ///
    /// Called from [`CompanyRuntime::resume_blocked_agent_node`](crate::company::runtime::CompanyRuntime::resume_blocked_agent_node)'s
    /// `Ok(())` arm, **before** it calls `retire_blocked_stash` — so this
    /// record lands even when that call's own durable write later fails.
    /// Idempotent by construction (a set insert), matching
    /// [`record_blocked_node_approved`](Self::record_blocked_node_approved).
    pub async fn record_blocked_node_dispatched(&self, turn: &str) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .blocked_node_dispatched
            .insert(turn.to_string());
        self.append(&JournalRecord::BlockedNodeDispatched {
            turn: turn.to_string(),
        })
        .await
    }

    /// The turn that parked `id`, if it is one this journal recorded
    /// (issue #469).
    ///
    /// Two levels of absence, and they mean different things — the same shape
    /// [`approval_thread`](Self::approval_thread) uses. `None`: nothing was ever
    /// parked under this id. `Some(None)`: parked, by a line written before the
    /// turn key existed.
    pub fn approval_cycle(&self, id: &ApprovalId) -> Option<Option<String>> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .origins
            .get(id)
            .map(|o| o.cycle.clone())
    }

    /// The effect an approval was parked with, payload scrubbed (issue #351).
    ///
    /// Answers the grant-consumption path's question: the agent just redeemed
    /// this approval's grant and the tool ran — what was it, and was it one that
    /// cannot be taken back? Superseded by an approve-with-edit, since that is
    /// what the grant was minted against.
    pub fn approval_effect(&self, id: &ApprovalId) -> Option<Effect> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .approval_effects
            .get(id)
            .cloned()
    }

    /// Whether replay read back an executed key it cannot describe (issue #351).
    ///
    /// Company-wide rather than per-task, and necessarily so: an undescribed
    /// record carries no card either, so there is nothing to attribute it to.
    /// The console's contract is that an empty
    /// [`irreversible_effects`](Self::irreversible_effects) means the journal
    /// holds nothing irreversible for a card — true only when this is `false`.
    /// When it is `true` the console confirms regardless and says the earlier
    /// activity cannot be described, instead of showing an all-clear it cannot
    /// stand behind.
    ///
    /// The related pre-#351 gap it does **not** detect on its own: an approval
    /// parked before the upgrade carries no `task_id`, so approving it
    /// afterwards executes an effect attributed to no card. That record is
    /// byte-identical to a legitimately card-less park written today, so
    /// flagging it would misreport every company that has ever parked an
    /// approval from operator chat. In practice a company old enough to hold a
    /// pre-#351 park also holds pre-#351 executed lines, so this flag is set and
    /// the same warning shows.
    pub fn has_undescribed_history(&self) -> bool {
        self.state
            .lock()
            .expect("journal state poisoned")
            .undescribed_executed
    }

    /// The irreversible effects this task has already executed, oldest first
    /// (issue #351).
    ///
    /// Drawn from the journal's own executed record — the same append-only set
    /// that makes effects at-most-once — rather than re-derived from timeline
    /// labels, which describe what an agent *said* and not what was committed.
    /// A direct index lookup, so a company's history length does not price a
    /// Task Detail read.
    pub fn irreversible_effects(&self, task_id: &str) -> Vec<ExecutedEffect> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .irreversible_by_task
            .get(task_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Records that a parked approval was resolved (removing it from the queue).
    pub async fn record_resolved(&self, id: &ApprovalId) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .parked
            .remove(id);
        self.append(&JournalRecord::ApprovalResolved { id: id.clone() })
            .await
    }

    /// Records that a parked approval expired to a default-deny, removing it
    /// from the queue. This is the durable audit entry for
    /// default-deny-on-silence.
    /// Drops the in-memory traces of a park whose durable line never landed
    /// (issue #1861).
    ///
    /// [`record_parked`](Self::record_parked) populates `origins`, `parked` and
    /// the retained effect *before* it appends, so a failing append leaves a
    /// live approval in the projection that no journal line will ever replay:
    /// present until this process exits, gone on the next boot. This removes
    /// the three entries and writes nothing — deliberately, since the caller is
    /// here precisely because the durable write is the thing that failed, and a
    /// compensating record would be a second write down the same broken path.
    ///
    /// **Not a retirement.** Nothing was durably parked, so there is nothing to
    /// retire and no default-deny to record; the caller reports the park as
    /// failed and its own path returns the card. Contrast
    /// [`CompanyRuntime::unpark_blocker`](crate::company::CompanyRuntime), which
    /// undoes a park that *did* land and therefore owes the full audit trail.
    pub fn discard_unrecorded_park(&self, id: &ApprovalId) {
        let mut state = self.state.lock().expect("journal state poisoned");
        state.parked.remove(id);
        state.origins.remove(id);
        state.approval_effects.remove(id);
    }

    pub async fn record_expired(
        &self,
        id: &ApprovalId,
        at_millis: u64,
        reason: ExpiryReason,
    ) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .parked
            .remove(id);
        self.append(&JournalRecord::ApprovalExpired {
            id: id.clone(),
            at_millis,
            reason,
        })
        .await
    }

    /// Records that an operator extended a parked approval's deadline, moving
    /// the live entry's TTL anchor to `at_millis` (issue #1805).
    ///
    /// Updates the in-memory queue so the very next `pending()` projects the new
    /// deadline, and appends the durable line so a redeploy replays the move. A
    /// no-op against the in-memory state for an id that is not parked — the
    /// caller (`CompanyRuntime::extend_approval`) has already asked the gate and
    /// refused with a 404 in that case, so this is only reached for a live entry.
    pub async fn record_extended(&self, id: &ApprovalId, at_millis: u64, by: Actor) -> Result<()> {
        if let Some(parked) = self
            .state
            .lock()
            .expect("journal state poisoned")
            .parked
            .get_mut(id)
        {
            parked.deadline_anchor_millis = at_millis;
        }
        self.append(&JournalRecord::ApprovalExtended {
            id: id.clone(),
            at_millis,
            by,
        })
        .await
    }

    /// Records an operator-amended approval (an approve-with-edit) for the audit
    /// trail. Removal from the queue is recorded separately by
    /// [`record_resolved`](Self::record_resolved).
    pub async fn record_amended(
        &self,
        id: &ApprovalId,
        amended_effect: &Effect,
        at_millis: u64,
    ) -> Result<()> {
        // The amendment supersedes the park as the description of what the
        // operator approved (issue #351) — a grant is minted against the
        // amended arguments, so an edited amount is the one to report.
        self.state
            .lock()
            .expect("journal state poisoned")
            .retain_approval_effect(id, amended_effect);
        self.append(&JournalRecord::ApprovalAmended {
            id: id.clone(),
            amended_effect: amended_effect.clone(),
            at_millis,
        })
        .await
    }

    /// A snapshot of what every approval ever parked *was*, keyed by
    /// [`ApprovalId`] — including approvals since resolved or expired.
    ///
    /// The read side joins this against the event log's
    /// [`ApprovalResolved`](crate::ports::CompanyEvent::ApprovalResolved) to
    /// recover how long an approval was waiting (issue #305) and which board
    /// task it belonged to (issue #333). Taken as one snapshot per request
    /// rather than per lookup, so a fold never holds the state lock while it
    /// works.
    pub fn approval_origins(&self) -> HashMap<ApprovalId, ApprovalOrigin> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .origins
            .clone()
    }

    /// What one approval was when it parked, without cloning the whole
    /// [`origins`](State::origins) map.
    ///
    /// The read path resolves a bounded number of ids per request — the
    /// approval events on one page of the fold, plus the parked queue — so it
    /// takes this per id rather than a snapshot. [`approval_origins`] copies an
    /// index that grows with every approval ever parked and is never pruned, and
    /// the task-detail route is polled, so a snapshot there costs the whole
    /// history on every poll.
    ///
    /// [`approval_origins`]: Self::approval_origins
    pub fn approval_origin(&self, id: &ApprovalId) -> Option<ApprovalOrigin> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .origins
            .get(id)
            .cloned()
    }

    /// The task link recorded for one approval, without cloning the whole
    /// [`origins`](State::origins) map.
    ///
    /// The map is unbounded and never pruned (see [`ApprovalOrigin`]), so a
    /// caller that needs the link for a couple of known ids — every cycle does,
    /// via [`cycle_task_id`](crate::runtime::cycle) — must not pay a full clone
    /// per cycle to read them. `approval_origins` stays the right call for a
    /// fold that will look up an unknown number of ids.
    ///
    /// The outer `Option` is "no such approval"; the inner is a pre-#333 line.
    pub fn approval_task(&self, id: &ApprovalId) -> Option<Option<TaskLink>> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .origins
            .get(id)
            .map(|o| o.task.clone())
    }

    /// The chat thread recorded for one approval (issue #379), read the same
    /// per-id way as [`approval_task`](Self::approval_task) and for the same
    /// reason — the origins map is unbounded, and a cycle needs at most the
    /// couple of ids in its own batch.
    ///
    /// The outer `Option` is "no such approval"; the inner is "no conversation
    /// behind it" (which a pre-#379 line is indistinguishable from, by design).
    /// Reading it off the retained origin rather than the live queue is what
    /// makes it answerable *after* the approval resolved — the case
    /// [`cycle_thread_id`](crate::runtime::cycle) needs so a second sign-off
    /// re-parks in the channel the first one was asked in.
    pub fn approval_thread(&self, id: &ApprovalId) -> Option<Option<String>> {
        self.approval_conversation(id).map(|c| c.thread)
    }

    /// Where one approval was raised, channel **and** thread, in a single read
    /// (issue #435).
    ///
    /// One accessor rather than an `approval_thread` plus an `approval_parent`,
    /// deliberately. The two values are only meaningful together — a parent is
    /// a sequence number with no channel to resolve it against — and reading
    /// them separately would take the state lock twice, admitting a torn pair
    /// that names one approval's channel and another's thread. Nothing today
    /// mutates an origin after it is inserted, so that tear is currently
    /// unreachable; this keeps it unreachable by construction rather than by
    /// coincidence.
    ///
    /// `None` is "no such approval". A present [`ApprovalConversation`] may
    /// still hold `None` in either field, on the terms each documents.
    pub fn approval_conversation(&self, id: &ApprovalId) -> Option<ApprovalConversation> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .origins
            .get(id)
            .map(|o| ApprovalConversation {
                thread: o.thread.clone(),
                parent: o.parent,
            })
    }

    /// Records a minted single-use grant (issue #243).
    ///
    /// Called *before* the grant enters the live set, so the ordering failure
    /// mode is "recorded but not live" — which replay fixes — rather than "live
    /// but not recorded", which a crash would lose silently.
    pub async fn record_granted(&self, grant: &GrantedCall) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .grants
            .insert(grant.approval_id.clone(), grant.clone());
        self.append(&JournalRecord::ApprovalGranted {
            grant: grant.clone(),
        })
        .await
    }

    /// Durably commits a single-use grant to one follow-up turn before the
    /// turn can re-issue its approved tool call.
    pub async fn record_grant_dispatched(&self, id: &ApprovalId, at_millis: u64) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .grants
            .remove(id);
        self.append(&JournalRecord::GrantDispatched {
            id: id.clone(),
            at_millis,
        })
        .await
    }

    /// Records a verdict-bearing explicit approval continuation before it is
    /// armed in memory.
    pub async fn record_approval_continuation(
        &self,
        continuation: &ApprovalContinuation,
    ) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .approval_continuations
            .insert(continuation.call.approval_id.clone(), continuation.clone());
        self.append(&JournalRecord::ApprovalContinuationQueued {
            continuation: continuation.clone(),
        })
        .await
    }

    /// Banks an operator's answer to a parked blocker before the resume is
    /// armed in memory (issue #1863), the blocker twin of
    /// [`record_approval_continuation`](Self::record_approval_continuation).
    pub async fn record_blocker_resolution(
        &self,
        id: &ApprovalId,
        resolution: &BlockerResolution,
    ) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .blocker_resolutions
            .insert(id.clone(), resolution.clone());
        self.append(&JournalRecord::BlockerResolved {
            id: id.clone(),
            resolution: resolution.clone(),
        })
        .await
    }

    /// Records that a parked blocker's answer was re-entered into the stopped
    /// step, so it is not re-armed on the next boot.
    pub async fn record_blocker_resumed(&self, id: &ApprovalId) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .blocker_resolutions
            .remove(id);
        self.append(&JournalRecord::BlockerResumed { id: id.clone() })
            .await
    }

    /// Records that an explicit approval continuation reached its agent.
    pub async fn record_approval_continuation_consumed(&self, id: &ApprovalId) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .approval_continuations
            .remove(id);
        self.append(&JournalRecord::ApprovalContinuationConsumed { id: id.clone() })
            .await
    }

    /// Durably claims one explicit continuation before its agent turn starts.
    /// Replay removes a claimed continuation from the recovery queue, choosing
    /// a possibly missed follow-up over repeating an external action.
    pub async fn record_approval_continuation_dispatched(
        &self,
        id: &ApprovalId,
        at_millis: u64,
    ) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .approval_continuations
            .remove(id);
        self.append(&JournalRecord::ApprovalContinuationDispatched {
            id: id.clone(),
            at_millis,
        })
        .await
    }

    /// Records that an explicit approval continuation expired undelivered.
    pub async fn record_approval_continuation_expired(
        &self,
        id: &ApprovalId,
        at_millis: u64,
    ) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .approval_continuations
            .remove(id);
        self.append(&JournalRecord::ApprovalContinuationExpired {
            id: id.clone(),
            at_millis,
        })
        .await
    }

    /// Records that a grant was redeemed — the agent re-issued the call and the
    /// tool ran. Removes it from the replay set so a restart cannot re-arm it.
    ///
    /// `effect` describes what the redeemed call was (issue #351), so an
    /// operator-approved tool call reaches the retry warning at all. This is the
    /// grant path's only chance to be described: it is settled by minting a
    /// grant, not by `execute_effect_once`, so it writes no `EffectExecuted`
    /// line. `None` when the approval's parked effect is no longer recoverable
    /// — the redemption is still recorded, it simply contributes no warning.
    pub async fn record_grant_consumed(
        &self,
        id: &ApprovalId,
        effect: Option<ExecutedEffect>,
    ) -> Result<()> {
        {
            let mut state = self.state.lock().expect("journal state poisoned");
            state.grants.remove(id);
            if let Some(effect) = effect.clone() {
                state.index_executed(effect);
            }
        }
        self.append(&JournalRecord::GrantConsumed {
            id: id.clone(),
            effect,
        })
        .await
    }

    /// Records that a grant expired unredeemed. Same replay removal as
    /// consumption: the operator has been told it lapsed, so a restart must not
    /// quietly hand the agent the permission back.
    pub async fn record_grant_expired(&self, id: &ApprovalId, at_millis: u64) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .grants
            .remove(id);
        self.append(&JournalRecord::GrantExpired {
            id: id.clone(),
            at_millis,
        })
        .await
    }

    /// Every grant still live according to the journal — what boot recovery
    /// seeds the in-memory [`GrantSet`](crate::runtime::grants::GrantSet) with.
    pub fn replayed_grants(&self) -> Vec<GrantedCall> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .grants
            .values()
            .cloned()
            .collect()
    }

    /// Explicit approval continuations still owed according to journal replay.
    pub fn replayed_approval_continuations(&self) -> Vec<ApprovalContinuation> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .approval_continuations
            .values()
            .cloned()
            .collect()
    }

    /// Every blocker answer replay left armed but not yet re-entered (issue
    /// #1863), for the boot rebuild to re-arm on the live grant set — the
    /// blocker twin of [`replayed_approval_continuations`](Self::replayed_approval_continuations).
    pub fn replayed_blocker_resolutions(&self) -> Vec<(ApprovalId, BlockerResolution)> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .blocker_resolutions
            .iter()
            .map(|(id, resolution)| (id.clone(), resolution.clone()))
            .collect()
    }

    /// Records a minted standing grant (issue #374).
    ///
    /// Called *before* the grant enters the live set, so the ordering failure
    /// mode is "recorded but not live" — which replay fixes — rather than "live
    /// but not recorded", which would leave a permission nobody can see or
    /// revoke.
    pub async fn record_standing_granted(&self, grant: &StandingGrant) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .standing_grants
            .insert(grant.id.clone(), grant.clone());
        self.append(&JournalRecord::StandingGrantMinted {
            grant: grant.clone(),
        })
        .await
    }

    /// Records that the operator revoked a standing grant (issue #374).
    pub async fn record_standing_revoked(
        &self,
        id: &GrantId,
        by: Actor,
        at_millis: u64,
    ) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .standing_grants
            .remove(id);
        self.append(&JournalRecord::StandingGrantRevoked {
            id: id.clone(),
            by,
            at_millis,
        })
        .await
    }

    /// Records that a standing grant reached its deadline (issue #374).
    pub async fn record_standing_expired(&self, id: &GrantId, at_millis: u64) -> Result<()> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .standing_grants
            .remove(id);
        self.append(&JournalRecord::StandingGrantExpired {
            id: id.clone(),
            at_millis,
        })
        .await
    }

    /// Every standing grant still live according to the journal, with anything
    /// already past its deadline folded out (issue #374).
    ///
    /// The expiry filter matters beyond tidiness: the sweep only runs while the
    /// process is up, so a host that was down across a grant's deadline has no
    /// `StandingGrantExpired` line for it. Replaying on `at_millis` alone would
    /// hand a lapsed permission back to the live set, and a restart would be a
    /// way to resurrect one — the exact silent accumulation this issue forbids.
    pub fn replayed_standing_grants(&self, now_millis: u64) -> Vec<StandingGrant> {
        self.state
            .lock()
            .expect("journal state poisoned")
            .standing_grants
            .values()
            .filter(|g| g.is_live_at(now_millis))
            .cloned()
            .collect()
    }

    /// A snapshot of the currently parked approvals, oldest first.
    pub fn pending(&self) -> Vec<PendingApproval> {
        let state = self.state.lock().expect("journal state poisoned");
        let mut out: Vec<PendingApproval> = state
            .parked
            .iter()
            .map(|(id, parked)| PendingApproval {
                id: id.clone(),
                effect: parked.effect.clone(),
                at_millis: parked.at_millis,
                deadline_anchor_millis: parked.deadline_anchor_millis,
                task: parked.task.clone(),
                thread: parked.thread.clone(),
                // Issue #842: the turn key the entry already carries for #469's
                // continuation counter, read out rather than recomputed. The
                // two must name the same set — the batch the operator is asked
                // about in one card is precisely the batch the runtime holds a
                // single continuation for — and reading one field is how that
                // stays true without a rule anyone has to remember.
                batch: parked.cycle.clone(),
            })
            .collect();
        out.sort_by(|a, b| {
            a.at_millis
                .cmp(&b.at_millis)
                .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
        });
        out
    }

    /// Appends one record, whole, and does not return until the sink has made it
    /// durable to the level the record asked for.
    ///
    /// **Durability is per record kind, by decision (issue #392).** Every record
    /// declares which failure it must outlast through
    /// [`JournalRecord::durability`], and this is the single choke point that
    /// passes that decision to the sink:
    ///
    /// * The three unconditional [`Durability::Host`] kinds — `EffectExecuted`,
    ///   `GrantConsumed` and `StandingGrantRevoked` — are on stable storage
    ///   before this returns. So the at-most-once contract holds against
    ///   **losing the machine** for precisely the records whose loss would
    ///   repeat an external action, and a failed flush fails the append — which
    ///   aborts `execute_effect_once` before `perform_effect` and so cannot
    ///   produce the duplicate it is guarding against.
    /// * `ApprovalParked` is [`Durability::Host`] **for a workflow gate only**
    ///   (issue #1145), and `Process` for every other park. It is the one kind
    ///   whose level is decided by its contents rather than by its tag, because
    ///   the reasoning behind `Process` is a property of the caller: a chat turn
    ///   re-enters its gate and re-parks, a paused workflow run has already
    ///   settled and never will. For that run the parked effect *is* the
    ///   continuation, so its loss strands the run permanently rather than
    ///   costing a second question.
    /// * The other nine are [`Durability::Process`]: killing the process cannot
    ///   lose them, a host crash can. That is the decision, not a gap left open.
    ///   Losing any of them makes the runtime **re-ask** — an approval is parked
    ///   again, an operator is prompted again, a cycle bracket reads as
    ///   interrupted — and never re-fire. Flushing them would tax the journal's
    ///   highest-volume records to protect against a re-asked question.
    ///
    /// What each backend does to honour the two levels is its own business and
    /// is documented on [`append_journal`](JournalStore::append_journal): an
    /// `O_APPEND` write with (or without) a `sync_data`, a sqlite commit under
    /// `synchronous=FULL` (or `NORMAL`), a mongodb insert with (or without)
    /// `j:true`.
    ///
    /// Issue #726 removed the bound this used to carry. The journal was
    /// constructed unconditionally on the filesystem, so a hosted tenant whose
    /// `/data` is ephemeral scratch did not keep its journal across a container
    /// replacement — let alone a host crash — and gained nothing from the flush.
    /// The sink now comes from the selected storage backend, so the flush is
    /// bought on a volume that outlives the container.
    ///
    /// The write lock is taken **around the store call**, not merely around the
    /// serialisation, and that is load-bearing: a backend that allocates a
    /// sequence number inside the append would otherwise be free to allocate two
    /// concurrent appends out of call order, and a park replayed after the
    /// resolution that drains it resurrects a resolved approval.
    async fn append(&self, record: &JournalRecord) -> Result<()> {
        let line = serde_json::to_string(record)?;
        let durability = record.durability();
        let _guard = self.write_lock.lock().await;
        self.store
            .append_journal(&self.company, &line, durability)
            .await
    }
}

/// Parses one journal line into the record or records it holds.
///
/// The healthy answer is one record. A line written by a pre-#386 host may hold
/// **two or more** with nothing between them, because `append` used to emit a
/// record and its newline as separate unflushed writes and the newline could
/// lose the race. `serde_json`'s stream deserializer reads concatenated values
/// natively, so such a line replays *in full* instead of being dropped — which
/// matters because dropping one would silently un-commit an `EffectExecuted`
/// key and let an at-most-once effect run a second time. Recovering the merge
/// is not a nicety; it is the difference between a cosmetic repair and a
/// duplicated payment.
///
/// A line that is truncated rather than merged — a crash partway through a
/// write, a filesystem that lost a tail — has no valid parse and is reported.
/// All-or-nothing per line: half a line applied is worse than none, because the
/// caller would have no way to know which half it got.
fn replay_line(line: &str) -> std::result::Result<Vec<JournalRecord>, String> {
    let single = match serde_json::from_str::<JournalRecord>(line) {
        Ok(record) => return Ok(vec![record]),
        Err(e) => e,
    };
    match serde_json::Deserializer::from_str(line)
        .into_iter::<JournalRecord>()
        .collect::<std::result::Result<Vec<_>, _>>()
    {
        Ok(records) if !records.is_empty() => Ok(records),
        // Report the single-value error, not the stream one: it is the error
        // that describes the line as it was meant to be written.
        _ => Err(single.to_string()),
    }
}

impl std::fmt::Debug for RuntimeJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeJournal")
            .field("company", &self.company)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "journal_tests_core.rs"]
mod tests_core;
#[cfg(test)]
#[path = "journal_tests_part1.rs"]
mod tests_part1;
#[cfg(test)]
#[path = "journal_tests_part3.rs"]
mod tests_part3;
#[cfg(test)]
#[path = "journal_tests_part4.rs"]
mod tests_part4;
#[cfg(test)]
#[path = "journal_tests_the_cycle_bracket_issue_390.rs"]
mod tests_the_cycle_bracket_issue_390;
