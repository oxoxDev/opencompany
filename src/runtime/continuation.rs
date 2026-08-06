//! One turn, one continuation (issue #469).
//!
//! A single agent turn can park several approvals — four `composio_execute`
//! calls with different arguments is a real, reported case. Before this, each
//! resolve spawned its own follow-up cycle, so approving all four re-ran the
//! same turn four times. The re-runs did not race (the per-company serial lock
//! in [`CycleRunner::run`](crate::runtime::CycleRunner::run) holds for a whole
//! cycle, so they queued), but they were four full agent turns over one turn's
//! worth of work, each told about one decision and blind to the other three.
//!
//! The missing fact was that the four belonged together. Nothing on a parked
//! approval said so: the thread key is shared by every turn in a conversation,
//! and the task and run keys are absent for exactly the case that matters most,
//! a chat turn. So [`ApprovalOrigin::cycle`](crate::runtime::journal::ApprovalOrigin::cycle)
//! now records the parking cycle, and this queue counts, per turn, how many of
//! its approvals are still undecided.
//!
//! The rule it enforces: **a turn is continued once, when the last decision it
//! was blocked on lands**, carrying every `ApprovalResolved` event the turn
//! accumulated on the way. That is the same continuation whether the operator
//! decides the four together or one at a time over a minute — the trigger is
//! the last decision, not the clock, so the two orders cannot diverge.
//!
//! ## Why counting, and not asking the journal
//!
//! "Is anything from this turn still parked?" is answerable from the journal,
//! and answering it there is racy. Resolves arrive over HTTP concurrently even
//! though the cycles they spawn do not: two requests can each un-park their own
//! approval and then each read a queue that looks empty, and both would run a
//! continuation. Counting here closes that by construction — the decrement and
//! the "was that the last one" test happen under one lock, so exactly one
//! caller is ever handed the batch.
//!
//! ## What is deliberately not gated
//!
//! An approval with no turn key — a journal line written before #469 — is never
//! armed, and [`decide`](ContinuationQueue::decide) hands its event straight
//! back. Those continue on their own exactly as they always did.
//!
//! ## The cost, stated plainly
//!
//! A turn whose approvals are only *partly* decided now waits, where before it
//! re-dispatched the approved ones immediately. That is the honest state — the
//! turn really is still blocked — and the alternative is what this issue
//! reports: a re-run that re-parks everything still undecided, which compounds
//! with every decision. The wait is bounded: an undecided approval expires to a
//! default-deny on the gate's TTL, and that expiry is a decision like any other
//! (see [`CompanyRuntime::sweep_expired_approvals`](crate::company::runtime::CompanyRuntime::sweep_expired_approvals)),
//! so the turn is always eventually continued rather than stranded.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::ports::types::CompanyEvent;

/// What a turn is still waiting on, and what it has collected so far.
#[derive(Default)]
struct PendingTurn {
    /// How many of this turn's approvals are still undecided.
    outstanding: usize,
    /// The `ApprovalResolved` events its decided approvals owe the brain, in
    /// decision order. Handed to the continuation cycle as one batch.
    decided: Vec<CompanyEvent>,
}

/// Per-turn continuation state: how many approvals each turn is still blocked
/// on, and the decisions it has banked (issue #469).
///
/// Cheap to [`Clone`] — a shared handle, like every other queue in the runtime —
/// so the parking side (the cycle host) and the resolving side (the runtime)
/// see one set of counters.
#[derive(Clone, Default)]
pub struct ContinuationQueue {
    inner: Arc<Mutex<HashMap<String, PendingTurn>>>,
}

impl ContinuationQueue {
    /// Records that `turn` just parked one more approval.
    ///
    /// Called from the one place approvals are parked, so the count cannot
    /// drift from the queue it describes.
    pub fn arm(&self, turn: &str) {
        self.inner
            .lock()
            .expect("continuation queue poisoned")
            .entry(turn.to_string())
            .or_default()
            .outstanding += 1;
    }

