use super::*;

// Issue #470: the `composio_execute` fixtures are built here, from the same
// key the classifier reads, so a call in a test reaches the same catalogue
// lookup a call in production does.
use super::policy_test_helpers_tests::*;
use crate::policy::test_support::composio_send_args;

// --- Redeeming a grant (issue #243) --------------------------------------

/// The point of the whole feature: a call the operator approved actually
/// runs, instead of parking a second time.
#[tokio::test]
async fn a_granted_call_is_allowed_once_and_then_parks_again() {
    in_cycle(async {
        let (p, grants) = granting_policy("supervised", &[], "finance");
        let args = composio_send_args();
        grants.grant(granted("finance", "composio_execute", args.clone()));

        assert_eq!(
            p.check(&request("composio_execute", args.clone())).await,
            ToolPolicyDecision::Allow,
            "the operator approved this exact call; it must run"
        );
        // Single-use: the next identical call has no grant left and parks.
        assert!(
            matches!(
                p.check(&request("composio_execute", args)).await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "one approval buys one call, not standing permission"
        );
    })
    .await;
}

/// A grant is consumed **above** `always_approve`.
///
/// This ordering is the one real judgement call in the change. Leaving
/// `always_approve` on top reads safer, and is in fact incoherent: a tool on
/// that list would park, the operator would approve it, and it would park
/// again forever. Approval would authorise nothing at all for precisely the
/// tools the operator most wants to authorise deliberately. Single-use +
/// exact-args + agent-scope is what keeps the widened path narrow.
#[tokio::test]
async fn a_grant_beats_always_approve_but_only_for_that_one_call() {
    in_cycle(async {
        let (p, grants) = granting_policy("full", &["payment"], "finance");
        let args = serde_json::json!({ "amount_usd": 40.0 });

        // Without a grant, `always_approve` parks it even under full autonomy.
        assert!(matches!(
            p.check(&request("payment.send", args.clone())).await,
            ToolPolicyDecision::RequireApproval { .. }
        ));

        grants.grant(granted("finance", "payment.send", args.clone()));
        assert_eq!(
            p.check(&request("payment.send", args.clone())).await,
            ToolPolicyDecision::Allow
        );
        // And the list reasserts itself immediately afterwards.
        assert!(matches!(
            p.check(&request("payment.send", args)).await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
    })
    .await;
}

/// A grant minted for one agent does not admit another agent's identical
/// call. The operator approved a specific desk's request, not the action in
/// the abstract.
#[tokio::test]
async fn a_grant_does_not_travel_to_another_agent() {
    in_cycle(async {
        let (marketing, grants) = granting_policy("supervised", &[], "marketing");
        let args = composio_send_args();
        // The grant belongs to `finance`.
        grants.grant(granted("finance", "composio_execute", args.clone()));

        assert!(
            matches!(
                marketing
                    .check(&request("composio_execute", args.clone()))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "another agent's grant must not admit this call"
        );
        // ...and the near-miss did not burn finance's grant.
        assert_eq!(grants.live_count(), 1);
    })
    .await;
}

/// Re-issuing with different arguments re-parks rather than riding the
/// grant.
///
/// This is the security boundary of the feature. If matching were on the
/// tool name alone, a model that came back with a larger amount or a
/// different recipient would execute it under an approval the operator gave
/// for something else entirely — the operator would have authorised a $40
/// payment and funded a $4,000 one.
#[tokio::test]
async fn drifted_arguments_re_park_instead_of_riding_the_grant() {
    in_cycle(async {
        let (p, grants) = granting_policy("supervised", &[], "finance");
        grants.grant(granted(
            "finance",
            "pay_invoice",
            serde_json::json!({ "amount_usd": 40.0, "to": "acme" }),
        ));

        for drifted in [
            serde_json::json!({ "amount_usd": 4000.0, "to": "acme" }),
            serde_json::json!({ "amount_usd": 40.0, "to": "someone-else" }),
            serde_json::json!({ "amount_usd": 40.0 }),
            serde_json::json!({ "amount_usd": 40.0, "to": "acme", "memo": "extra" }),
        ] {
            assert!(
                matches!(
                    p.check(&request("pay_invoice", drifted.clone())).await,
                    ToolPolicyDecision::RequireApproval { .. }
                ),
                "arguments the operator never saw must re-park: {drifted}"
            );
        }
        // Every near-miss left the grant intact for the genuine call.
        assert_eq!(grants.live_count(), 1);
        assert_eq!(
            p.check(&request(
                "pay_invoice",
                serde_json::json!({ "amount_usd": 40.0, "to": "acme" })
            ))
            .await,
            ToolPolicyDecision::Allow
        );
    })
    .await;
}

/// A grant cannot rescue a tool the tier denies outright.
///
/// Deliberate: this arm is reachable only if an approval was parked under a
/// permissive tier and the company was moved to `readonly` before it was
/// resolved. `readonly` promises nothing is spent and nothing moves, and a
/// stale grant must not be a hole in that promise.
#[tokio::test]
async fn a_grant_does_not_override_a_readonly_desk() {
    let (p, grants) = granting_policy("readonly", &[], "finance");
    let args = serde_json::json!({ "to": "a@b.test" });
    grants.grant(granted("finance", "publish_post", args.clone()));

    // `readonly` outranks the grant. A grant can be up to its TTL old, so the
    // company may have been switched to `readonly` between the operator
    // approving this call and the agent re-issuing it — and that switch is
    // the emergency stop. The tier's contract wins over the older consent.
    assert!(
        matches!(
            p.check(&request("publish_post", args.clone())).await,
            ToolPolicyDecision::Deny { .. }
        ),
        "a live grant must not survive the readonly brake"
    );

    // And the grant was NOT consumed by that denial — the call never ran, so
    // the operator's approval is still redeemable if the brake comes off
    // inside the TTL.
    assert!(
        grants
            .peek(&crate::ports::types::ApprovalId::new("appr-1"))
            .is_some(),
        "a denied call must not burn the grant it never used"
    );

    // Anything the operator did NOT approve is still denied outright.
    assert!(matches!(
        p.check(&request("publish_post", serde_json::json!({ "other": 1 })))
            .await,
        ToolPolicyDecision::Deny { .. }
    ));
}

/// A policy with no agent bound — every non-harness construction site —
/// never consults the grant set at all.
#[tokio::test]
async fn an_unbound_policy_ignores_grants_entirely() {
    in_cycle(async {
        let queue = ApprovalRequestQueue::default();
        let grants = queue.grants();
        let p = policy("supervised", &[], None).with_requests(queue);
        let args = serde_json::json!({ "to": "a@b.test" });
        // A grant naming *some* agent exists, but this policy is bound to none.
        grants.grant(granted("finance", "send_email", args.clone()));

        assert!(matches!(
            p.check(&request("send_email", args)).await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
        assert_eq!(grants.live_count(), 1, "the grant was never touched");
    })
    .await;
}

/// Issue #243, and the single most fragile thing about riding the grant set
/// inside this queue: `HarnessBrain::run_cycle` calls
/// [`ApprovalRequestQueue::clear`] at the top of **every** cycle.
///
/// A grant is minted by the approve, and redeemed during the follow-up cycle
/// that approve kicks off — so if `clear()` reached the grants, the feature
/// would be destroyed by its own happy path: the cycle dispatched to redeem
/// the grant would wipe it microseconds before the agent's tool call arrived,
/// and every approval would fall through and re-park. Separate inner locks
/// are what prevent that, and this pins it.
#[test]
fn grants_survive_a_queue_clear() {
    let queue = ApprovalRequestQueue::default();
    let grants = queue.grants();
    let args = composio_send_args();
    grants.grant(crate::runtime::grants::GrantedCall {
        approval_id: crate::ports::types::ApprovalId::new("appr-1"),
        agent: "finance".into(),
        tool: "composio_execute".into(),
        args: args.clone(),
        at_millis: 1_000,
        origin_thread: None,
        origin_parent: None,
        origin_task: None,
    });

    queue.clear();

    assert_eq!(
        grants.live_count(),
        1,
        "clearing the request queue must not clear the grants it rides with"
    );
    assert!(
        queue
            .grants()
            .consume("finance", "composio_execute", &args)
            .is_some(),
        "and the grant is still redeemable through a fresh handle"
    );
}

/// Issue #242: a dispatched card claims only the requests **its own** turns
/// added. The queue is shared with any chat turn earlier in the same cycle,
/// so stamping from position zero would tag somebody else's approval with
/// this run and make the card read as waiting on an approval it never
/// triggered.
#[test]
fn stamping_a_run_claims_only_the_requests_that_came_after_the_boundary() {
    CURRENT_SCOPE.sync_scope(ApprovalScope::Cycle, stamp_after_the_boundary);
}

fn stamp_after_the_boundary() {
    let queue = ApprovalRequestQueue::default();
    let queued = |kind: &str| ApprovalRequest {
        tool: kind.to_string(),
        reason: "gated".to_string(),
        effect: Effect {
            kind: kind.to_string(),
            group: EffectGroup::Other,
            amount_usd: None,
            established_thread: false,
            first_time_counterparty: false,
            payload: serde_json::json!({ "kind": kind }),
            agent: Some("ceo".to_string()),
            run_id: None,
        },
    };

    // A chat turn earlier in this cycle parked one…
    queue.push(queued("chat.thing"));
    // …and the dispatch takes its boundary here.
    let boundary = queue.queued();
    assert_eq!(boundary, 1);
    queue.push(queued("dispatch.thing"));
    queue.push(queued("dispatch.other"));

    assert_eq!(queue.stamp_run(boundary, "run-1"), 2);

    let drained = queue.drain(10).requests;
    assert_eq!(drained.len(), 3);
    assert_eq!(
        drained[0].effect.run_id, None,
        "the chat turn's approval belongs to no attempt"
    );
    assert_eq!(drained[1].effect.run_id.as_deref(), Some("run-1"));
    assert_eq!(drained[2].effect.run_id.as_deref(), Some("run-1"));
}

/// A dispatch that parked nothing stamps nothing — which is what keeps a
/// clean run reading as `Succeeded` rather than as waiting on a person.
#[test]
fn a_dispatch_that_parked_nothing_claims_nothing() {
    let queue = ApprovalRequestQueue::default();
    assert_eq!(queue.stamp_run(queue.queued(), "run-1"), 0);
}
