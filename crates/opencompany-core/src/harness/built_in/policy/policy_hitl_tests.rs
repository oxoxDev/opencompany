use super::*;

// Issue #470: the `composio_execute` fixtures are built here, from the same
// key the classifier reads, so a call in a test reaches the same catalogue
// lookup a call in production does.
use super::policy_test_helpers_tests::*;
use crate::policy::test_support::composio_send_args;

#[tokio::test]
async fn an_explicit_request_refuses_later_calls_in_the_same_turn() {
    let queue = ApprovalRequestQueue::default();
    let policy = policy("full", &[], None)
        .with_policy_hitl_disabled()
        .with_requests(queue.clone());
    let claim = queue.claim(ApprovalScope::Cycle);

    let (second_request, later_call) = claim
        .scoped(queue.turn_scoped(async {
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
            let second_request = policy
                .check(&request(
                    crate::harness::approval_tool::REQUEST_APPROVAL_TOOL,
                    serde_json::json!({
                        "title": "Ask twice",
                        "question": "May I ask again?"
                    }),
                ))
                .await;
            let later_call = policy
                .check(&request("composio_execute", composio_send_args()))
                .await;
            (second_request, later_call)
        }))
        .await;

    assert!(
        matches!(second_request, ToolPolicyDecision::Deny { .. }),
        "a second explicit request must hit the same turn boundary"
    );
    let ToolPolicyDecision::Deny { reason } = later_call else {
        panic!("later sibling call must be refused");
    };
    assert!(reason.contains("already asked the operator"));

    let unrelated_turn = queue
        .turn_scoped(policy.check(&request("composio_execute", composio_send_args())))
        .await;
    assert_eq!(
        unrelated_turn,
        ToolPolicyDecision::Allow,
        "a later agent turn in the same cycle/run scope gets a fresh boundary"
    );
}

/// BOUND-axis (TOOL-001): the boundary is a `Cell<bool>` that starts at
/// `false` inside every fresh `turn_scoped` call — the zero-vs-one
/// transition `an_explicit_request_refuses_later_calls_in_the_same_turn`
/// above only tests the "one" side of (the second request is denied),
/// never asserting that the FIRST request in a brand new turn is not
/// itself refused by a boundary nothing has tripped yet.
#[tokio::test]
async fn the_first_explicit_request_in_a_fresh_turn_is_not_refused() {
    let queue = ApprovalRequestQueue::default();
    let policy = policy("full", &[], None)
        .with_policy_hitl_disabled()
        .with_requests(queue.clone());
    let claim = queue.claim(ApprovalScope::Cycle);

    let first_request = claim
        .scoped(queue.turn_scoped(policy.check(&request(
            crate::harness::approval_tool::REQUEST_APPROVAL_TOOL,
            serde_json::json!({ "title": "Ask", "question": "May I send this?" }),
        ))))
        .await;
    assert_eq!(
        first_request,
        ToolPolicyDecision::Allow,
        "the very first explicit request in a fresh turn must not be refused: \
         {first_request:?}"
    );
}

#[tokio::test]
async fn a_duplicate_explicit_request_still_establishes_a_fresh_turn_boundary() {
    let queue = ApprovalRequestQueue::default();
    let policy = policy("full", &[], None)
        .with_policy_hitl_disabled()
        .with_requests(queue.clone());
    let claim = queue.claim(ApprovalScope::Cycle);
    let approval_request = ApprovalRequest {
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
    };

    claim
        .scoped(async { queue.push(approval_request.clone()) })
        .await;
    let later_call = claim
        .scoped(queue.turn_scoped(async {
            queue.push(approval_request);
            policy
                .check(&request("composio_execute", composio_send_args()))
                .await
        }))
        .await;

    assert!(matches!(later_call, ToolPolicyDecision::Deny { .. }));
    assert_eq!(
        claim.scoped(async { queue.drain(8).requests.len() }).await,
        1,
        "the duplicate card is suppressed without suppressing the boundary"
    );
}

