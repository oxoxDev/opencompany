use super::*;

// Issue #470: the `composio_execute` fixtures are built here, from the same
// key the classifier reads, so a call in a test reaches the same catalogue
// lookup a call in production does.
use super::policy_test_helpers_tests::*;

// ---- The path split (issue #674) --------------------------------------

/// A policy nobody told about the path judges the strict way.
///
/// The default is the safety property: a construction site added later,
/// which has not thought about #674 at all, gets the agent rule rather than
/// being silently exempted from it.
#[tokio::test]
async fn the_default_path_is_the_strict_one() {
    in_cycle(async {
        let p = policy("full", &[], None);
        assert_eq!(p.call_path, CallPath::Agent);
        let d = p
            .check(&request(
                "shell",
                serde_json::json!({ "command": "rm -rf ." }),
            ))
            .await;
        assert_eq!(decision_name(&d), "park");
    })
    .await;
}

/// ...and the workflow gate pass opts out of exactly that arm, because an
/// operator authored the node past the manifest grant.
#[tokio::test]
async fn an_authored_node_is_not_stopped_by_the_judgement_arm() {
    let p = policy("full", &[], None).for_authored_workflow_nodes();
    let d = p
        .check(&request(
            "shell",
            serde_json::json!({ "command": "rm -rf ." }),
        ))
        .await;
    assert_eq!(decision_name(&d), "allow");
}

/// Issue #875: the gate an operator actually meets, end to end.
///
/// The classification lives in `policy::consequence`; this is the assertion
/// that it reaches the decision — an agent grepping its own workspace runs,
/// and the same tool acting still parks, in both acting tiers.
#[tokio::test]
async fn a_read_shell_command_runs_and_an_acting_one_parks() {
    in_cycle(async {
        for tier in ["supervised", "auto"] {
            let p = policy(tier, &[], None);
            let read = p
                .check(&request(
                    "shell",
                    serde_json::json!({ "command": "grep -c foo session_raw/a.jsonl" }),
                ))
                .await;
            assert_eq!(
                decision_name(&read),
                "allow",
                "{tier}: a grep of the agent's own workspace must not interrupt an operator"
            );

            let act = p
                .check(&request(
                    "shell",
                    serde_json::json!({ "command": "rm -rf /tmp/x" }),
                ))
                .await;
            assert_eq!(decision_name(&act), "park", "{tier}: an act still parks");
        }

        // `readonly` is the tier that promises nothing changes. A read is still
        // a read there, and an act is still refused outright.
        let ro = policy("readonly", &[], None);
        assert_eq!(
            decision_name(
                &ro.check(&request(
                    "shell",
                    serde_json::json!({ "command": "ls -la" })
                ))
                .await
            ),
            "allow"
        );
        assert_eq!(
            decision_name(
                &ro.check(&request(
                    "shell",
                    serde_json::json!({ "command": "rm -rf ." })
                ))
                .await
            ),
            "deny"
        );
    })
    .await;
}

/// The operator's own always-ask list is keyed on the tool name, and it sits
/// above the classifier — so an operator who asks to be told about `shell`
/// is told about every shell call, read or not.
#[tokio::test]
async fn always_approve_still_catches_a_read() {
    in_cycle(async {
        let p = policy("auto", &["shell"], None);
        let d = p
            .check(&request("shell", serde_json::json!({ "command": "ls" })))
            .await;
        assert_eq!(decision_name(&d), "park");
    })
    .await;
}

/// The opt-out scopes ONE arm. This is the assertion that keeps it from
/// becoming a way to run a workflow node past the whole chain.
///
/// Each case below is decided by an arm ABOVE the judgement one, so each
/// must decide identically on both paths. A refactor that moved the path
/// check any earlier — the obvious "simplification", since it would let
/// `check` return before doing any work — fails here rather than shipping a
/// bypass.
#[tokio::test]
async fn the_authored_path_changes_nothing_above_the_judgement_arm() {
    in_cycle(async {
        let cases: &[(&str, &[&str], &str, &str)] = &[
            // `readonly` denies an external effect; it does not park it, and it
            // certainly does not allow it because a workflow authored it.
            ("readonly", &[], "send_email", "deny"),
            // `always_approve` is the operator asking to be told. It outranks
            // the tier on both paths — and on the authored path it is now the
            // operator's whole control surface, so it had better work.
            ("full", &["shell"], "shell", "park"),
            // `supervised` parks a consequence on its own.
            ("supervised", &[], "shell", "park"),
        ];
        for (mode, always, tool, expected) in cases {
            // An ACTING command, and since issue #875 that matters: `shell` is
            // classified by what it was handed, so a read would now be decided
            // by the classifier rather than by the arm this test is about.
            let args = serde_json::json!({ "command": "rm -rf ." });
            let agent_path = policy(mode, always, None)
                .check(&request(tool, args.clone()))
                .await;
            let node_path = policy(mode, always, None)
                .for_authored_workflow_nodes()
                .check(&request(tool, args))
                .await;
            assert_eq!(
                decision_name(&agent_path),
                *expected,
                "{mode}/{tool}: the arm under test is not the one deciding here"
            );
            assert_eq!(
                decision_name(&node_path),
                *expected,
                "{mode}/{tool}: the authored path must only scope the judgement \
             arm, and this decision is made above it"
            );
        }
    })
    .await;
}

/// The boundary condition, at the chain rather than in the pure module: the
/// same node, the same tier, the same path — and templated arguments.
#[tokio::test]
async fn an_authored_node_templated_from_upstream_output_still_stops() {
    in_cycle(async {
        let p = policy("full", &[], None).for_authored_workflow_nodes();
        let d = p
            .check(&request(
                "shell",
                serde_json::json!({ "command": "=previous.output" }),
            ))
            .await;
        assert_eq!(
            decision_name(&d),
            "park",
            "the operator declared the shape, not the command"
        );
    })
    .await;
}
