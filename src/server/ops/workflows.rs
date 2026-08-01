//! Workflow surfaces: create a graph (`POST /workflows`), read the company's
//! saved graphs (`GET /workflows`, `GET /workflows/{wid}`), and run one
//! (`POST /workflows/{wid}/run`) — under both scope forms.
//!
//! A company's graphs come from two places, unioned by
//! [`load_workflow_union`](crate::company::load_workflow_union) /
//! [`list_workflows_union`](crate::company::list_workflows_union):
//!
//! * the **seed** files committed to the company source directory
//!   (`companies/<name>/workflows/<wid>.toml`), and
//! * the **runtime-authored** graph bodies persisted on the [`CompanyRecord`]
//!   overlay.
//!
//! The seed wins on an id collision. A hosted tenant has no source directory at
//! all (or a read-only one), so every graph it owns is an overlay body — which
//! is why these routes never require a source directory to answer.
//!
//! Creation (issues #69, #112, #168) delegates to
//! [`create_company_workflow`](crate::company::create_company_workflow), the
//! single validated-persist core the orchestrator's `create_workflow` tool also
//! runs, so the two surfaces cannot drift. It persists the graph body **and**
//! the enabled id on the operator's live record in one save; the
//! version-controlled `company.toml` and the source tree are never written
//! (see `crate::server::ops::team`) — the source tree is read-only in hosted
//! mode, which is what issue #168 reports.
//!
//! `list_workflows` additionally unions in the manifest's `[workflows].enabled`
//! ids that have no body in either source, falling back to the id as the display
//! name — the same fallback the GraphQL `Company.workflows` resolver uses — so a
//! provisioned tenant's picker isn't empty.
//!
//! Execution is dependency-inverted behind the [`WorkflowRunner`] port: when no
//! runner is wired (the default build, or a runtime built without a harness) the
//! run route reports `not_wired` — the same 404 seam the DNS/SMTP surfaces use —
//! so the default build stays inert. The read routes need no runner: they only
//! parse the saved graphs, so the console can list and render workflows even on
//! a build that cannot execute them.

use std::collections::HashSet;
use std::path::Path as FsPath;

use axum::extract::Path;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AppState;
use crate::company::{
    RawEdge, RawNode, RawWorkflow, WorkflowDestinationDef, WorkflowEdgeDef, WorkflowFile,
    WorkflowNodeDef, WorkflowRetryDef, create_company_workflow, list_workflows_union,
    load_workflow_union,
};
use crate::error::OpenCompanyError;
use crate::ports::types::{CompanyRecord, OverlayWorkflow};
use crate::server::error::ApiError;
use crate::server::ops::{ScopedCompany, scoped};

/// Builds the workflow route fragment: create + list, one graph read, and the
/// run write.
pub fn router() -> Router<AppState> {
    scoped("/workflows", post(create_workflow).get(list_workflows))
        .merge(scoped("/workflows/{wid}", get(get_workflow)))
        .merge(scoped("/workflows/{wid}/run", post(run_workflow)))
}

/// A one-line workflow entry as the console's picker renders it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowSummary {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

impl From<WorkflowFile> for WorkflowSummary {
    fn from(f: WorkflowFile) -> Self {
        Self {
            id: f.id,
            name: f.name,
            description: f.description,
        }
    }
}

/// The full graph the canvas renders — nodes and directed edges, camelCase.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowGraph {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    nodes: Vec<WorkflowNode>,
    edges: Vec<WorkflowEdge>,
}

impl From<WorkflowFile> for WorkflowGraph {
    fn from(f: WorkflowFile) -> Self {
        Self {
            id: f.id,
            name: f.name,
            description: f.description,
            nodes: f.nodes.into_iter().map(WorkflowNode::from).collect(),
            edges: f.edges.into_iter().map(WorkflowEdge::from).collect(),
        }
    }
}

/// A single graph node. `kind` is the on-disk string
/// (`trigger`/`agent`/`tool_call`/`http_request`/`condition`/`output`); `agent`
/// is only meaningful on `agent` nodes; `schedule` (a 5-field UTC cron) only on
/// `trigger` nodes. The P1 fields (`config` / `onError` / `retry` /
/// `requiresApproval`) are serialized so `GET …/workflows/{wid}` does not drop
/// model data (they are omitted entirely when unset).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowNode {
    id: String,
    kind: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schedule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry: Option<WorkflowRetryOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requires_approval: Option<bool>,
    /// Where an `output` node's report is routed once the run finishes (issue
    /// #170). The model shape is reused verbatim in both directions: its two
    /// fields (`kind` / `target`) are single words, so there is no snake_case →
    /// camelCase gap to bridge and no second shape to drift from.
    #[serde(skip_serializing_if = "Option::is_none")]
    destination: Option<WorkflowDestinationDef>,
}

/// The camelCase retry policy shape the console reads back (`maxAttempts` /
/// `backoffMs` / `backoff`). Distinct from the snake_case
/// [`WorkflowRetryDef`] the model/TOML use.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowRetryOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_attempts: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backoff_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backoff: Option<String>,
}

impl From<WorkflowRetryDef> for WorkflowRetryOut {
    fn from(r: WorkflowRetryDef) -> Self {
        Self {
            max_attempts: r.max_attempts,
            backoff_ms: r.backoff_ms,
            backoff: r.backoff,
        }
    }
}

impl From<WorkflowNodeDef> for WorkflowNode {
    fn from(n: WorkflowNodeDef) -> Self {
        Self {
            id: n.id,
            kind: n.kind.as_str().to_string(),
            name: n.name,
            summary: n.summary,
            agent: n.agent,
            schedule: n.schedule,
            config: n.config,
            on_error: n.on_error,
            retry: n.retry.map(WorkflowRetryOut::from),
            requires_approval: n.requires_approval,
            destination: n.destination,
        }
    }
}

/// A directed edge between two node ids, with an optional branch label.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowEdge {
    from: String,
    to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

