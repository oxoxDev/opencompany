//! Compile and drive a company workflow on the tinyflows engine.
//!
//! [`run_workflow`] is the free driver: [`translate`](super::translate) the
//! [`WorkflowFile`] into a tinyflows graph, [`compile`](tinyflows::compiler)
//! it, build the [`Capabilities`](super::caps) bundle (agent nodes → harness
//! pool), and [`run`](tinyflows::engine) it to completion. [`HarnessWorkflowRunner`]
//! is the [`WorkflowRunner`] port implementation the runtime holds: it owns the
//! shared pool/deps/record, ensures the roster is resident, then delegates.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::Result;
use crate::company::WorkflowFile;
use crate::error::OpenCompanyError;
use crate::harness::{HarnessDeps, HarnessPool};
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::ports::{WorkflowRun, WorkflowRunner};

/// Runs `workflow` for the company described by `record` on the tinyflows engine
/// with the trigger `input`, returning the final run state and any nodes left
/// pending approval.
///
/// `record` (not a bare [`CompanyId`]) is threaded through so the outside-world
/// capabilities — the `tool_call` toolbelt and the `http_request` SSRF guard —
/// can read the company's `[policy].mode`, `[tools].allow` grants, and
/// `[tools].web_allowed_domains` (see [`super::caps::build_capabilities`]).
///
/// The caller is responsible for having the company's roster resident in `pool`
/// (agent nodes address it by teammate id) — [`HarnessWorkflowRunner::run`] does
/// this via [`HarnessPool::ensure`] before delegating here.
pub async fn run_workflow(
    pool: Arc<HarnessPool>,
    deps: HarnessDeps,
    record: &CompanyRecord,
    workflow: &WorkflowFile,
    input: Value,
) -> Result<WorkflowRun> {
    let graph = super::translate::translate(workflow);
    let compiled = tinyflows::compiler::compile(&graph).map_err(map_engine_error)?;
    let capabilities = super::caps::build_capabilities(pool, deps, record, &workflow.id);
    let outcome = tinyflows::engine::run(&compiled, input, &capabilities)
        .await
        .map_err(map_engine_error)?;
    Ok(WorkflowRun {
        output: outcome.output,
        pending_approvals: outcome.pending_approvals,
    })
}

/// Maps a tinyflows [`EngineError`](tinyflows::error::EngineError) onto the crate
/// error: a structural validation failure is a caller-facing bad request; every
/// other engine/capability failure is a harness error.
fn map_engine_error(err: tinyflows::error::EngineError) -> OpenCompanyError {
    use tinyflows::error::EngineError;
    match err {
        EngineError::Validation(v) => {
            OpenCompanyError::InvalidRequest(format!("workflow graph is invalid: {v}"))
        }
        other => OpenCompanyError::Harness(other.to_string()),
    }
}

/// The [`WorkflowRunner`] port backed by the embedded harness: it holds the
/// shared pool, its deps, and the company record so it can ensure the roster is
/// built before a run and route agent nodes onto it.
pub struct HarnessWorkflowRunner {
    pool: Arc<HarnessPool>,
    deps: HarnessDeps,
    record: CompanyRecord,
}

impl HarnessWorkflowRunner {
    /// Builds a runner sharing `pool`/`deps` with the rest of the harness surface
    /// for the company described by `record`.
    pub fn new(pool: Arc<HarnessPool>, deps: HarnessDeps, record: CompanyRecord) -> Self {
        Self { pool, deps, record }
    }
}

#[async_trait]
impl WorkflowRunner for HarnessWorkflowRunner {
    async fn run(
        &self,
        _company: &CompanyId,
        workflow: &WorkflowFile,
        input: Value,
    ) -> Result<WorkflowRun> {
        // Idempotent: builds the roster on first use, a no-op after. The run
        // addresses the record's own company; `_company` is the routed scope,
        // which the runtime resolves to this same record.
        self.pool.ensure(&self.record, &self.deps).await?;
        run_workflow(
            self.pool.clone(),
            self.deps.clone(),
            &self.record,
            workflow,
            input,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::company::parse_workflow;
    use crate::harness::provider::MockProvider;
    use crate::store::{FsCompanyStore, FsContextStore, FsOps};

    fn record() -> CompanyRecord {
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
            id: CompanyId::new("acme"),
            manifest,
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: Vec::new(),
            overlay_desk_members: Vec::new(),
        }
    }

    fn deps(dir: &std::path::Path) -> HarnessDeps {
        HarnessDeps {
            provider: Arc::new(MockProvider::new("mock: ")),
            provider_slug: "mock".to_string(),
            context: Arc::new(FsContextStore::new(dir)),
            store: Arc::new(FsCompanyStore::new(dir)),
            meter: Some(Arc::new(FsOps::new(dir))),
            workspace_root: dir.to_path_buf(),
            model_override: None,
            tasks: None,
            skills: None,
            skills_source_dir: None,
            mcp_servers: Vec::new(),
            facts: None,
            events: None,
            delegations: crate::harness::orchestrator::DelegationQueue::default(),
            workflow_runner: crate::harness::orchestrator::WorkflowRunnerHandle::default(),
            mcp_failures: crate::harness::mcp_probe::McpFailureQueue::default(),
            secrets: None,
            web_allowed_domains: Vec::new(),
            capabilities: crate::harness::toolbelt::CapabilityFilter::AllowAll,
            plan: None,
            media: None,
        }
    }

    /// A record whose `[tools].allow` grants every namespace, so the workflow
    /// `tool_call` capability can reach the Cell A toolbelt (policy `full` keeps
    /// the exec autonomy at Full so the tools can act).
    fn tools_record() -> CompanyRecord {
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
            id: CompanyId::new("acme"),
            manifest,
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: Vec::new(),
            overlay_desk_members: Vec::new(),
        }
    }

