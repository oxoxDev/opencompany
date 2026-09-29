use super::workflows_test_support::hosted_mode::*;
use super::workflows_test_support::*;
use super::*;
use crate::server::router;

/// `PUT …/workflows/{wid}/enabled` round-trips through the API and shows
/// up on the list read (issue #276).
#[tokio::test]
async fn the_enabled_route_toggles_and_the_list_reports_it() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, _id) = hosted_state(&home).await;

    let created = router(state.clone())
        .oneshot(request(
            "POST",
            "/api/v1/company/workflows",
            Some(create_body()),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    // A manual workflow is armed — there is nothing to disarm.
    assert_eq!(json_body(created).await["enabled"], serde_json::json!(true));

    let paused = router(state.clone())
        .oneshot(request(
            "PUT",
            "/api/v1/company/workflows/greeter/enabled",
            Some(serde_json::json!({ "enabled": false })),
        ))
        .await
        .unwrap();
    assert_eq!(paused.status(), StatusCode::OK);
    assert_eq!(json_body(paused).await["enabled"], serde_json::json!(false));

    // And the picker sees it, which is what the console renders from.
    let list = router(state.clone())
        .oneshot(request("GET", "/api/v1/company/workflows", None))
        .await
        .unwrap();
    let body = json_body(list).await;
    let row = body
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == "greeter")
        .expect("listed");
    assert_eq!(row["enabled"], serde_json::json!(false));
    assert_eq!(row["schedule"], serde_json::Value::Null);
    assert_eq!(row["nodeCount"], 2);
    assert_eq!(
        row["editable"],
        serde_json::json!(true),
        "pausing must not change whether the graph can be edited"
    );

    // Back on again.
    let armed = router(state)
        .oneshot(request(
            "PUT",
            "/api/v1/company/workflows/greeter/enabled",
            Some(serde_json::json!({ "enabled": true })),
        ))
        .await
        .unwrap();
    assert_eq!(armed.status(), StatusCode::OK);
    assert_eq!(json_body(armed).await["enabled"], serde_json::json!(true));
}