    /// Records one decision on `turn`, banking `event` for the continuation.
    ///
    /// Returns `Some(batch)` **exactly when this was the last decision the turn
    /// was blocked on** — the caller then owes one continuation cycle over the
    /// whole batch, and the turn's state is dropped. `None` means the turn is
    /// still waiting on another decision and the caller owes nothing.
    ///
    /// `event` is `None` for a decision that carries no event for the brain —
    /// an expiry, whose `ApprovalResolved` the sweep has already appended
    /// itself. It still counts as a decision, because it still unblocks.
    ///
    /// A turn this queue never armed is not gated: the event comes straight
    /// back as a batch of one. That covers a pre-#469 journal line and a
    /// restart that lost the counter, and in both cases it is the pre-#469
    /// behaviour rather than a turn that never continues.
    pub fn decide(&self, turn: &str, event: Option<CompanyEvent>) -> Option<Vec<CompanyEvent>> {
        let mut guard = self.inner.lock().expect("continuation queue poisoned");
        let Some(pending) = guard.get_mut(turn) else {
            return Some(event.into_iter().collect());
        };
        if let Some(event) = event {
            pending.decided.push(event);
        }
        pending.outstanding = pending.outstanding.saturating_sub(1);
        if pending.outstanding > 0 {
            return None;
        }
        guard.remove(turn).map(|pending| pending.decided)
    }

    /// Re-arms the counters after a journal replay, from the turn key of every
    /// approval that is still parked — one entry per approval.
    ///
    /// **Idempotent**: it *sets* each turn's count to what the journal says
    /// rather than adding to it, so replaying twice (boot loads the journal, and
    /// [`CompanyRuntime::recover`](crate::company::runtime::CompanyRuntime::recover)
    /// can be driven again) leaves a turn blocked on the number of decisions it
    /// is actually blocked on. Any decisions already banked for a turn are kept:
    /// the count is what replay knows, the bank is what this process has seen.
    ///
    /// A handed-over runtime inherits the live queue instead and never comes
    /// through here — the counters there include decisions the journal has
    /// already recorded as resolved.
    pub fn rearm(&self, turns: impl IntoIterator<Item = String>) {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for turn in turns {
            *counts.entry(turn).or_default() += 1;
        }
        let mut guard = self.inner.lock().expect("continuation queue poisoned");
        for (turn, count) in counts {
            guard.entry(turn).or_default().outstanding = count;
        }
    }

    /// How many turns are still waiting on a decision.
    pub fn waiting(&self) -> usize {
        self.inner
            .lock()
            .expect("continuation queue poisoned")
            .len()
    }

