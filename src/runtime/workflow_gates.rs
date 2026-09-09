//! One workflow run, one continuation (issue #978).
//!
//! # The amplification this closes
//!
//! A run that fans out to N gated nodes parks N cards. Before this, approving
//! one of them re-dispatched the **whole run** on the spot — the spawn lived in
//! `perform_effect`, which fires once per approved effect — and the replay
//! carried an `approvals` array naming only that one node, so the other N-1
//! paused again and parked again. Three approvals produced three runs and six
//! new cards; the reported staging tenant went 3 → 6 → 12 → 24 and accumulated
//! 77 runs of one disabled workflow.
//!
//! [`ContinuationQueue`](crate::runtime::continuation::ContinuationQueue) already
//! solves the shape of this problem for an agent turn: count the outstanding
//! decisions, release once when the last lands. Issue #978 makes a workflow run
//! a turn in exactly that sense, keyed by
//! [`workflow_turn_key`](crate::runtime::workflow_resume::workflow_turn_key).
//! This module is the half that queue cannot supply.
//!
//! # Why a second structure, and not a field on the batch
//!
//! `ContinuationQueue::decide` hands back `ApprovalResolved` events: an id, a
//! verdict, an actor. To re-dispatch a run the host needs two things that are
//! not in there — **which graph node** each id was gating, and the paused run's
//! **trigger input** with its delivery / outward-call ledgers.
//!
//! Neither is recoverable from the journal at release time, and that is not an
//! oversight to route around. `RuntimeJournal` drops the parked entry on resolve
//! (`parked.remove`), and the record it *does* retain past resolution —
//! `approval_effects` — is deliberately **payload-scrubbed** (issue #351), so a
//! resolved gate's id, input and ledgers are all gone by construction. Widening
//! that scrub to keep them would reopen a privacy rule for the benefit of one
//! caller.
//!
//! So the facts are stashed here instead, at **park** time, from the one place
//! gates are parked.
//!
//! # Why park time, and not approve time
//!
//! Because **deny** never reaches the approve path. A denied effect resolves to
//! [`ResolveOutcome`](crate::policy::ResolveOutcome)`::Denied` and never touches
//! `perform_effect`, so a stash populated when an approval is *performed* would
//! hold no node id for a refusal — and the continuation, not knowing the node
//! was refused, would replay into it, pause, and park a fresh card. An approval
//! round that cleared three and created one is the same defect in miniature.
//!
//! Arming at park time covers approve, deny and TTL expiry with one mechanism,
//! which is what makes the denial ledger
//! ([`PAYLOAD_DENIED`](crate::runtime::workflow_resume::PAYLOAD_DENIED))
//! expressible at all.
//!
//! # One representative effect per run, not one per gate
//!
//! Every gate of a single run is built by one `park_pending_gates` loop from one
//! `trigger_input`, one run-level `deliveries` slice and one run-level
//! `performed` slice, and `gate_effect` derives the two ledgers purely from
//! those. So `input` / `delivered` / `performed` are byte-identical across
//! siblings **by construction** — including the case where an earlier node
//! already delivered before the fan-out, because that delivery is in the
//! run-level list every sibling is handed, not in a per-branch subset. Only
//! `node_id`, the call description and the per-gate upstream `content` vary, and
//! a continuation needs none of those. Keeping one effect per run rather than
//! one per gate is what stops this from holding N copies of a trigger input.
//!
//! # Durability, stated plainly
//!
//! In-memory, on exactly [`ContinuationQueue`]'s terms, and rehydrated at
//! recovery from the journal's still-parked gates
//! ([`rearm`](WorkflowGateQueue::rearm)) the way that queue is rehydrated from
//! `parked_turns`. The limit is inherited rather than added: a restart in the
//! middle of a partly-decided run comes back knowing only the gates still
//! parked, so a batch released after it carries the last decision and not the
//! ones banked before it. Those un-carried siblings re-park. That is pre-#469
//! behaviour for agent turns; #978 is where a workflow run starts feeling it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::ports::types::{ApprovalId, Effect, Verdict};
use crate::runtime::workflow_resume::gate_node_id;