#[tokio::test]
async fn identical_explicit_requests_from_different_agents_are_not_deduplicated() {
    let queue = ApprovalRequestQueue::default();
    let claim = queue.claim(ApprovalScope::Cycle);
    claim
        .scoped(async {
            for agent in ["finance", "legal"] {
                queue.push(ApprovalRequest {
                    tool: crate::harness::approval_tool::REQUEST_APPROVAL_TOOL.to_string(),
                    reason: "Proceed?".to_string(),
                    effect: Effect {
                        kind: crate::harness::approval_tool::REQUEST_APPROVAL_TOOL.to_string(),
                        group: EffectGroup::Other,
                        amount_usd: None,
                        established_thread: false,
                        first_time_counterparty: false,
                        payload: serde_json::json!({
                            "title": "Proceed",
                            "question": "Proceed?"
                        }),
                        agent: Some(agent.to_string()),
                        run_id: None,
                    },
                });
            }
        })
        .await;

    assert_eq!(
        claim.scoped(async { queue.drain(8).requests.len() }).await,
        2
    );
}

/// May this call be granted standing? The rule as the mint path asks it,
/// so a test here and the enforcement in the default build cannot answer
/// differently.

#[tokio::test]
async fn disabled_policy_hitl_allows_calls_that_supervised_would_park() {
    let queue = ApprovalRequestQueue::default();
    let p = policy("supervised", &["payment"], None)
        .with_policy_hitl_disabled()
        .with_requests(queue.clone());

    assert_eq!(
        p.check(&request(
            "payment.send",
            serde_json::json!({ "amount_usd": 500.0 })
        ))
        .await,
        ToolPolicyDecision::Allow
    );
    assert!(
        queue
            .drain(MAX_APPROVAL_REQUESTS_PER_TURN)
            .requests
            .is_empty()
    );
    assert_eq!(p.toolbelt_mode(), PolicyMode::Full);
}

/// `toolbelt_mode` maps EVERY non-readonly tier to `Full` once policy HITL
/// is disabled, not just `supervised` — `auto` loses OpenHuman's own
/// `require_approval_for_medium_risk` exactly the same way, because
/// nothing in the mapping singles either tier out. `readonly` is the one
/// mode the guard excludes, so it must survive untouched.
#[tokio::test]
async fn disabled_hitl_maps_every_non_readonly_tier_to_full_toolbelt_mode() {
    for mode in ["auto", "supervised", "full"] {
        let p = policy(mode, &[], None).with_policy_hitl_disabled();
        assert_eq!(
            p.toolbelt_mode(),
            PolicyMode::Full,
            "{mode} with policy HITL disabled must hand OpenHuman Full, not its own tier"
        );
    }

    let readonly = policy("readonly", &[], None).with_policy_hitl_disabled();
    assert_eq!(
        readonly.toolbelt_mode(),
        PolicyMode::Readonly,
        "readonly is the one tier the mapping excludes — even with policy HITL disabled, \
         OpenHuman's own toolbelt must still see readonly"
    );
}

/// FAIL-axis: the mapping above is a value the test above already pins,
/// but nothing proved what that value does at the one place it is
/// actually consumed — `build.rs` feeds `policy.toolbelt_mode()` straight
/// into [`crate::harness::built_in::toolbelt::exec_security`], whose
/// `require_approval_for_medium_risk` is the last independent brake on a
/// shell/code/web call below this policy. This drives that real
/// composition, through the production constructor, rather than a
/// literal `PolicyMode::Full` — proving the brake really does go dark for
/// every non-readonly company once policy HITL is disabled, not just that
/// `toolbelt_mode()` returns a value that would imply it.
#[test]
fn disabled_hitl_also_disarms_the_shell_medium_risk_brake() {
    use crate::harness::built_in::toolbelt::exec_security;

    let ws = std::path::Path::new("/tmp/oc-policy-toolbelt-wiring");
    for mode in ["auto", "supervised", "full"] {
        let p = policy(mode, &[], None).with_policy_hitl_disabled();
        let security = exec_security(ws, p.toolbelt_mode());
        assert!(
            !security.require_approval_for_medium_risk,
            "{mode} with policy HITL disabled must leave OpenHuman's own medium-risk \
             shell gate unarmed, matching the mode this desk actually dispatches with"
        );
    }

    // The control: policy HITL enabled (a non-production shape) keeps the
    // brake exactly as `exec_security_shape_is_workspace_scoped_and_hardened`
    // and `auto_borrows_supervised_exec_security_rather_than_full` already
    // pin it — armed for `supervised` and `auto`.
    for mode in ["auto", "supervised"] {
        let p = policy(mode, &[], None);
        let security = exec_security(ws, p.toolbelt_mode());
        assert!(
            security.require_approval_for_medium_risk,
            "{mode} with policy HITL enabled must still arm the brake — the disarming \
             above must be the HITL-disabled path's effect, not `exec_security`'s"
        );
    }
}

