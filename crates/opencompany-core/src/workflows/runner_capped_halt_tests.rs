use super::tests_reclassify::{HaltOkTurn, RecordingLane};
use super::*;

use crate::company::parse_workflow;
use crate::harness::provider::MockProvider;
use crate::store::{FsCompanyStore, FsContextStore, FsOps};

#[async_trait]
impl crate::runtime::delegation::RunTurn for HaltOkTurn {
    async fn run(
        &self,
        _company: &CompanyId,
        _agent_id: &str,
        _message: &str,
        _chat_id: crate::runtime::delegation::ChatTarget<'_>,
    ) -> Result<crate::harness::TurnOutcome> {
        Ok(crate::harness::TurnOutcome {
            reply: "The requested report was already delivered last week.".to_string(),
            steps: Vec::new(),
            hit_iteration_cap: false,
            abnormal_stop: None,
            halted_for_spend: None,
            budget_paused: None,
        })
    }

    async fn run_steered(
        &self,
        _company: &CompanyId,
        _agent_id: &str,
        _message: &str,
        _control: &crate::company::steer::SteerControl,
        _chat_id: crate::runtime::delegation::ChatTarget<'_>,
        _run_sink: Option<Arc<crate::harness::run_trace::RunTraceSink>>,
    ) -> Result<crate::harness::TurnOutcome> {
        unreachable!("not exercised by this test")
    }

    async fn run_steered_background(
        &self,
        _company: &CompanyId,
        _agent_id: &str,
        _message: &str,
        _control: &crate::company::steer::SteerControl,
        _chat: crate::runtime::delegation::ChatTarget<'_>,
        _run_sink: Option<Arc<crate::harness::run_trace::RunTraceSink>>,
    ) -> Result<crate::harness::TurnOutcome> {
        unreachable!("not exercised by this test")
    }
}
fn halt_plus_fail_graph() -> WorkflowFile {
    parse_workflow(
        r#"
id = "halt_plus_fail"
name = "Halt plus fail"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "ok_branch"
kind = "agent"
name = "Ok branch"
summary = "Check whether the report is already done."
agent = "ok_agent"
[node.verify]
criteria = "The report must be delivered."
[[node]]
id = "bad_branch"
kind = "tool_call"
name = "Bad branch"
[node.config]
slug = "bogus_tool"
[[node]]
id = "done"
kind = "output"
name = "Done"
[[edge]]
from = "start"
to = "ok_branch"
[[edge]]
from = "start"
to = "bad_branch"
[[edge]]
from = "ok_branch"
to = "done"
[[edge]]
from = "bad_branch"
to = "done"
"#,
    )
    .expect("halt-plus-fail graph parses")
}

/// Codex review on #1990 (#3904894275): when parallel branches contain
/// both a benign halt and a genuine node failure, the `is_genuine_failure`
/// early return must scrub the halted node's output and raise its notice
/// exactly like the halt-only and halt-plus-block exits reached lower in
/// the same function — before this fix it reclassified the halted row but
/// persisted and returned the ORIGINAL `partial_output`, so the failed
/// run's snapshot presented `ok_branch`'s rejected reply as produced
/// output with no `Declined` explanation.
#[tokio::test]
async fn a_genuine_failure_scrubs_a_benign_halt_sibling_too() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base_url = crate::workflows::gated_tool_turn_tests::spawn_script(vec![
        crate::workflows::gated_tool_turn_tests::Turn::Say("{\"verdict\":\"halt_benign\"}"),
    ])
    .await;
    let (deps, _journal) = crate::workflows::gated_tool_turn_tests::deps(base_url, dir.path());
    let record = crate::workflows::gated_tool_turn_tests::record();
    let turn = Arc::new(HaltOkTurn);
    let ctx = WorkflowRunContext::new(false);

    let result = run_workflow_lane_aware(
        turn,
        deps,
        &record,
        &halt_plus_fail_graph(),
        serde_json::json!({ "request": "go" }),
        &ctx,
    )
    .await;

    let err = result.expect_err("a genuine sibling failure must fail the run");
    let partial = err
        .partial_run()
        .expect("a genuine failure carries the partial run");

    assert!(
        partial
            .notices
            .iter()
            .any(|n| n.contains("ok_branch") && n.contains("no further work was needed")),
        "the halted sibling's benign-stop notice must be raised even when a real \
         failure ends the run: {:?}",
        partial.notices
    );
    let nodes = partial
        .output
        .as_object()
        .expect("partial output is a node-keyed object");
    assert!(
        !nodes.contains_key("ok_branch"),
        "the halted sibling's rejected reply must be scrubbed from the persisted \
         snapshot, exactly like the halt-only and halt-plus-block exits: {:?}",
        partial.output
    );
}

