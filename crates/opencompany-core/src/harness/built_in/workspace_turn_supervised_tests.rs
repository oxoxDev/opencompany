use serde_json::json;

use super::workspace_turn_helpers_tests::*;
use crate::harness::{HarnessDeps, HarnessPool};
use crate::ports::types::CompanyRecord;

// ---------------------------------------------------------------------------
// The approval boundary, driven by a model (issues #443, #444)
// ---------------------------------------------------------------------------

/// Re-`ensure` the pool against the same deps under a different policy mode.
///
/// The fixture above is `full` on purpose — it exists to prove the workspace
/// tools work, and parking every call would get in the way. The gate is only
/// observable under `supervised`, which is also the **default** mode a company
/// gets, so it is the mode these last tests care about.
async fn supervised(deps: &HarnessDeps, grants: &str) -> (HarnessPool, CompanyRecord) {
    let mut record = CompanyRecord {
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: crate::test_support::per_test_company_id("acme"),
        manifest: manifest_in_mode(grants, "supervised"),
        ledger: Vec::new(),
        lifecycle: "running".to_string(),
        overlay_agents: Vec::new(),
        overlay_desk_members: Vec::new(),
        overlay_desk_order: Vec::new(),
        overlay_desks: Vec::new(),
        overlay_workflows: Vec::new(),
        overlay_budgets: Vec::new(),
        overlay_policy: None,
        overlay_tool_grants: None,
        overlay_desk_tools: Default::default(),
        disabled_workflows: Vec::new(),
        template_provenance: None,
        setup: None,
        name_confirmed: false,
        activation_completed_at: None,
        created_at_millis: None,
    };
    record.manifest.tools.allow = manifest(grants).tools.allow;
    let pool = HarnessPool::new();
    pool.ensure(&record, deps).await.expect("pool ensures");
    (pool, record)
}

/// End-to-end, through a model: workspace reads and writes both run without
/// policy-generated HITL.
///
/// The last clause is issue #444's headline, and nothing shorter than this can
/// show it. The two halves of the gate live in different modules and disagreed
/// about this one tool: `is_external_effect` refused to exempt `workspace_write`
/// because it overwrites guidance the operator wrote, while the standing-grant
/// rule read its `Other` group — a group it lands in only because the name
/// carries no consequence word — and offered it for a week. This drives one real
/// turn and asks both halves about the same call.
#[tokio::test]
async fn a_supervised_turn_reads_and_writes_the_workspace_without_policy_hitl() {
    let dir = tempfile::tempdir().unwrap();
    let (base, script) = spawn_script(vec![
        Turn::Call {
            tool: "workspace_read",
            args: json!({ "path": "standards/engineering-standards.md" }),
        },
        Turn::WriteWithObservedRev {
            path: "standards/engineering-standards.md",
            content: "# Engineering\nRewritten.",
            delta: 0,
        },
        Turn::Say("done"),
    ])
    .await;
    let (_pool, deps, _record, store) = harness(base, "\"workspace\"", dir.path()).await;
    let (pool, record) = supervised(&deps, "\"workspace\"").await;

    let cycle = deps
        .approval_requests
        .claim(crate::harness::policy::ApprovalScope::Cycle);
    cycle
        .scoped(pool.run(
            &record.id,
            "ceo",
            "tidy the standards",
            &deps,
            crate::runtime::delegation::ChatTarget::default(),
        ))
        .await
        .expect("the turn runs");
    let parked = cycle.drain(crate::harness::policy::MAX_APPROVAL_REQUESTS_PER_TURN);

    assert!(parked.requests.is_empty(), "{parked:?}");
    assert!(
        tool_results(&script).len() >= 2,
        "both the read and write must have run and fed a result back"
    );
    let (_, content) = store
        .read(&record.id, "n-eng")
        .await
        .expect("workspace read succeeds")
        .expect("standards note remains");
    assert_eq!(content, "# Engineering\nRewritten.");
}

