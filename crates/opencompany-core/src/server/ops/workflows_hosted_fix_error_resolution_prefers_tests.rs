use super::workflows_test_support::hosted_mode::*;
use super::*;
use crate::server::router;

#[test]
fn fix_error_resolution_prefers_journal_then_hint_then_nothing() {
    use super::{JournaledFailure, resolve_fix_error};
    // A journaled error wins, carrying the failing node id.
    assert_eq!(
        resolve_fix_error(
            Some(JournaledFailure {
                error: Some("boom".to_string()),
                failed_node_id: Some("n1".to_string()),
            }),
            Some("hint".to_string()),
        ),
        Some(("boom".to_string(), Some("n1".to_string())))
    );
    // A run that finished CLEAN (no error) falls back to the hint.
    assert_eq!(
        resolve_fix_error(
            Some(JournaledFailure {
                error: None,
                failed_node_id: None,
            }),
            Some("hint".to_string()),
        ),
        Some(("hint".to_string(), None))
    );
    // No finish for this run id at all → the hint is the only source.
    assert_eq!(
        resolve_fix_error(None, Some("hint".to_string())),
        Some(("hint".to_string(), None))
    );
    // A clean run and no hint → nothing to fix from.
    assert_eq!(
        resolve_fix_error(
            Some(JournaledFailure {
                error: None,
                failed_node_id: None,
            }),
            None
        ),
        None
    );
    // No run and no hint → nothing to fix from.
    assert_eq!(resolve_fix_error(None, None), None);
    // A whitespace-only hint is not usable.
    assert_eq!(resolve_fix_error(None, Some("   ".to_string())), None);
}