/// **Issue #276's safety half, over the wire.** Creating a workflow with
/// a schedule answers `enabled: false` on its own response, so a console
/// learns about the disarm from the write it made rather than from a
/// refresh it might not do.
#[tokio::test]
async fn creating_a_scheduled_workflow_answers_switched_off() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, _id) = hosted_state(&home).await;

    let created = router(state.clone())
        .oneshot(request(
            "POST",
            "/api/v1/company/workflows",
            Some(scheduled_create_body()),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    assert_eq!(
        json_body(created).await["enabled"],
        serde_json::json!(false)
    );

    // And the graph read agrees, so it is the store's answer rather than
    // something the write path made up on the way out.
    let listed = router(state.clone())
        .oneshot(request("GET", "/api/v1/company/workflows", None))
        .await
        .unwrap();
    let body = json_body(listed).await;
    let row = body
        .as_array()
        .unwrap()
        .iter()
        .find(|workflow| workflow["id"] == "digest")
        .expect("scheduled workflow is listed");
    assert_eq!(row["schedule"], "0 9 * * *");
    assert_eq!(row["nodeCount"], 2);

    let read = router(state)
        .oneshot(request("GET", "/api/v1/company/workflows/digest", None))
        .await
        .unwrap();
    assert_eq!(json_body(read).await["enabled"], serde_json::json!(false));
}

/// An unknown id is a 404 rather than a silently-created disable entry —
/// a switch that accepted any string would let a typo look like a
/// successful pause.
#[tokio::test]
async fn toggling_an_unknown_workflow_is_not_found() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, _id) = hosted_state(&home).await;

    let response = router(state)
        .oneshot(request(
            "PUT",
            "/api/v1/company/workflows/nowhere/enabled",
            Some(serde_json::json!({ "enabled": false })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// A missing workflow is a missing nested resource, not a missing
/// company. Both variants are 404, so the envelope code pins the
/// distinction that operators and clients actually consume.
#[tokio::test]
async fn reading_an_unknown_workflow_reports_resource_not_found() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, _id) = hosted_state(&home).await;

    let response = router(state)
        .oneshot(request("GET", "/api/v1/company/workflows/ghost", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json_body(response).await;
    assert_eq!(body["code"], "not_found", "{body}");
    assert_eq!(body["error"], "not found: workflow ghost", "{body}");
}

/// A **global-only** workflow — no seed file, no overlay body, just the
/// baseline every company gets — must still be toggleable: it has a
/// schedule to pause exactly like a company-authored one, and
/// `disabled_workflows` (what this route writes) is a separate
/// mechanism from `[globals].disable` (what drops the global outright).
/// Before this, `set_company_workflow_enabled`'s "does this company
/// have a body for `wid`" check only looked at seed files and overlays,
/// so a global-only id read as a bodiless manifest-`enabled` id and the
/// route answered 409 for a graph that plainly exists and runs.
#[tokio::test]
async fn the_enabled_route_toggles_a_global_only_workflow() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, _id) = hosted_state(&home).await;
    let global_id = crate::globals::workflows()[0].id.clone();

    let paused = router(state.clone())
        .oneshot(request(
            "PUT",
            &format!("/api/v1/company/workflows/{global_id}/enabled"),
            Some(serde_json::json!({ "enabled": false })),
        ))
        .await
        .unwrap();
    assert_eq!(paused.status(), StatusCode::OK);
    assert_eq!(json_body(paused).await["enabled"], serde_json::json!(false));

    let list = router(state.clone())
        .oneshot(request("GET", "/api/v1/company/workflows", None))
        .await
        .unwrap();
    let body = json_body(list).await;
    let row = body
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == global_id.as_str())
        .expect("still listed");
    assert_eq!(row["enabled"], serde_json::json!(false));

    // Back on, and still resolvable through `GET …/workflows/{wid}`
    // throughout — pausing a global must not make it unreadable.
    let armed = router(state.clone())
        .oneshot(request(
            "PUT",
            &format!("/api/v1/company/workflows/{global_id}/enabled"),
            Some(serde_json::json!({ "enabled": true })),
        ))
        .await
        .unwrap();
    assert_eq!(armed.status(), StatusCode::OK);
    assert_eq!(json_body(armed).await["enabled"], serde_json::json!(true));

    let read = router(state)
        .oneshot(request(
            "GET",
            &format!("/api/v1/company/workflows/{global_id}"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
}

/// A workflow this company has explicitly dropped via
/// `[globals].disable` no longer exists as far as this company is
/// concerned, so toggling it is the same 404 an unknown id gets — the
/// global-only arm above must not treat a disabled global as having a
/// body.
#[tokio::test]
async fn toggling_a_company_disabled_global_is_not_found() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    let global_id = crate::globals::workflows()[0].id.clone();
    let manifest: CompanyManifest = toml::from_str(&format!(
        "[company]\nname = \"Acme\"\n\n[globals]\ndisable = [\"workflow:{global_id}\"]\n"
    ))
    .unwrap();
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
    // Not `state_over`: it always builds with `empty_manifest()`, which
    // would overwrite this test's `[globals].disable` and silently pass
    // for the wrong reason.
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;

    let response = router(state)
        .oneshot(request(
            "PUT",
            &format!("/api/v1/company/workflows/{global_id}/enabled"),
            Some(serde_json::json!({ "enabled": false })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
    let items = own_rows(&listed);
    assert_eq!(items.len(), 1, "body: {listed}");
    assert_eq!(items[0]["id"], "greeter");
    assert_eq!(items[0]["name"], "Greeter");
}

/// **The issue, at the HTTP boundary.** A run's delivery rows read back
/// after the fact, newest first — which is what survives a console
/// reload, and the reason #228 exists.
#[tokio::test]
async fn run_history_reads_back_newest_first_with_its_rows() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, id) = hosted_state(&home).await;

    journal_run(
        &state,
        &id,
        "digest",
        true,
        vec![undelivered_row("owner")],
        None,
    )
    .await;
    journal_run(&state, &id, "greeter", false, Vec::new(), None).await;

    let response = router(state)
        .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let rows = body["runs"].as_array().expect("array");
    assert_eq!(rows.len(), 2, "body: {body}");

    // Newest first: the history panel leads with the run that just ran.
    assert_eq!(rows[0]["workflowId"], "greeter");
    assert_eq!(rows[0]["scheduled"], false);
    assert_eq!(rows[1]["workflowId"], "digest");
    assert_eq!(rows[1]["scheduled"], true);

    // The delivery row — including the `detail` that names the fix, and
    // the `target`, which the manual-run response already ships to this
    // same console.
    let deliveries = rows[1]["deliveries"].as_array().expect("deliveries");
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0]["node"], "owner");
    assert_eq!(deliveries[0]["status"], "skipped");
    assert_eq!(deliveries[0]["target"], "ada@example.com");
    assert!(
        deliveries[0]["detail"]
            .as_str()
            .unwrap()
            .contains("never written")
    );
    // A run that finished carries no `error` key at all.
    assert!(rows[0].get("error").is_none(), "{body}");
}

/// **Issue #981, part 2, at the HTTP boundary.** The history's own
/// reading of the three runs the issue distinguishes.
///
/// Journaled, then read back through the real fold — so this also pins
/// that the verdict is derived on the read. None of these rows was
/// written with one, which is exactly the situation every run already in
/// a company's history is in.
#[tokio::test]
async fn the_history_scores_a_dropped_report_without_calling_the_run_a_failure() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, id) = hosted_state(&home).await;

    // Oldest first — the fold reverses, so the assertions below read
    // newest first.
    journal_run(
        &state,
        &id,
        "dropped",
        true,
        vec![undelivered_row("owner")],
        None,
    )
    .await;
    journal_run(&state, &id, "clean", false, vec![sent_row("owner")], None).await;
    journal_run(
        &state,
        &id,
        "broke_and_dropped",
        false,
        vec![undelivered_row("owner")],
        Some("node `draft` errored"),
    )
    .await;

    let response = router(state)
        .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let rows = body["runs"].as_array().expect("array");
    assert_eq!(rows.len(), 3, "body: {body}");

    // The more serious fact first: a run that broke mid-graph AND did
    // not deliver reports the break, not the drop. Reversing these two
    // arms would hide a real failure behind a delivery problem.
    assert_eq!(rows[0]["workflowId"], "broke_and_dropped", "{body}");
    assert_eq!(rows[0]["verdict"], "failed", "{body}");

    // A run that delivered fine is unchanged.
    assert_eq!(rows[1]["workflowId"], "clean", "{body}");
    assert_eq!(rows[1]["verdict"], "ok", "{body}");

    // The defect: every node `ok`, no error — and the report is gone.
    assert_eq!(rows[2]["workflowId"], "dropped", "{body}");
    assert_eq!(rows[2]["verdict"], "undelivered", "{body}");
    // Not promoted to a failure, and nothing else on the row moved.
    assert!(rows[2].get("error").is_none(), "{body}");
    assert!(rows[2].get("cancelled").is_none(), "{body}");
}

