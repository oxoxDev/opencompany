use super::*;

// Issue #470: the `composio_execute` fixtures are built here, from the same
// key the classifier reads, so a call in a test reaches the same catalogue
// lookup a call in production does.
use super::policy_test_helpers_tests::*;
use crate::policy::test_support::composio_send_args;

/// The two approval paths must decide the same operator list the same way
/// (issue #684).
///
/// This is the assertion whose absence let the defect ship. Each path had
/// its own matcher and its own tests, and each path's tests passed: the
/// native gate matched dotted kinds exactly, this one matched tool names
/// with a leading-segment rule, and nothing anywhere compared them. So
/// `always_approve = ["payment"]` parked here and waved through there, and
/// the shipped default — three dotted kinds, no tool names — was live on
/// the gate and inert on the harness, which is the path a company using the
/// openhuman toolbelt actually runs.
///
/// It asserts agreement rather than a fixed verdict per path deliberately.
/// Pinning "the harness parks `payment`" would go green again the moment
/// the two implementations drifted apart in the other direction.
#[tokio::test]
async fn both_approval_paths_agree_on_the_same_always_approve_list() {
    in_cycle(async {
        use crate::policy::ManifestApprovalGate;
        use crate::ports::approvals::ApprovalGate;
        use crate::ports::types::PolicyDecision;

        // A leading segment, an exact dotted kind, a bare tool name, an
        // unrelated declared tool that must NOT be gated, and a case variant.
        //
        // Every name here is one both paths leave to the fence: the tier has no
        // opinion about it under `full`, **and** it is a declared, non-priced
        // tool, so the per-call judgement (issue #338) is silent on it too — the
        // thing that used to make a not-gated name diverge after `full` decided
        // to allow was that undeclared tools stop under the judgement. The
        // near-miss the segment boundary exists to exclude (`payment` vs
        // `payroll.export`) stopped fitting here once the judge began stopping
        // undeclared tools; that boundary is pinned in `always_approve::test`
        // instead. A priced name like `web_search` would drag the harness's
        // metered-read and budget arms into a comparison that is not about
        // `always_approve`, so it is deliberately absent here.
        let fence = &["payment", "filing.submit", "publish_artifact"];
        let names = [
            "payment.send",
            "payment",
            "filing.submit",
            "publish_artifact",
            "PUBLISH_ARTIFACT",
            "workspace_read",
        ];

        // `full` on both sides, so the tier decides nothing and any parking
        // observed is the override's doing.
        let harness = policy("full", fence, None);
        let gate = ManifestApprovalGate::new(Policy {
            mode: "full".to_string(),
            always_approve: fence.iter().map(|s| s.to_string()).collect(),
            auto_approve_under_usd: None,
            approval_ttl_hours: None,
        });

        let mut agreed = 0;
        for name in names {
            let harness_parks = matches!(
                harness.check(&request(name, serde_json::json!({}))).await,
                ToolPolicyDecision::RequireApproval { .. }
            );
            let effect = Effect {
                kind: name.to_string(),
                group: EffectGroup::Other,
                amount_usd: None,
                established_thread: false,
                first_time_counterparty: false,
                payload: serde_json::Value::Null,
                agent: None,
                run_id: None,
            };
            let gate_parks = matches!(
                gate.evaluate(&CompanyId::new("acme"), &effect)
                    .await
                    .unwrap(),
                PolicyDecision::RequireApproval
            );
            assert_eq!(
                harness_parks, gate_parks,
                "`{name}` parks on one approval path and not the other — \
             one operator list, two answers (issue #684)"
            );
            agreed += 1;
        }
        assert_eq!(agreed, names.len(), "every name must have been compared");

        // Non-vacuity: the comparison above is only worth something if the
        // fence actually separates these names. Two paths that both allowed
        // everything would agree perfectly and prove nothing.
        assert!(matches!(
            harness
                .check(&request("payment.send", serde_json::json!({})))
                .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
        assert_eq!(
            harness
                .check(&request("workspace_read", serde_json::json!({})))
                .await,
            ToolPolicyDecision::Allow
        );

        // The fence near-miss, pinned where the answer is stable.
        //
        // `payroll.export` is what the shared matcher tests call the near-miss:
        // sharing `payment`'s first four letters is not the same capability, so
        // the operator list names it nowhere and must not gate it. All three
        // statements are asserted because the two paths legitimately differ on
        // the last of them:
        //
        // * the *matcher* — the part both paths actually share — does not gate
        //   it;
        // * the gate, effect-level and mode-driven, hands `full` the Allow this
        //   fence implies;
        // * the harness instead parks it, because #338's per-call judgement
        //   fail-closes an undeclared non-read call — a layer the effect gate
        //   does not have, and the reason the near-miss cannot live inside the
        //   agreement loop above.
        let fence_list: Vec<String> = fence.iter().map(|s| s.to_string()).collect();
        assert!(
            !crate::policy::always_approve::matches(&fence_list, "payroll.export"),
            "the operator list must not gate the leading-segment near-miss"
        );
        assert!(matches!(
            harness
                .check(&request("payroll.export", serde_json::json!({})))
                .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
        let near_miss_effect = Effect {
            kind: "payroll.export".to_string(),
            group: EffectGroup::Other,
            amount_usd: None,
            established_thread: false,
            first_time_counterparty: false,
            payload: serde_json::Value::Null,
            agent: None,
            run_id: None,
        };
        assert_eq!(
            gate.evaluate(&CompanyId::new("acme"), &near_miss_effect)
                .await
                .unwrap(),
            PolicyDecision::Allow
        );
    })
    .await;
}

/// Issue #560's contract, stated as the operator reads it: the agent works
/// without interrupting me, and stops before anything that leaves the
/// building or spends money.
///
/// Both halves are asserted in one test on purpose. A tier is a *line*, and
/// a test that only checked the permissive half would pass just as happily
/// against `full` — which is precisely the mistake `auto` exists to avoid.
#[tokio::test]
async fn auto_runs_sandbox_writes_and_outward_reads_but_parks_anything_that_leaves() {
    in_cycle(async {
        let p = policy("auto", &[], None);

        // Runs unattended: the agent's own scratch space, this company's own
        // memory, catalogue reads, and a read scoped to one connected account.
        for (tool, args) in [
            ("file_write", serde_json::json!({})),
            ("edit", serde_json::json!({})),
            ("apply_patch", serde_json::json!({})),
            ("csv_export", serde_json::json!({})),
            ("memory_store", serde_json::json!({})),
            ("file_read", serde_json::json!({})),
            ("mcp_list_tools", serde_json::json!({})),
            ("composio_list_tools", serde_json::json!({})),
            (
                "composio_execute",
                serde_json::json!({ "tool": "GITHUB_LIST_PULL_REQUESTS" }),
            ),
            (
                "http_request",
                serde_json::json!({ "method": "GET", "url": "https://api.example.com/items" }),
            ),
            (
                "web_fetch",
                serde_json::json!({ "url": "https://example.com/docs" }),
            ),
            // Issue #903: handing the finished work to the operator. It writes
            // into the company's own workspace and artifact chain — no
            // counterparty, no address — and the chain versions it, so the
            // company can undo it alone. Parking it made every deliverable wait
            // on a human; one 9-node pipeline run produced 15 such waits.
            ("publish_artifact", serde_json::json!({})),
        ] {
            assert_eq!(
                p.check(&request(tool, args)).await,
                ToolPolicyDecision::Allow,
                "{tool} should run unattended under auto — the tier is unusable if it interrupts \
             the agent's own work"
            );
        }

        // Issue #903, the two ways an operator keeps a human on every hand-over.
        // Both must survive the change above, or `auto` has quietly become the
        // only tier and the choice is gone.
        assert!(
            matches!(
                policy("supervised", &[], None)
                    .check(&request("publish_artifact", serde_json::json!({})))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "a supervised desk must still see a publish before it lands"
        );
        assert!(
            matches!(
                policy("auto", &["publish_artifact"], None)
                    .check(&request("publish_artifact", serde_json::json!({})))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "always_approve names it, and always_approve wins over every tier"
        );

        // Still parks: arbitrary code, arbitrary addresses, a configured remote,
        // operator-authored guidance, third-party effects, real money on submit,
        // and a workflow whose contents this layer cannot see.
        for (tool, args) in [
            ("shell", serde_json::json!({})),
            (
                "http_request",
                serde_json::json!({ "method": "POST", "url": "https://api.example.com/items" }),
            ),
            // `curl` always streams its response to a file under the
            // workspace `downloads/` dir (`CurlTool::execute`), unlike
            // `web_fetch`/read-shaped `http_request` — so a readable host
            // must not exempt it from parking.
            (
                "curl",
                serde_json::json!({ "url": "https://example.com/data.json" }),
            ),
            ("git_operations", serde_json::json!({})),
            ("workspace_write", serde_json::json!({})),
            ("workspace_create", serde_json::json!({})),
            ("workspace_delete", serde_json::json!({})),
            ("workspace_rename", serde_json::json!({})),
            ("media_generate_image", serde_json::json!({})),
            ("media_generate_video", serde_json::json!({})),
            ("mcp_call_tool", serde_json::json!({})),
            ("mcp_registry_tool_call", serde_json::json!({})),
            ("run_workflow", serde_json::json!({})),
            ("composio_authorize", serde_json::json!({})),
            (
                "composio_execute",
                serde_json::json!({ "tool": "GMAIL_SEND_EMAIL" }),
            ),
            // An action the provider catalogue does not name is a send, so the
            // cautious verdict survives into the new tier rather than being
            // re-decided by it.
            (
                "composio_execute",
                serde_json::json!({ "tool": "GITHUB_INVENT_A_NEW_VERB" }),
            ),
            // A tool nobody has declared must not run unattended by omission.
            ("some_tool_nobody_declared", serde_json::json!({})),
        ] {
            assert!(
                matches!(
                    p.check(&request(tool, args)).await,
                    ToolPolicyDecision::RequireApproval { .. }
                ),
                "{tool} leaves the company or spends money and must still park under auto"
            );
        }
    })
    .await;
}

/// **Codex review finding on PR #2140 (`3952368155`).** `full` autonomy is
/// the one tier with no per-call gate at all
/// ([`PolicyMode::Full`](PolicyMode::Full) allows every consequential call
/// outright), so it never reaches `ManifestApprovalGate::evaluate` or
/// `park` — the choke point the emergency stop is enforced at everywhere
/// else. An in-flight turn that survives the stop by design (see
/// `CompanyRuntime::ensure_not_emergency_stopped`) could dispatch a
/// consequential harness tool through this tier with nothing to refuse it.
///
/// `EffectGroup::Other` calls (`spawn_task` and the like) still run, matching
/// `evaluate`/`park`'s own exemption.
#[tokio::test]
async fn full_autonomy_still_refuses_a_consequential_call_while_stopped() {
    use crate::policy::ManifestApprovalGate;

    let gate = Arc::new(ManifestApprovalGate::new(Policy {
        mode: "full".to_string(),
        always_approve: Vec::new(),
        auto_approve_under_usd: None,
        approval_ttl_hours: None,
    }));
    gate.set_emergency(true);
    let p = policy("full", &[], None).with_emergency_gate(gate.clone());

    assert!(
        matches!(
            p.check(&request("publish_artifact", serde_json::json!({})))
                .await,
            ToolPolicyDecision::Deny { .. }
        ),
        "a consequential call must refuse under `full` once the company is stopped, \
         the same way `evaluate`/`park` already refuse one on every other tier"
    );

    assert_eq!(
        p.check(&request("spawn_task", serde_json::json!({}))).await,
        ToolPolicyDecision::Allow,
        "an `EffectGroup::Other` call is exempt while stopped, matching evaluate/park"
    );

    gate.set_emergency(false);
    assert_eq!(
        p.check(&request("publish_artifact", serde_json::json!({})))
            .await,
        ToolPolicyDecision::Allow,
        "releasing the stop restores `full`'s ordinary blanket allow"
    );
}

/// **CodeRabbit review finding on PR #2140 (`3960328855`, CWE-863).** The
/// emergency-stop veto above used to sit AFTER a redeemed single-use
/// grant, `policy_hitl_enabled == false`, and `auto_approve_under_usd` —
/// each an unconditional `Allow` on its own, so any one of them let a
/// consequential call through on every tier, not just `full`. This one
/// pins the single-use-grant path.
#[tokio::test]
async fn a_redeemed_grant_still_refuses_a_consequential_call_while_stopped() {
    use crate::policy::ManifestApprovalGate;

    let (p, grants) = granting_policy("supervised", &[], "finance");
    let gate = Arc::new(ManifestApprovalGate::new(Policy {
        mode: "supervised".to_string(),
        always_approve: Vec::new(),
        auto_approve_under_usd: None,
        approval_ttl_hours: None,
    }));
    let p = p.with_emergency_gate(gate.clone());
    let args = composio_send_args();
    grants.grant(granted("finance", "composio_execute", args.clone()));

    gate.set_emergency(true);
    assert!(
        matches!(
            p.check(&request("composio_execute", args.clone())).await,
            ToolPolicyDecision::Deny { .. }
        ),
        "a live single-use grant must not let a consequential call through while stopped"
    );
    assert_eq!(
        grants.live_count(),
        1,
        "the refused call must not consume the grant it never redeemed"
    );

    gate.set_emergency(false);
    assert_eq!(
        p.check(&request("composio_execute", args)).await,
        ToolPolicyDecision::Allow,
        "releasing the stop lets the still-live grant redeem normally"
    );
}

/// See the single-use-grant test above for the finding this pins. This one
/// covers the `policy_hitl_enabled == false` path — every production
/// roster (`with_policy_hitl_disabled` at construction).
#[tokio::test]
async fn disabled_policy_hitl_still_refuses_a_consequential_call_while_stopped() {
    use crate::policy::ManifestApprovalGate;

    let gate = Arc::new(ManifestApprovalGate::new(Policy {
        mode: "full".to_string(),
        always_approve: Vec::new(),
        auto_approve_under_usd: None,
        approval_ttl_hours: None,
    }));
    gate.set_emergency(true);
    let p = policy("full", &[], None)
        .with_policy_hitl_disabled()
        .with_emergency_gate(gate.clone());

    assert!(
        matches!(
            p.check(&request("publish_artifact", serde_json::json!({})))
                .await,
            ToolPolicyDecision::Deny { .. }
        ),
        "a roster built with policy HITL disabled must still refuse a consequential call \
         while stopped"
    );

    gate.set_emergency(false);
    assert_eq!(
        p.check(&request("publish_artifact", serde_json::json!({})))
            .await,
        ToolPolicyDecision::Allow,
        "releasing the stop restores the HITL-disabled blanket allow"
    );
}

/// See the single-use-grant test above for the finding this pins. This one
/// covers the `auto_approve_under_usd` path.
#[tokio::test]
async fn auto_approve_under_usd_still_refuses_a_consequential_call_while_stopped() {
    use crate::policy::ManifestApprovalGate;

    let gate = Arc::new(ManifestApprovalGate::new(Policy {
        mode: "supervised".to_string(),
        always_approve: Vec::new(),
        auto_approve_under_usd: Some(50.0),
        approval_ttl_hours: None,
    }));
    gate.set_emergency(true);
    let p = policy("supervised", &[], Some(50.0)).with_emergency_gate(gate.clone());
    let args = serde_json::json!({ "amount_usd": 10.0 });

    assert!(
        matches!(
            p.check(&request("payment.send", args.clone())).await,
            ToolPolicyDecision::Deny { .. }
        ),
        "an under-threshold auto-approved spend must still refuse while stopped"
    );

    gate.set_emergency(false);
    assert_eq!(
        p.check(&request("payment.send", args)).await,
        ToolPolicyDecision::Allow,
        "releasing the stop restores the auto-approve-under-threshold allow"
    );
}

/// **Issue #1124, end to end at the policy layer.** A bridge call to a
/// remote tool the operator has declared read-only on this server runs
/// unattended under `auto`; the same call with no declaration, a write on
/// the same server, and a read on an undeclared server all still park.
///
/// The `consequence.rs` tests prove [`mcp_call_reach`](crate::policy::consequence::mcp_call_reach)
/// in isolation. This one proves the wiring that makes it fire in
/// production — `with_mcp_reads` installs the declaration, `consequence_for`
/// routes the bridge tool through it, and the `auto` arm of `check` reads
/// the downgraded reach. Reverting any link (dropping `with_mcp_reads`,
/// routing the bridge tool through the plain `consequence_of`, or reverting
/// the classifier) puts the first two `Allow`s back to `RequireApproval`.
#[tokio::test]
async fn auto_runs_a_server_declared_read_only_mcp_call_but_parks_the_rest() {
    in_cycle(async {
        let reads = crate::policy::McpReadSet::from_pairs([
            ("jira".to_string(), "get_issue".to_string()),
            ("registry-42".to_string(), "list_rows".to_string()),
        ]);
        let p = policy("auto", &[], None).with_mcp_reads(reads);

        // The declared read on each bridge tool runs unattended.
        for (tool, args) in [
            (
                "mcp_call_tool",
                serde_json::json!({ "server": "jira", "tool": "get_issue", "arguments": {} }),
            ),
            (
                "mcp_registry_tool_call",
                serde_json::json!({
                    "server_id": "registry-42",
                    "tool_name": "list_rows",
                    "arguments": {},
                }),
            ),
        ] {
            assert_eq!(
                p.check(&request(tool, args)).await,
                ToolPolicyDecision::Allow,
                "{tool} names a server-declared read and must run unattended under auto"
            );
        }

        // Everything else through the same policy still parks: a write on the
        // declared server, a read on an undeclared server, and a bridge call the
        // gate cannot read the (server, tool) pair from.
        for (tool, args) in [
            (
                "mcp_call_tool",
                serde_json::json!({ "server": "jira", "tool": "create_issue", "arguments": {} }),
            ),
            (
                "mcp_call_tool",
                serde_json::json!({ "server": "confluence", "tool": "get_issue", "arguments": {} }),
            ),
            (
                "mcp_registry_tool_call",
                serde_json::json!({
                    "server_id": "registry-42",
                    "tool_name": "write_row",
                    "arguments": {},
                }),
            ),
            ("mcp_call_tool", serde_json::json!({})),
        ] {
            assert!(
                matches!(
                    p.check(&request(tool, args)).await,
                    ToolPolicyDecision::RequireApproval { .. }
                ),
                "{tool} is not an affirmatively-declared read and must still park under auto"
            );
        }

        // And with NO declaration — the default at every non-harness site — even
        // the declared pair parks, exactly as it did before this issue.
        let no_reads = policy("auto", &[], None);
        assert!(
            matches!(
            no_reads
                .check(&request(
                    "mcp_call_tool",
                    serde_json::json!({ "server": "jira", "tool": "get_issue", "arguments": {} }),
                ))
                .await,
            ToolPolicyDecision::RequireApproval { .. }
        ),
            "a policy with no read declaration must gate every bridge call, as before #1124"
        );
    })
    .await;
}

/// `always_approve` wins over `auto` exactly as it wins over `full`, and the
/// two tiers below `auto` are untouched by its arrival.
///
/// The `readonly`/`supervised` half is not ceremony: `auto` was added by
/// widening a `match` on the mode and by adding a predicate next to the two
/// the other arms read, so the way this change fails is by moving a
/// neighbouring line, not by getting its own arm wrong.
#[tokio::test]
async fn auto_yields_to_always_approve_and_leaves_the_lower_tiers_alone() {
    in_cycle(async {
        let auto = policy("auto", &["file_write"], None);
        assert!(
            matches!(
                auto.check(&request("file_write", serde_json::json!({})))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "a tool on the always-approve list must park even though auto would otherwise run it"
        );

        // `readonly` still denies a sandbox write outright rather than parking
        // it, and still allows a pure read.
        let readonly = policy("readonly", &[], None);
        assert!(matches!(
            readonly
                .check(&request("file_write", serde_json::json!({})))
                .await,
            ToolPolicyDecision::Deny { .. }
        ));
        assert_eq!(
            readonly
                .check(&request("file_read", serde_json::json!({})))
                .await,
            ToolPolicyDecision::Allow
        );

        // `supervised` still parks the sandbox write `auto` now runs — the one
        // difference between the tiers, asserted as a difference.
        let supervised = policy("supervised", &[], None);
        assert!(matches!(
            supervised
                .check(&request("file_write", serde_json::json!({})))
                .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
    })
    .await;
}

#[tokio::test]
async fn supervised_requires_approval_for_external_effects() {
    in_cycle(async {
        let p = policy("supervised", &[], None);
        assert!(matches!(
            p.check(&request("send_email", serde_json::json!({}))).await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
        assert_eq!(
            p.check(&request("read_file", serde_json::json!({}))).await,
            ToolPolicyDecision::Allow
        );
    })
    .await;
}

#[tokio::test]
async fn supervised_parks_mcp_tool_calls_as_external_other_effects() {
    in_cycle(async {
        let p = policy("supervised", &[], None);
        let args = serde_json::json!({
            "server_id": "server-1",
            "tool_name": "echo",
            "arguments": {"text": "hello"}
        });
        assert!(matches!(
            p.check(&request("mcp_registry_tool_call", args.clone()))
                .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
        assert_eq!(
            p.effect_for("mcp_registry_tool_call", &args).group,
            EffectGroup::Other
        );
    })
    .await;
}

#[tokio::test]
async fn readonly_denies_mutations_allows_reads() {
    let p = policy("readonly", &[], None);
    assert!(matches!(
        p.check(&request("publish_post", serde_json::json!({})))
            .await,
        ToolPolicyDecision::Deny { .. }
    ));
    assert_eq!(
        p.check(&request("list_files", serde_json::json!({}))).await,
        ToolPolicyDecision::Allow
    );
}

#[tokio::test]
async fn auto_approve_under_threshold_allows_small_spends() {
    in_cycle(async {
        let p = policy("supervised", &[], Some(5.0));
        // $3 spend is under the $5 threshold → allowed even though it's external.
        assert_eq!(
            p.check(&request(
                "pay_invoice",
                serde_json::json!({ "amount_usd": 3.0 })
            ))
            .await,
            ToolPolicyDecision::Allow
        );
        // $9 spend exceeds the threshold → requires approval.
        assert!(matches!(
            p.check(&request(
                "pay_invoice",
                serde_json::json!({ "amount_usd": 9.0 })
            ))
            .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
    })
    .await;
}

/// Media generation (issue #109): the paid `media_generate_*` tools park
/// under supervised and deny under readonly (external spend effect), while
/// the read-only `media_list_models` catalog GET is always allowed.
#[tokio::test]
async fn media_generate_parks_supervised_and_denies_readonly_but_list_is_read_only() {
    in_cycle(async {
        let supervised = policy("supervised", &[], None);
        for tool in ["media_generate_image", "media_generate_video"] {
            assert!(
                matches!(
                    supervised
                        .check(&request(tool, serde_json::json!({})))
                        .await,
                    ToolPolicyDecision::RequireApproval { .. }
                ),
                "{tool} must park under supervised"
            );
        }
        let explicit_staging = policy("full", &[], None).with_policy_hitl_disabled();
        assert!(matches!(
            explicit_staging
                .check(&request("media_generate_image", serde_json::json!({})))
                .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
        // The catalog GET is read-only — allowed even under supervised.
        assert_eq!(
            supervised
                .check(&request("media_list_models", serde_json::json!({})))
                .await,
            ToolPolicyDecision::Allow
        );

        let readonly = policy("readonly", &[], None);
        assert!(
            matches!(
                readonly
                    .check(&request("media_generate_image", serde_json::json!({})))
                    .await,
                ToolPolicyDecision::Deny { .. }
            ),
            "media_generate must be denied under readonly"
        );
        // Even a read-only desk can list the model catalog.
        assert_eq!(
            readonly
                .check(&request("media_list_models", serde_json::json!({})))
                .await,
            ToolPolicyDecision::Allow
        );
    })
    .await;
}