/// What one workflow run's gate batch has settled into, handed to the caller
/// that released it.
#[derive(Clone, Debug)]
pub struct ReleasedGates {
    /// Any one of the run's parked gate effects — they agree on everything a
    /// continuation reads. See the module docs for why one is enough.
    pub effect: Effect,
    /// The gate nodes the operator approved, in decision order.
    pub approved: Vec<String>,
    /// The gate nodes the operator refused, or that expired to a default-deny,
    /// in decision order. A continuation neither runs these nor re-asks.
    pub denied: Vec<String>,
}

/// One run's parked gates, and the verdicts landed on them so far.
#[derive(Clone, Debug)]
struct Batch {
    effect: Effect,
    /// Gate nodes still awaiting a verdict, by the approval deciding them.
    undecided: HashMap<ApprovalId, String>,
    approved: Vec<String>,
    denied: Vec<String>,
}

/// Per-run gate state: which node each parked approval is gating, and what the
/// run's trigger input was (issue #978).
///
/// Cheap to [`Clone`] — a shared handle like every other queue in the runtime —
/// so the parking side (the workflow runner, through `DeliveryParking`) and the
/// resolving side (the runtime) see one set of batches.
#[derive(Clone, Default)]
pub struct WorkflowGateQueue {
    inner: Arc<Mutex<HashMap<String, Batch>>>,
    /// The company's emergency-stop flag, consulted by [`release`](Self::release)
    /// under the same lock that takes the batch — the same treatment
    /// `RunSupervisor::begin` gives its own admission check, and for the same
    /// reason: every caller has already asked
    /// `CompanyRuntime::ensure_not_emergency_stopped` earlier, but that ask sits
    /// behind at least one `.await` before release is reached.
    ///
    /// `None` at the default construction every test uses, so nothing here
    /// changes for a queue with no company to ask.
    emergency: Option<Arc<crate::policy::gate::ManifestApprovalGate>>,
}

/// Why [`WorkflowGateQueue::release`] did not hand back a batch.
#[derive(Debug)]
pub enum ReleaseRefusal {
    /// The company is stopped. The batch is untouched — still sitting in the
    /// queue with every verdict it already banked, ready for
    /// [`ready_for_release`](WorkflowGateQueue::ready_for_release) to find it
    /// once an operator releases the stop.
    EmergencyStop,
}

impl WorkflowGateQueue {
    /// Installs the emergency-stop flag [`release`](Self::release) refuses a
    /// decided batch against.
    ///
    /// Without this the queue releases regardless of the flag — the default
    /// for every construction site that has no company to ask.
    pub fn with_emergency_gate(
        mut self,
        gate: Arc<crate::policy::gate::ManifestApprovalGate>,
    ) -> Self {
        self.emergency = Some(gate);
        self
    }
    /// Records that `turn` just parked one more gate, `id` deciding it.
    ///
    /// Called from the one place gates are parked and on its **successful-park
    /// path only**, immediately beside
    /// [`ContinuationQueue::arm`](crate::runtime::continuation::ContinuationQueue::arm),
    /// so the two cannot disagree about how many decisions a run is blocked on.
    /// Arming on a dedupe-skip or a failed park would leave the run waiting for
    /// a decision no card can ever deliver.
    ///
    /// A non-gate effect is ignored rather than stored: `gate_node_id` checks the
    /// kind, so nothing but a `workflow.approve` card can enter a batch.
    ///
    /// The first gate through sets the batch's representative effect; later ones
    /// do not replace it, because they agree with it on everything read back.
    pub fn arm(&self, turn: &str, id: &ApprovalId, effect: &Effect) {
        let Some(node) = gate_node_id(effect) else {
            return;
        };
        let node = node.to_string();
        let mut guard = self.inner.lock().expect("workflow gate queue poisoned");
        guard
            .entry(turn.to_string())
            .or_insert_with(|| Batch {
                effect: effect.clone(),
                undecided: HashMap::new(),
                approved: Vec::new(),
                denied: Vec::new(),
            })
            .undecided
            .insert(id.clone(), node);
    }