/// Every row carries a verdict, including one still in flight — the
/// field is unconditional precisely so no reader has to fall back to
/// re-deriving it from the six fields around it.
#[tokio::test]
async fn a_run_still_in_flight_is_scored_running_not_ok() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, id) = hosted_state(&home).await;

    // A start with no finish, and a run id the supervisor knows nothing
    // about would be settled by the #1009 cross-check — so this test
    // registers it, which is what a genuinely live run looks like.
    let runtime = state.registry().get(&id).expect("registered");
    // `_guard` must outlive the read: dropping it deregisters the run,
    // and the #1009 cross-check would then settle it as interrupted.
    let (ctx, _guard) = runtime
        .run_supervisor()
        .begin("digest", false)
        .expect("register the run");
    runtime
        .events()
        .append(
            &id,
            CompanyEvent::WorkflowRunStarted {
                workflow_id: "digest".to_string(),
                run_id: ctx.run_id.clone(),
                scheduled: false,
                started_by: None,
                resume_semantic: None,
            },
        )
        .await
        .expect("append");

    let response = router(state.clone())
        .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let rows = body["runs"].as_array().expect("array");
    assert_eq!(rows.len(), 1, "body: {body}");
    assert_eq!(rows[0]["running"], true, "{body}");
    assert_eq!(rows[0]["verdict"], "running", "{body}");
}

/// Issue #596: the run-output route serves a stored snapshot (200) and
/// 404s a run with none. Runs in the DEFAULT lane, which also proves the
/// route + store are present with the openhuman-gated *writer* compiled
/// out — the default build reads back exactly what was written and 404s
/// otherwise.
#[tokio::test]
async fn run_output_route_serves_a_snapshot_and_404s_an_unknown_run() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, id) = hosted_state(&home).await;

    let runtime = state.registry().get(&id).expect("registered");
    let record = crate::ports::WorkflowRunOutputRecord {
        run_id: "run-xyz".to_string(),
        workflow_id: "greeter".to_string(),
        at_millis: 123,
        nodes: serde_json::json!({
            "writer": { "items": [{ "json": { "text": "the draft" } }] }
        }),
        truncated: false,
        partial: false,
    };
    runtime
        .workflow_run_outputs()
        .put_run_output(&id, &record)
        .await
        .expect("store write");

    // 200 with the record for a stored run.
    let response = router(state.clone())
        .oneshot(request(
            "GET",
            "/api/v1/company/workflows/runs/run-xyz/output",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["runId"], "run-xyz", "{body}");
    assert_eq!(body["workflowId"], "greeter");
    assert_eq!(body["truncated"], false);
    assert_eq!(
        body["nodes"]["writer"]["items"][0]["json"]["text"], "the draft",
        "the durable per-node output must round-trip through the route: {body}"
    );

    // 404 for a run with no captured output.
    let missing = router(state)
        .oneshot(request(
            "GET",
            "/api/v1/company/workflows/runs/nope/output",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}
