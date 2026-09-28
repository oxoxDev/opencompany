use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::company::CompanyManifest;
use crate::company::steer::InflightKind;
use crate::ports::tasks::TaskTitle;
use crate::ports::types::{CompanyId, CompanyRecord, EventSeq};
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

/// A company with one run already in flight under the key `active`, so the
/// steer route reaches its accept path. The caller must hold the returned
/// registration guard: dropping it deregisters the run.
async fn state_with_inflight_run(
    home: &std::path::Path,
) -> (AppState, crate::company::steer::RegistrationGuard) {
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
    let guard = runtime.steer().register(
        &id,
        InflightEntry {
            key: "active".into(),
            task_id: Some("active".into()),
            kind: InflightKind::Task,
            title: "Active".into(),
            agent_id: "ceo".into(),
            started_at_millis: 1,
            pending_action: None,
        },
    );
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    (state, guard)
}

async fn steer(state: &AppState, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/company/tasks/active/steer")
        .header("content-type", "application/json")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

// ── Deleting a card that is running (issue #984) ─────────────────────────

/// Puts a card on the board so a delete has something to remove.
async fn seed_card(state: &AppState, id: &str) {
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();
    runtime
        .tasks()
        .upsert(
            &company,
            &crate::ports::tasks::TaskRecord {
                id: id.to_string(),
                title: TaskTitle::authored("Draft the launch note"),
                note: None,
                column: crate::ports::tasks::COLUMN_IN_PROGRESS.to_string(),
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
                bounced: None,
            },
        )
        .await
        .unwrap();
}

async fn delete_card(state: &AppState, id: &str) -> StatusCode {
    let request = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/company/tasks/{id}"))
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::empty())
        .unwrap();
    router(state.clone())
        .oneshot(request)
        .await
        .unwrap()
        .status()
}

async fn board_has(state: &AppState, id: &str) -> bool {
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();
    runtime
        .tasks()
        .list(&company)
        .await
        .unwrap()
        .iter()
        .any(|task| task.id == id)
}

/// **A running card cannot be deleted out from under its turn.**
///
/// The settle path writes the card back from the harness's in-memory clone,
/// so a delete that lands mid-turn does not remove the card — it removes it
/// until the turn finishes and then gets it back, in `in_review`/`done`,
/// after every chat chip naming it has already gone. That is a card on the
/// board that nothing can reach, which is the failure #984 is about.
///
/// The card staying on the board is asserted as well as the status: a `409`
/// that had already deleted the row would be worse than no check at all.
#[tokio::test]
async fn deleting_a_running_card_is_refused_and_leaves_it_on_the_board() {
    let home = tempfile::tempdir().unwrap();
    let (state, _guard) = state_with_inflight_run(home.path()).await;
    seed_card(&state, "active").await;

    assert_eq!(delete_card(&state, "active").await, StatusCode::CONFLICT);
    assert!(
        board_has(&state, "active").await,
        "the refusal must not have deleted it anyway"
    );
}

/// And the refusal is aimed at the running card, not at deletes in general.
///
/// Without this, a guard that refused *every* delete would satisfy the test
/// above — the board's own delete would be broken and the suite would still
/// be green.
#[tokio::test]
async fn deleting_a_card_that_is_not_running_still_works() {
    let home = tempfile::tempdir().unwrap();
    let (state, _guard) = state_with_inflight_run(home.path()).await;
    seed_card(&state, "idle").await;

    assert_eq!(delete_card(&state, "idle").await, StatusCode::NO_CONTENT);
    assert!(!board_has(&state, "idle").await);
}

/// The instruction the run's audit event recorded, if any.
async fn journaled_redirect(state: &AppState) -> Option<String> {
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();
    runtime
        .events()
        .read_from(&company, EventSeq::new(0), usize::MAX)
        .await
        .unwrap()
        .into_iter()
        .find_map(|stored| match stored.event {
            CompanyEvent::TaskSteered { instruction, .. } => instruction,
            _ => None,
        })
}

#[tokio::test]
async fn an_over_length_redirect_is_refused_naming_the_bound() {
    let home = tempfile::Builder::new()
        .prefix("opencompany-steer-")
        .tempdir()
        .unwrap();
    let (state, _inflight) = state_with_inflight_run(home.path()).await;

    let too_long = "a".repeat(MAX_REDIRECT_CHARS + 1);
    let (status, body) = steer(
        &state,
        json!({"action": "redirect", "instruction": too_long}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = body["error"].as_str().unwrap_or_default();
    assert!(
        message.contains(&MAX_REDIRECT_CHARS.to_string()),
        "the refusal names the limit: {message}"
    );
    assert!(
        message.contains(&(MAX_REDIRECT_CHARS + 1).to_string()),
        "the refusal names the actual length: {message}"
    );
    assert!(
        journaled_redirect(&state).await.is_none(),
        "a refused redirect never reaches the run or the audit trail"
    );
}

#[tokio::test]
async fn an_over_length_multibyte_redirect_is_refused_not_split() {
    let home = tempfile::Builder::new()
        .prefix("opencompany-steer-")
        .tempdir()
        .unwrap();
    let (state, _inflight) = state_with_inflight_run(home.path()).await;

    // Every character is 2 bytes, so a byte-indexed bound would land
    // mid-codepoint and panic. The route counts characters.
    let too_long = "é".repeat(MAX_REDIRECT_CHARS + 1);
    let (status, body) = steer(
        &state,
        json!({"action": "redirect", "instruction": too_long}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains(&(MAX_REDIRECT_CHARS + 1).to_string()),
        "length is counted in characters, not bytes: {body}"
    );
}

#[tokio::test]
async fn an_at_limit_redirect_is_accepted_verbatim() {
    let home = tempfile::Builder::new()
        .prefix("opencompany-steer-")
        .tempdir()
        .unwrap();
    let (state, _inflight) = state_with_inflight_run(home.path()).await;

    let exact = "a".repeat(MAX_REDIRECT_CHARS);
    let (status, _) = steer(&state, json!({"action": "redirect", "instruction": exact})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        journaled_redirect(&state).await.as_deref(),
        Some(exact.as_str()),
        "an at-limit instruction reaches the run whole — no cut, no marker"
    );
}