/// A turn double for `start -> capped_work -> gated_work -> done`:
/// `capped_work` always truncates at the iteration cap like
/// `CappedThenSettlingTurn`'s node of the same name, and `gated_work`
/// announces arrival on `entered` and then blocks on `release` — the same
/// hold-and-release shape [`GatedProvider`] uses for the clean-cancel
/// keystone test, just at the `RunTurn` layer instead of `ChatModel`, so
/// this test does not need a `HarnessPool`.
struct CappedThenGatedTurn {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl CappedThenGatedTurn {
    async fn execute(&self, agent_id: &str) -> Result<crate::harness::TurnOutcome> {
        if agent_id == "capped_agent" {
            return Ok(crate::harness::TurnOutcome {
                reply: "partial answer, still going".to_string(),
                steps: Vec::new(),
                hit_iteration_cap: true,
                abnormal_stop: None,
                halted_for_spend: None,
                budget_paused: None,
            });
        }
        // `gated_agent`: announce arrival, then wait to be released. The
        // test cancels and releases in that order, so the token is already
        // flipped by the time this turn resolves and the engine winds down
        // at the next boundary instead of starting `done`.
        self.entered.notify_waiters();
        self.release.notified().await;
        Ok(crate::harness::TurnOutcome {
            reply: "acknowledged".to_string(),
            steps: Vec::new(),
            hit_iteration_cap: false,
            abnormal_stop: None,
            halted_for_spend: None,
            budget_paused: None,
        })
    }
}

#[async_trait]
impl crate::runtime::delegation::RunTurn for CappedThenGatedTurn {
    async fn run(
        &self,
        _company: &CompanyId,
        agent_id: &str,
        _message: &str,
        _chat_id: crate::runtime::delegation::ChatTarget<'_>,
    ) -> Result<crate::harness::TurnOutcome> {
        self.execute(agent_id).await
    }

    async fn run_steered(
        &self,
        _company: &CompanyId,
        agent_id: &str,
        _message: &str,
        _control: &crate::company::steer::SteerControl,
        _chat_id: crate::runtime::delegation::ChatTarget<'_>,
        _run_sink: Option<Arc<crate::harness::run_trace::RunTraceSink>>,
    ) -> Result<crate::harness::TurnOutcome> {
        self.execute(agent_id).await
    }

    async fn run_steered_background(
        &self,
        _company: &CompanyId,
        agent_id: &str,
        _message: &str,
        _control: &crate::company::steer::SteerControl,
        _chat: crate::runtime::delegation::ChatTarget<'_>,
        _run_sink: Option<Arc<crate::harness::run_trace::RunTraceSink>>,
    ) -> Result<crate::harness::TurnOutcome> {
        self.execute(agent_id).await
    }
}

fn capped_then_gated_graph() -> WorkflowFile {
    parse_workflow(
        r#"
id = "capped_then_gated"
name = "Capped then gated"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "capped_work"
kind = "agent"
name = "Capped work"
summary = "Loop until the iteration cap."
agent = "capped_agent"
[[node]]
id = "gated_work"
kind = "agent"
name = "Gated work"
summary = "Hold until released, after the operator cancels."
agent = "gated_agent"
[[node]]
id = "done"
kind = "output"
name = "Done"
[[edge]]
from = "start"
to = "capped_work"
[[edge]]
from = "capped_work"
to = "gated_work"
[[edge]]
from = "gated_work"
to = "done"
"#,
    )
    .expect("capped-then-gated graph parses")
}

/// PR #1883 review (Codex #3878277996): the clean node-boundary cancel arm
/// (`if outcome.cancelled` in `run_workflow_inner`) is a THIRD early return
/// that built its `WorkflowRun` straight from the collector's raw `nodes`,
/// never calling `reclassify_capped_nodes` — distinct from the
/// genuine-failure/blocked `Err` arm `94c8e0507` already fixed, and from
/// the clean-finish arm the original unit tests covered. A node upstream of
/// where the operator cancels, which itself only truncated at the
/// iteration cap, kept its `Ok` row on a stopped run even though its own
/// attempt already settled `Failed`.
#[tokio::test]
async fn a_capped_node_is_reclassified_when_the_run_is_cleanly_cancelled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (deps, _journal) = crate::workflows::gated_tool_turn_tests::deps(
        "http://127.0.0.1:1/unused".to_string(),
        dir.path(),
    );
    let record = crate::workflows::gated_tool_turn_tests::record();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let turn = Arc::new(CappedThenGatedTurn {
        entered: entered.clone(),
        release: release.clone(),
    });
    let ctx = WorkflowRunContext::new(false);
    let cancel = ctx.cancel.clone();
    let reached_gated = entered.notified();
    let graph = capped_then_gated_graph();

