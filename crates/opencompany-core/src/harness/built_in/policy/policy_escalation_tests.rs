use super::*;

// Issue #470: the `composio_execute` fixtures are built here, from the same
// key the classifier reads, so a call in a test reaches the same catalogue
// lookup a call in production does.
use super::policy_test_helpers_tests::*;
use crate::policy::test_support::{composio_send_args, composio_unclassified_args_numbered};

#[tokio::test]
async fn escalate_to_human_sets_the_turn_boundary_and_explicitly_refuses_overflow() {
    in_cycle(async {
        use tinytools::Tool as _;

        let queue = ApprovalRequestQueue::default();
        let tool = crate::harness::built_in::blockers::EscalateToHumanTool::new(
            queue.clone(),
            "engineer".to_string(),
            "engineer".to_string(),
        );

        queue
            .turn_scoped(async {
                for i in 0..MAX_APPROVAL_REQUESTS_PER_TURN {
                    let asked = tool
                        .execute(serde_json::json!({ "question": format!("question {i}") }))
                        .await
                        .expect("the tool runs");
                    assert!(!asked.is_error, "{}", asked.text());
                }
                let refused = tool
                    .execute(serde_json::json!({ "question": "ninth question" }))
                    .await
                    .expect("the tool returns its refusal");
                assert!(
                    refused.is_error,
                    "the ninth question must be explicitly refused, not reported as raised: {}",
                    refused.text()
                );
                assert!(refused.text().contains("not raised"));
                assert!(
                    refused
                        .text()
                        .contains(&MAX_APPROVAL_REQUESTS_PER_TURN.to_string())
                );
                assert!(
                    queue.explicit_request_pending(),
                    "escalation must end the turn"
                );
            })
            .await;

        let drained = queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN);
        assert_eq!(
            drained.discarded, 0,
            "a question reported as raised must not be lost at drain"
        );
        assert_eq!(drained.requests.len(), MAX_APPROVAL_REQUESTS_PER_TURN);
        assert!(
            drained
                .requests
                .iter()
                .all(|request| request.reason != "ninth question")
        );
    })
    .await;
}

/// LIMIT-axis (TOOL-008): `media_generate_image`/`media_generate_video` are
/// the one policy-manufactured approval left once policy HITL is disabled
/// — the production shape, since they park above that bypass rather than
/// below it — but they still file into the exact same
/// `MAX_APPROVAL_REQUESTS_PER_TURN` bucket `escalate_to_human` floods
/// above. Nothing owns that overflow story for a paid card specifically: a
/// chatty turn that raises the cap's worth of questions before the agent
/// ever reaches its media call pushes the real spend request off the
/// drain, and the operator never sees a card for the money the agent is
/// about to commit to spending.
#[tokio::test]
async fn a_flood_of_escalations_can_push_a_paid_media_card_off_the_shared_cap() {
    in_cycle(async {
        use tinytools::Tool as _;

        let queue = ApprovalRequestQueue::default();
        let policy = policy("full", &[], None)
            .with_policy_hitl_disabled()
            .with_requests(queue.clone());
        let blockers = crate::harness::built_in::blockers::EscalateToHumanTool::new(
            queue.clone(),
            "engineer".to_string(),
            "engineer".to_string(),
        );

        for i in 0..MAX_APPROVAL_REQUESTS_PER_TURN {
            let asked = blockers
                .execute(serde_json::json!({ "question": format!("question {i}?") }))
                .await
                .expect("the question runs");
            assert!(!asked.is_error, "{}", asked.output());
        }

        assert!(
            matches!(
                policy
                    .check(&request("media_generate_image", serde_json::json!({})))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "policy HITL disabled must still stage the paid media call for approval"
        );

        let drained = queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN);
        assert_eq!(
            drained.requests.len(),
            MAX_APPROVAL_REQUESTS_PER_TURN,
            "the cap is shared across kinds, not per-kind"
        );
        assert_eq!(
            drained.discarded, 1,
            "the ninth card — the paid one — is what overflows the shared cap"
        );
        assert!(
            drained
                .requests
                .iter()
                .all(|r| r.tool != "media_generate_image"),
            "the media card lost the race to the questions asked before it and never reached \
         the operator's queue: {:?}",
            drained.requests.iter().map(|r| &r.tool).collect::<Vec<_>>()
        );
    })
    .await;
}

