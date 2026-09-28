//! Running-mode fixtures for the `workflows` ops test cluster — the
//! `running` half of `workflows_test_support.rs`, split out to keep
//! each source file under the 750-line cap.

pub(crate) use std::sync::Arc;
pub(crate) use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) use axum::body::{Body, to_bytes};
pub(crate) use axum::http::Request;
pub(crate) use tower::ServiceExt;

pub(crate) use crate::company::CompanyManifest;
pub(crate) use crate::ports::types::{CompanyEvent, CompanyId, CompanyRecord, EventSeq};
pub(crate) use crate::ports::{CompanyStore, WorkflowRun, WorkflowRunContext, WorkflowRunner};
pub(crate) use crate::runtime::RuntimeBuilder;
pub(crate) use crate::server::router;
pub(crate) use crate::store::FsCompanyStore;
pub(crate) use crate::{AppConfig, AppState};

/// A runner that parks until released, and settles as cancelled if the
/// run's stop signal fires first.
///
/// It is the real `WorkflowRunner` port, so everything above it — the
/// route, the supervisor registration, the spawned task, the journal
/// write — is production code. Only the graph walk is stubbed, which is
/// what lets these tests be about the *entry point* rather than about
/// the engine (the engine's own cancel behaviour is pinned in
/// `workflows::runner`).
pub(crate) struct StalledRunner {
    pub(crate) entered: Arc<tokio::sync::Notify>,
    pub(crate) release: Arc<tokio::sync::Notify>,
    /// Set only if the run was allowed to finish on its own terms —
    /// which is how a test tells "the run completed" from "the run was
    /// dropped with the connection".
    pub(crate) completed: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl WorkflowRunner for StalledRunner {
    async fn run(
        &self,
        _company: &CompanyId,
        _workflow: &crate::company::WorkflowFile,
        _input: serde_json::Value,
        ctx: &WorkflowRunContext,
    ) -> crate::Result<WorkflowRun> {
        // `notify_one` on both, not `notify_waiters`: a permit is
        // stored, so neither side has to be already parked. The detached
        // run answers before its task has even been polled, so a test
        // that waits on `entered` afterwards would otherwise race the
        // notification and hang.
        self.entered.notify_one();
        let released = self.release.notified();
        tokio::select! {
            () = released => {}
            () = ctx.cancel.cancelled() => {
                return Ok(WorkflowRun {
                    output: serde_json::Value::Null,
                    pending_approvals: Vec::new(),
                    deliveries: Vec::new(),
                    cancelled: true,
                    nodes: Vec::new(),
                    notices: Vec::new(),
                    board: Vec::new(),
                    blocked_nodes: Vec::new(),
                    approvals: Vec::new(),
                });
            }
        }
        self.completed.store(true, Ordering::SeqCst);
        Ok(WorkflowRun {
            output: serde_json::json!({ "run": {}, "nodes": {} }),
            pending_approvals: Vec::new(),
            deliveries: Vec::new(),
            cancelled: false,
            nodes: Vec::new(),
            notices: Vec::new(),
            board: Vec::new(),
            blocked_nodes: Vec::new(),
            approvals: Vec::new(),
        })
    }
}

pub(crate) struct Stalled {
    pub(crate) app: axum::Router,
    pub(crate) runtime: Arc<crate::company::runtime::CompanyRuntime>,
    pub(crate) entered: Arc<tokio::sync::Notify>,
    pub(crate) release: Arc<tokio::sync::Notify>,
    pub(crate) completed: Arc<AtomicBool>,
}

pub(crate) const GRAPH: &str = r#"
id = "demo"
name = "Demo"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "done"
kind = "output"
name = "Report"
[[edge]]
from = "start"
to = "done"
label = "ok"
"#;

pub(crate) fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-workflows-running-")
        .tempdir()
        .expect("tempdir")
}