/// The brake's other half: it denies now, but it does not consume the
/// grant. `readonly` is a mode a company sits in temporarily, so the same
/// approval must still be redeemable once the brake releases — inside its
/// TTL — exactly as if the readonly window had never happened.
#[tokio::test]
async fn a_readonly_denial_leaves_the_grant_redeemable_once_the_brake_releases() {
    let locked_down_queue = ApprovalRequestQueue::default();
    let grants = locked_down_queue.grants();
    let args = serde_json::json!({ "amount_usd": 40.0 });
    grants.grant(granted("finance", "payment.send", args.clone()));

    let locked_down = policy("readonly", &[], None)
        .with_requests(locked_down_queue)
        .with_agent("finance");
    assert!(
        matches!(
            locked_down
                .check(&request("payment.send", args.clone()))
                .await,
            ToolPolicyDecision::Deny { .. }
        ),
        "readonly denies the call outright"
    );

    let released = policy("full", &[], None)
        .with_requests(ApprovalRequestQueue::with_grants(grants))
        .with_agent("finance");
    assert_eq!(
        released.check(&request("payment.send", args)).await,
        ToolPolicyDecision::Allow,
        "the same grant is still redeemable once the brake releases — the readonly denial \
         must not have consumed it"
    );
}

/// INPUT-axis (TOOL-004): the brake classifies purely on `tool` and the
/// INCOMING call's own arguments (`is_external_effect`), never on whether
/// those arguments happen to match a live grant. A malformed/empty
/// argument object — missing every field the grant itself was minted
/// with — must still be denied under readonly, and the unrelated,
/// well-formed grant must still be left intact rather than being touched
/// (or the classifier panicking) on the garbage shape.
#[tokio::test]
async fn a_readonly_denial_on_malformed_arguments_still_leaves_the_grant_intact() {
    let queue = ApprovalRequestQueue::default();
    let grants = queue.grants();
    let well_formed_args = serde_json::json!({ "amount_usd": 40.0 });
    grants.grant(granted("finance", "payment.send", well_formed_args.clone()));

    let locked_down = policy("readonly", &[], None)
        .with_requests(queue)
        .with_agent("finance");
    assert!(
        matches!(
            locked_down
                .check(&request("payment.send", serde_json::json!({})))
                .await,
            ToolPolicyDecision::Deny { .. }
        ),
        "readonly must deny an external-effect call even with a garbage/empty argument \
         object, not just a well-formed one"
    );

    let released = policy("full", &[], None)
        .with_requests(ApprovalRequestQueue::with_grants(grants))
        .with_agent("finance");
    assert_eq!(
        released
            .check(&request("payment.send", well_formed_args))
            .await,
        ToolPolicyDecision::Allow,
        "the well-formed grant must be untouched by a denial evaluated against unrelated, \
         malformed arguments"
    );
}

/// CONC-axis (TOOL-004): the brake's early `return` happens strictly
/// before `consume_grant` is ever called, so it should be impossible for
/// a race to sneak a grant redemption in underneath a readonly deny. Two
/// threads hammer the SAME live grant concurrently while readonly, via
/// worker threads and a barrier — not `tokio::join!`, which has no
/// suspension point here to interleave on and would just run the two
/// calls to completion one after the other. Both must be denied, and the
/// grant must still redeem exactly once after the brake releases.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_calls_against_a_readonly_denied_grant_never_consume_it() {
    use std::sync::{Arc, Barrier};

    let queue = ApprovalRequestQueue::default();
    let grants = queue.grants();
    let args = serde_json::json!({ "amount_usd": 40.0 });
    grants.grant(granted("finance", "payment.send", args.clone()));

    let locked_down = Arc::new(
        policy("readonly", &[], None)
            .with_requests(queue)
            .with_agent("finance"),
    );
    let gate = Arc::new(Barrier::new(2));

    let call = |policy: Arc<ApprovalPolicy>, args: serde_json::Value, gate: Arc<Barrier>| {
        tokio::task::spawn_blocking(move || {
            gate.wait();
            tokio::runtime::Handle::current().block_on(policy.check(&request("payment.send", args)))
        })
    };
    let a = call(locked_down.clone(), args.clone(), gate.clone());
    let b = call(locked_down.clone(), args.clone(), gate);
    let (a, b) = (a.await.expect("joins"), b.await.expect("joins"));
    assert!(matches!(a, ToolPolicyDecision::Deny { .. }), "{a:?}");
    assert!(matches!(b, ToolPolicyDecision::Deny { .. }), "{b:?}");

    let released = policy("full", &[], None)
        .with_requests(ApprovalRequestQueue::with_grants(grants))
        .with_agent("finance");
    assert_eq!(
        released.check(&request("payment.send", args.clone())).await,
        ToolPolicyDecision::Allow,
        "the grant must still be there, unconsumed by the race"
    );
    assert!(
        matches!(
            released.check(&request("payment.send", args)).await,
            ToolPolicyDecision::RequireApproval { .. } | ToolPolicyDecision::Deny { .. }
        ),
        "and it must redeem exactly once — a second call must not still be Allow"
    );
}