/// `check`'s fail-closed boundary (the block right above `Deny`ing every
/// call once `request_approval` has fired) reads the same task-local as
/// `explicit_request_pending`. Since `escalate_to_human` never sets it, a
/// sibling gated call queued in the same turn right after a question is
/// evaluated on its own terms rather than refused outright the way a
/// second `request_approval` would be.
#[tokio::test]
async fn escalate_to_human_respects_the_cycle_claims_drain_cap() {
    in_cycle(async {
        use tinytools::Tool as _;

        let queue = ApprovalRequestQueue::default();
        let claim = queue.claim(ApprovalScope::Cycle);
        let tool = crate::harness::built_in::blockers::EscalateToHumanTool::new(
            queue.clone(),
            "engineer".to_string(),
            "engineer".to_string(),
        );
        for i in 0..MAX_APPROVAL_REQUESTS_PER_TURN - 1 {
            let args = serde_json::json!({ "question": format!("question {i}") });
            let asked = claim
                .scoped(tool.execute(args))
                .await
                .expect("the tool runs");
            assert!(!asked.is_error, "{}", asked.text());
        }

        for (question, refused) in [("last available slot", false), ("overflow", true)] {
            let args = serde_json::json!({ "question": question });
            let asked = claim
                .scoped(tool.execute(args))
                .await
                .expect("the tool runs");
            assert_eq!(
                asked.is_error,
                refused,
                "the cycle claim's drain cap is shared across every push filed into it: {}",
                asked.text()
            );
        }

        let drained = claim
            .scoped(async { queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN) })
            .await;
        assert_eq!(
            drained.discarded, 0,
            "no accepted question may be discarded"
        );
        assert_eq!(drained.requests.len(), MAX_APPROVAL_REQUESTS_PER_TURN);
        assert!(
            drained
                .requests
                .iter()
                .any(|r| r.reason == "last available slot")
        );
        assert!(drained.requests.iter().all(|r| r.reason != "overflow"));
    })
    .await;
}

#[tokio::test]
async fn accepted_blockers_survive_later_ordinary_approvals() {
    in_cycle(async {
        use tinytools::Tool as _;

        for preceding in [0, MAX_APPROVAL_REQUESTS_PER_TURN - 1] {
            let (policy, queue) = queued_policy("supervised", &[]);
            let cycle = queue.claim(ApprovalScope::Cycle);
            let tool = super::super::blockers::EscalateToHumanTool::new(
                queue.clone(),
                "engineer".to_string(),
                "engineer".to_string(),
            );
            for i in 0..preceding {
                let call = request("composio_execute", composio_unclassified_args_numbered(i));
                let decision = cycle.scoped(policy.check(&call)).await;
                assert!(matches!(
                    decision,
                    ToolPolicyDecision::RequireApproval { .. }
                ));
            }
            let args = serde_json::json!({ "question": "must survive later approvals" });
            let asked = cycle
                .scoped(tool.execute(args.clone()))
                .await
                .expect("the tool runs");
            assert!(!asked.is_error, "{}", asked.text());

            for i in preceding..preceding + MAX_APPROVAL_REQUESTS_PER_TURN {
                let call = request("composio_execute", composio_unclassified_args_numbered(i));
                let decision = cycle.scoped(policy.check(&call)).await;
                assert!(matches!(
                    decision,
                    ToolPolicyDecision::RequireApproval { .. }
                ));
            }
            let duplicate = cycle
                .scoped(tool.execute(args))
                .await
                .expect("the tool runs");
            assert!(
                !duplicate.is_error,
                "the accepted duplicate retains its slot"
            );

            let drained = cycle
                .scoped(async { queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN) })
                .await;
            assert_eq!(
                drained
                    .requests
                    .iter()
                    .filter(|r| r.reason == "must survive later approvals")
                    .count(),
                1,
                "an accepted blocker must survive later ordinary approvals; preceding={preceding}"
            );
            assert_eq!(drained.requests.len(), MAX_APPROVAL_REQUESTS_PER_TURN);
            assert_eq!(drained.discarded, preceding + 1);
            assert!(drained.overflow_notice().is_some());
        }
    })
    .await;
}

