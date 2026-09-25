use super::*;

// Issue #470: the `composio_execute` fixtures are built here, from the same
// key the classifier reads, so a call in a test reaches the same catalogue
// lookup a call in production does.
use super::policy_spend_cap_tests::FixedMeter;
use super::policy_test_helpers_tests::*;
use crate::policy::test_support::{composio_send_args, composio_unclassified_args};

// -----------------------------------------------------------------------
// The S2 http_request deflection guardrail (issue #1759)
// -----------------------------------------------------------------------

/// The headline case: `http_request` to `api.github.com` when `github` is
/// connected is denied, and the refusal names the Composio path.
#[tokio::test]
async fn http_request_to_a_connected_provider_is_denied_with_the_composio_route() {
    let p = full_with_connected(&["github"]);
    let decision = p
        .check(&request(
            "http_request",
            serde_json::json!({"url": "https://api.github.com/repos/o/r/issues"}),
        ))
        .await;
    match decision {
        ToolPolicyDecision::Deny { reason } => {
            assert!(reason.contains("composio_execute"), "{reason}");
            assert!(reason.contains("composio_list_tools"), "{reason}");
            assert!(reason.contains("401") || reason.contains("403"), "{reason}");
        }
        other => panic!("expected a deny naming the Composio route, got {other:?}"),
    }
}

/// Requirement #2: the same host passes through UNCHANGED when its toolkit is
/// NOT connected — the company may legitimately hit a public endpoint of a
/// provider it has not wired. Proven by asserting the decision is identical
/// to the one a policy with no connected toolkits gives: S2 did not touch it.
/// (Under `full` an un-deflected `http_request` is not `Allow` outright — the
/// per-call judge parks it — so "unchanged" is the precise claim, not
/// "allowed".)
#[tokio::test]
async fn http_request_to_the_same_host_passes_through_when_its_toolkit_is_not_connected() {
    let url = "https://api.github.com/repos/o/r";
    let baseline = full_with_connected(&[])
        .check(&request("http_request", serde_json::json!({ "url": url })))
        .await;
    // The S2 arm must never itself be a deny — the baseline is the
    // un-guarded decision this passthrough must reproduce.
    assert!(
        !matches!(baseline, ToolPolicyDecision::Deny { .. }),
        "baseline (no connected toolkits) must not deny: {baseline:?}"
    );

    // Some other toolkit connected, but not github → identical to baseline.
    let other_connected = full_with_connected(&["slack"])
        .check(&request("http_request", serde_json::json!({ "url": url })))
        .await;
    assert_eq!(
        other_connected, baseline,
        "an unconnected provider host must pass through unchanged"
    );
}

/// A non-provider host is never deflected, whatever is connected — it passes
/// through to the ordinary policy exactly as if S2 were not installed.
#[tokio::test]
async fn http_request_to_a_non_provider_host_passes_through() {
    let url = "https://example.com/data.json";
    let baseline = full_with_connected(&[])
        .check(&request("http_request", serde_json::json!({ "url": url })))
        .await;
    let guarded = full_with_connected(&["github", "gmail"])
        .check(&request("http_request", serde_json::json!({ "url": url })))
        .await;
    assert_eq!(
        guarded, baseline,
        "a non-provider host must pass through unchanged"
    );
}

/// `curl` and `web_fetch` follow the same rule as `http_request` — the whole
/// `url`-taking web family is deflected, not just one tool.
#[tokio::test]
async fn curl_and_web_fetch_are_deflected_on_the_same_terms() {
    let p = full_with_connected(&["github"]);
    for tool in ["curl", "web_fetch"] {
        let decision = p
            .check(&request(
                tool,
                serde_json::json!({"url": "https://api.github.com/repos/o/r"}),
            ))
            .await;
        assert!(
            matches!(decision, ToolPolicyDecision::Deny { .. }),
            "{tool} must be deflected to Composio, got {decision:?}"
        );
    }
}