#[tokio::test]
async fn fix_from_run_returns_the_corrected_graph_on_success() {
    use crate::harness::provider::HarnessModel;
    use crate::harness::workflow_build::WorkflowBuilder;
    use crate::harness::workflow_build::test::{
        NativeCopilotModel, NativeStep, agent_deps, propose_step,
    };

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, id) = hosted_state(&home).await;

    // Wire a builder AND the harness deps `run_copilot` builds its agent
    // from — the route's own capability gate only checks the former, but
    // the copilot needs both (issue #840, PR-2's `HarnessDeps` wiring).
    let model = NativeCopilotModel::scripting(vec![
        propose_step(
            "dropped the unwired step",
            serde_json::json!({
                "name": "Greeter",
                "nodes": [
                    { "id": "start", "kind": "trigger", "name": "Start" },
                    { "id": "done", "kind": "output", "name": "Report" }
                ],
                "edges": [ { "from": "start", "to": "done" } ]
            }),
        ),
        NativeStep::done("Corrected the workflow."),
    ]);
    {
        let mut runtime =
            std::sync::Arc::into_inner(state.registry().remove(&id).expect("registered"))
                .expect("uniquely held in this test");
        let deps = agent_deps(&runtime, model.clone() as std::sync::Arc<dyn HarnessModel>);
        runtime.set_builder(std::sync::Arc::new(WorkflowBuilder::new(
            model as std::sync::Arc<dyn HarnessModel>,
            "test-model",
        )));
        runtime.set_workflow_harness_deps(deps);
        state
            .registry()
            .insert(id.clone(), std::sync::Arc::new(runtime));
    }

    // Seed the workflow the run failed on (hosted mode has no source
    // dir, so it exists only as an overlay created via the API).
    let created = router(state.clone())
        .oneshot(request(
            "POST",
            "/api/v1/company/workflows",
            Some(create_body()),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    journal_run_with_id(
        &state,
        &id,
        "greeter",
        "run-1",
        "the tool `web_search` is not wired on this deployment",
    )
    .await;

    let response = router(state)
        .oneshot(request(
            "POST",
            "/api/v1/company/workflows/greeter/fix-from-run",
            Some(serde_json::json!({ "runId": "run-1" })),
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["automatable"], true, "body: {body}");
    assert_eq!(
        body["workflow"]["id"], "greeter",
        "the fix keeps the workflow's id"
    );
    assert!(
        body["workflow"]["nodes"]
            .as_array()
            .is_some_and(|n| !n.is_empty()),
        "body: {body}"
    );
    assert!(body["readiness"]["ok"].is_boolean(), "body: {body}");
}

#[tokio::test]
async fn fix_from_run_notes_a_dropped_repeatable_declaration() {
    use crate::harness::provider::HarnessModel;
    use crate::harness::workflow_build::WorkflowBuilder;
    use crate::harness::workflow_build::test::{
        NativeCopilotModel, NativeStep, agent_deps, propose_step,
    };

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, id) = hosted_state(&home).await;

    let model = NativeCopilotModel::scripting(vec![
        propose_step(
            "dropped the unwired step",
            serde_json::json!({
                "name": "Greeter",
                "nodes": [
                    { "id": "start", "kind": "trigger", "name": "Start" },
                    { "id": "done", "kind": "output", "name": "Report" }
                ],
                "edges": [ { "from": "start", "to": "done" } ]
            }),
        ),
        NativeStep::done("Corrected the workflow."),
    ]);
    {
        let mut runtime =
            std::sync::Arc::into_inner(state.registry().remove(&id).expect("registered"))
                .expect("uniquely held in this test");
        let deps = agent_deps(&runtime, model.clone() as std::sync::Arc<dyn HarnessModel>);
        runtime.set_builder(std::sync::Arc::new(WorkflowBuilder::new(
            model as std::sync::Arc<dyn HarnessModel>,
            "test-model",
        )));
        runtime.set_workflow_harness_deps(deps);
        state
            .registry()
            .insert(id.clone(), std::sync::Arc::new(runtime));
    }

    // Seed a workflow whose middle node declares `repeatable: false` —
    // the exact declaration `fix_from_run`'s correction cannot carry
    // through the builder's `WorkflowNodeSpec`.
    let mut body = create_body();
    body["nodes"].as_array_mut().unwrap().insert(
        1,
        serde_json::json!({
            "id": "publish",
            "kind": "tool_call",
            "name": "Publish",
            "config": { "slug": "shell", "args": { "command": "./bin/announce" } },
            "repeatable": false
        }),
    );
    body["edges"] = serde_json::json!([
        { "from": "start", "to": "publish" },
        { "from": "publish", "to": "done" }
    ]);
    let created = router(state.clone())
        .oneshot(request("POST", "/api/v1/company/workflows", Some(body)))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    journal_run_with_id(
        &state,
        &id,
        "greeter",
        "run-1",
        "the tool `web_search` is not wired on this deployment",
    )
    .await;

    let response = router(state)
        .oneshot(request(
            "POST",
            "/api/v1/company/workflows/greeter/fix-from-run",
            Some(serde_json::json!({ "runId": "run-1" })),
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let notes = body["notes"].as_array().cloned().unwrap_or_default();
    assert!(
        notes.iter().any(|n| n
            .as_str()
            .is_some_and(|s| s.contains("repeatable") && s.contains("Publish"))),
        "notes must name the dropped repeatable declaration on `Publish`: {body}"
    );
}

#[tokio::test]
async fn fix_from_run_notes_a_dropped_postcondition_declaration() {
    use crate::harness::provider::HarnessModel;
    use crate::harness::workflow_build::WorkflowBuilder;
    use crate::harness::workflow_build::test::{
        NativeCopilotModel, NativeStep, agent_deps, propose_step,
    };

    let home_dir = home();
    let id = CompanyId::new("acme");
    let state = desk_state(home_dir.path()).await;

    let model = NativeCopilotModel::scripting(vec![
        propose_step(
            "dropped the unwired step",
            serde_json::json!({
                "name": "Greeter",
                "nodes": [
                    { "id": "start", "kind": "trigger", "name": "Start" },
                    { "id": "done", "kind": "output", "name": "Report" }
                ],
                "edges": [ { "from": "start", "to": "done" } ]
            }),
        ),
        NativeStep::done("Corrected the workflow."),
    ]);
    {
        let mut runtime =
            std::sync::Arc::into_inner(state.registry().remove(&id).expect("registered"))
                .expect("uniquely held in this test");
        let deps = agent_deps(&runtime, model.clone() as std::sync::Arc<dyn HarnessModel>);
        runtime.set_builder(std::sync::Arc::new(WorkflowBuilder::new(
            model as std::sync::Arc<dyn HarnessModel>,
            "test-model",
        )));
        runtime.set_workflow_harness_deps(deps);
        state
            .registry()
            .insert(id.clone(), std::sync::Arc::new(runtime));
    }

    // Seed a workflow whose middle node is an agent naming the roster
    // teammate `desk_manifest` declares (`ceo`) and carries a
    // `postcondition` — the exact declaration `fix_from_run`'s
    // correction cannot carry through the builder's `WorkflowNodeSpec`.
    let mut body = create_body();
    body["nodes"].as_array_mut().unwrap().insert(
        1,
        serde_json::json!({
            "id": "ask",
            "kind": "agent",
            "name": "Ask",
            "agent": "ceo",
            "postcondition": { "require": "non_empty" }
        }),
    );
    body["edges"] = serde_json::json!([
        { "from": "start", "to": "ask" },
        { "from": "ask", "to": "done" }
    ]);
    let created = router(state.clone())
        .oneshot(request("POST", "/api/v1/company/workflows", Some(body)))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    journal_run_with_id(
        &state,
        &id,
        "greeter",
        "run-1",
        "the tool `web_search` is not wired on this deployment",
    )
    .await;

    let response = router(state)
        .oneshot(request(
            "POST",
            "/api/v1/company/workflows/greeter/fix-from-run",
            Some(serde_json::json!({ "runId": "run-1" })),
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let notes = body["notes"].as_array().cloned().unwrap_or_default();
    assert!(
        notes.iter().any(|n| n
            .as_str()
            .is_some_and(|s| s.contains("postcondition") && s.contains("Ask"))),
        "notes must name the dropped postcondition declaration on `Ask`: {body}"
    );
}

/// Issue #783: the per-workflow copilot's tool-grounding read answers
/// `200 {"slugs":[…],"unwired":[…]}` on **both** scope forms — which also
/// proves the static prefix is wired ahead of the dynamic
/// `/workflows/{wid}` (a route-miss, or a `tool-slugs` swallowed as a
/// `wid`, would not be this shape). The blank tenant grants no tools, so
/// both lists are empty here; the point pinned is the contract shape and
/// that the route exists.
///
/// Issue #874 added `unwired` and it is pinned here as **always present**,
/// because the console reads it unconditionally: a body that omitted the
/// key on a wired host would read as "nothing is unwired" and silently
/// restore the bug this route was narrowed to fix.
#[tokio::test]
async fn tool_slugs_answers_a_slug_array_on_both_scope_forms() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, _id) = hosted_state(&home).await;

    for uri in [
        "/api/v1/company/workflows/tool-slugs",
        "/api/v1/companies/acme/workflows/tool-slugs",
    ] {
        let response = router(state.clone())
            .oneshot(request("GET", uri, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "tool-slugs on {uri}");
        let body = json_body(response).await;
        assert!(
            body["slugs"].is_array(),
            "tool-slugs answers a `slugs` array on {uri}, got: {body}"
        );
        assert!(
            body["unwired"].is_array(),
            "tool-slugs answers an `unwired` array on {uri}, got: {body}"
        );
    }
}

#[tokio::test]
async fn tool_slugs_omits_a_granted_but_unwired_tool_and_says_why() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let id = CompanyId::new("acme");
    let mut manifest = empty_manifest();
    manifest.tools.allow = vec!["search".to_string(), "shell".to_string()];

    let store = FsCompanyStore::new(home.clone());
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: manifest.clone(),
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

    let mut runtime = RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    // `workflow_wiring_deps` pins `search: None` — the deployment half of
    // the repro. Everything else is allowed, so `shell` stays wired.
    runtime.set_workflow_harness_deps(crate::harness::workflow_wiring_deps(
        &runtime,
        None,
        crate::harness::toolbelt::CapabilityFilter::AllowAll,
        None,
    ));
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;

    let response = router(state)
        .oneshot(request("GET", "/api/v1/company/workflows/tool-slugs", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;

    let slugs: Vec<&str> = body["slugs"]
        .as_array()
        .expect("slugs")
        .iter()
        .map(|v| v.as_str().expect("slug"))
        .collect();
    assert!(
        !slugs.contains(&"web_search"),
        "a granted-but-unwired tool is not offered for grounding: {body}"
    );
    assert!(
        slugs.contains(&"shell"),
        "a granted AND wired tool is still offered: {body}"
    );

    let unwired = body["unwired"].as_array().expect("unwired");
    let entry = unwired
        .iter()
        .find(|e| e["slug"] == "web_search")
        .unwrap_or_else(|| panic!("web_search is reported as unwired: {body}"));
    assert_eq!(
        entry["reason"], "searchBackendNotConfigured",
        "the reason distinguishes an unconfigured provider from a filtered \
         capability tier: {body}"
    );
    assert!(
        entry["detail"]
            .as_str()
            .is_some_and(|d| d.contains("search backend")),
        "the prose reason is servable as-is: {body}"
    );
}