#[tokio::test]
async fn a_blocker_duplicate_outside_the_drain_budget_is_refused() {
    in_cycle(async {
        use tinytools::Tool as _;

        let fixture = ApprovalRequestQueue::default();
        let args = serde_json::json!({ "question": "outside the budget" });
        super::super::blockers::EscalateToHumanTool::new(
            fixture.clone(),
            "engineer".to_string(),
            "engineer".to_string(),
        )
        .execute(args.clone())
        .await
        .expect("the fixture tool runs");
        let existing = fixture
            .drain(MAX_APPROVAL_REQUESTS_PER_TURN)
            .requests
            .remove(0);

        for (existing_in_cycle, ordinary_in_cycle) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let queue = ApprovalRequestQueue::default();
            let cycle = queue.claim(ApprovalScope::Cycle);
            for i in 0..MAX_APPROVAL_REQUESTS_PER_TURN {
                let request = gated(&format!("ordinary.{i}"));
                if ordinary_in_cycle {
                    cycle.scoped(async { queue.push(request) }).await;
                } else {
                    queue.push(request);
                }
            }
            let tool = super::super::blockers::EscalateToHumanTool::new(
                queue.clone(),
                "engineer".to_string(),
                "engineer".to_string(),
            );
            let asked = if existing_in_cycle {
                cycle
                    .scoped(async {
                        queue.push(existing.clone());
                        tool.execute(args.clone()).await
                    })
                    .await
            } else {
                queue.push(existing.clone());
                tool.execute(args.clone()).await
            }
            .expect("the tool runs");
            assert!(
                asked.is_error,
                "an overflow duplicate must not be reported as raised"
            );
            assert!(asked.text().contains("not raised"));
            let drained = cycle
                .scoped(async { queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN) })
                .await;
            assert_eq!(drained.requests.len(), MAX_APPROVAL_REQUESTS_PER_TURN);
            assert_eq!(drained.discarded, 1);
            assert!(
                drained
                    .requests
                    .iter()
                    .all(|r| r.reason != "outside the budget")
            );
        }
    })
    .await;
}

#[tokio::test]
async fn interleaved_scopes_drain_in_their_own_enqueue_order_with_their_own_stamps() {
    for cap in [0, 3, MAX_APPROVAL_REQUESTS_PER_TURN] {
        let queue = ApprovalRequestQueue::default();
        let cycle = queue.claim(ApprovalScope::Cycle);
        let run = queue.claim(ApprovalScope::Run("independent".to_string()));
        for i in 0..10 {
            cycle
                .scoped(async {
                    let boundary = queue.queued();
                    assert_eq!(boundary, i);
                    queue.push(gated(&format!("ordinary.{i}")));
                    assert_eq!(queue.stamp_run(boundary, &format!("cycle.{i}")), 1);
                })
                .await;
            run.scoped(async { queue.push(gated(&format!("run.{i}"))) })
                .await;
        }
        let drained = cycle.drain(cap);
        assert_eq!(drained.cap(), cap);
        assert_eq!(drained.discarded, 10 - cap);
        assert_eq!(drained.requests.len(), cap);
        for (i, request) in drained.requests.iter().enumerate() {
            assert_eq!(request.tool, format!("ordinary.{i}"));
            assert_eq!(request.effect.run_id, Some(format!("cycle.{i}")));
        }
        let independent = run.drain(cap);
        assert_eq!(independent.discarded, 10 - cap);
        assert_eq!(independent.requests.len(), cap);
        for (i, request) in independent.requests.iter().enumerate() {
            assert_eq!(request.tool, format!("run.{i}"));
            assert!(request.effect.run_id.is_none());
        }
    }
}