/// A hosted company with one overlay workflow and a runner that stalls.
pub(crate) async fn stalled_company(home: &std::path::Path) -> Stalled {
    let manifest: CompanyManifest =
        toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap();
    let id = CompanyId::new("acme");
    FsCompanyStore::new(home.to_path_buf())
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: manifest.clone(),
            ledger: Vec::new(),
            overlay_agents: Vec::new(),
            overlay_desk_members: Vec::new(),
            overlay_desk_order: Vec::new(),
            overlay_desks: Vec::new(),
            overlay_workflows: vec![crate::ports::types::OverlayWorkflow {
                id: "demo".to_string(),
                toml: GRAPH.to_string(),
            }],
            overlay_budgets: Vec::new(),
            overlay_policy: None,
            overlay_tool_grants: None,
            overlay_desk_tools: Default::default(),
            disabled_workflows: Vec::new(),
            lifecycle: "running".to_string(),
            template_provenance: None,
            setup: None,
            name_confirmed: false,
            activation_completed_at: None,
            created_at_millis: None,
        })
        .await
        .unwrap();

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let completed = Arc::new(AtomicBool::new(false));
    let mut runtime = RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    runtime.set_workflow_runner(Arc::new(StalledRunner {
        entered: entered.clone(),
        release: release.clone(),
        completed: completed.clone(),
    }));
    let runtime = Arc::new(runtime);

    let state = AppState::new(AppConfig::default());
    state.registry().insert(id.clone(), runtime.clone());
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;

    Stalled {
        app: router(state),
        runtime,
        entered,
        release,
        completed,
    }
}

pub(crate) fn run_request(body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/company/workflows/demo/run")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

pub(crate) fn cancel_request(run_id: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/api/v1/company/workflows/runs/{run_id}/cancel"))
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::empty())
        .unwrap()
}

pub(crate) fn get_workflow_request() -> Request<Body> {
    Request::builder()
        .uri("/api/v1/company/workflows/demo")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::empty())
        .unwrap()
}

pub(crate) fn delete_workflow_request(version: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(format!(
            "/api/v1/company/workflows/demo?expectedVersion={version}"
        ))
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::empty())
        .unwrap()
}

pub(crate) async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// Every event the company journaled, oldest first.
pub(crate) async fn journal(
    runtime: &Arc<crate::company::runtime::CompanyRuntime>,
) -> Vec<CompanyEvent> {
    runtime
        .events()
        .read_from(runtime.id(), EventSeq::new(0), usize::MAX)
        .await
        .expect("read")
        .into_iter()
        .map(|s| s.event)
        .collect()
}

