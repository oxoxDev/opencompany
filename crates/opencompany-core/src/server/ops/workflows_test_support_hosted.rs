//! Hosted-mode fixtures for the `workflows` ops test cluster — the
//! `hosted_mode` half of `workflows_test_support.rs`, split out to keep
//! each source file under the 750-line cap.

pub(crate) use axum::body::{Body, to_bytes};
pub(crate) use axum::http::{Request, StatusCode};
pub(crate) use tower::ServiceExt;

pub(crate) use super::super::WorkflowRunOutcome;
pub(in crate::server::ops::workflows) use super::super::select_run_page;
pub(crate) use crate::company::CompanyManifest;
pub(crate) use crate::ports::CompanyStore;
pub(crate) use crate::ports::types::{CompanyEvent, WorkflowNodeStatus};
pub(crate) use crate::ports::types::{CompanyId, CompanyRecord};
pub(crate) use crate::ports::workflow_verdict::WorkflowRunVerdict;
pub(crate) use crate::runtime::RuntimeBuilder;
pub(crate) use crate::server::router;
pub(crate) use crate::store::FsCompanyStore;
pub(crate) use crate::{AppConfig, AppState};

pub(crate) fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-workflows-hosted-")
        .tempdir()
        .expect("tempdir")
}

/// A manifest declaring one enabled workflow — mirrors what a
/// platform tenant provisions with, minus any `workflows/` directory
/// on disk (there isn't one: hosted tenants have no source dir).
pub(crate) fn manifest_with_enabled() -> CompanyManifest {
    toml::from_str(
            "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[workflows]\nenabled = [\"demo\"]\n",
        )
        .unwrap()
}

/// Builds a running company whose runtime has **no source directory**
/// (built without `with_seed_dir`, matching how the platform builds a
/// provisioned tenant) but whose persisted record declares an enabled
/// workflow — the exact hosted-mode gap #70 reports.
pub(crate) async fn state_with_hosted_company(home: &std::path::Path) -> AppState {
    state_with_hosted_company_lifecycle(home, "running").await
}

/// The same fixture at a chosen lifecycle, so a paused company is
/// reachable without a second copy of the record literal.
pub(crate) async fn state_with_hosted_company_lifecycle(
    home: &std::path::Path,
    lifecycle: &str,
) -> AppState {
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: manifest_with_enabled(),
            ledger: Vec::new(),
            lifecycle: lifecycle.to_string(),
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

/// A hosted tenant's record with no manifest-enabled workflows — the
/// blank-slate a real tenant starts from before it creates anything.
pub(crate) fn empty_manifest() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap()
}

