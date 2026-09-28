use super::tests_owner_setup::{Harness, graph, graph_without_destination, reached_output, record};
use super::*;

use async_trait::async_trait;

use crate::company::parse_workflow;
use crate::ports::types::{Actor, ActorKind, CompanyId, Verdict};

/// An `output` node that only exists to pause for approval is control flow,
/// not a report-back that lost its address. It contributes no row, so a
/// correct gated workflow does not grow a "not delivered" badge on every
/// continuation run.
#[tokio::test]
async fn an_approval_gate_with_no_destination_is_not_reported_as_misconfigured() {
    let gate = parse_workflow(
        r#"
id = "gated"
name = "Gated"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "done"
kind = "output"
name = "Gate"
requires_approval = true
[[edge]]
from = "start"
to = "done"
"#,
    )
    .expect("a gate graph is valid");
    let reports = deliver_outputs(
        None,
        &record(&["*"]),
        &gate,
        "run-1",
        &reached_output(),
        &[],
    )
    .await;
    assert!(
        reports.is_empty(),
        "a gate is not a report-back with a missing address: {reports:?}"
    );
}

/// A test run is where an author most wants to find this, so the dry router
/// takes the same rule.
#[test]
fn deliver_outputs_dry_reports_a_node_with_no_destination() {
    let reports = deliver_outputs_dry(
        &record(&["email"]),
        &graph_without_destination(),
        &reached_output(),
    );
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0].node, "done");
    assert_eq!(reports[0].status, DeliveryStatus::Skipped);
    assert_eq!(reports[0].reason, DeliveryReason::NoDestinationConfigured);
}

/// An output node the dry run never reached contributes no row at all —
/// exactly the "absent means not reached" rule the live path takes.
#[test]
fn deliver_outputs_dry_skips_an_unreached_node() {
    let workflow = graph("owner", None);
    // Output where `done` was NOT reached.
    let output = serde_json::json!({ "nodes": { "start": { "items": [] } } });
    let reports = deliver_outputs_dry(&record(&["email"]), &workflow, &output);
    assert!(
        reports.is_empty(),
        "an unreached node routes nothing: {reports:?}"
    );
}

/// Issue #1825 (P1, fifth follow-up — found by chatgpt-codex-connector):
/// "Prevent the synthetic hold from consuming a real decision".
///
/// Pre-fix, `park_and_journal` called `self.approvals.park` — which is what
/// makes an approval id exist for an operator to resolve — strictly
/// *before* arming this card's own `ContinuationQueue` slot (that arm ran
/// only after `record_parked` returned, on the success path). A resolve
/// racing in on another tokio worker thread during `record_parked`'s own
/// async durable append therefore saw a turn whose only armed slot was
/// `park_gated_calls`'s pre-loop synthetic hold, decided against it, and
/// released the batch before this card had been counted; this card's own
/// arm then still landed once the journal write returned, into a fresh,
/// orphaned queue entry no further decision would ever redeem.
///
/// This spies on the approval gate `park_and_journal` calls first and
/// captures `continuations.outstanding(turn)` at that exact point —
/// deterministic, no wall-clock race needed, on the same principle as
/// `approving_the_first_card_of_a_multi_call_node_does_not_complete_the_batch_early`
/// in `workflows::caps::mod`. Pre-fix this captures `0` (nothing armed
/// yet); post-fix it must capture `1`.
#[tokio::test]
async fn park_and_journal_arms_the_continuation_slot_before_the_card_is_parkable() {
    use crate::ports::types::PolicyDecision;

    /// Delegates every call to `inner`, except that `park` first records
    /// how many decisions `turn` is already counted as blocking on —
    /// the moment an operator's resolve could first reach this approval.
    struct Spy {
        inner: Arc<dyn ApprovalGate>,
        continuations: crate::runtime::continuation::ContinuationQueue,
        turn: String,
        outstanding_at_park: std::sync::Mutex<Option<usize>>,
    }

    #[async_trait]
    impl ApprovalGate for Spy {
        async fn evaluate(
            &self,
            company: &CompanyId,
            effect: &Effect,
        ) -> crate::Result<PolicyDecision> {
            self.inner.evaluate(company, effect).await
        }

        async fn park(&self, company: &CompanyId, effect: Effect) -> crate::Result<ApprovalId> {
            *self.outstanding_at_park.lock().expect("spy lock") =
                Some(self.continuations.outstanding(&self.turn));
            self.inner.park(company, effect).await
        }

        async fn resolve(
            &self,
            id: &ApprovalId,
            verdict: Verdict,
            by: Actor,
        ) -> crate::Result<Option<Effect>> {
            self.inner.resolve(id, verdict, by).await
        }
    }

    let dir = tempfile::Builder::new()
        .prefix("oc-1825-p1-5-")
        .tempdir()
        .expect("tempdir");
    let h = Harness::new(dir.path(), false, false).with_parking(dir.path(), "full");
    let parking = h.deps.parking.clone().expect("with_parking wired it");

    let turn = "workflow-node:run-1825-p1-5:work".to_string();
    let spy = Arc::new(Spy {
        inner: parking.approvals.clone(),
        continuations: parking.continuations.clone(),
        turn: turn.clone(),
        outstanding_at_park: std::sync::Mutex::new(None),
    });
    let mut spied_parking = parking.clone();
    spied_parking.approvals = spy.clone();

    let effect = Effect {
        kind: "shell".to_string(),
        group: EffectGroup::Other,
        amount_usd: None,
        established_thread: false,
        first_time_counterparty: false,
        payload: serde_json::json!({ "call": "shell" }),
        agent: Some("ceo".to_string()),
        run_id: None,
    };

    let approval_id = spied_parking
        .park_and_journal(
            &CompanyId::new("acme"),
            effect,
            crate::runtime::journal::TaskLink::Unlinked,
            None,
            Some(turn.clone()),
        )
        .await
        .expect("parks");

    let captured = spy
        .outstanding_at_park
        .lock()
        .expect("spy lock")
        .expect("park was called");
    assert_eq!(
        captured, 1,
        "this card's continuation slot must already be armed by the time the approval \
         gate's park() runs, before record_parked's synchronous insert can make the card \
         resolvable to a concurrent operator — otherwise a decision racing in during \
         record_parked's async durable append can consume a hold this card was never \
         counted against"
    );

    // Sanity: the ordinary, non-racing shape is unchanged — one card on
    // this turn, one decision, releases it immediately.
    assert_eq!(parking.continuations.outstanding(&turn), 1);
    let event = CompanyEvent::ApprovalResolved {
        approval_id,
        verdict: Verdict::Approve,
        by: Actor {
            kind: ActorKind::Operator,
            id: "operator".to_string(),
        },
    };
    assert!(
        parking.continuations.decide(&turn, Some(event)).is_some(),
        "the only card parked on this turn must still release it on its own decision"
    );
}