    /// Banks one verdict on `turn`.
    ///
    /// Recorded here rather than derived from the released
    /// [`ContinuationQueue`](crate::runtime::continuation::ContinuationQueue)
    /// batch because that batch cannot describe every decision: a TTL expiry
    /// contributes no event at all (the sweep appends its own), so a run whose
    /// last gate timed out would otherwise release with the expired node in
    /// neither ledger — and the continuation would replay into it and park a
    /// fresh card.
    ///
    /// A decision on a turn this queue is not tracking, or on an id it never
    /// armed, is a no-op: the caller is deciding something that is not a
    /// run-scoped workflow gate.
    pub fn decide(&self, turn: &str, id: &ApprovalId, verdict: Verdict) {
        let mut guard = self.inner.lock().expect("workflow gate queue poisoned");
        let Some(batch) = guard.get_mut(turn) else {
            return;
        };
        let Some(node) = batch.undecided.remove(id) else {
            return;
        };
        let ledger = match verdict {
            Verdict::Approve => &mut batch.approved,
            Verdict::Deny => &mut batch.denied,
        };
        if !ledger.contains(&node) {
            ledger.push(node);
        }
    }

    /// Takes `turn`'s whole batch, dropping it from the queue.
    ///
    /// Called once, by whichever caller the continuation queue handed the
    /// release to — that queue's counting decides who, under one lock, so this
    /// cannot be entered twice for one run.
    ///
    /// Refuses — ahead of taking the batch — while the company's emergency
    /// stop is engaged, when [`with_emergency_gate`](Self::with_emergency_gate)
    /// installed one. **Codex review finding on PR #2140 (`3955615141`):**
    /// before this, a mixed batch (at least one gate approved before the pause,
    /// the last sibling expiring while paused) was removed here and then
    /// refused two awaits later at `RunSupervisor::begin` — by which point
    /// `resume_run`'s caller had nothing left to preserve and pruned the
    /// checkpoint, discarding already-approved work an operator had to notice
    /// and manually re-run. Checking here, under the same lock that takes the
    /// batch, leaves it fully intact — every verdict still banked — for
    /// [`ready_for_release`](Self::ready_for_release) to hand back once the
    /// stop lifts, instead of destroying it on the way to a refusal two frames
    /// downstream would have made anyway.
    pub fn release(&self, turn: &str) -> Result<Option<ReleasedGates>, ReleaseRefusal> {
        let mut guard = self.inner.lock().expect("workflow gate queue poisoned");
        if self
            .emergency
            .as_deref()
            .is_some_and(|gate| gate.is_emergency())
        {
            return Err(ReleaseRefusal::EmergencyStop);
        }
        Ok(guard.remove(turn).map(|batch| ReleasedGates {
            effect: batch.effect,
            approved: batch.approved,
            denied: batch.denied,
        }))
    }

    /// Every turn this queue is holding whose gates are all decided but which
    /// has not yet been released — the batches an emergency stop's own
    /// [`release`](Self::release) refusal left behind, ready for
    /// `CompanyRuntime::emergency_resume` to hand to
    /// [`resume_run`](crate::runtime::workflow_resume::resume_run) once the
    /// stop lifts.
    pub fn ready_for_release(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("workflow gate queue poisoned")
            .iter()
            .filter(|(_, batch)| batch.undecided.is_empty())
            .map(|(turn, _)| turn.clone())
            .collect()
    }

    /// Whether `turn` is a run-scoped batch this queue is holding.
    ///
    /// What the approve path forks on: a gate whose run is armed defers to the
    /// batch release, and one that is not — a card parked by a build from before
    /// this issue, whose journal line carries no turn key — re-dispatches
    /// immediately, exactly as it always did.
    pub fn is_armed(&self, turn: &str) -> bool {
        self.inner
            .lock()
            .expect("workflow gate queue poisoned")
            .contains_key(turn)
    }

    /// How many of `turn`'s gates are still undecided. `0` for a turn this queue
    /// is not tracking.
    pub fn undecided(&self, turn: &str) -> usize {
        self.inner
            .lock()
            .expect("workflow gate queue poisoned")
            .get(turn)
            .map(|batch| batch.undecided.len())
            .unwrap_or(0)
    }