/// Waits (bounded) for a `WorkflowRunFinished` to appear.
pub(crate) async fn await_finished(
    runtime: &Arc<crate::company::runtime::CompanyRuntime>,
) -> Option<CompanyEvent> {
    for _ in 0..200 {
        if let Some(event) = journal(runtime)
            .await
            .into_iter()
            .find(|e| matches!(e, CompanyEvent::WorkflowRunFinished { .. }))
        {
            return Some(event);
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    None
}

// ── Issue #542: dry run through the real route ──────────────────────

/// A runner that completes immediately, returning one node row — enough
/// to prove the route maps `WorkflowRun.nodes` onto the response and
/// echoes the request's `dry_run` as the discriminator.
pub(crate) struct EchoRunner;

#[async_trait::async_trait]
impl WorkflowRunner for EchoRunner {
    async fn run(
        &self,
        _company: &CompanyId,
        _workflow: &crate::company::WorkflowFile,
        _input: serde_json::Value,
        _ctx: &WorkflowRunContext,
    ) -> crate::Result<WorkflowRun> {
        Ok(WorkflowRun {
            output: serde_json::json!({ "run": {}, "nodes": {} }),
            pending_approvals: Vec::new(),
            deliveries: Vec::new(),
            cancelled: false,
            nodes: vec![crate::ports::WorkflowRunNodeRow {
                node_id: "done".to_string(),
                status: crate::ports::types::WorkflowNodeStatus::Ok,
                elapsed_ms: 3,
                diagnostics: Vec::new(),
            }],
            notices: Vec::new(),
            board: Vec::new(),
            blocked_nodes: Vec::new(),
            approvals: Vec::new(),
        })
    }
}

/// A runner that settles cleanly and hands back one delivery row —
/// every node `ok`, no error, nothing cancelled, and a report that did
/// not go out. The exact shape issue #981 caught reading green.
pub(crate) struct DroppedReportRunner;

/// A runner whose only delivery row is a **dry run**'s (issue #542): the
/// report was routed as far as its destination and deliberately not
/// dispatched. The row shape a real `deliver_outputs_dry` writes.
pub(crate) struct DryRunRunner;

#[async_trait::async_trait]
impl WorkflowRunner for DryRunRunner {
    async fn run(
        &self,
        _company: &CompanyId,
        _workflow: &crate::company::WorkflowFile,
        _input: serde_json::Value,
        _ctx: &WorkflowRunContext,
    ) -> crate::Result<WorkflowRun> {
        Ok(WorkflowRun {
            output: serde_json::json!({ "run": {}, "nodes": {} }),
            pending_approvals: Vec::new(),
            deliveries: vec![crate::ports::DeliveryReport {
                node: "done".to_string(),
                kind: "channel".to_string(),
                target: Some("engineering".to_string()),
                status: crate::ports::DeliveryStatus::Skipped,
                detail: "this was a test run — nothing was sent".to_string(),
                reason: crate::ports::DeliveryReason::DryRun,
            }],
            cancelled: false,
            nodes: vec![crate::ports::WorkflowRunNodeRow {
                node_id: "done".to_string(),
                status: crate::ports::types::WorkflowNodeStatus::Ok,
                elapsed_ms: 3,
                diagnostics: Vec::new(),
            }],
            notices: Vec::new(),
            board: Vec::new(),
            blocked_nodes: Vec::new(),
            approvals: Vec::new(),
        })
    }
}

#[async_trait::async_trait]
impl WorkflowRunner for DroppedReportRunner {
    async fn run(
        &self,
        _company: &CompanyId,
        _workflow: &crate::company::WorkflowFile,
        _input: serde_json::Value,
        _ctx: &WorkflowRunContext,
    ) -> crate::Result<WorkflowRun> {
        Ok(WorkflowRun {
            output: serde_json::json!({ "run": {}, "nodes": {} }),
            pending_approvals: Vec::new(),
            deliveries: vec![crate::ports::DeliveryReport {
                node: "done".to_string(),
                kind: "channel".to_string(),
                target: Some("operator".to_string()),
                status: crate::ports::DeliveryStatus::Failed,
                detail: "`operator` is not an automation delivery channel".to_string(),
                reason: crate::ports::DeliveryReason::ChannelNotWired,
            }],
            cancelled: false,
            nodes: vec![crate::ports::WorkflowRunNodeRow {
                node_id: "done".to_string(),
                status: crate::ports::types::WorkflowNodeStatus::Ok,
                elapsed_ms: 3,
                diagnostics: Vec::new(),
            }],
            notices: Vec::new(),
            board: Vec::new(),
            blocked_nodes: Vec::new(),
            approvals: Vec::new(),
        })
    }
}

/// A hosted company whose runner echoes immediately.
pub(crate) async fn echo_company(home: &std::path::Path) -> axum::Router {
    company_with_runner(home, Arc::new(EchoRunner)).await
}

/// A hosted company with one overlay graph and the given runner behind
/// the port, so the route, the supervisor and the journal write are all
/// production code and only the graph walk is stubbed.
pub(crate) async fn company_with_runner(
    home: &std::path::Path,
    runner: Arc<dyn WorkflowRunner>,
) -> axum::Router {
    let manifest: CompanyManifest =
        toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap();
    let id = CompanyId::new("acme");
    FsCompanyStore::new(home.to_path_buf())
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: manifest.clone(),
            ledger: Vec::new(),
            overlay_agents: Vec::new(),
            overlay_desk_members: Vec::new(),
            overlay_desk_order: Vec::new(),
            overlay_desks: Vec::new(),
            overlay_workflows: vec![crate::ports::types::OverlayWorkflow {
                id: "demo".to_string(),
                toml: GRAPH.to_string(),
            }],
            overlay_budgets: Vec::new(),
            overlay_policy: None,
            overlay_tool_grants: None,
            overlay_desk_tools: Default::default(),
            disabled_workflows: Vec::new(),
            lifecycle: "running".to_string(),
            template_provenance: None,
            setup: None,
            name_confirmed: false,
            activation_completed_at: None,
            created_at_millis: None,
        })
        .await
        .unwrap();
    let mut runtime = RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    runtime.set_workflow_runner(runner);
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id.clone(), Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    router(state)
}
