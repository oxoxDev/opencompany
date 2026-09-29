use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

fn manifest() -> CompanyManifest {
    toml::from_str(
        "[company]\nname = \"Acme\"\n[[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n[policy]\nmode = \"full\"\n",
    )
    .unwrap()
}

async fn state(home: &std::path::Path) -> AppState {
    use crate::ports::CompanyStore;
    let id = CompanyId::new("acme");
    FsCompanyStore::new(home.to_path_buf())
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: manifest(),
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
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

/// Writes a bounced To-do card straight through the store, bypassing
/// `upsert_task`'s dispatch/plan edges — the seed only needs the row to
/// exist with a `bounced` chip already on it, not to fire either trigger.
async fn seed_bounced_card(state: &AppState, id: &str) {
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();
    runtime
        .tasks()
        .upsert(
            &company,
            &crate::ports::tasks::TaskRecord {
                id: id.to_string(),
                title: crate::ports::tasks::TaskTitle::authored("Draft the launch note"),
                note: None,
                column: crate::ports::tasks::COLUMN_TODO.to_string(),
                priority: "medium".to_string(),
                assignee: String::new(),
                updated_at_millis: 1,
                origin: None,
                parent_task_id: None,
                output: None,
                plan: None,
                planning_attempts: Vec::new(),
                deliverable: crate::ports::tasks::TaskDeliverable::Once,
                workflow_proposal: None,
                origin_run_id: None,
                origin_workflow_id: None,
                origin_message_seq: None,
                bounced: Some("a previous run's dispatch failed".to_string()),
            },
        )
        .await
        .unwrap();
}

async fn patch_column(state: &AppState, id: &str, column: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("PATCH")
        .uri(format!("/api/v1/company/tasks/{id}"))
        .header("content-type", "application/json")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::from(json!({"column": column}).to_string()))
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    (status, value)
}

#[tokio::test]
async fn patching_a_bounced_card_straight_to_done_returns_the_cleared_state() {
    let home = tempfile::tempdir().unwrap();
    let state = state(home.path()).await;
    seed_bounced_card(&state, "card-1").await;

    let (status, body) = patch_column(&state, "card-1", "done").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("bounced").is_none(),
        "the PATCH response still carries the stale bounce chip: {body}"
    );

    // The response is not just accidentally right while the persisted row
    // stays wrong — the store must agree too.
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();
    let stored = runtime
        .tasks()
        .list(&company)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.id == "card-1")
        .unwrap();
    assert!(
        stored.bounced.is_none(),
        "the stored row should also have cleared the chip"
    );
}