impl From<WorkflowEdgeDef> for WorkflowEdge {
    fn from(e: WorkflowEdgeDef) -> Self {
        Self {
            from: e.from,
            to: e.to,
            label: e.label,
        }
    }
}

/// The company's runtime-authored graph bodies, read once per request from the
/// live record. A company with no persisted record contributes none — the read
/// routes stay tolerant (they still serve the seed files), and the create route
/// re-loads the record under its write lock and 404s properly there.
async fn overlay_workflows(company: &ScopedCompany) -> Result<Vec<OverlayWorkflow>, ApiError> {
    let record: Option<CompanyRecord> = company
        .runtime
        .store()
        .load(company.id())
        .await
        .map_err(ApiError)?;
    Ok(record.map(|r| r.overlay_workflows).unwrap_or_default())
}

/// `GET …/workflows` — the company's saved workflows as picker summaries.
///
/// Summaries come from the union of the company's two graph sources — the seed
/// `workflows/*.toml` files and the record's runtime-authored bodies — so a
/// hosted tenant with no source directory still lists everything it created.
/// The manifest's `[workflows].enabled` ids are then unioned in (deduped by id),
/// falling back to the id as the name for an id with no body in either source —
/// the same fallback the GraphQL resolver uses. Only when all three are empty
/// does this return `200 []`, so the console renders "no workflows yet" rather
/// than a failure.
async fn list_workflows(company: ScopedCompany) -> Result<Json<Vec<WorkflowSummary>>, ApiError> {
    let overlays = overlay_workflows(&company).await?;
    let files = list_workflows_union(company.runtime.source_dir(), &overlays);
    let mut seen: HashSet<String> = files.iter().map(|f| f.id.clone()).collect();
    let mut summaries: Vec<WorkflowSummary> =
        files.into_iter().map(WorkflowSummary::from).collect();

    let enabled_ids = company
        .runtime
        .enabled_workflow_ids()
        .await
        .map_err(ApiError)?;
    for id in enabled_ids {
        // Already summarized from a real body — skip so an id that is both
        // enabled and saved doesn't double-list.
        if !seen.insert(id.clone()) {
            continue;
        }
        summaries.push(WorkflowSummary {
            id: id.clone(),
            name: id,
            description: None,
        });
    }

    Ok(Json(summaries))
}

/// `GET …/workflows/{wid}` — the full graph for one workflow.
///
/// Reads through the seed ∪ overlay union, so a graph created on a hosted
/// tenant (no source directory) resolves here too. An unknown `wid` is a `404`,
/// mirroring the sub-resource-not-found shape the task routes use.
async fn get_workflow(
    company: ScopedCompany,
    Path(WorkflowPath { wid }): Path<WorkflowPath>,
) -> Result<Json<WorkflowGraph>, ApiError> {
    // `wid` may become a filename on the seed side — reject anything that could
    // escape `workflows/`.
    if !safe_wid(&wid) {
        return Err(ApiError(OpenCompanyError::CompanyNotFound(format!(
            "workflow {wid}"
        ))));
    }
    let overlays = overlay_workflows(&company).await?;
    let file = load_workflow_union(company.runtime.source_dir(), &overlays, &wid)
        .map_err(ApiError)?
        .ok_or_else(|| ApiError(OpenCompanyError::CompanyNotFound(format!("workflow {wid}"))))?;
    Ok(Json(WorkflowGraph::from(file)))
}

/// The create-workflow body — the same camelCase graph shape the GET routes
/// return (`id`/`name`/`description?`/`nodes`/`edges`), so the console's
/// creator can post exactly what it would otherwise read back.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateWorkflowBody {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    nodes: Vec<CreateNode>,
    #[serde(default)]
    edges: Vec<CreateEdge>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateNode {
    id: String,
    kind: String,
    name: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    /// A 5-field UTC cron saying when the workflow starts on its own — only
    /// valid on a `trigger` node (issue #169). Validated by the render → parse
    /// round trip below, so a bad expression or a schedule on the wrong node
    /// kind is a `400`, not a persisted graph that never fires.
    #[serde(default)]
    schedule: Option<String>,
    /// Free-form, kind-specific node config (P2): `switch`/`transform`
    /// `=expr` bindings, a `sub_workflow` `workflow_id`, a `tool_call` slug/args,
    /// … . Carried as JSON on the wire and converted to a `toml::Value` on the
    /// way to disk; a JSON `null` anywhere inside is rejected as a 4xx (TOML has
    /// no null to represent it).
    #[serde(default)]
    config: Option<serde_json::Value>,
    /// Per-node error policy: `stop` (default) / `continue` / `route`.
    #[serde(default)]
    on_error: Option<String>,
    /// Per-node retry policy the engine honors.
    #[serde(default)]
    retry: Option<CreateRetry>,
    /// When `true`, the node pauses awaiting operator approval before it runs.
    #[serde(default)]
    requires_approval: Option<bool>,
    /// Where an `output` node's report goes once the run finishes:
    /// `{"kind": "owner"|"email"|"channel", "target"?: "…"}`. Rejected on any
    /// other node kind, and each kind's target contract is enforced by
    /// `parse_workflow` before anything is persisted.
    #[serde(default)]
    destination: Option<WorkflowDestinationDef>,
}

/// The camelCase retry policy the create body carries (`maxAttempts` /
/// `backoffMs` / `backoff`), mapped to the snake_case [`WorkflowRetryDef`] the
/// model + TOML use — the inverse of the [`WorkflowRetryOut`] read shape.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateRetry {
    #[serde(default)]
    max_attempts: Option<u32>,
    #[serde(default)]
    backoff_ms: Option<u64>,
    #[serde(default)]
    backoff: Option<String>,
}