/// INPUT-axis (TOOL-003): the deflection reads `arguments["url"]` as a
/// plain string. A call missing `url` entirely — despite every deflectable
/// tool declaring it required — must not read as "nothing to check" and
/// walk past the guardrail; it must fail CLOSED, the same as a real
/// connected-provider hit would.
#[tokio::test]
async fn s2_deflection_fails_closed_when_url_is_missing() {
    let p = full_with_connected(&["github"]);
    let decision = p
        .check(&request(
            "http_request",
            serde_json::json!({ "method": "GET" }),
        ))
        .await;
    assert!(
        matches!(decision, ToolPolicyDecision::Deny { .. }),
        "a missing `url` must not walk past the guardrail unchecked: {decision:?}"
    );
}

/// The other half: `url` present but not a plain string — a number, an
/// object, an array — is exactly as unreadable to `.as_str()` as a missing
/// key, so it must fail CLOSED on the same terms rather than silently
/// passing through because the type did not match.
#[tokio::test]
async fn s2_deflection_fails_closed_when_url_is_not_a_string() {
    let p = full_with_connected(&["github"]);
    for bad_url in [
        serde_json::json!(12345),
        serde_json::json!({ "host": "api.github.com" }),
        serde_json::json!(["https://api.github.com"]),
        serde_json::json!(null),
    ] {
        let decision = p
            .check(&request(
                "http_request",
                serde_json::json!({ "url": bad_url }),
            ))
            .await;
        assert!(
            matches!(decision, ToolPolicyDecision::Deny { .. }),
            "a non-string `url` ({bad_url:?}) must not walk past the guardrail unchecked: \
             {decision:?}"
        );
    }
}

/// Requirement #2 still holds once the arm fails closed: with NO connected
/// toolkits at all, a missing/malformed `url` is not this guardrail's
/// business — the arm's outer condition (`!connected_composio_toolkits.is_empty()`)
/// never engages, so the call falls through to whatever the ordinary
/// policy decides for an `http_request`/`curl`/`web_fetch` with no
/// bounded target. That ordinary decision may reasonably be a park (an
/// unbounded target is not automatically safe) — the property this pins
/// is narrower and precise: it must never be a DENY manufactured by THIS
/// arm, since with nothing connected the arm has nothing to deny it for.
#[tokio::test]
async fn s2_missing_url_passes_through_with_no_connected_toolkits() {
    in_cycle(async {
        for tool in ["http_request", "curl", "web_fetch"] {
            let decision = full_with_connected(&[])
                .check(&request(tool, serde_json::json!({ "method": "GET" })))
                .await;
            assert!(
                !matches!(decision, ToolPolicyDecision::Deny { .. }),
                "with nothing connected, a missing `url` on `{tool}` must not be denied by this \
             arm: {decision:?}"
            );
        }
    })
    .await;
}

/// STATE-axis (TOOL-003): the "connected" state a company record supplies
/// is free text an operator or an upstream sync wrote, not a normalised
/// key, so it can arrive with stray casing or whitespace. Deflection must
/// still recognise it — the same normalisation
/// `http_request_to_a_connected_provider_is_denied_with_the_composio_route`
/// relies on implicitly, pinned here explicitly against a messy entry.
#[tokio::test]
async fn s2_deflection_normalises_a_messily_cased_connected_toolkit_entry() {
    let p = full_with_connected(&["  GitHub  "]);
    let decision = p
        .check(&request(
            "http_request",
            serde_json::json!({ "url": "https://api.github.com/repos/o/r" }),
        ))
        .await;
    assert!(
        matches!(decision, ToolPolicyDecision::Deny { .. }),
        "a connected entry with stray case/whitespace must still be recognised: {decision:?}"
    );
}