    /// The workflow workspace directory the tool_call toolbelt is sandboxed to.
    fn workflow_workspace(home: &std::path::Path, company: &str) -> std::path::PathBuf {
        home.join(company).join("_workflow").join("workspace")
    }

    /// A three-node workflow (trigger → agent → output) runs to completion with
    /// the agent node executing on the harness pool: the offline mock provider
    /// echoes the node's prompt, proving the turn went through the openhuman
    /// agent rather than being skipped.
    const GREET: &str = r#"
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

    /// The port implementation ensures the roster itself, so a caller need not
    /// pre-`ensure`.
    #[tokio::test]
    async fn port_impl_ensures_roster_and_runs() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(HarnessPool::new());
        let rec = record();
        let runner = HarnessWorkflowRunner::new(pool, deps(dir.path()), rec.clone());

        let file = parse_workflow(GREET).expect("workflow parses");
        let run = WorkflowRunner::run(&runner, &rec.id, &file, serde_json::json!({}))
            .await
            .expect("workflow runs");
        assert!(run.output.to_string().contains("hello-marker"));
    }

    /// A workflow with no trigger is a caller-facing bad request, not a harness
    /// error. (Built by hand — `parse_workflow` would reject it earlier.)
    #[tokio::test]
    async fn missing_trigger_is_invalid_request() {
        use crate::company::{WorkflowFile, WorkflowNodeDef, WorkflowNodeKind};

        let dir = tempfile::tempdir().unwrap();
        let file = WorkflowFile {
            id: "bad".to_string(),
            name: "Bad".to_string(),
            description: None,
            nodes: vec![WorkflowNodeDef {
                id: "only".to_string(),
                kind: WorkflowNodeKind::Output,
                name: "Only".to_string(),
                summary: None,
                agent: None,
                config: None,
                on_error: None,
                retry: None,
                requires_approval: None,
            }],
            edges: Vec::new(),
        };
        let err = run_workflow(
            Arc::new(HarnessPool::new()),
            deps(dir.path()),
            &record(),
            &file,
            serde_json::json!({}),
        )
        .await
        .expect_err("missing trigger rejected");
        assert!(
            matches!(err, OpenCompanyError::InvalidRequest(_)),
            "{err:?}"
        );
    }

    // --- P1: real capability wiring (T1–T5) --------------------------------

    /// T1 — a config-driven `tool_call` (slug `csv_export`) executes through the
    /// real Cell A toolbelt and the CSV lands on disk in the dedicated workflow
    /// workspace (on-disk proof the tool actually ran).
    #[tokio::test]
    async fn t1_config_driven_tool_call_writes_csv_to_workflow_workspace() {
        let src = r#"
id = "csv"
name = "CSV"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "export"
kind = "tool_call"
name = "Export"
[node.config]
slug = "csv_export"
[node.config.args]
filename = "wf-out.csv"
data = "[{\"name\":\"Ada\"},{\"name\":\"Bob\"}]"
[[node]]
id = "done"
kind = "output"
name = "Done"
[[edge]]
from = "start"
to = "export"
[[edge]]
from = "export"
to = "done"
"#;
        let dir = tempfile::tempdir().unwrap();
        let file = parse_workflow(src).expect("parses");
        let run = run_workflow(
            Arc::new(HarnessPool::new()),
            deps(dir.path()),
            &tools_record(),
            &file,
            serde_json::json!({ "seed": 1 }),
        )
        .await
        .expect("workflow runs");
        assert!(run.pending_approvals.is_empty());

        let csv = workflow_workspace(dir.path(), "acme")
            .join("exports")
            .join("wf-out.csv");
        assert!(
            csv.is_file(),
            "csv_export should land the file in the workflow workspace: {}",
            csv.display()
        );
        let content = std::fs::read_to_string(&csv).unwrap();
        assert!(
            content.contains("Ada") && content.contains("Bob"),
            "{content}"
        );
    }

    /// T2 — an unknown slug with `retry.max_attempts = 2` and `on_error =
    /// "continue"` exhausts its retries then turns the failure into a data item,
    /// so the run completes (no hard error) carrying the error.
    #[tokio::test]
    async fn t2_unknown_slug_retries_then_continues_with_error_item() {
        let src = r#"
id = "t2"
name = "T2"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "call"
kind = "tool_call"
name = "Call"
on_error = "continue"
[node.config]
slug = "bogus_tool"
[node.retry]
max_attempts = 2
[[node]]
id = "done"
kind = "output"
name = "Done"
[[edge]]
from = "start"
to = "call"
[[edge]]
from = "call"
to = "done"
"#;
        let dir = tempfile::tempdir().unwrap();
        let file = parse_workflow(src).expect("parses");
        let run = run_workflow(
            Arc::new(HarnessPool::new()),
            deps(dir.path()),
            &tools_record(),
            &file,
            serde_json::json!({ "seed": 1 }),
        )
        .await
        .expect("run completes despite the failing node");
        // `on_error = continue` turns the failure into a data item; the message
        // names the unwired slug.
        assert!(
            run.output.to_string().contains("bogus_tool"),
            "the continued error item should carry the failure: {}",
            run.output
        );
    }

    /// T3 — `on_error = "route"` plus an `error`-labeled edge routes the failure
    /// item down the recovery branch.
    #[tokio::test]
    async fn t3_on_error_route_sends_failure_down_the_error_edge() {
        let src = r#"
id = "t3"
name = "T3"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "call"
kind = "tool_call"
name = "Call"
on_error = "route"
[node.config]
slug = "bogus_tool"
[[node]]
id = "recover"
kind = "output"
name = "Recover"
[[node]]
id = "done"
kind = "output"
name = "Done"
[[edge]]
from = "start"
to = "call"
[[edge]]
from = "call"
to = "done"
[[edge]]
from = "call"
to = "recover"
label = "error"
"#;
        let dir = tempfile::tempdir().unwrap();
        let file = parse_workflow(src).expect("parses");
        let run = run_workflow(
            Arc::new(HarnessPool::new()),
            deps(dir.path()),
            &tools_record(),
            &file,
            serde_json::json!({ "seed": 1 }),
        )
        .await
        .expect("run completes via the recovery route");
        let recover_items = &run.output["nodes"]["recover"]["items"];
        assert!(
            recover_items
                .as_array()
                .map(|a| !a.is_empty())
                .unwrap_or(false),
            "the recovery node should receive the routed error item: {}",
            run.output
        );
        assert!(
            run.output.to_string().contains("bogus_tool"),
            "{}",
            run.output
        );
    }

    /// T4 — `requires_approval = true` pauses the node before it runs; the run
    /// reports it on `pending_approvals`.
    #[tokio::test]
    async fn t4_requires_approval_pauses_the_run() {
        let src = r#"
id = "t4"
name = "T4"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "gate"
kind = "tool_call"
name = "Gate"
requires_approval = true
[node.config]
slug = "csv_export"
[[node]]
id = "done"
kind = "output"
name = "Done"
[[edge]]
from = "start"
to = "gate"
[[edge]]
from = "gate"
to = "done"
"#;
        let dir = tempfile::tempdir().unwrap();
        let file = parse_workflow(src).expect("parses");
        let run = run_workflow(
            Arc::new(HarnessPool::new()),
            deps(dir.path()),
            &tools_record(),
            &file,
            serde_json::json!({ "seed": 1 }),
        )
        .await
        .expect("run pauses cleanly");
        assert!(
            run.pending_approvals.iter().any(|id| id == "gate"),
            "the approval-gated node should be pending: {:?}",
            run.pending_approvals
        );
    }

    /// T5 — an `http_request` to a loopback address is refused by the upstream
    /// `url_guard` SSRF check (the happy path is impossible offline by design, so
    /// the guard-in-path is proven via the denial). `on_error` defaults to
    /// `stop`, so the run fails with the guard error.
    #[tokio::test]
    async fn t5_http_request_to_loopback_is_ssrf_denied() {
        let src = r#"
id = "t5"
name = "T5"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "fetch"
kind = "http_request"
name = "Fetch"
[node.config]
method = "GET"
url = "http://127.0.0.1:9/"
[[node]]
id = "done"
kind = "output"
name = "Done"
[[edge]]
from = "start"
to = "fetch"
[[edge]]
from = "fetch"
to = "done"
"#;
        let dir = tempfile::tempdir().unwrap();
        let file = parse_workflow(src).expect("parses");
        let err = run_workflow(
            Arc::new(HarnessPool::new()),
            deps(dir.path()),
            &tools_record(),
            &file,
            serde_json::json!({ "seed": 1 }),
        )
        .await
        .expect_err("the SSRF guard must block the loopback request");
        assert!(
            err.to_string().contains("http_request"),
            "the failure should come from the guarded http client: {err}"
        );
    }
}