/// The other side of the same boundary, so the feature is not proved dead:
/// The same policy-HITL-off boundary applies to the agent's own sandbox.
#[tokio::test]
async fn a_write_to_the_agents_own_workspace_runs_without_policy_hitl() {
    let dir = tempfile::tempdir().unwrap();
    let (base, script) = spawn_script(vec![
        Turn::Call {
            tool: "file_write",
            args: json!({ "path": "notes.md", "content": "draft" }),
        },
        Turn::Say("done"),
    ])
    .await;
    let (_pool, deps, _record, _store) =
        harness(base, "\"files\", \"workspace\"", dir.path()).await;
    let (pool, record) = supervised(&deps, "\"files\", \"workspace\"").await;

    let cycle = deps
        .approval_requests
        .claim(crate::harness::policy::ApprovalScope::Cycle);
    cycle
        .scoped(pool.run(
            &record.id,
            "ceo",
            "jot a note",
            &deps,
            crate::runtime::delegation::ChatTarget::default(),
        ))
        .await
        .expect("the turn runs");
    let parked = cycle.drain(crate::harness::policy::MAX_APPROVAL_REQUESTS_PER_TURN);

    assert!(parked.requests.is_empty(), "{parked:?}");
    assert!(
        tool_results(&script)
            .iter()
            .all(|result| !result.contains("error")),
        "the file write must succeed: {:?}",
        tool_results(&script)
    );
    let note =
        crate::harness::build::agent_workspace(dir.path(), &record.id, "ceo").join("notes.md");
    assert_eq!(std::fs::read_to_string(note).unwrap(), "draft");
}

/// Issue #443, through the turn loop: the reads that used to park.
///
/// `file_read` and `grep` are pure reads of the agent's own workspace, and both
/// parked under the DEFAULT mode — not by anyone's decision, but because the
/// read-only rule matched a name *prefix* and neither begins with a read-only
/// word. Nobody had reported them. They were found by asking the same question
/// of every registered tool, which is the mechanism this lane adds.
#[tokio::test]
async fn a_supervised_turn_reads_its_own_workspace_without_asking() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("seed.md"), "hello").ok();
    let (base, script) = spawn_script(vec![
        Turn::Call {
            tool: "grep",
            args: json!({ "pattern": "hello", "path": "." }),
        },
        Turn::Call {
            tool: "file_read",
            args: json!({ "path": "seed.md" }),
        },
        Turn::Say("done"),
    ])
    .await;
    let (_pool, deps, _record, _store) = harness(base, "\"files\"", dir.path()).await;
    let (pool, record) = supervised(&deps, "\"files\"").await;

    let cycle = deps
        .approval_requests
        .claim(crate::harness::policy::ApprovalScope::Cycle);
    cycle
        .scoped(pool.run(
            &record.id,
            "ceo",
            "what do we have?",
            &deps,
            crate::runtime::delegation::ChatTarget::default(),
        ))
        .await
        .expect("the turn runs");
    let parked = cycle.drain(crate::harness::policy::MAX_APPROVAL_REQUESTS_PER_TURN);

    assert!(
        parked.requests.is_empty(),
        "reading the agent's own workspace must not interrupt an operator: {:?}",
        parked
            .requests
            .iter()
            .map(|r| r.tool.clone())
            .collect::<Vec<_>>()
    );
    // Not vacuous: both reads were genuinely offered to the model and both
    // came back with a result, so the calls reached the gate and returned.
    let offered = advertised_tools(&script);
    for tool in ["grep", "file_read"] {
        assert!(
            offered.contains(&tool.to_string()),
            "`{tool}` was never on the belt, so this proves nothing: {offered:?}"
        );
    }
    // `tool_results` reads every request body the stub saw, and each body
    // repeats the conversation so far — so this counts cumulatively rather
    // than once per call. Two distinct calls is the floor.
    assert!(
        tool_results(&script).len() >= 2,
        "both reads must have run and fed a result back"
    );
}