/// FAIL-axis (TOOL-003): a `url` that IS a plain string but does not parse
/// as one (unlike the INPUT-axis cases above, which are the wrong JSON
/// *type*) intentionally passes through — `url::Url::parse` fails,
/// `web_call_deflection` has no host to check, and nothing this call could
/// reach depends on that host either, since the underlying web tool cannot
/// make an unparseable string into a request. Fail-open here is the
/// deliberate, safe direction; pinned so it is not confused with the
/// missing/wrong-type cases that were fixed to fail closed.
#[tokio::test]
async fn s2_deflection_passes_through_an_unparseable_url_string() {
    let baseline = full_with_connected(&[])
        .check(&request(
            "http_request",
            serde_json::json!({ "url": "not a url" }),
        ))
        .await;
    let guarded = full_with_connected(&["github"])
        .check(&request(
            "http_request",
            serde_json::json!({ "url": "not a url" }),
        ))
        .await;
    assert_eq!(
        guarded, baseline,
        "a syntactically invalid url string cannot resolve to any host, so it must pass \
         through exactly as if nothing were connected: {guarded:?}"
    );
}

/// BOUND-axis (TOOL-003): the path-prefix boundary on a toolkit whose
/// table requires one (`gmail`'s `www.googleapis.com` entry). A path that
/// starts with the required prefix is caught; a path one character short
/// of it — missing the trailing slash the table requires — is not, and
/// must pass through rather than being caught by a looser `starts_with`.
#[tokio::test]
async fn s2_deflection_respects_the_path_prefix_boundary() {
    let p = full_with_connected(&["gmail"]);
    let inside = p
        .check(&request(
            "http_request",
            serde_json::json!({ "url": "https://www.googleapis.com/gmail/v1/users/me" }),
        ))
        .await;
    assert!(
        matches!(inside, ToolPolicyDecision::Deny { .. }),
        "a path starting with the required prefix must be caught: {inside:?}"
    );

    let one_short = p
        .check(&request(
            "http_request",
            serde_json::json!({ "url": "https://www.googleapis.com/gmail" }),
        ))
        .await;
    let baseline = full_with_connected(&[])
        .check(&request(
            "http_request",
            serde_json::json!({ "url": "https://www.googleapis.com/gmail" }),
        ))
        .await;
    assert_eq!(
        one_short, baseline,
        "a path one character short of the required prefix (no trailing slash) must not be \
         caught by a looser match: {one_short:?}"
    );
}

/// BOUND-axis (TOOL-003), the other edge: a connected-toolkit list that is
/// non-empty but holds only blank entries must behave like the empty-list
/// baseline — a stray blank string must not accidentally become a
/// wildcard that matches every host.
#[tokio::test]
async fn s2_deflection_skips_blank_connected_entries_without_matching_everything() {
    let p = full_with_connected(&["", "   "]);
    let decision = p
        .check(&request(
            "http_request",
            serde_json::json!({ "url": "https://api.github.com/repos/o/r" }),
        ))
        .await;
    let baseline = full_with_connected(&[])
        .check(&request(
            "http_request",
            serde_json::json!({ "url": "https://api.github.com/repos/o/r" }),
        ))
        .await;
    assert_eq!(
        decision, baseline,
        "blank connected entries must not match any host: {decision:?}"
    );
}

/// The deflection outranks a single-use grant: a grant is an operator
/// approving one call, but a raw call to a connected provider cannot succeed
/// by this route for anyone, so the guardrail still refuses it.
#[tokio::test]
async fn the_deflection_outranks_a_grant() {
    let p = full_with_connected(&["github"]);
    let args = serde_json::json!({"url": "https://api.github.com/repos/o/r"});
    // Even if a grant existed for this exact call, the arm above the grant
    // check refuses it. `http_request` is not grantable, but the ordering is
    // what this pins: the deny fires before any grant arm is consulted.
    assert!(
        matches!(
            p.check(&request("http_request", args)).await,
            ToolPolicyDecision::Deny { .. }
        ),
        "the S2 arm must sit above the grant checks"
    );
}

// Issue #2150 (Rung 3, epic #1817): trusted-dispatch-origin admission.
//
// These tests exercise `trusted_dispatch_admits` both through `check()`
// (real declared tools, real tiers) and directly (the `ScopedGrantable`
// scope-matching branch, which no tool in today's declaration table
// reaches through `check()` — the same gap
// `a_grant_scoped_to_one_provider_does_not_admit_another_providers_read`
// above records for the standing-grant scope check this mirrors).