impl From<CreateRetry> for WorkflowRetryDef {
    fn from(r: CreateRetry) -> Self {
        Self {
            max_attempts: r.max_attempts,
            backoff_ms: r.backoff_ms,
            backoff: r.backoff,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateEdge {
    from: String,
    to: String,
    #[serde(default)]
    label: Option<String>,
}

impl TryFrom<CreateWorkflowBody> for RawWorkflow {
    type Error = ApiError;

    fn try_from(body: CreateWorkflowBody) -> Result<Self, ApiError> {
        let mut nodes = Vec::with_capacity(body.nodes.len());
        for n in body.nodes {
            // JSON config → TOML value. TOML has no `null`, so a `null` anywhere
            // in the config is a caller error (400), not a 500 on write.
            let config = match n.config {
                Some(json) => Some(toml::Value::try_from(json).map_err(|err| {
                    ApiError(OpenCompanyError::InvalidRequest(format!(
                        "node `{}` has config that can't be stored ({err}) — TOML has no null; drop null-valued keys.",
                        n.id
                    )))
                })?),
                None => None,
            };
            nodes.push(RawNode {
                id: n.id,
                kind: n.kind,
                name: n.name,
                summary: n.summary,
                agent: n.agent,
                schedule: n.schedule,
                config,
                on_error: n.on_error,
                retry: n.retry.map(WorkflowRetryDef::from),
                requires_approval: n.requires_approval,
                destination: n.destination,
            });
        }
        Ok(Self {
            id: body.id,
            name: body.name,
            description: body.description,
            nodes,
            edges: body
                .edges
                .into_iter()
                .map(|e| RawEdge {
                    from: e.from,
                    to: e.to,
                    label: e.label,
                })
                .collect(),
        })
    }
}

/// `POST …/workflows` — authors a new workflow graph (issues #69, #112, #168):
/// the console's form creator, or any direct API caller, posts the graph shape
/// and it is persisted on the company record.
///
/// The whole validated-persist sequence lives in
/// [`create_company_workflow`] — the same core the orchestrator's
/// `create_workflow` tool runs — so the two surfaces cannot drift: safe id,
/// size caps, exactly one trigger, every `agent` node on the roster, unique id
/// (409) and unique display name (409), full [`parse_workflow`] structural
/// validation, one atomic record save, and an audit event.
///
/// No source directory is required: the body lands on the record, so a hosted
/// tenant whose source tree is a read-only mount can create workflows (issue
/// #168 — this used to fail with `EROFS`).
async fn create_workflow(
    company: ScopedCompany,
    Json(body): Json<CreateWorkflowBody>,
) -> Result<Json<WorkflowGraph>, ApiError> {
    let draft = RawWorkflow::try_from(body)?;
    let file = create_company_workflow(
        company.id(),
        company.runtime.source_dir(),
        company.runtime.store(),
        Some(company.runtime.events()),
        draft,
    )
    .await
    .map_err(ApiError)?;
    Ok(Json(WorkflowGraph::from(file)))
}

/// Whether `wid` is a single safe on-disk filename stem — no path separators,
/// no `..`, not empty — so it can't escape the `workflows/` directory.
fn safe_wid(wid: &str) -> bool {
    use std::path::Component;
    let mut comps = FsPath::new(wid).components();
    matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
}

/// The sub-resource path (`wid`); the scope `id` is consumed by the extractor.
#[derive(Debug, Deserialize)]
struct WorkflowPath {
    wid: String,
}

/// The run body: an optional trigger `input` payload seeded as the trigger
/// node's item. An empty object (`{}`) runs with a null input.
#[derive(Debug, Default, Deserialize)]
struct RunWorkflowBody {
    #[serde(default)]
    input: Value,
}

/// The run response: the engine's final state plus any nodes left pending
/// approval.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunWorkflowResponse {
    output: Value,
    pending_approvals: Vec<String>,
    /// One row per attempt to route a reached `output` node's report to its
    /// destination (issue #170). Empty for a graph that routes nothing. This is
    /// where an operator learns a report was NOT delivered — a delivery failure
    /// never fails the run, so it has nowhere else to surface.
    deliveries: Vec<crate::ports::DeliveryReport>,
}

/// `POST …/workflows/{wid}/run` (both scope forms).
async fn run_workflow(
    company: ScopedCompany,
    Path(WorkflowPath { wid }): Path<WorkflowPath>,
    body: Option<Json<RunWorkflowBody>>,
) -> Result<Json<RunWorkflowResponse>, Response> {
    // No runner wired (default build / no harness) → the same "not wired" seam
    // the networked surfaces use.
    let Some(runner) = company.runtime.workflow_runner() else {
        return Err(super::not_wired("workflow execution"));
    };

    // `wid` becomes a filename — reject anything that could escape `workflows/`.
    if !safe_wid(&wid) {
        return Err(
            ApiError(OpenCompanyError::CompanyNotFound(format!("workflow {wid}"))).into_response(),
        );
    }

    // Load the saved graph from the seed ∪ overlay union, so a graph created on
    // a hosted tenant (no source directory) runs the same as a committed one.
    let overlays = overlay_workflows(&company)
        .await
        .map_err(IntoResponse::into_response)?;
    let file = load_workflow_union(company.runtime.source_dir(), &overlays, &wid)
        .map_err(|e| ApiError(e).into_response())?
        .ok_or_else(|| {
            ApiError(OpenCompanyError::CompanyNotFound(format!("workflow {wid}"))).into_response()
        })?;

    let input = body.map(|Json(b)| b.input).unwrap_or(Value::Null);
    let run = runner
        .run(company.id(), &file, input)
        .await
        .map_err(|e| ApiError(e).into_response())?;

    Ok(Json(RunWorkflowResponse {
        output: run.output,
        pending_approvals: run.pending_approvals,
        deliveries: run.deliveries,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEMO: &str = r#"
        id = "demo"
        name = "Demo flow"
        description = "A tiny trigger → agent → output graph."
        [[node]]
        id = "start"
        kind = "trigger"
        name = "Start"
        summary = "Kicks it off."
        [[node]]
        id = "worker"
        kind = "agent"
        name = "Worker"
        summary = "Does the thing."
        agent = "assistant"
        [[node]]
        id = "done"
        kind = "output"
        name = "Report"
        [[edge]]
        from = "start"
        to = "worker"
        [[edge]]
        from = "worker"
        to = "done"
        label = "ok"
    "#;

    /// Writes `DEMO` to `<dir>/workflows/demo.toml` and returns `dir`.
    fn seed_demo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let workflows = dir.path().join("workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(workflows.join("demo.toml"), DEMO).unwrap();
        dir
    }

    #[test]
    fn list_returns_a_summary_per_saved_workflow() {
        let dir = seed_demo();
        let files = list_workflows_union(Some(dir.path()), &[]);
        let summaries: Vec<WorkflowSummary> =
            files.into_iter().map(WorkflowSummary::from).collect();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, "demo");
        assert_eq!(summaries[0].name, "Demo flow");
        assert_eq!(
            summaries[0].description.as_deref(),
            Some("A tiny trigger → agent → output graph.")
        );
    }

    #[test]
    fn get_returns_the_full_graph_with_nodes_and_edges() {
        let dir = seed_demo();
        let file = load_workflow_union(Some(dir.path()), &[], "demo")
            .expect("loads")
            .expect("one file");
        let graph = WorkflowGraph::from(file);

        assert_eq!(graph.id, "demo");
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.edges.len(), 2);

        // The `kind` field is the on-disk string via `as_str()`.
        let worker = graph.nodes.iter().find(|n| n.id == "worker").unwrap();
        assert_eq!(worker.kind, "agent");
        assert_eq!(worker.agent.as_deref(), Some("assistant"));

        let trigger = graph.nodes.iter().find(|n| n.id == "start").unwrap();
        assert_eq!(trigger.kind, "trigger");
        assert!(trigger.agent.is_none());

        let labeled = graph.edges.iter().find(|e| e.to == "done").unwrap();
        assert_eq!(labeled.from, "worker");
        assert_eq!(labeled.label.as_deref(), Some("ok"));
    }

    #[test]
    fn no_source_dir_and_no_overlay_lists_empty() {
        assert!(list_workflows_union(None, &[]).is_empty());
    }

    #[test]
    fn no_workflows_dir_lists_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_workflows_union(Some(dir.path()), &[]).is_empty());
    }

    #[test]
    fn json_serializes_camelcase_and_omits_empty_options() {
        let dir = seed_demo();
        let file = load_workflow_union(Some(dir.path()), &[], "demo")
            .unwrap()
            .unwrap();
        let json = serde_json::to_value(WorkflowGraph::from(file)).unwrap();
        // A node with no summary/agent omits those keys entirely.
        let done = json["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "done")
            .unwrap();
        assert!(done.get("agent").is_none());
        assert!(done.get("summary").is_none());
        assert_eq!(done["kind"], "output");
    }

    #[test]
    fn json_serializes_p1_node_fields_in_camelcase() {
        use crate::company::{WorkflowNodeDef, WorkflowNodeKind, WorkflowRetryDef};

        let file = WorkflowFile {
            id: "wf".into(),
            name: "WF".into(),
            description: None,
            nodes: vec![WorkflowNodeDef {
                id: "call".into(),
                kind: WorkflowNodeKind::ToolCall,
                name: "Call".into(),
                summary: None,
                agent: None,
                schedule: None,
                config: Some(serde_json::json!({ "slug": "csv_export" })),
                on_error: Some("continue".into()),
                retry: Some(WorkflowRetryDef {
                    max_attempts: Some(3),
                    backoff_ms: Some(250),
                    backoff: Some("exponential".into()),
                }),
                requires_approval: Some(true),
                destination: None,
            }],
            edges: Vec::new(),
        };
        let json = serde_json::to_value(WorkflowGraph::from(file)).unwrap();
        let node = &json["nodes"][0];
        assert_eq!(node["config"]["slug"], "csv_export");
        assert_eq!(node["onError"], "continue");
        assert_eq!(node["retry"]["maxAttempts"], 3);
        assert_eq!(node["retry"]["backoffMs"], 250);
        assert_eq!(node["retry"]["backoff"], "exponential");
        assert_eq!(node["requiresApproval"], true);
    }

    // --- P2: create body maps the new node fields (config/error/retry/approval)

    /// A create body carrying P2 node fields round-trips them through the
    /// render → parse pipeline the endpoint uses before writing to disk: config
    /// (with an `=expr` binding), `onError`, `retry` (camelCase → snake), and
    /// `requiresApproval` all survive.
    #[test]
    fn create_body_round_trips_p2_node_fields() {
        use crate::company::WorkflowNodeKind;

        let body: CreateWorkflowBody = serde_json::from_value(serde_json::json!({
            "id": "wf",
            "name": "WF",
            "nodes": [
                { "id": "start", "kind": "trigger", "name": "Start" },
                {
                    "id": "tf", "kind": "transform", "name": "Transform",
                    "config": { "set": { "count": "=items | length" } },
                    "onError": "continue",
                    "retry": { "maxAttempts": 3, "backoffMs": 250, "backoff": "exponential" },
                    "requiresApproval": true
                }
            ],
            "edges": [ { "from": "start", "to": "tf" } ]
        }))
        .expect("body deserializes");

        let raw = RawWorkflow::try_from(body).expect("converts");
        let toml_src = crate::company::render_workflow(&raw).expect("renders");
        let file = crate::company::parse_workflow(&toml_src).expect("re-parses");

        let tf = file.nodes.iter().find(|n| n.id == "tf").unwrap();
        assert_eq!(tf.kind, WorkflowNodeKind::Transform);
        assert_eq!(tf.on_error.as_deref(), Some("continue"));
        assert_eq!(tf.requires_approval, Some(true));
        let retry = tf.retry.as_ref().expect("retry present");
        assert_eq!(retry.max_attempts, Some(3));
        assert_eq!(retry.backoff_ms, Some(250));
        assert_eq!(retry.backoff.as_deref(), Some("exponential"));
        // The `=expr` binding is preserved verbatim for the engine to evaluate.
        assert_eq!(
            tf.config.as_ref().unwrap()["set"]["count"],
            "=items | length"
        );
    }

    /// An old create body (no P2 fields) still produces a bare node — every new
    /// field unset — so nothing changes for existing callers.
    #[test]
    fn create_body_without_new_fields_is_unchanged() {
        let body: CreateWorkflowBody = serde_json::from_value(serde_json::json!({
            "id": "wf",
            "name": "WF",
            "nodes": [ { "id": "start", "kind": "trigger", "name": "Start" } ],
            "edges": []
        }))
        .unwrap();
        let raw = RawWorkflow::try_from(body).expect("converts");
        let node = &raw.nodes[0];
        assert!(node.config.is_none());
        assert!(node.on_error.is_none());
        assert!(node.retry.is_none());
        assert!(node.requires_approval.is_none());
    }

    /// A JSON `null` inside node config is a 400 — TOML has no null to store it,
    /// so it is rejected before anything touches disk.
    #[test]
    fn create_body_with_null_config_is_a_bad_request() {
        use axum::http::StatusCode;

        let body: CreateWorkflowBody = serde_json::from_value(serde_json::json!({
            "id": "wf",
            "name": "WF",
            "nodes": [
                { "id": "call", "kind": "tool_call", "name": "Call", "config": { "slug": null } }
            ],
            "edges": []
        }))
        .unwrap();
        // `RawWorkflow` is not `Debug`, so unwrap the error by hand rather than
        // via `expect_err`.
        let err = match RawWorkflow::try_from(body) {
            Ok(_) => panic!("a null config value must be rejected"),
            Err(err) => err,
        };
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert!(matches!(err.0, OpenCompanyError::InvalidRequest(_)));
    }

    // --- trigger schedule (issue #169) --------------------------------------

    /// A create body carrying a trigger `schedule` round-trips it through the
    /// render → parse pipeline the endpoint runs before persisting.
    #[test]
    fn create_body_round_trips_a_trigger_schedule() {
        let body: CreateWorkflowBody = serde_json::from_value(serde_json::json!({
            "id": "wf",
            "name": "WF",
            "nodes": [
                { "id": "start", "kind": "trigger", "name": "Start", "schedule": "0 9 * * MON" },
                { "id": "done", "kind": "output", "name": "Done" }
            ],
            "edges": [ { "from": "start", "to": "done" } ]
        }))
        .expect("body deserializes");

        let raw = RawWorkflow::try_from(body).expect("converts");
        let toml_src = crate::company::render_workflow(&raw).expect("renders");
        let file = crate::company::parse_workflow(&toml_src).expect("re-parses");

        let start = file.nodes.iter().find(|n| n.id == "start").unwrap();
        assert_eq!(start.schedule.as_deref(), Some("0 9 * * MON"));
        let done = file.nodes.iter().find(|n| n.id == "done").unwrap();
        assert!(done.schedule.is_none());
    }

    /// The create route needs no schedule-specific validation code: the same
    /// render → parse round trip surfaces a bad cron (and a schedule on the
    /// wrong node kind) as the model's prosumer error, which the handler maps
    /// to a `400`.
    #[test]
    fn create_body_with_a_bad_schedule_fails_reparse() {
        let bad_cron: CreateWorkflowBody = serde_json::from_value(serde_json::json!({
            "id": "wf",
            "name": "WF",
            "nodes": [ { "id": "start", "kind": "trigger", "name": "Start", "schedule": "hourly" } ],
            "edges": []
        }))
        .unwrap();
        let raw = RawWorkflow::try_from(bad_cron).expect("converts");
        let toml_src = crate::company::render_workflow(&raw).expect("renders");
        let err = crate::company::parse_workflow(&toml_src).unwrap_err();
        assert!(err.to_string().contains("not a valid cron"), "{err}");

        let wrong_kind: CreateWorkflowBody = serde_json::from_value(serde_json::json!({
            "id": "wf",
            "name": "WF",
            "nodes": [
                { "id": "start", "kind": "trigger", "name": "Start" },
                { "id": "done", "kind": "output", "name": "Done", "schedule": "0 * * * *" }
            ],
            "edges": [ { "from": "start", "to": "done" } ]
        }))
        .unwrap();
        let raw = RawWorkflow::try_from(wrong_kind).expect("converts");
        let toml_src = crate::company::render_workflow(&raw).expect("renders");
        let err = crate::company::parse_workflow(&toml_src).unwrap_err();
        assert!(err.to_string().contains("only `trigger` nodes"), "{err}");
    }

    /// `GET …/workflows/{wid}` serializes the schedule in camelCase (it is a
    /// single word, so the key is `schedule`) and omits it when unset.
    #[test]
    fn json_serializes_the_trigger_schedule() {
        use crate::company::{WorkflowNodeDef, WorkflowNodeKind};

        let file = WorkflowFile {
            id: "wf".into(),
            name: "WF".into(),
            description: None,
            nodes: vec![
                WorkflowNodeDef {
                    id: "start".into(),
                    kind: WorkflowNodeKind::Trigger,
                    name: "Start".into(),
                    summary: None,
                    agent: None,
                    schedule: Some("0 * * * *".into()),
                    config: None,
                    on_error: None,
                    retry: None,
                    requires_approval: None,
                    destination: None,
                },
                WorkflowNodeDef {
                    id: "done".into(),
                    kind: WorkflowNodeKind::Output,
                    name: "Done".into(),
                    summary: None,
                    agent: None,
                    schedule: None,
                    config: None,
                    on_error: None,
                    retry: None,
                    requires_approval: None,
                    destination: None,
                },
            ],
            edges: Vec::new(),
        };
        let json = serde_json::to_value(WorkflowGraph::from(file)).unwrap();
        assert_eq!(json["nodes"][0]["schedule"], "0 * * * *");
        assert!(json["nodes"][1].get("schedule").is_none());
    }

    /// A legacy create body with no `schedule` key still converts, with the
    /// field unset — nothing changes for existing callers.
    #[test]
    fn create_body_without_a_schedule_is_unchanged() {
        let body: CreateWorkflowBody = serde_json::from_value(serde_json::json!({
            "id": "wf",
            "name": "WF",
            "nodes": [ { "id": "start", "kind": "trigger", "name": "Start" } ],
            "edges": []
        }))
        .unwrap();
        let raw = RawWorkflow::try_from(body).expect("converts");
        assert!(raw.nodes[0].schedule.is_none());
    }

    // --- Output destination on the wire (issue #170) ------------------------

    /// A create body's `destination` survives the render → parse pipeline the
    /// endpoint runs before persisting, and comes back out on the GET shape
    /// under the same key — so the console can post exactly what it reads.
    #[test]
    fn create_body_round_trips_an_output_destination() {
        let body: CreateWorkflowBody = serde_json::from_value(serde_json::json!({
            "id": "wf",
            "name": "WF",
            "nodes": [
                { "id": "start", "kind": "trigger", "name": "Start" },
                {
                    "id": "done", "kind": "output", "name": "Report",
                    "destination": { "kind": "email", "target": "ada@example.com" }
                }
            ],
            "edges": [ { "from": "start", "to": "done" } ]
        }))
        .expect("body deserializes");

        let raw = RawWorkflow::try_from(body).expect("converts");
        let toml_src = crate::company::render_workflow(&raw).expect("renders");
        let file = crate::company::parse_workflow(&toml_src).expect("re-parses");

        let done = file.nodes.iter().find(|n| n.id == "done").unwrap();
        let dest = done.destination.as_ref().expect("destination survived");
        assert_eq!(dest.kind, "email");
        assert_eq!(dest.target.as_deref(), Some("ada@example.com"));

        // …and back out on the read shape, under the same key.
        let json = serde_json::to_value(WorkflowGraph::from(file)).unwrap();
        let node = json["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "done")
            .unwrap()
            .clone();
        assert_eq!(node["destination"]["kind"], "email");
        assert_eq!(node["destination"]["target"], "ada@example.com");
    }

    /// An `owner` destination carries no target, and the key is omitted rather
    /// than serialized as `null`.
    #[test]
    fn an_owner_destination_omits_the_target_key() {
        let body: CreateWorkflowBody = serde_json::from_value(serde_json::json!({
            "id": "wf",
            "name": "WF",
            "nodes": [
                { "id": "start", "kind": "trigger", "name": "Start" },
                {
                    "id": "done", "kind": "output", "name": "Report",
                    "destination": { "kind": "owner" }
                }
            ],
            "edges": [ { "from": "start", "to": "done" } ]
        }))
        .unwrap();
        let raw = RawWorkflow::try_from(body).expect("converts");
        let file = crate::company::parse_workflow(
            &crate::company::render_workflow(&raw).expect("renders"),
        )
        .expect("re-parses");
        let json = serde_json::to_value(WorkflowGraph::from(file)).unwrap();
        let node = &json["nodes"][1];
        assert_eq!(node["destination"]["kind"], "owner");
        assert!(node["destination"].get("target").is_none());
    }

    /// A node with no destination omits the key entirely — the pre-#170 read
    /// shape is byte-identical for every existing graph.
    #[test]
    fn a_node_without_a_destination_omits_the_key() {
        let dir = seed_demo();
        let file = load_workflow_union(Some(dir.path()), &[], "demo")
            .unwrap()
            .unwrap();
        let json = serde_json::to_value(WorkflowGraph::from(file)).unwrap();
        for node in json["nodes"].as_array().unwrap() {
            assert!(node.get("destination").is_none(), "{node}");
        }
    }

    /// A destination the host cannot honour is rejected before anything is
    /// persisted — the create route surfaces the same prosumer-language problem
    /// a hand-authored file gets.
    #[test]
    fn create_body_with_a_bad_destination_is_rejected_at_validation() {
        for (dest, expected) in [
            (
                serde_json::json!({ "kind": "email", "target": "ada" }),
                "not an email address",
            ),
            (
                serde_json::json!({ "kind": "carrier_pigeon" }),
                "unknown `destination.kind`",
            ),
            (serde_json::json!({ "kind": "channel" }), "no `target`"),
        ] {
            let body: CreateWorkflowBody = serde_json::from_value(serde_json::json!({
                "id": "wf",
                "name": "WF",
                "nodes": [
                    { "id": "start", "kind": "trigger", "name": "Start" },
                    { "id": "done", "kind": "output", "name": "Report", "destination": dest }
                ],
                "edges": [ { "from": "start", "to": "done" } ]
            }))
            .unwrap();
            let raw = RawWorkflow::try_from(body).expect("converts");
            let toml_src = crate::company::render_workflow(&raw).expect("renders");
            let err = crate::company::parse_workflow(&toml_src)
                .expect_err("an unhonourable destination must not persist");
            assert!(err.to_string().contains(expected), "{err}");
        }
    }

    /// The run response carries `deliveries` in camelCase — this is the ONLY
    /// place an operator learns a report was not delivered, since a delivery
    /// failure never fails the run.
    #[test]
    fn run_response_serializes_delivery_rows_in_camelcase() {
        use crate::ports::{DeliveryReport, DeliveryStatus};

        let json = serde_json::to_value(RunWorkflowResponse {
            output: serde_json::json!({ "nodes": {} }),
            pending_approvals: Vec::new(),
            deliveries: vec![DeliveryReport {
                node: "done".into(),
                kind: "email".into(),
                target: Some("ada@example.com".into()),
                status: DeliveryStatus::Skipped,
                detail: "never written in".into(),
            }],
        })
        .unwrap();
        assert_eq!(json["deliveries"][0]["node"], "done");
        assert_eq!(json["deliveries"][0]["status"], "skipped");
        assert_eq!(json["deliveries"][0]["target"], "ada@example.com");
        assert_eq!(json["deliveries"][0]["detail"], "never written in");
        assert!(json["pendingApprovals"].is_array());
    }

    /// Issue #227: a parked report rides the run response as `"pending"`. The
    /// console's `DeliveryStatus` union is spelled in lowercase strings, so the
    /// wire word is the contract — a rename here silently drops the row into
    /// the frontend's fallback tone.
    #[test]
    fn run_response_serializes_a_parked_delivery_as_pending() {
        use crate::ports::{DeliveryReport, DeliveryStatus};

        let json = serde_json::to_value(RunWorkflowResponse {
            output: serde_json::json!({ "nodes": {} }),
            pending_approvals: Vec::new(),
            deliveries: vec![DeliveryReport {
                node: "done".into(),
                kind: "email".into(),
                target: Some("stranger@example.com".into()),
                status: DeliveryStatus::Pending,
                detail: "waiting for you in Approvals".into(),
            }],
        })
        .unwrap();
        assert_eq!(json["deliveries"][0]["status"], "pending");
    }

    /// A graph that routes nothing serializes an empty list, not a missing key —
    /// the console can render "no deliveries" without a null check.
    #[test]
    fn run_response_with_no_deliveries_is_an_empty_list() {
        let json = serde_json::to_value(RunWorkflowResponse {
            output: Value::Null,
            pending_approvals: Vec::new(),
            deliveries: Vec::new(),
        })
        .unwrap();
        assert_eq!(json["deliveries"], serde_json::json!([]));
    }

    #[test]
    fn safe_wid_rejects_traversal() {
        assert!(safe_wid("demo"));
        assert!(safe_wid("my-workflow_2"));
        assert!(!safe_wid(""));
        assert!(!safe_wid(".."));
        assert!(!safe_wid("."));
        assert!(!safe_wid("../secrets"));
        assert!(!safe_wid("a/b"));
        assert!(!safe_wid("/etc/passwd"));
        assert!(!safe_wid("foo/../bar"));
    }

    #[test]
    fn one_malformed_workflow_does_not_break_the_list() {
        let dir = seed_demo();
        // A second, broken workflow file must not 500 the whole picker.
        std::fs::write(
            dir.path().join("workflows").join("broken.toml"),
            "id = \"broken\"\nname = \n[[node]] oops",
        )
        .unwrap();
        let files = list_workflows_union(Some(dir.path()), &[]);
        let ids: Vec<_> = files.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["demo"]);
    }

    // HTTP-level: a hosted tenant has no source directory to scan, so these
    // exercise the manifest-enabled union path end to end via the router.
    mod hosted_mode {
        use axum::body::{Body, to_bytes};
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        use crate::company::CompanyManifest;
        use crate::ports::CompanyStore;
        use crate::ports::types::{CompanyId, CompanyRecord};
        use crate::runtime::RuntimeBuilder;
        use crate::server::router;
        use crate::store::FsCompanyStore;
        use crate::{AppConfig, AppState};

        fn home() -> tempfile::TempDir {
            tempfile::Builder::new()
                .prefix("oc-workflows-hosted-")
                .tempdir()
                .expect("tempdir")
        }

        /// A manifest declaring one enabled workflow — mirrors what a
        /// platform tenant provisions with, minus any `workflows/` directory
        /// on disk (there isn't one: hosted tenants have no source dir).
        fn manifest_with_enabled() -> CompanyManifest {
            toml::from_str(
                "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[workflows]\nenabled = [\"demo\"]\n",
            )
            .unwrap()
        }

        /// Builds a running company whose runtime has **no source directory**
        /// (built without `with_seed_dir`, matching how the platform builds a
        /// provisioned tenant) but whose persisted record declares an enabled
        /// workflow — the exact hosted-mode gap #70 reports.
        async fn state_with_hosted_company(home: &std::path::Path) -> AppState {
            let store = FsCompanyStore::new(home.to_path_buf());
            let id = CompanyId::new("acme");
            store
                .save(&CompanyRecord {
                    id: id.clone(),
                    manifest: manifest_with_enabled(),
                    ledger: Vec::new(),
                    lifecycle: "running".to_string(),
                    overlay_agents: Vec::new(),
                    overlay_desk_members: Vec::new(),
                    overlay_desk_order: Vec::new(),
                    overlay_desks: Vec::new(),
                    overlay_workflows: Vec::new(),
                    template_provenance: None,
                })
                .await
                .unwrap();
            let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest_with_enabled())
                .with_id(id.clone())
                .build()
                .await
                .unwrap();
            assert!(
                runtime.source_dir().is_none(),
                "test setup must simulate hosted mode: no source dir"
            );
            let state = AppState::new(AppConfig::default());
            state.registry().insert(id, std::sync::Arc::new(runtime));
            crate::server::test_support::seed_fixed_admin(&state, "acme").await;
            state
        }

        #[tokio::test]
        async fn manifest_enabled_workflow_lists_with_no_source_dir() {
            let home_dir = home();
            let home = home_dir.path().to_path_buf();
            let state = state_with_hosted_company(&home).await;

            let response = router(state)
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/api/v1/company/workflows")
                        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

            // Regression for #70: the REST list used to scan the filesystem
            // only, so a hosted tenant with no source dir always got `[]`
            // here even though its manifest declared an enabled workflow.
            let items = body.as_array().expect("array response");
            assert_eq!(items.len(), 1, "body: {body}");
            assert_eq!(items[0]["id"], "demo");
            // No file to load a real name from, so the id is the fallback
            // name — same fallback the GraphQL `Company.workflows` resolver
            // uses for the same case.
            assert_eq!(items[0]["name"], "demo");
        }

        /// A hosted tenant's record with no manifest-enabled workflows — the
        /// blank-slate a real tenant starts from before it creates anything.
        fn empty_manifest() -> CompanyManifest {
            toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap()
        }

        /// Same as [`state_with_hosted_company`] but with nothing enabled, and
        /// returning the store so a test can rebuild state from it.
        async fn hosted_state(home: &std::path::Path) -> (AppState, FsCompanyStore, CompanyId) {
            let store = FsCompanyStore::new(home.to_path_buf());
            let id = CompanyId::new("acme");
            store
                .save(&CompanyRecord {
                    id: id.clone(),
                    manifest: empty_manifest(),
                    ledger: Vec::new(),
                    lifecycle: "running".to_string(),
                    overlay_agents: Vec::new(),
                    overlay_desk_members: Vec::new(),
                    overlay_desk_order: Vec::new(),
                    overlay_desks: Vec::new(),
                    overlay_workflows: Vec::new(),
                    template_provenance: None,
                })
                .await
                .unwrap();
            let state = state_over(home, &id, true).await;
            (state, store, id)
        }

        /// Builds an `AppState` whose runtime for `id` has **no source
        /// directory** — the hosted shape — over the store rooted at `home`.
        ///
        /// `seed_admin` seeds the fixed admin + session; a *rebuild* over the
        /// same home must pass `false` (the durable user store already has that
        /// admin, and its session survives with it).
        async fn state_over(home: &std::path::Path, id: &CompanyId, seed_admin: bool) -> AppState {
            let runtime = RuntimeBuilder::new(home.to_path_buf(), empty_manifest())
                .with_id(id.clone())
                .build()
                .await
                .unwrap();
            assert!(
                runtime.source_dir().is_none(),
                "test setup must simulate hosted mode: no source dir"
            );
            let state = AppState::new(AppConfig::default());
            state
                .registry()
                .insert(id.clone(), std::sync::Arc::new(runtime));
            if seed_admin {
                crate::server::test_support::seed_fixed_admin(&state, "acme").await;
            }
            state
        }

        /// The graph body the console posts.
        fn create_body() -> serde_json::Value {
            serde_json::json!({
                "id": "greeter",
                "name": "Greeter",
                "description": "Say hi.",
                "nodes": [
                    { "id": "start", "kind": "trigger", "name": "Start" },
                    { "id": "done", "kind": "output", "name": "Report" }
                ],
                "edges": [ { "from": "start", "to": "done", "label": "ok" } ]
            })
        }

        fn request(method: &str, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
            let builder = Request::builder()
                .method(method)
                .uri(uri)
                .header("cookie", crate::server::test_support::fixed_cookie("acme"));
            match body {
                Some(json) => builder
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&json).unwrap()))
                    .unwrap(),
                None => builder.body(Body::empty()).unwrap(),
            }
        }

        async fn json_body(response: axum::response::Response) -> serde_json::Value {
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        }

        /// **The #168 regression test.** Creating a workflow on a tenant with no
        /// (writable) source directory used to fail with
        /// `Read-only file system (os error 30)` — the handler wrote the graph
        /// into the crate's read-only company source tree. It now persists on
        /// the record, so the create succeeds, the graph lists under its real
        /// name, and the full body reads back.
        #[tokio::test]
        async fn create_persists_and_reads_back_with_no_source_dir() {
            let home_dir = home();
            let home = home_dir.path().to_path_buf();
            let (state, _store, _id) = hosted_state(&home).await;

            // POST → 200 with the graph echoed back.
            let response = router(state.clone())
                .oneshot(request(
                    "POST",
                    "/api/v1/company/workflows",
                    Some(create_body()),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let created = json_body(response).await;
            assert_eq!(created["id"], "greeter");
            assert_eq!(created["nodes"].as_array().unwrap().len(), 2);

            // GET list → the real name, not the id fallback.
            let response = router(state.clone())
                .oneshot(request("GET", "/api/v1/company/workflows", None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let listed = json_body(response).await;
            let items = listed.as_array().expect("array");
            assert_eq!(items.len(), 1, "body: {listed}");
            assert_eq!(items[0]["id"], "greeter");
            assert_eq!(items[0]["name"], "Greeter");
            assert_eq!(items[0]["description"], "Say hi.");

            // GET one → the full graph.
            let response = router(state)
                .oneshot(request("GET", "/api/v1/company/workflows/greeter", None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let graph = json_body(response).await;
            assert_eq!(graph["id"], "greeter");
            assert_eq!(graph["edges"][0]["label"], "ok");
        }

        /// A duplicate id is a clean 409, not a 500 — the id-uniqueness check
        /// that replaced the filesystem's `create_new(true)`.
        #[tokio::test]
        async fn duplicate_create_is_a_conflict() {
            let home_dir = home();
            let home = home_dir.path().to_path_buf();
            let (state, _store, _id) = hosted_state(&home).await;

            let first = router(state.clone())
                .oneshot(request(
                    "POST",
                    "/api/v1/company/workflows",
                    Some(create_body()),
                ))
                .await
                .unwrap();
            assert_eq!(first.status(), StatusCode::OK);

            let second = router(state)
                .oneshot(request(
                    "POST",
                    "/api/v1/company/workflows",
                    Some(create_body()),
                ))
                .await
                .unwrap();
            assert_eq!(second.status(), StatusCode::CONFLICT);
        }

        /// Restart survival: a workflow created through the API is still listed
        /// by a completely fresh `AppState` rebuilt over the same store — proving
        /// the body is durable, not process-local.
        #[tokio::test]
        async fn a_created_workflow_survives_a_state_rebuild() {
            let home_dir = home();
            let home = home_dir.path().to_path_buf();
            let (state, _store, id) = hosted_state(&home).await;

            let response = router(state)
                .oneshot(request(
                    "POST",
                    "/api/v1/company/workflows",
                    Some(create_body()),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);

            // Rebuild everything from the same durable store.
            let rebuilt = state_over(&home, &id, false).await;
            let response = router(rebuilt)
                .oneshot(request("GET", "/api/v1/company/workflows", None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let listed = json_body(response).await;
            let items = listed.as_array().expect("array");
            assert_eq!(items.len(), 1, "body: {listed}");
            assert_eq!(items[0]["id"], "greeter");
            assert_eq!(items[0]["name"], "Greeter");
        }
    }
}
