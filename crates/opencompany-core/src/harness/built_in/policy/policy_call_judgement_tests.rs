use super::*;

// Issue #470: the `composio_execute` fixtures are built here, from the same
// key the classifier reads, so a call in a test reaches the same catalogue
// lookup a call in production does.
use super::policy_test_helpers_tests::*;

// ---- Per-call judgement (issue #338) ----------------------------------
//
// The unit tests for the verdict itself live in `crate::policy::judgement`,
// which is pure and compiles in the default build. What is tested HERE is
// the only thing that needs the harness: **where the arm sits in the
// chain**. Every test below is an assertion that the arm did not move
// something above it.

/// The agent-path behaviour change after #658's ruling: sends, payments and
/// undeclared publish-shaped calls stop regardless of mode. The declared
/// `publish_artifact` exception has its own tests in `judgement.rs`.
#[tokio::test]
async fn full_autonomy_stops_for_an_irreversible_call() {
    in_cycle(async {
        let p = policy("full", &[], None);
        for tool in ["send_email", "publish_post", "pay_invoice"] {
            let d = p.check(&request(tool, serde_json::json!({}))).await;
            assert_eq!(decision_name(&d), "park", "`{tool}` must stop under full");
        }
    })
    .await;
}

/// The other half: `full` still means full for everything that does not
/// warrant a human. A gate that stopped reads would simply be `supervised`
/// with extra steps.
#[tokio::test]
async fn full_autonomy_still_allows_reads_and_drafts() {
    let p = policy("full", &[], None);
    for tool in ["file_read", "grep", "list", "memory_recall", "web_search"] {
        let d = p.check(&request(tool, serde_json::json!({}))).await;
        assert_eq!(decision_name(&d), "allow", "`{tool}` must still run");
    }
}

/// `readonly` DENIES an irreversible call; it does not park it.
///
/// The failure this guards is subtle and would look like an improvement: if
/// the judgement arm ran before the mode, a send on a read-only desk would
/// come back as "ask the operator" instead of "no". That converts the
/// emergency stop into a prompt, which is the one thing the brake exists to
/// not be.
#[tokio::test]
async fn the_readonly_brake_still_denies_rather_than_parking() {
    let p = policy("readonly", &[], None);
    let d = p.check(&request("send_email", serde_json::json!({}))).await;
    assert_eq!(decision_name(&d), "deny");
}

/// `supervised` is untouched: the call already parked, and it still parks
/// with the reason `supervised` gives rather than the judgement one.
///
/// Reason text is asserted because it is the operator-visible half — a tier
/// silently re-labelling its stops would be a change nobody asked for.
#[tokio::test]
async fn supervised_keeps_its_own_reason() {
    in_cycle(async {
        let p = policy("supervised", &[], None);
        let d = p.check(&request("send_email", serde_json::json!({}))).await;
        match d {
            ToolPolicyDecision::RequireApproval { reason } => {
                assert!(
                    reason.contains("supervised"),
                    "expected the supervised reason, got: {reason}"
                );
            }
            other => panic!("expected a park, got {}", decision_name(&other)),
        }
    })
    .await;
}

/// A pre-granted call still runs under `full` — "unless explicitly
/// pre-granted", in the acceptance's words.
///
/// This is the arm's placement doing the work: the grant check returns
/// `Allow` long before the judgement arm is reached, so the operator's
/// approval is not re-litigated by a classifier.
#[tokio::test]
async fn a_pre_granted_irreversible_call_still_runs() {
    in_cycle(async {
        let (p, grants) = granting_policy("full", &[], "finance");
        let args = serde_json::json!({ "to": "customer@example.com" });
        grants.grant(granted("finance", "send_email", args.clone()));
        let d = p.check(&request("send_email", args.clone())).await;
        assert_eq!(
            decision_name(&d),
            "allow",
            "the grant must still be honoured"
        );

        // ...and it was consumed, so the next identical call stops again
        // (#243 semantics, and #183 decision 3: a run may stop more than once).
        let again = p.check(&request("send_email", args)).await;
        assert_eq!(decision_name(&again), "park");
    })
    .await;
}