#[tokio::test]
async fn escalate_to_human_refuses_a_sibling_gated_call_in_the_same_turn() {
    use tinytools::Tool as _;

    let queue = ApprovalRequestQueue::default();
    let policy = policy("supervised", &[], None).with_requests(queue.clone());
    let claim = queue.claim(ApprovalScope::Cycle);

    let tool = crate::harness::built_in::blockers::EscalateToHumanTool::new(
        queue.clone(),
        "engineer".to_string(),
        "engineer".to_string(),
    );
    let later_call = claim
        .scoped(queue.turn_scoped(async {
            let asked = tool
                .execute(serde_json::json!({ "question": "staging or prod?" }))
                .await
                .expect("the question runs");
            assert!(!asked.is_error, "{}", asked.output());
            policy
                .check(&request("composio_execute", composio_send_args()))
                .await
        }))
        .await;

    assert!(
        matches!(later_call, ToolPolicyDecision::Deny { .. }),
        "escalation must refuse later calls in the same turn: {later_call:?}"
    );
    let next_turn = claim
        .scoped(queue.turn_scoped(policy.check(&request("composio_execute", composio_send_args()))))
        .await;
    assert!(matches!(
        next_turn,
        ToolPolicyDecision::RequireApproval { .. }
    ));
}

/// A repeated identical question in one turn — a model retrying a call it
/// is unsure landed — collapses into the card already queued via `push`'s
/// per-scope de-duplication (issue #439), same as a duplicate
/// `request_approval`. Both calls still report success to the model, so a
/// distinct question asked right after must not vanish along with the
/// duplicate.
#[tokio::test]
async fn a_repeated_identical_escalation_collapses_but_a_distinct_one_survives() {
    in_cycle(async {
        use tinytools::Tool as _;

        let queue = ApprovalRequestQueue::default();
        let tool = crate::harness::built_in::blockers::EscalateToHumanTool::new(
            queue.clone(),
            "engineer".to_string(),
            "engineer".to_string(),
        );

        let first = tool
            .execute(serde_json::json!({ "question": "staging or prod?" }))
            .await
            .expect("the tool runs");
        let second = tool
            .execute(serde_json::json!({ "question": "staging or prod?" }))
            .await
            .expect("the tool runs");
        assert!(!first.is_error);
        assert!(!second.is_error, "a duplicate ask is not itself a failure");

        let distinct = tool
            .execute(serde_json::json!({ "question": "which key rotates first?" }))
            .await
            .expect("the tool runs");
        assert!(!distinct.is_error);

        let drained = queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN);
        assert_eq!(
            drained.requests.len(),
            2,
            "the repeated question collapses into the card already queued, but the distinct \
         question still gets its own: {:?}",
            drained
                .requests
                .iter()
                .map(|r| &r.reason)
                .collect::<Vec<_>>()
        );
    })
    .await;
}

/// Outside `turn_scoped`, the task-local backing the boundary was never
/// installed. `explicit_request_pending` reads that absence through
/// `unwrap_or(false)` rather than erroring or denying, so a call site
/// that forgot to wrap its turn in `turn_scoped` gets "no request
/// pending" — the boundary fails OPEN outside its scope, not closed.
#[tokio::test]
async fn the_turn_boundary_reads_as_not_pending_outside_any_turn_scope() {
    let queue = ApprovalRequestQueue::default();
    let policy = policy("full", &[], None)
        .with_policy_hitl_disabled()
        .with_requests(queue.clone());

    // No `turn_scoped` anywhere in this call's ancestry.
    queue.push(ApprovalRequest {
        tool: crate::harness::approval_tool::REQUEST_APPROVAL_TOOL.to_string(),
        reason: "May I send this?".to_string(),
        effect: Effect {
            kind: crate::harness::approval_tool::REQUEST_APPROVAL_TOOL.to_string(),
            group: EffectGroup::Other,
            amount_usd: None,
            established_thread: false,
            first_time_counterparty: false,
            payload: serde_json::json!({
                "title": "Send update",
                "question": "May I send it?"
            }),
            agent: Some("ceo".to_string()),
            run_id: None,
        },
    });

    let decision = policy
        .check(&request("composio_execute", composio_send_args()))
        .await;
    assert!(
        !matches!(decision, ToolPolicyDecision::Deny { .. }),
        "outside any turn_scoped call, the boundary reads as not-pending and does not \
         refuse a sibling call: {decision:?}"
    );
}
