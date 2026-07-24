//! Workflow surfaces: create a graph (`POST /workflows`), read the company's
//! saved graphs (`GET /workflows`, `GET /workflows/{wid}`), and run one
//! (`POST /workflows/{wid}/run`) — under both scope forms.
//!
//! Graphs are loaded from the company's on-disk source directory
//! (`companies/<name>/workflows/<wid>.toml`) via
//! [`load_company_workflows`](crate::company::load_company_workflows), which
//! takes an explicit id list (it never scans) — so `list_workflows` enumerates
//! the `workflows/` directory itself to build that list.
//!
//! A platform-provisioned tenant has no source directory, so there is nothing
//! to scan — but it can still declare `[workflows].enabled` ids in its
//! manifest. `list_workflows` unions those manifest-enabled ids in (deduped by
//! id) so the console's picker isn't empty just because the deployment has no
//! `workflows/*.toml` files on disk, mirroring the `Company.workflows`
//! GraphQL resolver. Where the definition body isn't available to load (no
//! source directory, or the id has no matching file), the summary falls back
//! to the id as its name — the same fallback the GraphQL resolver uses. Full
//! graphs (`GET …/workflows/{wid}`) still require a source directory, since
//! there is currently no other place a graph body can come from.
//!
//! Creation (issue #69) writes a new `workflows/<id>.toml` into that same
//! source directory — reusing [`parse_workflow`](crate::company::parse_workflow)
//! to validate the graph a caller posts before anything touches disk — and
//! records the id as enabled on the operator's live [`CompanyRecord`], mirroring
//! the team overlay convention: the version-controlled `company.toml` is never
//! rewritten (see `crate::server::ops::team`). A deployment with no source
//! directory (platform-provisioned mode with nothing seeded on disk yet) has
//! nowhere to write the graph file, so creation is refused with a 4xx rather
//! than crashing.
//!
//! Execution is dependency-inverted behind the [`WorkflowRunner`] port: when no
//! runner is wired (the default build, or a runtime built without a harness) the
//! run route reports `not_wired` — the same 404 seam the DNS/SMTP surfaces use —
//! so the default build stays inert. The read routes need no runner: they only
//! parse the committed graph files, so the console can list and render workflows
//! even on a build that cannot execute them.

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
    RawEdge, RawNode, RawWorkflow, WorkflowEdgeDef, WorkflowFile, WorkflowNodeDef,
    WorkflowRetryDef, create_company_workflow, load_company_workflows,
};
use crate::error::OpenCompanyError;
use crate::server::error::ApiError;
use crate::server::ops::language;
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
/// is only meaningful on `agent` nodes. The P1 fields (`config` / `onError` /
/// `retry` / `requiresApproval`) are serialized so `GET …/workflows/{wid}` does
/// not drop model data (they are omitted entirely when unset).
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
    config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry: Option<WorkflowRetryOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requires_approval: Option<bool>,
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
            config: n.config,
            on_error: n.on_error,
            retry: n.retry.map(WorkflowRetryOut::from),
            requires_approval: n.requires_approval,
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

/// `GET …/workflows` — the company's saved workflows as picker summaries.
///
/// The loader takes an explicit id list rather than scanning, so this reads the
/// company's `workflows/` directory to collect the `*.toml` file stems as ids,
/// then loads and summarizes them. No source directory (platform-provisioned
/// mode) or no `workflows/` directory yields an empty filesystem scan — but not
/// necessarily an empty response: the manifest's `[workflows].enabled` ids are
/// unioned in (deduped against the filesystem scan by id), falling back to the
/// id as the name when there's no file to load a real name from. Only when
/// both the scan and the manifest are empty does this return `200 []`, so the
/// console renders "no workflows yet" rather than a failure.
async fn list_workflows(company: ScopedCompany) -> Result<Json<Vec<WorkflowSummary>>, ApiError> {
    let source_dir = company.runtime.source_dir();
    let files = load_source_workflows(source_dir)?;
    let mut seen: HashSet<String> = files.iter().map(|f| f.id.clone()).collect();
    let mut summaries: Vec<WorkflowSummary> =
        files.into_iter().map(WorkflowSummary::from).collect();

    let enabled_ids = company
        .runtime
        .enabled_workflow_ids()
        .await
        .map_err(ApiError)?;
    for id in enabled_ids {
        // Already present from the filesystem scan — skip so hosted mode
        // (source dir present, manifest also lists the same ids) doesn't
        // double-list.
        if !seen.insert(id.clone()) {
            continue;
        }
        let loaded = source_dir
            .and_then(|dir| load_company_workflows(dir, std::slice::from_ref(&id)).ok())
            .and_then(|mut files| files.pop());
        summaries.push(match loaded {
            Some(file) => WorkflowSummary::from(file),
            None => WorkflowSummary {
                id: id.clone(),
                name: id,
                description: None,
            },
        });
    }

    Ok(Json(summaries))
}

