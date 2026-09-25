use std::sync::Arc;

use super::workflow_build_fixtures_tests::*;
use super::workflow_build_shared_tests::*;
use super::*;
use crate::ports::runs::RunStatus;

// ---------------------------------------------------------------------------
// Pass tier
// ---------------------------------------------------------------------------

/// A [`HarnessDeps`] wiring the copilot agent onto `model` — everything else is
/// inert fixture wiring reusing the runtime's own context/store. The copilot path
/// reads only `provider`, `provider.profile()` and `model_override`; the rest is
/// present to satisfy the struct.
pub(crate) fn agent_deps(
    runtime: &CompanyRuntime,
    model: Arc<dyn HarnessModel>,
) -> crate::harness::HarnessDeps {
    crate::harness::HarnessDeps {
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: model,
        provider_slug: "managed".to_string(),
        serves: None,
        context: runtime.context.clone(),
        store: runtime.store().clone(),
        meter: None,
        workspace_root: std::env::temp_dir(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: std::env::temp_dir(),
        model_override: None,
        tasks: None,
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: Arc::from([]),
        mcp_servers: Vec::new(),
        default_mcp_servers: Vec::new(),
        facts: None,
        events: None,
        delegations: crate::harness::orchestrator::DelegationQueue::default(),
        workflow_runner: crate::harness::orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: crate::harness::mcp_probe::McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_revisions: None,
        approval_requests: crate::harness::policy::ApprovalRequestQueue::default(),
        approval_parker: None,
        secrets: None,
        web_allowed_domains: Vec::new(),
        capabilities: crate::harness::toolbelt::CapabilityFilter::AllowAll,
        workflow_source_dir: None,
        plan: None,
        media: None,
        composio: None,
        #[cfg(feature = "chargebee")]
        chargebee: None,
        #[cfg(feature = "paypal")]
        paypal: None,
        hosting: None,
        search: None,
        tenant_search: None,
        steer: crate::company::steer::InflightRegistry::default(),
        run_supervisor: crate::runtime::RunSupervisor::default(),
        delivery: None,
        workspace: None,
        workflow_runs: None,
        deep_trace: None,
    }
}

/// The happy path: a valid graph lands a proposal In Review, the card carries it,
/// the attempt settles Succeeded, and the stored `ops` carries the host-assigned
/// id (not the model's) and the schedule.
#[tokio::test]
async fn a_valid_graph_lands_a_proposal_in_review() {
    let model = ScriptedModel::replying(VALID_GRAPH);
    let (_home, runtime) = runtime_with(Arc::clone(&model)).await;
    runtime
        .tasks()
        .upsert(runtime.id(), &card("t-1", None))
        .await
        .unwrap();
    let run_id = open_run(&runtime, "t-1").await;

    run_workflow_build_pass(
        Arc::clone(&runtime),
        "t-1".to_string(),
        Some(run_id.clone()),
    )
    .await;

    let after = read(&runtime, "t-1").await;
    assert_eq!(after.column, COLUMN_IN_REVIEW);
    let proposal = after
        .workflow_proposal
        .expect("the proposal is on the card");
    assert!(proposal.summary.contains("digest"));
    assert_eq!(
        proposal.run_id, run_id,
        "the proposal links to the build attempt"
    );
    // The host owns the id; the model's suggestion is ignored, and the schedule
    // survives in the stored ops the apply route will rebuild from.
    let spec: WorkflowGraphSpec = serde_json::from_value(proposal.ops).unwrap();
    assert_eq!(spec.id, "weekly-digest");
    assert_eq!(spec.nodes[0].schedule.as_deref(), Some("0 9 * * 1"));
    assert_eq!(run_status(&runtime, &run_id).await, RunStatus::Succeeded);
    assert_eq!(model.calls(), 1, "one card, one model call");
}

/// A card with no plan still builds — from its title and note.
#[tokio::test]
async fn a_card_with_no_plan_builds_from_title_and_note() {
    let (_home, runtime) = runtime_with(ScriptedModel::replying(VALID_GRAPH)).await;
    runtime
        .tasks()
        .upsert(runtime.id(), &card("t-2", None))
        .await
        .unwrap();
    let run_id = open_run(&runtime, "t-2").await;

    run_workflow_build_pass(
        Arc::clone(&runtime),
        "t-2".to_string(),
        Some(run_id.clone()),
    )
    .await;

    let after = read(&runtime, "t-2").await;
    assert_eq!(after.column, COLUMN_IN_REVIEW);
    assert!(after.workflow_proposal.is_some());
    assert_eq!(run_status(&runtime, &run_id).await, RunStatus::Succeeded);
}

/// A not-automatable answer returns the card to To-do with the reason and no
/// proposal (decision D2c).
///
/// Issue #873 + #1809: the attempt settles **Declined** — its own terminal
/// state, neither the failure it used to be (#873) nor the success that #873
/// first repurposed — and the card is converted to a `once` deliverable. It used
/// to settle Failed and keep `workflow`, which is what trapped the card — see the
/// loop test below.
#[tokio::test]
async fn a_not_automatable_answer_returns_the_card_to_todo() {
    let reply = r#"{"automatable":false,"reason":"this only ever runs once"}"#;
    let (_home, runtime) = runtime_with(ScriptedModel::replying(reply)).await;
    runtime
        .tasks()
        .upsert(runtime.id(), &card("t-3", None))
        .await
        .unwrap();
    let run_id = open_run(&runtime, "t-3").await;

    run_workflow_build_pass(
        Arc::clone(&runtime),
        "t-3".to_string(),
        Some(run_id.clone()),
    )
    .await;

    let after = read(&runtime, "t-3").await;
    assert_eq!(after.column, COLUMN_TODO);
    assert!(
        after.workflow_proposal.is_none(),
        "no proposal on a not-automatable card"
    );
    assert!(after.note.unwrap().contains("done once"));
    // Issue #1809: a by-design decline is its own terminal state, not a failure
    // and not a plain success — so the external "work that stopped" surface stops
    // bucketing the compiler's correct refusal as the product breaking.
    assert_eq!(run_status(&runtime, &run_id).await, RunStatus::Declined);
}

/// The loop #873 reports, asserted at the seam that closes it.
///
/// `CompanyRuntime::dispatch_task` sends a `workflow`-deliverable card to the
/// builder pass rather than to its assignee. So a verdict that returned the card
/// to To-do still carrying `workflow` guaranteed the next dispatch re-entered
/// the builder, drew the same verdict, and failed again — builder → To-do →
/// builder, with a red error on every pass and no way for the card to reach the
/// person who could just do the work.
///
/// Converting the deliverable is what breaks it: the card keeps its assignee and
/// becomes ordinary one-off work.
#[tokio::test]
async fn a_not_automatable_verdict_converts_the_card_so_it_stops_re_entering_the_builder() {
    let reply = r#"{"automatable":false,"reason":"a workflow for this already exists"}"#;
    let (_home, runtime) = runtime_with(ScriptedModel::replying(reply)).await;
    let before = card("t-loop", None);
    assert_eq!(
        before.deliverable,
        TaskDeliverable::Workflow,
        "the card starts as builder-routed work"
    );
    runtime.tasks().upsert(runtime.id(), &before).await.unwrap();
    let run_id = open_run(&runtime, "t-loop").await;

    run_workflow_build_pass(
        Arc::clone(&runtime),
        "t-loop".to_string(),
        Some(run_id.clone()),
    )
    .await;

    let after = read(&runtime, "t-loop").await;
    assert_eq!(
        after.deliverable,
        TaskDeliverable::Once,
        "a declined card must stop routing to the builder, or it loops forever"
    );
    assert_eq!(
        after.assignee, "maya",
        "the assignee is who the verdict hands the work to; it must survive"
    );
    assert_eq!(after.column, COLUMN_TODO);
}

/// The operator-facing half. The reason is on the card, and the run row carries
/// **no** error — a decision filed with an error is how the console showed red
/// for a reasoned "do this by hand" in the first place.
#[tokio::test]
async fn a_not_automatable_verdict_files_no_error_and_says_what_happened_to_the_card() {
    let reply = r#"{"automatable":false,"reason":"the search tool is not wired here"}"#;
    let (_home, runtime) = runtime_with(ScriptedModel::replying(reply)).await;
    runtime
        .tasks()
        .upsert(runtime.id(), &card("t-note", None))
        .await
        .unwrap();
    let run_id = open_run(&runtime, "t-note").await;

    run_workflow_build_pass(
        Arc::clone(&runtime),
        "t-note".to_string(),
        Some(run_id.clone()),
    )
    .await;

    let row = runtime
        .runs()
        .get_run(runtime.id(), &run_id)
        .await
        .expect("read")
        .expect("the attempt row exists");
    assert_eq!(row.status, RunStatus::Declined);
    assert!(
        row.error.is_none(),
        "a verdict is not an error: {:?}",
        row.error
    );

    let note = read(&runtime, "t-note").await.note.expect("a note");
    assert!(note.contains("the search tool is not wired here"), "{note}");
    assert!(
        note.contains("one-off"),
        "the note must say what became of the card, not only the verdict: {note}"
    );
}

/// The discrimination that makes the change safe: a build that could not be
/// *attempted* is still a failure, and its card still routes to the builder so a
/// retry re-attempts the build rather than landing on a person.
#[tokio::test]
async fn a_genuine_build_failure_still_fails_and_stays_builder_routed() {
    // A draft that parses and decides nothing: no graph, no reason, no refusal.
    // This is the `BuildOutcome::NoAnswer` path — the case that used to share a
    // variant with a real verdict and would otherwise now convert the card.
    let (_home, runtime) = runtime_with(ScriptedModel::replying(r#"{"automatable":true}"#)).await;
    runtime
        .tasks()
        .upsert(runtime.id(), &card("t-fault", None))
        .await
        .unwrap();
    let run_id = open_run(&runtime, "t-fault").await;

    run_workflow_build_pass(
        Arc::clone(&runtime),
        "t-fault".to_string(),
        Some(run_id.clone()),
    )
    .await;

    assert_eq!(run_status(&runtime, &run_id).await, RunStatus::Failed);
    let after = read(&runtime, "t-fault").await;
    assert_eq!(
        after.deliverable,
        TaskDeliverable::Workflow,
        "a fault must stay builder-routed — retrying the build is the right next move"
    );
    assert_eq!(after.column, COLUMN_TODO);
}
