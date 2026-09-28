use super::*;

use crate::ports::runs::{NewRun, RunFilter};
use crate::ports::types::{TurnStepKind, TurnStepStatus};
use crate::store::FsOps;

fn started(call_id: &str, tool: &str, label: Option<&str>) -> AgentProgress {
    AgentProgress::ToolCallStarted {
        call_id: call_id.to_string(),
        tool_name: tool.to_string(),
        arguments: serde_json::Value::Null,
        iteration: 1,
        display_label: label.map(str::to_string),
        display_detail: None,
    }
}

fn thinking(delta: &str) -> AgentProgress {
    AgentProgress::ThinkingDelta {
        delta: delta.to_string(),
        iteration: 1,
    }
}

fn completed(call_id: &str, tool: &str) -> AgentProgress {
    AgentProgress::ToolCallCompleted {
        call_id: call_id.to_string(),
        tool_name: tool.to_string(),
        success: true,
        output_chars: 2,
        output: "ok".to_string(),
        arguments: Some(serde_json::json!({ "server": "brave", "tool": "search" })),
        elapsed_ms: 42,
        iteration: 1,
        failure: None,
        display_label: None,
        display_detail: None,
        structured: None,
    }
}

/// Builds an fs-backed run store with one `Pending` run, and returns the
/// sink over it.
async fn sink(home: &std::path::Path) -> (Arc<dyn RunStore>, CompanyId, RunTraceSink) {
    let company = CompanyId::new("acme");
    let runs: Arc<dyn RunStore> = Arc::new(FsOps::new(home.to_path_buf()));
    let run = runs
        .create_run(&company, NewRun::for_task("run-1", "t-1", "ceo"))
        .await
        .expect("mint");
    let sink = RunTraceSink::new(company.clone(), run.id, Arc::clone(&runs));
    (runs, company, sink)
}

/// The property the whole feature exists for: steps are durable **during**
/// the run, so a host killed mid-tool-call still leaves the prefix behind.
#[tokio::test]
async fn steps_land_while_the_turn_is_still_running() {
    let home = tempfile::Builder::new()
        .prefix("opencompany-run-trace-")
        .tempdir()
        .expect("tempdir");
    let (runs, company, sink) = sink(home.path()).await;

    sink.record(&started("c1", "mcp_call_tool", Some("Searching")))
        .await;

    // Read it back BEFORE the turn ends — nothing has been folded yet.
    let steps = runs
        .list_run_steps(&company, "run-1")
        .await
        .expect("list steps");
    assert_eq!(steps.len(), 1, "the step is durable mid-turn");
    assert_eq!(steps[0].step_seq, 0);
    assert_eq!(steps[0].step.status, TurnStepStatus::Running);
    assert_eq!(steps[0].step.label, "Searching");
    assert_eq!(sink.step_count(), 1);

    // …and the completion finalizes that same row rather than adding one.
    sink.record(&completed("c1", "mcp_call_tool")).await;
    let steps = runs
        .list_run_steps(&company, "run-1")
        .await
        .expect("list steps");
    assert_eq!(steps.len(), 1, "a finalized start must not duplicate");
    assert_eq!(steps[0].step.status, TurnStepStatus::Ok);
    assert_eq!(steps[0].step.detail.as_deref(), Some("brave · search"));
    assert_eq!(steps[0].step.kind, TurnStepKind::ToolCall);
    assert_eq!(sink.step_count(), 1);
}

/// The EOF path end-to-end: a thought whose stream closes without a
/// `TextDelta` or tool call still lands in the deep store when the sink is
/// flushed. The tail below the interim flush threshold is exactly what an
/// aborted turn leaves behind, and it must not vanish.
#[tokio::test]
async fn flush_persists_an_aborted_thoughts_tail() {
    let home = tempfile::Builder::new()
        .prefix("opencompany-run-trace-deep-")
        .tempdir()
        .expect("tempdir");
    let company = CompanyId::new("acme");
    let runs: Arc<dyn RunStore> = Arc::new(FsOps::new(home.path().to_path_buf()));
    let run = runs
        .create_run(&company, NewRun::for_task("run-1", "t-1", "ceo"))
        .await
        .expect("mint");
    let deep: Arc<dyn crate::ports::deep_trace::DeepTraceStore> =
        Arc::new(FsOps::new(home.path().to_path_buf()));
    let sink = RunTraceSink::new(company.clone(), run.id, Arc::clone(&runs))
        .with_deep(Some(Arc::clone(&deep)));

    sink.record(&thinking("first ")).await;
    sink.record(&thinking("second")).await; // under DEEP_THINK_FLUSH_BYTES
    // No text, no tool call — the turn just ends.
    sink.flush().await;

    let details = deep
        .list_step_details(&company, "run-1")
        .await
        .expect("list step details");
    let reasoning: String = details
        .iter()
        .filter_map(|d| d.detail.reasoning.clone())
        .collect();
    assert_eq!(
        reasoning, "first second",
        "the tail of an aborted thought was dropped: {reasoning:?}"
    );
}