use crate::harness::built_in::run_origin::{DispatchSource, RunOrigin, claim};

#[tokio::test]
async fn an_unlabelled_turn_decides_exactly_as_before() {
    in_cycle(async {
        let p = policy("supervised", &[], None).with_agent("ops");
        assert!(
            matches!(
                p.check(&request("file_write", serde_json::json!({}))).await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "no origin was scoped, so this must park exactly as it did before #2150"
        );
    })
    .await;
}

#[tokio::test]
async fn an_operator_origin_decides_the_same_as_unlabelled() {
    in_cycle(async {
        let p = policy("supervised", &[], None).with_agent("ops");
        let origin = claim(RunOrigin::Operator);
        assert!(
            matches!(
                origin
                    .scoped(p.check(&request("file_write", serde_json::json!({}))))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "a live operator turn earns no trust from this arm — only `Dispatched` does"
        );
    })
    .await;
}

/// The headline case: a call an operator could already have granted
/// standing for (`Standing::Grantable`, so no scope to check) parks under
/// `supervised` absent a grant — `an_expired_standing_grant_re_parks`
/// above pins that baseline — and a dispatched run whose agent matches is
/// admitted without ever raising an approval row.
#[tokio::test]
async fn a_dispatched_run_admits_a_grantable_call_with_no_approval_row() {
    let queue = ApprovalRequestQueue::default();
    let p = policy("supervised", &[], None)
        .with_requests(queue.clone())
        .with_agent("ops");
    let origin = dispatched("ops");
    assert_eq!(
        origin
            .scoped(p.check(&request("file_write", serde_json::json!({}))))
            .await,
        ToolPolicyDecision::Allow,
        "a scratch write is exactly what an operator could have granted standing for"
    );
    assert_eq!(
        queue.queued(),
        0,
        "an admitted call must never raise an approval row"
    );
}

/// A dispatched run carrying money still reaches a person.
///
/// This pins the **outcome**, not any one arm, and the distinction is
/// deliberate. `trusted_dispatch_admits` consults the consequence floor,
/// but deleting that consultation leaves this test — and the whole suite —
/// green, because `judge` re-judges whatever the tier allowed and stops a
/// declared amount at the tail of `check`. The floor consultation is a belt
/// whose braces are `judge`; see the note at that line for the two facts
/// that make it unreachable today and what would change either.
///
/// What this test is worth is the property itself: trust granted for being
/// dispatched must not become permission to spend. Whichever arm enforces
/// that, an operator sees the call.
///
/// `supervised` rather than `auto`, deliberately: under `auto` a
/// `Grantable` tool never reaches the trusted arm at all, so the test would
/// pass without exercising the admission path.
#[tokio::test]
async fn the_consequence_floor_outranks_a_trusted_dispatch() {
    in_cycle(async {
        let queue = ApprovalRequestQueue::default();
        let p = policy("supervised", &[], None)
            .with_requests(queue.clone())
            .with_agent("ops");
        let origin = dispatched("ops");
        let spend = serde_json::json!({ "amount_usd": 500.0 });
        assert!(
            matches!(
                origin.scoped(p.check(&request("file_write", spend))).await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "a dispatched run carrying money must reach a person, whatever the tool's standing"
        );
        assert_eq!(
            queue.queued(),
            1,
            "and it must raise exactly one approval row for them to answer"
        );
    })
    .await;
}

/// The same run, calling a `Standing::PerCall` tool, still raises exactly
/// one approval row — trust never reaches a call nobody could have handed
/// over ahead of time.
#[tokio::test]
async fn a_dispatched_run_still_parks_a_percall_send() {
    in_cycle(async {
        let queue = ApprovalRequestQueue::default();
        let p = policy("supervised", &[], None)
            .with_requests(queue.clone())
            .with_agent("ops");
        let origin = dispatched("ops");
        assert!(
            matches!(
                origin
                    .scoped(p.check(&request("composio_execute", composio_send_args())))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "a send is `Standing::PerCall` — nothing here for a dispatch to have earned"
        );
        assert_eq!(queue.queued(), 1, "exactly one approval row for the send");
    })
    .await;
}

/// Delegation must not be a privilege-escalation primitive: an origin
/// dispatched to one agent must not admit a call from a different agent's
/// policy instance, even though the task-local is still ambient (the
/// same-task inheritance `run_origin`'s own tests cover).
#[tokio::test]
async fn a_mismatched_agent_does_not_admit() {
    in_cycle(async {
        let p = policy("supervised", &[], None).with_agent("marketing");
        let origin = dispatched("ops");
        assert!(
            matches!(
                origin
                    .scoped(p.check(&request("file_write", serde_json::json!({}))))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "the dispatch named `ops`; a different agent's policy must not be trusted by it"
        );
    })
    .await;
}

/// Issue #674's split, reasserted at the constructor: `judge` is silent on
/// an authored workflow node, so admitting one through trust as well would
/// remove the ceiling `always_approve` still leaves on that path.
#[tokio::test]
async fn the_arm_never_fires_for_an_authored_workflow_node() {
    in_cycle(async {
        let p = policy("supervised", &[], None)
            .with_agent("ops")
            .for_authored_workflow_nodes();
        let origin = dispatched("ops");
        assert!(
            matches!(
                origin
                    .scoped(p.check(&request("file_write", serde_json::json!({}))))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "an authored node must never be admitted by this arm, whatever the origin says"
        );
    })
    .await;
}

/// `shell` is `Standing::PerCall` (arbitrary code, unbounded reach) and an
/// undeclared tool defaults to `Standing::PerCall` too (never `Grantable`
/// by omission) — both stop for a human whatever this run's origin says.
#[tokio::test]
async fn shell_and_an_undeclared_tool_still_park_under_a_dispatched_origin() {
    in_cycle(async {
        let p = policy("supervised", &[], None).with_agent("ops");
        for tool in ["shell", "some_tool_nobody_declared"] {
            let origin = dispatched("ops");
            assert!(
                matches!(
                    origin
                        .scoped(p.check(&request(tool, serde_json::json!({}))))
                        .await,
                    ToolPolicyDecision::RequireApproval { .. }
                ),
                "`{tool}` must still park inside a dispatched run"
            );
        }
    })
    .await;
}

/// The consequence floor (issue #1817's own arm) outranks a trusted
/// origin for every tool it declares irreversible — exhaustive over the
/// declaration table, the same style `floor`'s own tests use, so a future
/// tool added to either table is covered without a second hand-written
/// list.
#[tokio::test]
async fn a_dispatched_origin_still_parks_every_floor_covered_tool() {
    for tool in crate::policy::consequence::declared_tools() {
        let args = serde_json::json!({});
        let consequence = crate::policy::consequence_of(tool, &args);
        if !crate::policy::floor::evaluate_consequence(tool, consequence, &args, None)
            .requires_human()
        {
            continue;
        }
        for mode in ["auto", "supervised"] {
            let p = policy(mode, &[], None).with_agent("ops");
            let origin = dispatched("ops");
            let decision = origin.scoped(p.check(&request(tool, args.clone()))).await;
            assert!(
                !matches!(decision, ToolPolicyDecision::Allow),
                "`{tool}` commits the company under `{mode}`; a dispatched origin must \
                 never admit it, got {decision:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_scope_matched_call_inside_a_trusted_run_is_admitted() {
    let p = policy("supervised", &[], None).with_agent("ops");
    let origin = claim(RunOrigin::Dispatched {
        agent: "ops".to_string(),
        source: DispatchSource::Task,
        scope: Some("gmail".to_string()),
    });
    let admitted = origin
        .scoped(async {
            p.trusted_dispatch_admits(
                "composio_execute",
                &composio_send_args(),
                scoped_grantable(),
            )
        })
        .await;
    assert!(
        admitted,
        "the call's own scope (gmail, from GMAIL_SEND_EMAIL) matches the run's declared scope"
    );
}

#[tokio::test]
async fn an_out_of_scope_call_inside_a_trusted_run_still_parks() {
    let p = policy("supervised", &[], None).with_agent("ops");
    let origin = claim(RunOrigin::Dispatched {
        agent: "ops".to_string(),
        source: DispatchSource::Task,
        scope: Some("github".to_string()),
    });
    let admitted = origin
        .scoped(async {
            p.trusted_dispatch_admits(
                "composio_execute",
                &composio_send_args(),
                scoped_grantable(),
            )
        })
        .await;
    assert!(
        !admitted,
        "the run is scoped to github; a call whose own scope resolves to gmail must still park"
    );
}

#[tokio::test]
async fn an_underivable_scope_refuses_rather_than_admits() {
    let p = policy("supervised", &[], None).with_agent("ops");
    // No scope declared on the run either — the underivable-call-scope
    // refusal must hold even when it would otherwise be the more
    // permissive reading (an unscoped run admitting an unscoped call).
    let origin = dispatched("ops");
    let admitted = origin
        .scoped(async {
            p.trusted_dispatch_admits(
                "composio_execute",
                &composio_unclassified_args(),
                scoped_grantable(),
            )
        })
        .await;
    assert!(
        !admitted,
        "an action the catalogue cannot place resolves no scope; refuse rather than admit"
    );
}

/// Everything above the mode dispatch keeps deciding first, whatever this
/// run's origin says: the `readonly` brake, `always_approve`, the daily
/// spend cap, and a standing deny.
#[tokio::test]
async fn a_dispatched_origin_does_not_bypass_the_arms_above_the_mode_dispatch() {
    in_cycle(async {
        // `readonly` denies an external effect before any grant or origin is
        // consulted.
        let p = policy("readonly", &[], None).with_agent("ops");
        assert!(
            matches!(
                dispatched("ops")
                    .scoped(p.check(&request("file_write", serde_json::json!({}))))
                    .await,
                ToolPolicyDecision::Deny { .. }
            ),
            "readonly must still deny, dispatched origin or not"
        );

        // `always_approve` wins over every tier, `full` included.
        let p = policy("full", &["file_write"], None).with_agent("ops");
        assert!(
            matches!(
                dispatched("ops")
                    .scoped(p.check(&request("file_write", serde_json::json!({}))))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "always_approve must still park under full, dispatched origin or not"
        );

        // The per-agent daily cap parks a priced call once the agent is out of
        // budget, above the tier dispatch entirely.
        let meter = FixedMeter::with(vec![spend_sample("ops", 5.00, today())]);
        let (capped, _) = capped_policy("supervised", None, 5.0, "ops", meter);
        assert!(
            matches!(
                dispatched("ops")
                    .scoped(capped.check(&request(
                        "web_search",
                        serde_json::json!({ "query": "acme pricing" })
                    )))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "the daily cap must still park at cap, dispatched origin or not"
        );

        // A standing deny is a company saying "not this", which a run's own
        // origin cannot override.
        let (denying, grants) = granting_policy("supervised", &[], "ops");
        grants.grant_standing(standing_verdict(
            "ops",
            "file_write",
            far_future(),
            Verdict::Deny,
        ));
        assert!(
            matches!(
                dispatched("ops")
                    .scoped(denying.check(&request("file_write", serde_json::json!({}))))
                    .await,
                ToolPolicyDecision::Deny { .. }
            ),
            "a standing deny must still refuse, dispatched origin or not"
        );
    })
    .await;
}

/// Structural proof that the arm adds nothing outside `auto`/`supervised`:
/// scoping a dispatched origin must never change `full`'s or `readonly`'s
/// verdict, because the code for this arm is not reachable from either of
/// their match arms.
#[tokio::test]
async fn the_arm_is_inert_under_full_and_readonly() {
    for mode in ["full", "readonly"] {
        let p = policy(mode, &[], None).with_agent("ops");
        let baseline = p.check(&request("file_write", serde_json::json!({}))).await;
        let with_origin = dispatched("ops")
            .scoped(p.check(&request("file_write", serde_json::json!({}))))
            .await;
        assert_eq!(
            with_origin, baseline,
            "`{mode}` must decide identically whether or not a dispatch origin is scoped"
        );
    }
}