/// BOUND-axis (TOOL-004): the brake denies on the tool's classification,
/// not on the declared amount, so it must hold at both ends of the amount
/// range a grant could carry — a zero-amount call and one carrying an
/// enormous declared amount both deny under readonly with their
/// respective grants left intact.
#[tokio::test]
async fn a_readonly_denial_holds_at_zero_and_at_a_very_large_declared_amount() {
    for amount in [0.0_f64, 1_000_000_000.0_f64] {
        let queue = ApprovalRequestQueue::default();
        let grants = queue.grants();
        let args = serde_json::json!({ "amount_usd": amount });
        grants.grant(granted("finance", "payment.send", args.clone()));

        let locked_down = policy("readonly", &[], None)
            .with_requests(queue)
            .with_agent("finance");
        assert!(
            matches!(
                locked_down
                    .check(&request("payment.send", args.clone()))
                    .await,
                ToolPolicyDecision::Deny { .. }
            ),
            "amount {amount}: readonly must deny regardless of the amount at either bound"
        );

        let released = policy("full", &[], None)
            .with_requests(ApprovalRequestQueue::with_grants(grants))
            .with_agent("finance");
        assert_eq!(
            released.check(&request("payment.send", args)).await,
            ToolPolicyDecision::Allow,
            "amount {amount}: the grant must still be redeemable once the brake releases"
        );
    }
}

#[tokio::test]
async fn disabled_policy_hitl_keeps_readonly_as_a_hard_denial() {
    let p = policy("readonly", &[], None).with_policy_hitl_disabled();

    assert!(matches!(
        p.check(&request(
            "payment.send",
            serde_json::json!({ "amount_usd": 5.0 })
        ))
        .await,
        ToolPolicyDecision::Deny { .. }
    ));
    assert_eq!(
        p.check(&request(
            crate::harness::approval_tool::REQUEST_APPROVAL_TOOL,
            serde_json::json!({ "title": "Ask", "question": "Proceed?" })
        ))
        .await,
        ToolPolicyDecision::Allow
    );
}

/// The shadow floor observes; it never decides (issue #2147).
///
/// Every tool below is one the consequence floor names — a send, a launch,
/// an identity change, a call carrying money. With policy HITL disabled,
/// which is the production build, all of them must still be allowed. A
/// failure here means the measurement grew teeth, which is the one way this
/// instrumentation could do harm.
#[tokio::test]
async fn the_shadow_floor_decides_nothing_with_hitl_disabled() {
    for mode in ["auto", "supervised", "full"] {
        let p = policy(mode, &[], None).with_policy_hitl_disabled();
        for (tool, args) in [
            ("chargebee_send_invoice", serde_json::json!({})),
            ("hosting_launch_site", serde_json::json!({})),
            ("composio_authorize", serde_json::json!({})),
            ("publish_artifact", serde_json::json!({})),
            ("file_write", serde_json::json!({ "amount_usd": 500.0 })),
            (
                "composio_execute",
                serde_json::json!({ "tool": "GMAIL_SEND_EMAIL" }),
            ),
        ] {
            assert_eq!(
                p.check(&request(tool, args)).await,
                ToolPolicyDecision::Allow,
                "{tool} under {mode}: the shadow floor must observe, not gate"
            );
        }
    }
}