    /// Rebuilds the batches from every gate the journal still has parked, one
    /// entry per approval.
    ///
    /// **Idempotent**, on
    /// [`ContinuationQueue::rearm`](crate::runtime::continuation::ContinuationQueue::rearm)'s
    /// terms: it *replaces* each turn it sees rather than adding to it, so
    /// replaying twice (boot loads the journal, and `recover` can be driven
    /// again) leaves a run blocked on the gates it is actually blocked on.
    ///
    /// Verdicts banked by **this** process are dropped along with the rest of
    /// the turn, which is the honest reading: a rehydrate is only reached from a
    /// journal replay, and the journal records that an approval resolved without
    /// recording what it was gating. See the module docs on durability.
    pub fn rearm<'e>(&self, gates: impl IntoIterator<Item = (String, ApprovalId, &'e Effect)>) {
        let mut rebuilt: HashMap<String, Batch> = HashMap::new();
        for (turn, id, effect) in gates {
            let Some(node) = gate_node_id(effect) else {
                continue;
            };
            rebuilt
                .entry(turn)
                .or_insert_with(|| Batch {
                    effect: effect.clone(),
                    undecided: HashMap::new(),
                    approved: Vec::new(),
                    denied: Vec::new(),
                })
                .undecided
                .insert(id, node.to_string());
        }
        let mut guard = self.inner.lock().expect("workflow gate queue poisoned");
        for (turn, batch) in rebuilt {
            guard.insert(turn, batch);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::ports::types::EffectGroup;
    use crate::runtime::workflow_resume::gate_effect;
    use serde_json::json;

    fn emergency_gate_effect(node: &str) -> Effect {
        Effect {
            kind: crate::runtime::workflow_resume::WORKFLOW_APPROVE_KIND.to_string(),
            group: EffectGroup::Other,
            amount_usd: None,
            established_thread: false,
            first_time_counterparty: false,
            payload: serde_json::json!({
                crate::runtime::workflow_resume::PAYLOAD_NODE_ID: node,
            }),
            agent: None,
            run_id: Some("wr-1".to_string()),
        }
    }

    fn gate(workflow: &str, node: &str) -> Effect {
        gate_effect(workflow, node, &json!({}), "run-1", &[], &[], None)
    }

    fn non_gate() -> Effect {
        Effect {
            kind: "payment.send".to_string(),
            group: EffectGroup::Spend,
            amount_usd: Some(10.0),
            established_thread: false,
            first_time_counterparty: false,
            payload: json!({}),
            agent: None,
            run_id: None,
        }
    }

    fn gate_kind_effect_without_node_id() -> Effect {
        Effect {
            kind: crate::runtime::workflow_resume::WORKFLOW_APPROVE_KIND.to_string(),
            group: EffectGroup::Other,
            amount_usd: None,
            established_thread: false,
            first_time_counterparty: false,
            payload: json!({}),
            agent: None,
            run_id: Some("wr-1".to_string()),
        }
    }

    fn gate_kind_effect_with_blank_node_id() -> Effect {
        Effect {
            kind: crate::runtime::workflow_resume::WORKFLOW_APPROVE_KIND.to_string(),
            group: EffectGroup::Other,
            amount_usd: None,
            established_thread: false,
            first_time_counterparty: false,
            payload: json!({
                crate::runtime::workflow_resume::PAYLOAD_NODE_ID: "   ",
            }),
            agent: None,
            run_id: Some("wr-1".to_string()),
        }
    }

    /// **Codex review finding on PR #2140 (`3955615141`).** Before this,
    /// `release` removed a fully-decided batch unconditionally, and only the
    /// caller two `.await`s downstream (`RunSupervisor::begin`) could refuse
    /// it — by which point the batch was already gone and its checkpoint got
    /// pruned as a terminal failure. This proves the batch survives a refusal
    /// here instead: still queryable, every verdict still banked.
    #[test]
    fn release_refuses_and_preserves_a_decided_batch_while_stopped() {
        let gate = Arc::new(crate::policy::gate::ManifestApprovalGate::new(
            crate::company::Policy {
                mode: "full".to_string(),
                always_approve: Vec::new(),
                auto_approve_under_usd: None,
                approval_ttl_hours: None,
            },
        ));
        let queue = WorkflowGateQueue::default().with_emergency_gate(gate.clone());

        let a = ApprovalId::new("appr-a");
        let b = ApprovalId::new("appr-b");
        queue.arm("turn-1", &a, &emergency_gate_effect("node-a"));
        queue.arm("turn-1", &b, &emergency_gate_effect("node-b"));
        queue.decide("turn-1", &a, Verdict::Approve);
        queue.decide("turn-1", &b, Verdict::Deny);
        assert_eq!(
            queue.undecided("turn-1"),
            0,
            "both gates on the turn are decided"
        );

        gate.set_emergency(true);
        match queue.release("turn-1") {
            Err(ReleaseRefusal::EmergencyStop) => {}
            Ok(_) => panic!("a decided batch must be refused, not released, while stopped"),
        }

        assert!(
            queue.is_armed("turn-1"),
            "the refused release must leave the batch in the queue, not destroy it"
        );
        assert_eq!(
            queue.ready_for_release(),
            vec!["turn-1".to_string()],
            "the preserved, fully-decided batch is discoverable for a later redrive"
        );

        gate.set_emergency(false);
        let released = queue
            .release("turn-1")
            .expect("not stopped, so release succeeds")
            .expect("the batch is still there");
        assert_eq!(released.approved, vec!["node-a".to_string()]);
        assert_eq!(released.denied, vec!["node-b".to_string()]);
        assert!(
            !queue.is_armed("turn-1"),
            "a successful release still takes the batch out of the queue"
        );
    }

    /// A queue built with no [`with_emergency_gate`](WorkflowGateQueue::with_emergency_gate)
    /// call — every construction site with no company to ask — releases
    /// regardless of any flag, exactly as before this refusal existed.
    #[test]
    fn release_ignores_emergency_state_with_no_gate_installed() {
        let queue = WorkflowGateQueue::default();
        let a = ApprovalId::new("appr-a");
        queue.arm("turn-1", &a, &emergency_gate_effect("node-a"));
        queue.decide("turn-1", &a, Verdict::Approve);

        queue
            .release("turn-1")
            .expect("no gate installed, so nothing here can refuse on that basis")
            .expect("the batch is there to release");
    }

    /// The core loop the module exists for: two gates park on one run, one
    /// approved and one denied, and release hands back exactly that split on
    /// the one representative effect.
    #[test]
    fn arm_decide_release_splits_approved_and_denied() {
        let q = WorkflowGateQueue::default();
        let id_a = ApprovalId::new("a");
        let id_b = ApprovalId::new("b");
        q.arm("turn-1", &id_a, &gate("wf", "node-a"));
        q.arm("turn-1", &id_b, &gate("wf", "node-b"));
        assert_eq!(q.undecided("turn-1"), 2);

        q.decide("turn-1", &id_a, Verdict::Approve);
        assert_eq!(q.undecided("turn-1"), 1);
        q.decide("turn-1", &id_b, Verdict::Deny);
        assert_eq!(q.undecided("turn-1"), 0);

        let released = q
            .release("turn-1")
            .expect("no emergency gate installed")
            .expect("a batch was armed");
        assert_eq!(released.approved, vec!["node-a".to_string()]);
        assert_eq!(released.denied, vec!["node-b".to_string()]);
    }

    /// `arm` only ever accepts a `workflow.approve` effect — the kind check is
    /// what stops a native effect (say, a payment) from silently being treated
    /// as a gate this queue must eventually release.
    #[test]
    fn arm_ignores_a_non_gate_effect() {
        let q = WorkflowGateQueue::default();
        let id = ApprovalId::new("a");
        q.arm("turn-1", &id, &non_gate());
        assert!(
            !q.is_armed("turn-1"),
            "a non-gate effect must not arm a batch"
        );
        assert_eq!(q.undecided("turn-1"), 0);
    }

    /// `arm` and `rearm` both delegate the "is this really a gate" question to
    /// `gate_node_id`, which is kind-checked AND rejects an absent or
    /// whitespace-only `node_id`. This proves that rejection actually stops a
    /// batch from forming — a `workflow.approve`-kind effect with no usable
    /// node id must arm nothing, the same as a wrong-kind effect, not a batch
    /// keyed on an empty string a continuation could never match a real card
    /// against.
    #[test]
    fn arm_and_rearm_ignore_a_gate_kind_effect_with_no_usable_node_id() {
        let q = WorkflowGateQueue::default();
        let id_a = ApprovalId::new("a");
        q.arm("turn-1", &id_a, &gate_kind_effect_without_node_id());
        assert!(
            !q.is_armed("turn-1"),
            "a gate-kind effect with no node_id key must not arm a batch"
        );

        let id_b = ApprovalId::new("b");
        q.arm("turn-1", &id_b, &gate_kind_effect_with_blank_node_id());
        assert!(
            !q.is_armed("turn-1"),
            "a gate-kind effect with a whitespace-only node_id must not arm a batch"
        );

        let missing = gate_kind_effect_without_node_id();
        q.rearm(vec![("turn-2".to_string(), id_a.clone(), &missing)]);
        assert!(
            !q.is_armed("turn-2"),
            "rearm must skip the same malformed gate rather than rehydrate a phantom batch"
        );
    }

    /// Deciding an id/turn this queue never armed — the shape of a stale or
    /// forged approval id — must be a silent no-op, not a panic and not a
    /// phantom entry that later corrupts a real release.
    #[test]
    fn decide_on_an_unarmed_turn_or_unknown_id_is_a_no_op() {
        let q = WorkflowGateQueue::default();
        // Unknown turn entirely.
        q.decide("ghost-turn", &ApprovalId::new("x"), Verdict::Approve);
        assert!(!q.is_armed("ghost-turn"));

        // Known turn, unknown id.
        let id = ApprovalId::new("a");
        q.arm("turn-1", &id, &gate("wf", "node-a"));
        q.decide("turn-1", &ApprovalId::new("not-armed"), Verdict::Deny);
        assert_eq!(
            q.undecided("turn-1"),
            1,
            "a decision on an id this batch never armed must not consume the real one"
        );
    }

    /// Deciding the same id twice — a retried resolve, or two callers racing
    /// one approval — must not double-count the node into the ledger the
    /// second time, since the first `decide` already removed it from
    /// `undecided`.
    #[test]
    fn deciding_the_same_id_twice_does_not_double_count() {
        let q = WorkflowGateQueue::default();
        let id = ApprovalId::new("a");
        q.arm("turn-1", &id, &gate("wf", "node-a"));
        q.decide("turn-1", &id, Verdict::Approve);
        q.decide("turn-1", &id, Verdict::Approve);
        let released = q
            .release("turn-1")
            .expect("no emergency gate installed")
            .expect("armed");
        assert_eq!(released.approved, vec!["node-a".to_string()]);
    }

    /// TTL expiry is documented to "contribute no event at all" beyond the
    /// sweep's own `decide` call — there is no `ApprovalResolved` behind it.
    /// This proves `decide` alone, with no prior continuation event, still
    /// moves the node into `denied` so the run does not replay into it.
    #[test]
    fn a_ttl_expiry_decide_with_no_prior_event_still_lands_in_denied() {
        let q = WorkflowGateQueue::default();
        let id = ApprovalId::new("a");
        q.arm("turn-1", &id, &gate("wf", "node-a"));
        // The sweep calls decide() directly; nothing else touched this queue.
        q.decide("turn-1", &id, Verdict::Deny);
        let released = q
            .release("turn-1")
            .expect("no emergency gate installed")
            .expect("armed");
        assert!(released.denied.contains(&"node-a".to_string()));
        assert!(released.approved.is_empty());
    }

    /// `release` takes the batch, so a second release (or any further decide)
    /// on the same turn finds nothing left to corrupt or double-release.
    #[test]
    fn release_drops_the_batch_and_a_repeat_release_is_none() {
        let q = WorkflowGateQueue::default();
        let id = ApprovalId::new("a");
        q.arm("turn-1", &id, &gate("wf", "node-a"));
        assert!(
            q.release("turn-1")
                .expect("no emergency gate installed")
                .is_some()
        );
        assert!(
            q.release("turn-1")
                .expect("no emergency gate installed")
                .is_none()
        );
        assert!(!q.is_armed("turn-1"));

        // A decide arriving after the release must not panic or resurrect it.
        q.decide("turn-1", &id, Verdict::Approve);
        assert!(!q.is_armed("turn-1"));
    }

    /// The first gate through sets the batch's representative effect; a
    /// second sibling gate for the same run must not replace it, since every
    /// gate of one run is documented to agree on everything but `node_id`.
    #[test]
    fn the_first_gates_effect_is_the_batchs_representative_effect() {
        let q = WorkflowGateQueue::default();
        let id_a = ApprovalId::new("a");
        let id_b = ApprovalId::new("b");
        let first = gate_effect(
            "wf",
            "node-a",
            &json!({"trigger": "one"}),
            "run-1",
            &[],
            &[],
            None,
        );
        let second = gate_effect(
            "wf",
            "node-b",
            &json!({"trigger": "different"}),
            "run-1",
            &[],
            &[],
            None,
        );
        q.arm("turn-1", &id_a, &first);
        q.arm("turn-1", &id_b, &second);
        q.decide("turn-1", &id_a, Verdict::Approve);
        q.decide("turn-1", &id_b, Verdict::Approve);
        let released = q
            .release("turn-1")
            .expect("no emergency gate installed")
            .expect("armed");
        assert_eq!(
            released.effect.payload, first.payload,
            "the representative effect must stay the first gate's, not the last"
        );
    }

    /// `rearm` is idempotent and *replaces* a turn rather than adding to it:
    /// replaying the same journal snapshot twice must not accumulate
    /// duplicate `undecided` entries for one gate.
    #[test]
    fn rearm_is_idempotent_and_does_not_accumulate() {
        let q = WorkflowGateQueue::default();
        let id = ApprovalId::new("a");
        let g = gate("wf", "node-a");
        q.rearm(vec![("turn-1".to_string(), id.clone(), &g)]);
        assert_eq!(q.undecided("turn-1"), 1);
        q.rearm(vec![("turn-1".to_string(), id.clone(), &g)]);
        assert_eq!(
            q.undecided("turn-1"),
            1,
            "replaying the same rehydrate twice must not double the undecided count"
        );
    }

    /// A verdict banked before a rearm is documented to be dropped along with
    /// the rest of the turn — the rehydrated batch only knows what the
    /// journal still shows parked, so a gate this process already decided
    /// (but that a fresh rearm sees as still-parked) comes back undecided.
    #[test]
    fn rearm_drops_verdicts_banked_before_it() {
        let q = WorkflowGateQueue::default();
        let id_a = ApprovalId::new("a");
        let id_b = ApprovalId::new("b");
        let ga = gate("wf", "node-a");
        let gb = gate("wf", "node-b");
        q.arm("turn-1", &id_a, &ga);
        q.arm("turn-1", &id_b, &gb);
        q.decide("turn-1", &id_a, Verdict::Approve);
        assert_eq!(q.undecided("turn-1"), 1);

        // A journal replay that still sees both gates parked rebuilds the
        // batch from scratch — the in-process approval of `id_a` is gone.
        q.rearm(vec![
            ("turn-1".to_string(), id_a.clone(), &ga),
            ("turn-1".to_string(), id_b.clone(), &gb),
        ]);
        assert_eq!(
            q.undecided("turn-1"),
            2,
            "rearm must rebuild from the journal's view, not preserve this process's own verdicts"
        );
    }

    /// `undecided` on a turn nobody ever armed is `0`, not a panic — the
    /// documented contract for an untracked turn.
    #[test]
    fn undecided_on_an_untracked_turn_is_zero() {
        let q = WorkflowGateQueue::default();
        assert_eq!(q.undecided("no-such-turn"), 0);
        assert!(!q.is_armed("no-such-turn"));
    }

    /// `ready_for_release` is the list `CompanyRuntime::emergency_resume`
    /// redrives once a stop lifts — handing it a batch that still has an
    /// undecided gate would replay a run one decision short. This proves the
    /// boundary: a batch with one of two gates decided is excluded, and only
    /// crossing into zero undecided makes it appear.
    #[test]
    fn ready_for_release_excludes_a_batch_with_a_gate_still_undecided() {
        let q = WorkflowGateQueue::default();
        let id_a = ApprovalId::new("a");
        let id_b = ApprovalId::new("b");
        q.arm("turn-1", &id_a, &gate("wf", "node-a"));
        q.arm("turn-1", &id_b, &gate("wf", "node-b"));

        q.decide("turn-1", &id_a, Verdict::Approve);
        assert_eq!(q.undecided("turn-1"), 1);
        assert!(
            q.ready_for_release().is_empty(),
            "a batch with one gate still undecided must not be offered for release"
        );

        q.decide("turn-1", &id_b, Verdict::Deny);
        assert_eq!(
            q.ready_for_release(),
            vec!["turn-1".to_string()],
            "the batch becomes ready only once every gate has landed"
        );
    }

    /// The emergency-stop refusal in `release` must hold at any point in a
    /// batch's life, not just once every gate has landed — and refusing
    /// release must not also freeze `decide`, or a verdict banked while
    /// stopped would have nowhere to go and the operator's decision would be
    /// silently lost. This proves both: release is refused on a batch with
    /// one gate still undecided, `decide` still lands the remaining verdict
    /// while the stop is engaged, and the now-fully-decided batch surfaces via
    /// `ready_for_release` before anyone releases it.
    #[test]
    fn emergency_stop_refuses_a_partial_batch_but_decide_still_banks_during_the_stop() {
        let emergency_gate = Arc::new(crate::policy::gate::ManifestApprovalGate::new(
            crate::company::Policy {
                mode: "full".to_string(),
                always_approve: Vec::new(),
                auto_approve_under_usd: None,
                approval_ttl_hours: None,
            },
        ));
        let q = WorkflowGateQueue::default().with_emergency_gate(emergency_gate.clone());
        let id_a = ApprovalId::new("a");
        let id_b = ApprovalId::new("b");
        q.arm("turn-1", &id_a, &gate("wf", "node-a"));
        q.arm("turn-1", &id_b, &gate("wf", "node-b"));
        q.decide("turn-1", &id_a, Verdict::Approve);

        emergency_gate.set_emergency(true);
        match q.release("turn-1") {
            Err(ReleaseRefusal::EmergencyStop) => {}
            Ok(_) => panic!("a partial batch must be refused, not released, while stopped"),
        }
        assert_eq!(
            q.undecided("turn-1"),
            1,
            "the refusal must leave the still-undecided gate exactly as it was"
        );

        q.decide("turn-1", &id_b, Verdict::Deny);
        assert_eq!(
            q.undecided("turn-1"),
            0,
            "a verdict must still bank while the company is stopped, or it is lost"
        );
        assert_eq!(
            q.ready_for_release(),
            vec!["turn-1".to_string()],
            "a batch decided during a stop must be discoverable for the post-lift redrive"
        );

        emergency_gate.set_emergency(false);
        let released = q
            .release("turn-1")
            .expect("stop lifted")
            .expect("the batch is still there");
        assert_eq!(released.approved, vec!["node-a".to_string()]);
        assert_eq!(released.denied, vec!["node-b".to_string()]);
    }

    /// A poisoned lock (some other caller panicked while holding it) must
    /// make every further call on this queue panic loudly rather than hand
    /// back stale or partial batch state that a caller could mistake for a
    /// clean read.
    #[test]
    fn a_poisoned_lock_panics_rather_than_silently_serving_stale_state() {
        let q = WorkflowGateQueue::default();
        let id = ApprovalId::new("a");
        q.arm("turn-1", &id, &gate("wf", "node-a"));

        let poison_q = q.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = poison_q.inner.lock().expect("workflow gate queue poisoned");
            panic!("simulated holder panic while the lock is held");
        }));

        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| q.undecided("turn-1")));
        assert!(
            result.is_err(),
            "a call against a poisoned lock must panic, not silently return a count"
        );
    }
}