/// Same as [`state_with_hosted_company`] but with nothing enabled, and
/// returning the store so a test can rebuild state from it.
pub(crate) async fn hosted_state(home: &std::path::Path) -> (AppState, FsCompanyStore, CompanyId) {
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: empty_manifest(),
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
pub(crate) async fn state_over(
    home: &std::path::Path,
    id: &CompanyId,
    seed_admin: bool,
) -> AppState {
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
pub(crate) fn create_body() -> serde_json::Value {
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

pub(crate) fn request(method: &str, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
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

pub(crate) async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

// ------------------------------------------------------------------
// `POST …/workflows/validate` — the author-time verdict, no save (#1074)
// ------------------------------------------------------------------

pub(crate) async fn post_validate(
    state: &AppState,
    body: serde_json::Value,
) -> axum::response::Response {
    router(state.clone())
        .oneshot(request(
            "POST",
            "/api/v1/company/workflows/validate",
            Some(body),
        ))
        .await
        .unwrap()
}

pub(crate) async fn post_create_on(
    state: &AppState,
    body: serde_json::Value,
) -> axum::response::Response {
    router(state.clone())
        .oneshot(request("POST", "/api/v1/company/workflows", Some(body)))
        .await
        .unwrap()
}

/// `create_body` with an extra node nothing points at — the reachability
/// rule (`crate::company::workflow_file`), which is one of the two a
/// client cannot pre-empt without re-implementing it.
pub(crate) fn body_with_an_unreachable_node() -> serde_json::Value {
    let mut body = create_body();
    body["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(
            { "id": "orphan", "kind": "output", "name": "Orphan" }
        ));
    body
}

/// `create_body` with a `condition` node whose branch carries `label`,
/// and `onError` set on the condition when `on_error` is given.
pub(crate) fn body_with_condition(label: &str, on_error: Option<&str>) -> serde_json::Value {
    let mut gate = serde_json::json!({
        "id": "gate",
        "kind": "condition",
        "name": "Gate",
        "config": { "field": "=item.approved" }
    });
    if let Some(on_error) = on_error {
        gate["onError"] = serde_json::Value::String(on_error.to_string());
    }
    serde_json::json!({
        "id": "greeter",
        "name": "Greeter",
        "description": "Say hi.",
        "nodes": [
            { "id": "start", "kind": "trigger", "name": "Start" },
            gate,
            { "id": "done", "kind": "output", "name": "Report" }
        ],
        "edges": [
            { "from": "start", "to": "gate" },
            { "from": "gate", "to": "done", "label": label }
        ]
    })
}

/// A company whose runtime HAS a source directory holding one seed
/// workflow at `workflows/child.toml` — the self-hosted / local `serve`
/// shape. `hosted_state` deliberately has none, so the seed-file half of
/// the `sub_workflow` existence probe is unreachable from it.
pub(crate) async fn seeded_state(home: &std::path::Path) -> (AppState, tempfile::TempDir) {
    let source = tempfile::Builder::new()
        .prefix("oc-workflows-source-")
        .tempdir()
        .expect("tempdir");
    let workflows = source.path().join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(
        workflows.join("child.toml"),
        "id = \"child\"\nname = \"Child\"\n[[node]]\nid = \"start\"\n\
             kind = \"trigger\"\nname = \"Start\"\n",
    )
    .unwrap();

    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            id: id.clone(),
            manifest: empty_manifest(),
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            overlay_retired_agents: Vec::new(),
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
            setup: Default::default(),
            name_confirmed: false,
            activation_completed_at: None,
            created_at_millis: None,
        })
        .await
        .unwrap();
    let runtime = RuntimeBuilder::new(home.to_path_buf(), empty_manifest())
        .with_id(id.clone())
        .with_seed_dir(source.path())
        .build()
        .await
        .unwrap();
    assert!(
        runtime.source_dir().is_some(),
        "this fixture only proves anything with a source directory"
    );
    let state = AppState::new(AppConfig::default());
    state
        .registry()
        .insert(id.clone(), std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    (state, source)
}

/// A graph whose `sub_workflow` node runs `child` — which exists ONLY as
/// a seed file, so the probe can only see it with a source directory.
pub(crate) fn body_with_sub_workflow() -> serde_json::Value {
    serde_json::json!({
        "id": "parent",
        "name": "Parent",
        "nodes": [
            { "id": "start", "kind": "trigger", "name": "Start" },
            {
                "id": "child_run",
                "kind": "sub_workflow",
                "name": "Run the child",
                "config": { "workflow_id": "child" }
            }
        ],
        "edges": [ { "from": "start", "to": "child_run" } ]
    })
}

// --- Save-time channel-destination guard (issue #981) ---------------

/// A hosted tenant WITH a desk, so it has one real delivery channel.
/// `hosted_state`'s manifest declares none, which makes its deliverable
/// set empty — fine for the nowhere-to-deliver case below, useless for
/// telling an accepted target from a refused one.
pub(crate) fn desk_manifest() -> CompanyManifest {
    toml::from_str(
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
             [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n\
             [[group_chat]]\nid = \"engineering\"\nname = \"Engineering\"\nmembers = [\"ceo\"]\n",
    )
    .unwrap()
}

/// `hosted_state` over [`desk_manifest`] — a running company whose
/// deliverable set is exactly `["engineering"]`.
pub(crate) async fn desk_state(home: &std::path::Path) -> AppState {
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: desk_manifest(),
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
        })
        .await
        .unwrap();
    let runtime = RuntimeBuilder::new(home.to_path_buf(), desk_manifest())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    assert_eq!(
        runtime.deliverable_channel_ids(),
        vec!["operator".to_string(), "engineering".to_string()],
        "the fixture must have the operator channel plus exactly one desk channel, or \
             these tests prove nothing"
    );
    let state = AppState::new(AppConfig::default());
    state
        .registry()
        .insert(id.clone(), std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

/// [`create_body`] with the output node routing its report to `target`
/// on `kind`.
pub(crate) fn body_with_destination(kind: &str, target: Option<&str>) -> serde_json::Value {
    let mut destination = serde_json::json!({ "kind": kind });
    if let Some(target) = target {
        destination["target"] = serde_json::Value::String(target.to_string());
    }
    let mut body = create_body();
    body["nodes"][1]["destination"] = destination;
    body
}

pub(crate) async fn post_create(
    state: AppState,
    body: serde_json::Value,
) -> axum::response::Response {
    router(state)
        .oneshot(request("POST", "/api/v1/company/workflows", Some(body)))
        .await
        .unwrap()
}

/// [`create_body`] with `done` turned into an `agent` node naming the
/// roster teammate [`desk_manifest`] declares (`ceo`), carrying a
/// declared `postcondition` — the shape a real create/edit sends.
pub(crate) fn body_with_postcondition() -> serde_json::Value {
    let mut body = create_body();
    body["nodes"][1]["kind"] = serde_json::json!("agent");
    body["nodes"][1]["agent"] = serde_json::json!("ceo");
    body["nodes"][1]["postcondition"] = serde_json::json!({ "require": "non_empty" });
    body
}

#[path = "workflows_test_support_hosted_runs.rs"]
mod runs;
pub(crate) use runs::*;

pub(crate) async fn preview(state: &AppState, expr: &str) -> serde_json::Value {
    json_body(
        router(state.clone())
            .oneshot(request(
                "POST",
                "/api/v1/company/workflows/cron/preview",
                Some(serde_json::json!({ "expr": expr, "after": AFTER })),
            ))
            .await
            .unwrap(),
    )
    .await
}

// ── Issue #259: edit + delete at the HTTP boundary ──────────────────

/// Creates `greeter` and returns its current version token.
pub(crate) async fn create_greeter(state: &AppState) -> String {
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
    // A freshly created overlay graph is editable and carries a token.
    assert_eq!(created["editable"], true, "{created}");
    created["version"]
        .as_str()
        .unwrap_or_else(|| panic!("create must return a version token: {created}"))
        .to_string()
}

/// `create_body()` with a schedule on the trigger and a changed
/// description — the exact "I typo'd my cron" edit the issue is about.
pub(crate) fn edited_body(expected_version: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::json!({
        "id": "greeter",
        "name": "Greeter",
        "description": "Say hi, every morning.",
        "nodes": [
            { "id": "start", "kind": "trigger", "name": "Start", "schedule": "0 9 * * *" },
            { "id": "done", "kind": "output", "name": "Report" }
        ],
        "edges": [ { "from": "start", "to": "done", "label": "ok" } ]
    });
    if let Some(v) = expected_version {
        body["expectedVersion"] = serde_json::json!(v);
    }
    body
}

// ── Issue #274: revision history + rollback at the HTTP boundary ────

/// Edits `greeter` once (adding a schedule) so exactly one revision — the
/// original, schedule-less body — is captured, and returns the token of
/// the now-current (scheduled) graph.
pub(crate) async fn create_then_edit_greeter(state: &AppState) -> String {
    let version = create_greeter(state).await;
    let response = router(state.clone())
        .oneshot(request(
            "PUT",
            "/api/v1/company/workflows/greeter",
            Some(edited_body(Some(&version))),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await["version"]
        .as_str()
        .expect("new token")
        .to_string()
}

// ── Issue #1009: settle eternal-`running` rows on the read ─────────

/// Every `WorkflowRunFinished` the company journaled carrying `run_id`.
pub(crate) async fn finishes_for(
    state: &AppState,
    id: &CompanyId,
    run_id: &str,
) -> Vec<(Option<String>, bool)> {
    let runtime = state.registry().get(id).expect("registered");
    runtime
        .events()
        .read_from(id, crate::ports::types::EventSeq::new(0), usize::MAX)
        .await
        .expect("read")
        .into_iter()
        .filter_map(|s| match s.event {
            CompanyEvent::WorkflowRunFinished {
                run_id: Some(rid),
                error,
                cancelled,
                ..
            } if rid == run_id => Some((error, cancelled)),
            _ => None,
        })
        .collect()
}