/// The floor's own reading of a call agrees with what the tier does with it
/// while HITL is on.
///
/// Not a tautology: it pins that the population the shadow counts is the
/// population a real floor arm would decide. If a later change moved the
/// shadow call site above a hard deny or below the tier, this is what
/// notices — the count would silently start describing a different
/// question, which is the failure mode a measurement cannot self-report.
#[tokio::test]
async fn what_the_shadow_counts_is_what_full_autonomy_already_stops() {
    in_cycle(async {
        let p = policy("full", &[], None);
        for tool in ["chargebee_send_invoice", "hosting_launch_site"] {
            let args = serde_json::json!({});
            assert!(
                crate::policy::floor::evaluate(tool, &args, None).requires_human(),
                "{tool} is a floor call"
            );
            assert!(
                matches!(
                    p.check(&request(tool, args)).await,
                    ToolPolicyDecision::RequireApproval { .. }
                ),
                "{tool} is stopped by the judgement arm under full autonomy today"
            );
        }
    })
    .await;
}

/// Every tier is reachable from a manifest, parses to its own variant, and
/// nothing else parses to any of them.
///
/// This replaces `mode_maps_one_to_one_to_security_tiers`, which asserted
/// the same thing through `PolicyMode::security_tier()` — a `&'static str`
/// getter whose only caller in the tree was that test. It was deleted with
/// issue #560 rather than given a fourth arm: its entire premise was a 1:1
/// correspondence with OpenHuman's security-tier words, and `auto` has no
/// such word. The two available arms were both wrong — `"auto"` names a tier
/// upstream does not have, and `"supervised"` breaks the 1:1 the function
/// documented. Keeping a dead accessor alive by making it lie is worse than
/// the deletion.
///
/// # Why it walks [`PolicyMode::ALL`] and not [`POLICY_MODES`]
///
/// The first draft of this test walked `POLICY_MODES`, and a revert-and-check
/// caught it passing vacuously: deleting `"auto"` from that list does not
/// break a test that derives its cases from it — it just stops testing
/// `auto`. The same shape hid the trap this whole change turns on, since
/// `PolicyMode::parse` is never reached for a word the validator rejects.
///
/// So the cases come from a third list, and the two under test are checked
/// against it in both directions: `ALL` → `parse`, and `ALL` ↔
/// `POLICY_MODES` by membership *and* by length, so a word in one and not
/// the other cannot pass. Adding a variant without extending `ALL` is caught
/// by the exhaustive `match` below, which the compiler will refuse.
#[test]
fn every_tier_is_reachable_from_a_manifest_and_parses_to_itself() {
    use crate::company::POLICY_MODES;

    // A new variant makes this match non-exhaustive — the compiler forces
    // whoever adds it to come here, and the length assertions below then
    // fail until `ALL` and `POLICY_MODES` are both extended.
    for (word, mode) in PolicyMode::ALL {
        let expected = match mode {
            PolicyMode::Readonly => "readonly",
            PolicyMode::Supervised => "supervised",
            PolicyMode::Auto => "auto",
            PolicyMode::Full => "full",
        };
        assert_eq!(word, expected, "ALL pairs `{word}` with the wrong variant");

        assert_eq!(
            PolicyMode::parse(word),
            mode,
            "`{word}` does not parse to {mode:?} — it is silently downgraded"
        );
        assert!(
            POLICY_MODES.contains(&word),
            "`{word}` is a tier the runtime knows but the manifest validator rejects — \
             unreachable from a company.toml, and no policy-side test would notice"
        );
    }
    assert_eq!(
        POLICY_MODES.len(),
        PolicyMode::ALL.len(),
        "POLICY_MODES {POLICY_MODES:?} and PolicyMode::ALL disagree about how many tiers \
         exist"
    );

    // Unknown falls back to supervised — the safe default, unchanged.
    assert_eq!(PolicyMode::parse("bogus"), PolicyMode::Supervised);
}

#[tokio::test]
async fn full_allows_but_always_approve_still_parks() {
    in_cycle(async {
        let p = policy("full", &["payment"], None);
        // `file_write`, not `write_file`. This asserted the undeclared
        // `write_file`, which the per-call judgement arm (issue #338) now stops
        // fail-closed — nobody has declared what it does. The point being made
        // here is that `full` allows a tool absent from `always_approve`, so it
        // wants a tool `full` genuinely allows: `file_write` is declared, and is
        // one of the low-consequence scratch writes #444 found safe enough to
        // grant standing.
        assert_eq!(
            p.check(&request("file_write", serde_json::json!({}))).await,
            ToolPolicyDecision::Allow
        );
        assert!(matches!(
            p.check(&request("payment.send", serde_json::json!({})))
                .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
    })
    .await;
}