/// `always_approve` keeps its own reason under `full`.
///
/// Both arms would park this call, so the decision alone cannot tell them
/// apart — the reason can, and the operator's card shows the reason. If the
/// judgement arm had been placed above `always_approve`, the operator would
/// stop being told that this stop is one they themselves configured.
#[tokio::test]
async fn always_approve_keeps_its_own_reason_under_full() {
    in_cycle(async {
        let p = policy("full", &["send_email"], None);
        let d = p.check(&request("send_email", serde_json::json!({}))).await;
        match d {
            ToolPolicyDecision::RequireApproval { reason } => {
                assert!(
                    reason.contains("always-approve"),
                    "expected the always-approve reason, got: {reason}"
                );
            }
            other => panic!("expected a park, got {}", decision_name(&other)),
        }
    })
    .await;
}

/// `auto_approve_under_usd` is static configuration about spend, and it
/// still speaks first.
///
/// Worth stating as a test rather than leaving implicit: an operator who
/// wrote "anything under $5 is fine" said something specific about money,
/// and this arm does not get to overrule it. The daily cap above still
/// does.
#[tokio::test]
async fn a_sub_threshold_spend_is_still_auto_approved() {
    let p = policy("full", &[], Some(5.0));
    let d = p
        .check(&request(
            "pay_invoice",
            serde_json::json!({ "amount_usd": 1.0 }),
        ))
        .await;
    assert_eq!(decision_name(&d), "allow");
}

/// Fail closed: a tool nobody declared stops under `full` rather than
/// running because nothing recognised it.
#[tokio::test]
async fn an_undeclared_tool_stops_under_full() {
    in_cycle(async {
        let p = policy("full", &[], None);
        let d = p
            .check(&request("frobnicate_the_widget", serde_json::json!({})))
            .await;
        assert_eq!(decision_name(&d), "park");
    })
    .await;
}

/// `curl` is declared `EffectGroup::Other`, so the irreversible-group rule
/// does not reach it, and it is declared, so the undeclared-tool rule does
/// not either. `judge`'s `UNBOUNDED` list is the only remaining gate once
/// `PolicyMode::Full` has already returned `Allow` above — and `curl`
/// writes an arbitrary response into the workspace `downloads/` directory,
/// which this layer cannot bound. It must still park on a `full` desk.
#[tokio::test]
async fn curl_still_parks_under_full_via_the_judgement_arm() {
    in_cycle(async {
        let p = policy("full", &[], None);
        let d = p
            .check(&request(
                "curl",
                serde_json::json!({ "url": "https://example.com/report.csv" }),
            ))
            .await;
        assert_eq!(decision_name(&d), "park", "`curl` must stop under full");
    })
    .await;
}

/// Every stop reaches the operator. A `RequireApproval` that skipped
/// `require_approval` would refuse the tool without ever queueing anything
/// to park — the bug issue #172 closed — so the new arm is checked to go
/// through the same door as every other.
#[tokio::test]
async fn a_judgement_stop_is_queued_for_the_operator() {
    in_cycle(async {
        let queue = ApprovalRequestQueue::default();
        let p = policy("full", &[], None).with_requests(queue.clone());
        let d = p.check(&request("send_email", serde_json::json!({}))).await;
        assert_eq!(decision_name(&d), "park");
        // `queued()`, deliberately, and neither `drain` nor `take_from`. Both of
        // those are being reshaped by PRs in flight — #625 changes `drain`'s
        // return type to `DrainedRequests`, and #439 removes `take_from`
        // entirely — and neither would conflict with this branch *textually*,
        // so both lanes would be green and whoever merged second would break
        // the tree. `queued()` is untouched by both and answers the question
        // this test is actually asking.
        assert_eq!(queue.queued(), 1, "the stop must be queued to park");
        // The reason reaching the operator is the same string the decision
        // carries — `require_approval` clones it onto the queued request — so
        // asserting it here needs no queue read at all.
        match d {
            ToolPolicyDecision::RequireApproval { reason } => assert!(
                !reason.is_empty(),
                "the operator needs a reason on the card"
            ),
            other => panic!("expected a park, got {}", decision_name(&other)),
        }
    })
    .await;
}