    let mut run = Box::pin(run_workflow_lane_aware(
        turn,
        deps,
        &record,
        &graph,
        serde_json::json!({ "request": "go" }),
        &ctx,
    ));
    tokio::select! {
        _ = &mut run => panic!("the run finished before the gated node was reached"),
        () = reached_gated => {}
    }

    // Stop the run, THEN let the gated node complete — the token is
    // already flipped by the time `gated_work` resolves, so the engine
    // winds down at the boundary before `done` runs.
    cancel.cancel();
    release.notify_one();
    let run = tokio::time::timeout(std::time::Duration::from_secs(30), run)
        .await
        .expect("the cleanly cancelled run never returned")
        .expect("a cancelled run is Ok, not Err");

    assert!(run.cancelled, "the run must report that it was stopped");
    assert!(
        !run.nodes.iter().any(|n| n.node_id == "done"),
        "the node past the cancel boundary must never run: {:?}",
        run.nodes
    );
    let capped_row = run
        .nodes
        .iter()
        .find(|n| n.node_id == "capped_work")
        .expect("the capped node's row must be in the cancelled run");
    assert_eq!(
        capped_row.status,
        WorkflowNodeStatus::Error,
        "a capped sibling's row must be reclassified Error on the clean-cancel arm too, not \
         only the genuine-failure/blocked early returns and the clean-finish arm — \
         {:?}",
        run.nodes
    );
}

#[async_trait]
impl crate::runtime::delegation::RunTurn for RecordingLane {
    async fn run(
        &self,
        _company: &CompanyId,
        agent_id: &str,
        _message: &str,
        _chat_id: crate::runtime::delegation::ChatTarget<'_>,
    ) -> Result<crate::harness::TurnOutcome> {
        self.seen.lock().unwrap().push(agent_id.to_string());
        Ok(crate::harness::TurnOutcome {
            reply: self.label.to_string(),
            steps: Vec::new(),
            hit_iteration_cap: false,
            // Test fixture, not the ACP fold (PR #1880 review).
            abnormal_stop: None,
            halted_for_spend: None,
            budget_paused: None,
        })
    }

    async fn run_steered(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
        _control: &crate::company::steer::SteerControl,
        chat_id: crate::runtime::delegation::ChatTarget<'_>,
        _run_sink: Option<Arc<crate::harness::run_trace::RunTraceSink>>,
    ) -> Result<crate::harness::TurnOutcome> {
        self.run(company, agent_id, message, chat_id).await
    }