/// Cost folds across every turn of the attempt, and tokens are recorded even
/// at zero USD (the managed passthrough bills off the wire).
#[test]
fn usage_folds_across_turns_including_token_only_ones() {
    // Never written to — this test only exercises the in-memory fold.
    let runs: Arc<dyn RunStore> = Arc::new(FsOps::new(std::path::PathBuf::from("unused")));
    let sink = RunTraceSink::new(CompanyId::new("acme"), "run-1", runs);
    assert_eq!(sink.usage(), TokenUsage::default());

    sink.add_usage(&TurnUsage {
        input_tokens: 100,
        output_tokens: 20,
        cached_input_tokens: 5,
        cost_usd: 0.25,
    });
    sink.add_usage(&TurnUsage {
        input_tokens: 10,
        output_tokens: 2,
        cached_input_tokens: 0,
        cost_usd: 0.0,
    });

    let usage = sink.usage();
    assert_eq!(usage.input, 110);
    assert_eq!(usage.output, 22);
    assert_eq!(usage.cached_input, 5);
    assert_eq!(usage.cost_usd, 0.25);
}

/// A store that cannot take a step must not be able to fail the turn — the
/// agent's work is already done by the time a step is recorded.
#[tokio::test]
async fn a_store_failure_never_reaches_the_turn() {
    use async_trait::async_trait;

    use crate::error::OpenCompanyError;
    use crate::ports::runs::{RunRecord, RunStatus};

    struct BrokenRuns;

    #[async_trait]
    impl RunStore for BrokenRuns {
        async fn create_run(&self, company: &CompanyId, spec: NewRun) -> crate::Result<RunRecord> {
            Ok(RunRecord {
                id: spec.id,
                company: company.clone(),
                task_id: spec.task_id,
                chat_id: spec.chat_id,
                agent_id: spec.agent_id,
                attempt: 1,
                status: RunStatus::Pending,
                trigger_event_seq: None,
                thread_root: None,
                created_at_millis: 0,
                started_at_millis: None,
                finished_at_millis: None,
                error: None,
                usage: TokenUsage::default(),
                step_count: 0,
                workflow_run_id: None,
                node_id: None,
                episode_id: None,
                round_revision: None,
            })
        }
        async fn get_run(
            &self,
            _company: &CompanyId,
            _id: &str,
        ) -> crate::Result<Option<RunRecord>> {
            Ok(None)
        }
        async fn put_run(&self, _company: &CompanyId, _run: &RunRecord) -> crate::Result<()> {
            Ok(())
        }
        async fn list_runs(
            &self,
            _company: &CompanyId,
            _filter: &RunFilter,
        ) -> crate::Result<Vec<RunRecord>> {
            Ok(Vec::new())
        }
        async fn append_run_step(
            &self,
            _company: &CompanyId,
            _step: &RunStepRecord,
        ) -> crate::Result<()> {
            Err(OpenCompanyError::Store("disk on fire".to_string()))
        }
        async fn list_run_steps(
            &self,
            _company: &CompanyId,
            _run_id: &str,
        ) -> crate::Result<Vec<RunStepRecord>> {
            Ok(Vec::new())
        }
    }

    let sink = RunTraceSink::new(CompanyId::new("acme"), "run-1", Arc::new(BrokenRuns));
    // No panic, no propagation — and the count stays honest about what
    // actually landed.
    sink.record(&started("c1", "mcp_call_tool", None)).await;
    assert_eq!(sink.step_count(), 0);
}

/// A runaway tool loop is bounded: past the cap nothing more is written, and
/// the reported count stops at the cap rather than claiming rows that do not
/// exist.
#[tokio::test]
async fn a_runaway_turn_stops_writing_at_the_cap() {
    let home = tempfile::Builder::new()
        .prefix("opencompany-run-trace-cap-")
        .tempdir()
        .expect("tempdir");
    let (runs, company, sink) = sink(home.path()).await;

    for i in 0..(MAX_RUN_STEPS + 5) {
        sink.record(&started(&format!("c{i}"), "spawn_task", None))
            .await;
    }
    assert_eq!(sink.step_count(), MAX_RUN_STEPS);
    assert_eq!(
        runs.list_run_steps(&company, "run-1")
            .await
            .expect("list")
            .len() as u32,
        MAX_RUN_STEPS
    );
}