/// `GET …/workflows/{wid}` — the full graph for one workflow.
///
/// An unknown `wid` (or a deployment with no source directory) is a `404`,
/// mirroring the sub-resource-not-found shape the task routes use.
async fn get_workflow(
    company: ScopedCompany,
    Path(WorkflowPath { wid }): Path<WorkflowPath>,
) -> Result<Json<WorkflowGraph>, ApiError> {
    // `wid` becomes a filename — reject anything that could escape `workflows/`.
    if !safe_wid(&wid) {
        return Err(ApiError(OpenCompanyError::CompanyNotFound(format!(
            "workflow {wid}"
        ))));
    }
    let source_dir = company
        .runtime
        .source_dir()
        .ok_or_else(|| ApiError(OpenCompanyError::CompanyNotFound(format!("workflow {wid}"))))?;
    // Only try to load ids that exist on disk, so a missing file is a clean 404
    // rather than the loader's `DataRead` (a 500).
    let path = source_dir.join("workflows").join(format!("{wid}.toml"));
    if !path.is_file() {
        return Err(ApiError(OpenCompanyError::CompanyNotFound(format!(
            "workflow {wid}"
        ))));
    }
    let file = load_company_workflows(source_dir, std::slice::from_ref(&wid))
        .map_err(ApiError)?
        .into_iter()
        .next()
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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateEdge {
    from: String,
    to: String,
    #[serde(default)]
    label: Option<String>,
}

impl From<CreateWorkflowBody> for RawWorkflow {
    fn from(body: CreateWorkflowBody) -> Self {
        Self {
            id: body.id,
            name: body.name,
            description: body.description,
            nodes: body
                .nodes
                .into_iter()
                .map(|n| RawNode {
                    id: n.id,
                    kind: n.kind,
                    name: n.name,
                    summary: n.summary,
                    agent: n.agent,
                    // The create endpoint (#69/#112) does not author per-node
                    // config/error/retry policy yet — a P2/P4 concern. Left unset
                    // here so an authored graph round-trips unchanged.
                    config: None,
                    on_error: None,
                    retry: None,
                    requires_approval: None,
                })
                .collect(),
            edges: body
                .edges
                .into_iter()
                .map(|e| RawEdge {
                    from: e.from,
                    to: e.to,
                    label: e.label,
                })
                .collect(),
        }
    }
}

/// `POST …/workflows` — authors a new workflow graph (issue #69): the console's
/// form creator, or any direct API caller, posts the graph shape and it lands
/// as a new `workflows/<id>.toml` in the company's source directory.
///
/// A thin shim over [`create_company_workflow`] (issue #112), the shared
/// validated-persist core the orchestrator's `create_workflow` tool also runs —
/// so both surfaces enforce the same checks (safe id + length/size caps,
/// exactly one trigger, roster cross-check, case-insensitive name uniqueness,
/// [`parse_workflow`](crate::company::parse_workflow) revalidation, atomic
/// write, enable, best-effort audit event) and land the identical artifact.
///
/// The only work left at this layer is the two request-shaped concerns: refuse
/// a deployment with no writable source directory (a hosted tenant with nothing
/// seeded) before calling the core, and map the core's error variants to HTTP
/// statuses — [`InvalidRequest`](OpenCompanyError::InvalidRequest) → 400,
/// [`Conflict`](OpenCompanyError::Conflict) → 409 — via [`ApiError`].
async fn create_workflow(
    company: ScopedCompany,
    Json(body): Json<CreateWorkflowBody>,
) -> Result<Json<WorkflowGraph>, ApiError> {
    // A deployment with no source directory (platform-provisioned mode with
    // nothing seeded on disk yet) has nowhere to write the graph file.
    let source_dir = company.runtime.source_dir().ok_or_else(|| {
        ApiError(OpenCompanyError::InvalidRequest(
            language::WORKFLOW_NEEDS_SOURCE_DIR.to_string(),
        ))
    })?;

    let file = create_company_workflow(
        company.id(),
        source_dir,
        company.runtime.store(),
        Some(company.runtime.events()),
        body.into(),
    )
    .await
    .map_err(ApiError)?;

    Ok(Json(WorkflowGraph::from(file)))
}

/// Loads every saved workflow under `source_dir/workflows/`, or an empty list
/// when there is no source directory or no `workflows/` directory.
fn load_source_workflows(source_dir: Option<&FsPath>) -> Result<Vec<WorkflowFile>, ApiError> {
    let Some(source_dir) = source_dir else {
        return Ok(Vec::new());
    };
    let Ok(entries) = std::fs::read_dir(source_dir.join("workflows")) else {
        return Ok(Vec::new());
    };
    let mut ids: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("toml"))
        .filter_map(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
        })
        .collect();
    // Stable, deterministic order for the picker.
    ids.sort();
    // Load each id on its own so one malformed `workflows/*.toml` skips only
    // itself instead of 500-ing the whole picker.
    let mut files = Vec::with_capacity(ids.len());
    for id in &ids {
        match load_company_workflows(source_dir, std::slice::from_ref(id)) {
            Ok(loaded) => files.extend(loaded),
            Err(err) => tracing::warn!(workflow = %id, error = %err, "skipping malformed workflow"),
        }
    }
    Ok(files)
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

    // Load the saved graph from the company's on-disk source directory. Without
    // one (platform-provisioned mode) there is nothing to run.
    let source_dir = company.runtime.source_dir().ok_or_else(|| {
        super::not_wired("workflow source (no company definition directory on this deployment)")
    })?;
    let file = load_company_workflows(source_dir, std::slice::from_ref(&wid))
        .map_err(|e| ApiError(e).into_response())?
        .into_iter()
        .next()
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
        let files = load_source_workflows(Some(dir.path())).expect("lists");
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
        let ids = ["demo".to_string()];
        let file = load_company_workflows(dir.path(), &ids)
            .expect("loads")
            .into_iter()
            .next()
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
    fn no_source_dir_lists_empty() {
        assert!(load_source_workflows(None).unwrap().is_empty());
    }

    #[test]
    fn no_workflows_dir_lists_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_source_workflows(Some(dir.path())).unwrap().is_empty());
    }

    #[test]
    fn json_serializes_camelcase_and_omits_empty_options() {
        let dir = seed_demo();
        let ids = ["demo".to_string()];
        let file = load_company_workflows(dir.path(), &ids)
            .unwrap()
            .into_iter()
            .next()
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
                config: Some(serde_json::json!({ "slug": "csv_export" })),
                on_error: Some("continue".into()),
                retry: Some(WorkflowRetryDef {
                    max_attempts: Some(3),
                    backoff_ms: Some(250),
                    backoff: Some("exponential".into()),
                }),
                requires_approval: Some(true),
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
        let files = load_source_workflows(Some(dir.path())).expect("lists despite a bad file");
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

        fn home() -> std::path::PathBuf {
            std::env::temp_dir().join(format!(
                "oc-workflows-hosted-{}",
                crate::ports::generate_id()
            ))
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
            let home = home();
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

            std::fs::remove_dir_all(&home).ok();
        }
    }
}