    async fn run_steered_background(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
        _control: &crate::company::steer::SteerControl,
        _chat: crate::runtime::delegation::ChatTarget<'_>,
        _run_sink: Option<Arc<crate::harness::run_trace::RunTraceSink>>,
    ) -> Result<crate::harness::TurnOutcome> {
        self.run(
            company,
            agent_id,
            message,
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await
    }
}

pub(super) fn record() -> CompanyRecord {
    let manifest = toml::from_str(
        r#"
[company]
name = "Acme"

[policy]
mode = "full"

[[agent]]
id = "ceo"
role = "Chief Executive"
description = "Runs Acme."
"#,
    )
    .expect("valid manifest");
    CompanyRecord {
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: CompanyId::new("acme"),
        manifest,
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
    }
}

pub(super) fn deps(dir: &std::path::Path) -> HarnessDeps {
    HarnessDeps {
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        run_supervisor: crate::runtime::RunSupervisor::default(),
        provider: Arc::new(MockProvider::new("mock: ")),
        provider_slug: "mock".to_string(),
        serves: None,
        context: Arc::new(FsContextStore::new(dir)),
        store: Arc::new(FsCompanyStore::new(dir)),
        meter: Some(Arc::new(FsOps::new(dir))),
        workspace_root: dir.to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.to_path_buf(),
        model_override: None,
        tasks: None,
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
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
        steer: crate::company::steer::InflightRegistry::default(),
        delivery: None,
        search: None,
        tenant_search: None,
        workspace: None,
        workflow_runs: None,
        deep_trace: None,
    }
}

/// Deps with a `workflow_source_dir` wired, so `sub_workflow`-by-id resolves
/// children from `source`'s `workflows/` directory.
pub(super) fn deps_with_source(dir: &std::path::Path, source: &std::path::Path) -> HarnessDeps {
    let mut deps = deps(dir);
    deps.workflow_source_dir = Some(source.to_path_buf());
    deps
}

/// Writes `src` to `<source>/workflows/<id>.toml`.
pub(super) fn write_wf(source: &std::path::Path, id: &str, src: &str) {
    let workflows = source.join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(workflows.join(format!("{id}.toml")), src).unwrap();
}

/// A record whose `[tools].allow` grants every namespace, so the workflow
/// `tool_call` capability can reach the Cell A toolbelt (policy `full` keeps
/// the exec autonomy at Full so the tools can act).
pub(super) fn tools_record() -> CompanyRecord {
    let manifest = toml::from_str(
        r#"
[company]
name = "Acme"

[policy]
mode = "full"

[tools]
allow = ["*"]
"#,
    )
    .expect("valid manifest");
    CompanyRecord {
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: CompanyId::new("acme"),
        manifest,
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
    }
}

/// The workflow workspace directory the tool_call toolbelt is sandboxed to.
pub(super) fn workflow_workspace(home: &std::path::Path, company: &str) -> std::path::PathBuf {
    let workflows = home.join(company).join("_workflow");
    let workflow = std::fs::read_dir(workflows)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let run = std::fs::read_dir(workflow)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    run.join("workspace")
}

/// A three-node workflow (trigger → agent → output) runs to completion with
/// the agent node executing on the harness pool: the offline mock provider
/// echoes the node's prompt, proving the turn went through the openhuman
/// agent rather than being skipped.
pub(super) const GREET: &str = r#"
id = "greet"
name = "Greet"

[[node]]
id = "start"
kind = "trigger"
name = "Start"

[[node]]
id = "ceo"
kind = "agent"
name = "CEO"
summary = "say hello-marker"
agent = "ceo"

[[node]]
id = "done"
kind = "output"
name = "Report back"

[[edge]]
from = "start"
to = "ceo"

[[edge]]
from = "ceo"
to = "done"
"#;

#[tokio::test]
async fn agent_node_runs_on_the_harness_pool() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Arc::new(HarnessPool::new());
    let rec = record();
    let deps = deps(dir.path());
    pool.ensure(&rec, &deps).await.expect("roster builds");

    let file = parse_workflow(GREET).expect("workflow parses");
    let run = run_workflow(
        pool,
        deps,
        &rec,
        &file,
        serde_json::json!({ "brief": "launch" }),
        &WorkflowRunContext::new(false),
    )
    .await
    .expect("workflow runs");

    assert!(run.pending_approvals.is_empty());
    // The mock provider echoes the agent node's prompt into its reply, and
    // the reply flows into the run state — proof the agent node executed on
    // the pool through the engine.
    let output = run.output.to_string();
    assert!(output.contains("hello-marker"), "{output}");
}