    /// How many decisions `turn` is still blocked on. `0` for a turn this queue
    /// is not tracking.
    pub fn outstanding(&self, turn: &str) -> usize {
        self.inner
            .lock()
            .expect("continuation queue poisoned")
            .get(turn)
            .map(|p| p.outstanding)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::ports::types::{Actor, ActorKind, ApprovalId, Verdict};

    fn resolved(id: &str) -> CompanyEvent {
        CompanyEvent::ApprovalResolved {
            approval_id: ApprovalId::new(id),
            verdict: Verdict::Approve,
            by: Actor {
                kind: ActorKind::Operator,
                id: "operator".into(),
            },
        }
    }

    fn ids(batch: &[CompanyEvent]) -> Vec<String> {
        batch
            .iter()
            .map(|e| match e {
                CompanyEvent::ApprovalResolved { approval_id, .. } => approval_id.to_string(),
                _ => unreachable!(),
            })
            .collect()
    }

    /// The keystone: four approvals from one turn owe **one** continuation, and
    /// it carries all four decisions.
    #[test]
    fn a_turn_continues_once_after_its_last_decision() {
        let q = ContinuationQueue::default();
        for _ in 0..4 {
            q.arm("cycle-1");
        }

        assert!(q.decide("cycle-1", Some(resolved("a"))).is_none());
        assert!(q.decide("cycle-1", Some(resolved("b"))).is_none());
        assert!(q.decide("cycle-1", Some(resolved("c"))).is_none());
        let batch = q
            .decide("cycle-1", Some(resolved("d")))
            .expect("the last decision unblocks the turn");

        assert_eq!(ids(&batch), vec!["a", "b", "c", "d"]);
        assert_eq!(q.waiting(), 0, "the turn's state is dropped once it runs");
    }

    /// Two turns blocked at once do not unblock each other — the whole reason
    /// the key is the parking cycle and not the thread they share.
    #[test]
    fn turns_are_independent() {
        let q = ContinuationQueue::default();
        q.arm("cycle-1");
        q.arm("cycle-1");
        q.arm("cycle-2");

        assert!(q.decide("cycle-1", Some(resolved("a"))).is_none());
        let second = q
            .decide("cycle-2", Some(resolved("z")))
            .expect("cycle-2 was blocked on one decision only");
        assert_eq!(ids(&second), vec!["z"]);
        assert_eq!(q.outstanding("cycle-1"), 1, "cycle-1 still waits");

        let first = q.decide("cycle-1", Some(resolved("b"))).expect("unblocked");
        assert_eq!(ids(&first), vec!["a", "b"]);
    }

    /// A turn that parked exactly one approval continues on that decision, so
    /// the ordinary single-approval path is byte-for-byte what it was.
    #[test]
    fn a_single_approval_turn_continues_immediately() {
        let q = ContinuationQueue::default();
        q.arm("cycle-1");
        let batch = q.decide("cycle-1", Some(resolved("a"))).expect("unblocked");
        assert_eq!(ids(&batch), vec!["a"]);
    }

    /// An approval with no turn key — a pre-#469 journal line — is never gated.
    #[test]
    fn an_unarmed_turn_continues_alone() {
        let q = ContinuationQueue::default();
        let batch = q
            .decide("never-armed", Some(resolved("a")))
            .expect("an unknown turn is not a turn to wait on");
        assert_eq!(ids(&batch), vec!["a"]);
    }

    /// An expiry carries no event of its own — the sweep appends that itself —
    /// but it is still a decision, so it still unblocks the turn.
    #[test]
    fn an_expiry_counts_as_a_decision() {
        let q = ContinuationQueue::default();
        q.arm("cycle-1");
        q.arm("cycle-1");

        assert!(q.decide("cycle-1", Some(resolved("a"))).is_none());
        let batch = q
            .decide("cycle-1", None)
            .expect("the expiry was the last thing the turn waited on");
        assert_eq!(
            ids(&batch),
            vec!["a"],
            "the expiry contributes no event, only the unblocking"
        );
    }

    /// A turn whose every approval expired unblocks with nothing to say. The
    /// caller must be able to tell that from "one decision arrived", because an
    /// empty batch owes no cycle at all.
    #[test]
    fn a_wholly_expired_turn_yields_an_empty_batch() {
        let q = ContinuationQueue::default();
        q.arm("cycle-1");
        assert_eq!(
            q.decide("cycle-1", None).expect("unblocked"),
            Vec::new(),
            "nothing to tell the brain"
        );
    }

    /// Recovery rebuilds the counters, so a restart mid-turn comes back still
    /// knowing that turn is blocked rather than continuing it early.
    #[test]
    fn recovery_rearms_the_outstanding_counts() {
        let q = ContinuationQueue::default();
        q.rearm(vec![
            "cycle-1".to_string(),
            "cycle-1".to_string(),
            "cycle-2".to_string(),
        ]);
        assert_eq!(q.outstanding("cycle-1"), 2);
        assert_eq!(q.outstanding("cycle-2"), 1);

        assert!(q.decide("cycle-1", Some(resolved("a"))).is_none());
        assert!(q.decide("cycle-1", Some(resolved("b"))).is_some());
    }

    /// The decrement and the "was that the last one" test are one critical
    /// section, so concurrent resolves cannot both be handed a batch. Exactly
    /// one of eight threads may run the continuation.
    #[test]
    fn concurrent_decisions_hand_the_batch_to_exactly_one_caller() {
        let q = ContinuationQueue::default();
        for _ in 0..8 {
            q.arm("cycle-1");
        }

        let batches = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for i in 0..8 {
            let q = q.clone();
            let batches = Arc::clone(&batches);
            handles.push(std::thread::spawn(move || {
                if let Some(batch) = q.decide("cycle-1", Some(resolved(&format!("a{i}")))) {
                    batches.lock().unwrap().push(batch);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let batches = batches.lock().unwrap();
        assert_eq!(batches.len(), 1, "two continuations for one turn");
        assert_eq!(
            batches[0].len(),
            8,
            "the single continuation carries every decision"
        );
    }
}
