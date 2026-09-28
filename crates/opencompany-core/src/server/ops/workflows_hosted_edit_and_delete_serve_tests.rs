use super::workflows_test_support::hosted_mode::*;
use super::*;
use crate::server::router;

/// The write verbs are reachable under the platform scope form too, not
/// just the prosumer alias.
#[tokio::test]
async fn edit_and_delete_serve_both_scope_forms() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, _id) = hosted_state(&home).await;
    let version = create_greeter(&state).await;

    let response = router(state.clone())
        .oneshot(request(
            "PUT",
            "/api/v1/companies/acme/workflows/greeter",
            Some(edited_body(Some(&version))),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // The edit moved the token; delete with the one it just returned.
    let next = json_body(response).await["version"]
        .as_str()
        .expect("edit returns a fresh token")
        .to_string();

    let response = router(state)
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/companies/acme/workflows/greeter?expectedVersion={next}"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// A manifest-`enabled` id with no saved graph is listed but NOT
/// editable — there is nothing to replace or remove, and the console
/// must not offer a button that can only 409.
#[tokio::test]
async fn a_bodiless_enabled_id_is_listed_but_not_editable() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let store = FsCompanyStore::new(home.clone());
    let id = CompanyId::new("acme");
    let mut manifest = empty_manifest();
    manifest.workflows.enabled.push("legacy".to_string());
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
    // The runtime carries its own manifest — the enabled list the list
    // route reads comes from there, not from the record we just saved.
    let runtime = RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state
        .registry()
        .insert(id.clone(), std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;

    let response = router(state.clone())
        .oneshot(request("GET", "/api/v1/company/workflows", None))
        .await
        .unwrap();
    let items = json_body(response).await;
    let legacy = items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "legacy")
        .expect("listed under its id");
    assert_eq!(legacy["editable"], false, "{items}");

    // And the host agrees when actually asked to delete it. A token is
    // required (issue #1013), so send one; the body-less id is a 409
    // before the token is ever compared.
    let response = router(state)
        .oneshot(request(
            "DELETE",
            "/api/v1/company/workflows/legacy?expectedVersion=deadbeef",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

/// **The decisive case (issue #1009, path B).** A run whose start was
/// journaled but whose finish never landed — and whose id is absent from
/// the live run set — is settled by `list_runs` itself, between boots.
///
/// Two halves, both asserted: the returned row is `running: false` +
/// `INTERRUPTED_BY_RESTART` (so this very response is self-consistent),
/// AND a synthetic finish is **durably appended** so the next read folds
/// it settled and the boot sweep has nothing left to do. Before the fix
/// the row read `running: true` and nothing was appended.
#[tokio::test]
async fn a_run_absent_from_the_live_set_is_settled_by_the_read() {
    let home_dir = home();
    let (state, _store, id) = hosted_state(home_dir.path()).await;

    // A start with no finish. Nothing is registered on the supervisor,
    // so this id is absent from `live()`: the process that owned it went
    // away without journaling a finish.
    journal_start(&state, &id, "digest", "run-dead", true).await;

    let response = router(state.clone())
        .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
        .await
        .unwrap();
    let body = json_body(response).await;
    assert_eq!(body["runs"].as_array().unwrap().len(), 1);
    // `running` is skip-serialized when false, so a settled row simply
    // omits it — assert it is not `true` rather than equal to `false`.
    assert_ne!(
        body["runs"][0]["running"], true,
        "an absent run is settled on the read, not left spinning: {body}"
    );
    assert_eq!(
        body["runs"][0]["error"],
        crate::runtime::workflow_outcome::INTERRUPTED_BY_RESTART
    );

    // Durable half: exactly one synthetic finish is now in the journal.
    let finishes = finishes_for(&state, &id, "run-dead").await;
    assert_eq!(finishes.len(), 1, "exactly one synthetic finish appended");
    assert_eq!(
        finishes[0].0.as_deref(),
        Some(crate::runtime::workflow_outcome::INTERRUPTED_BY_RESTART)
    );
    assert!(!finishes[0].1, "a host-restart settle is not a cancel");
}

/// **The mandatory negative (issue #1009, rebuild/clean guard).** A run
/// the current process is genuinely running is registered on the
/// supervisor, so its id is in `live()` — and `list_runs` must leave it
/// alone: the row stays `running: true` and **nothing** is appended.
///
/// This is what keeps the cross-check keyed strictly on `live()`
/// membership rather than on "has no finish yet", which would stamp
/// `INTERRUPTED_BY_RESTART` on a run still walking its graph and then
/// contradict its real finish. The guard is held across the read so the
/// registration stays live for the whole request.
#[tokio::test]
async fn a_run_in_the_live_set_is_left_running() {
    let home_dir = home();
    let (state, _store, id) = hosted_state(home_dir.path()).await;

    let runtime = state.registry().get(&id).expect("registered");
    // Register a live run and HOLD its guard: its id is now in `live()`.
    let (ctx, _guard) = runtime
        .run_supervisor()
        .begin("digest", true)
        .expect("under the default cap");
    journal_start(&state, &id, "digest", &ctx.run_id, true).await;

    let response = router(state.clone())
        .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
        .await
        .unwrap();
    let body = json_body(response).await;
    assert_eq!(body["runs"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["runs"][0]["running"], true,
        "a run the process is running must not be settled from under it"
    );

    assert!(
        finishes_for(&state, &id, &ctx.run_id).await.is_empty(),
        "a live run gets no synthetic finish appended"
    );
}