/// Companion to the test above: when the durable journal write fails, the
/// slot armed before the attempt must be released rather than left
/// blocking the turn on a card that will now never exist.
#[tokio::test]
async fn park_and_journal_releases_the_continuation_slot_when_the_journal_write_fails() {
    let dir = tempfile::Builder::new()
        .prefix("oc-1825-p1-5-fail-")
        .tempdir()
        .expect("tempdir");
    let h = Harness::new(dir.path(), false, false).with_failing_journal(dir.path(), "full");
    let parking = h
        .deps
        .parking
        .clone()
        .expect("with_failing_journal wired it");

    let turn = "workflow-node:run-1825-p1-5-fail:work".to_string();
    let effect = Effect {
        kind: "shell".to_string(),
        group: EffectGroup::Other,
        amount_usd: None,
        established_thread: false,
        first_time_counterparty: false,
        payload: serde_json::json!({ "call": "shell" }),
        agent: Some("ceo".to_string()),
        run_id: None,
    };

    let result = parking
        .park_and_journal(
            &CompanyId::new("acme"),
            effect,
            crate::runtime::journal::TaskLink::Unlinked,
            None,
            Some(turn.clone()),
        )
        .await;
    assert!(
        result.is_err(),
        "the failing journal must still fail the park"
    );
    assert_eq!(
        parking.continuations.outstanding(&turn),
        0,
        "a park whose durable write failed leaves no card for an operator to ever decide, \
         so the slot armed for it before the attempt must be released — otherwise the turn \
         is left permanently blocked on a decision that can never arrive"
    );
}

/// A workflow park is the same transaction a cycle's is: it holds the card's
/// checkout on the grant set and tells every console it parked.
#[tokio::test]
async fn park_and_journal_holds_the_work_unit_and_announces_the_park() {
    let dir = tempfile::tempdir().expect("tempdir");
    let h = Harness::new(dir.path(), false, false).with_parking(dir.path(), "full");
    let parking = h.deps.parking.clone().expect("with_parking wired it");
    let effect = Effect {
        kind: "shell".to_string(),
        group: EffectGroup::Other,
        amount_usd: None,
        established_thread: false,
        first_time_counterparty: false,
        payload: serde_json::json!({ "call": "shell" }),
        agent: Some("ceo".to_string()),
        run_id: None,
    };

    let id = parking
        .park_and_journal(
            &CompanyId::new("acme"),
            effect,
            crate::runtime::journal::TaskLink::from_task_id(Some("card-7")),
            Some("ops".to_string()),
            None,
        )
        .await
        .expect("park succeeds");

    assert!(
        parking.grants.any_for_task("card-7"),
        "the card's checkout is held while its approval waits"
    );
    let parked: Vec<_> = h
        .events
        .read_from(
            &CompanyId::new("acme"),
            crate::ports::types::EventSeq::new(0),
            usize::MAX,
        )
        .await
        .expect("event log reads")
        .into_iter()
        .filter_map(|stored| match stored.event {
            CompanyEvent::ApprovalParked { approval_id, .. } => Some(approval_id),
            _ => None,
        })
        .collect();
    assert_eq!(parked, vec![id]);
}
