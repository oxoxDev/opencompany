use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

const MANIFEST: &str = "[company]\nname = \"Acme\"\n\
     [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n";

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-presence-")
        .tempdir()
        .expect("tempdir")
}

async fn state(home: &std::path::Path) -> AppState {
    let manifest: CompanyManifest = toml::from_str(MANIFEST).unwrap();
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
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
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    body: Option<Value>,
    signed_in: bool,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/api/v1/company{path}"))
        .header("content-type", "application/json");
    if signed_in {
        request = request.header("cookie", crate::server::test_support::fixed_cookie("acme"));
    }
    let request = match body {
        Some(value) => request.body(Body::from(value.to_string())).unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn a_heartbeat_makes_the_caller_visible_and_a_disconnect_clears_them() {
    let home = home();
    let state = state(home.path()).await;

    let (status, listed) = call(&state, "GET", "/presence", None, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["people"].as_array().unwrap().len(), 0);

    let (status, _) = call(
        &state,
        "PUT",
        "/presence",
        Some(json!({"status": "online"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, listed) = call(&state, "GET", "/presence", None, true).await;
    let people = listed["people"].as_array().expect("people");
    assert_eq!(people.len(), 1);
    assert_eq!(people[0]["status"], "online");

    let (status, _) = call(&state, "DELETE", "/presence", None, true).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, listed) = call(&state, "GET", "/presence", None, true).await;
    assert_eq!(listed["people"].as_array().unwrap().len(), 0);
}

/// The impersonation guard, and the reason no route here takes a `userId`:
/// a body naming somebody else must not be able to move their dot. An
/// unknown key is ignored by serde, so the announcement lands under the
/// *caller* — which is the safe outcome, and the one asserted here.
#[tokio::test]
async fn a_body_cannot_name_somebody_else_as_the_subject() {
    let home = home();
    let state = state(home.path()).await;

    let (status, _) = call(
        &state,
        "PUT",
        "/presence",
        Some(json!({"status": "away", "userId": "somebody-else"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, listed) = call(&state, "GET", "/presence", None, true).await;
    let people = listed["people"].as_array().expect("people");
    assert_eq!(people.len(), 1, "one dot moved, not two");
    assert_ne!(
        people[0]["userId"], "somebody-else",
        "the subject is the session, never the body"
    );
    assert_eq!(people[0]["status"], "away");
}

#[tokio::test]
async fn an_unknown_status_is_refused_rather_than_stored() {
    let home = home();
    let state = state(home.path()).await;
    let (status, _) = call(
        &state,
        "PUT",
        "/presence",
        Some(json!({"status": "invisible"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_typing_ping_is_accepted_and_stores_nothing() {
    let home = home();
    let state = state(home.path()).await;
    let (status, _) = call(
        &state,
        "POST",
        "/chat/typing",
        Some(json!({"chatId": "engineering"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Typing is not presence: a ping must not make somebody "here".
    let (_, listed) = call(&state, "GET", "/presence", None, true).await;
    assert_eq!(listed["people"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn a_typing_ping_needs_a_channel() {
    let home = home();
    let state = state(home.path()).await;
    let (status, body) = call(
        &state,
        "POST",
        "/chat/typing",
        Some(json!({"chatId": "  "})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_request");
}

/// A machine credential has no person to be, so it has no dot to own and
/// nothing to say it is typing. Same rule the read-state routes apply.
#[tokio::test]
async fn a_caller_with_no_person_behind_it_is_refused_everywhere() {
    let home = home();
    let state = state(home.path()).await;
    for (method, path, body) in [
        ("GET", "/presence", None),
        ("PUT", "/presence", Some(json!({"status": "online"}))),
        ("DELETE", "/presence", None),
        (
            "POST",
            "/chat/typing",
            Some(json!({"chatId": "engineering"})),
        ),
    ] {
        let (status, body) = call(&state, method, path, body, false).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} must refuse a caller with no person"
        );
        assert_eq!(body["code"], "unauthorized", "{method} {path}");
    }
}

/// Announcing `offline` must behave exactly like `DELETE`: the caller
/// disappears from `GET /presence` at once, not just after its lease
/// lapses. See the module header's "the wire and the registry must agree"
/// note on `announce`.
#[tokio::test]
async fn announcing_offline_disconnects_like_a_delete_would() {
    let home = home();
    let state = state(home.path()).await;

    let (status, _) = call(
        &state,
        "PUT",
        "/presence",
        Some(json!({"status": "online"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, listed) = call(&state, "GET", "/presence", None, true).await;
    assert_eq!(listed["people"].as_array().unwrap().len(), 1);

    let (status, _) = call(
        &state,
        "PUT",
        "/presence",
        Some(json!({"status": "offline"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, listed) = call(&state, "GET", "/presence", None, true).await;
    assert_eq!(
        listed["people"].as_array().unwrap().len(),
        0,
        "an offline announcement must drop the lease, not store it"
    );
}

/// The multi-tab fix: two consoles for the same signed-in person, and
/// closing one must not disconnect the other.
#[tokio::test]
async fn closing_one_tab_leaves_the_other_open() {
    let home = home();
    let state = state(home.path()).await;

    for console in ["tab-1", "tab-2"] {
        let (status, _) = call(
            &state,
            "PUT",
            "/presence",
            Some(json!({"status": "online", "consoleId": console})),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let (_, listed) = call(&state, "GET", "/presence", None, true).await;
    assert_eq!(listed["people"].as_array().unwrap().len(), 1);

    let (status, _) = call(&state, "DELETE", "/presence?consoleId=tab-1", None, true).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, listed) = call(&state, "GET", "/presence", None, true).await;
    assert_eq!(
        listed["people"].as_array().unwrap().len(),
        1,
        "tab-2 is still open"
    );

    let (status, _) = call(&state, "DELETE", "/presence?consoleId=tab-2", None, true).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, listed) = call(&state, "GET", "/presence", None, true).await;
    assert_eq!(listed["people"].as_array().unwrap().len(), 0);
}

/// Closing the console that was carrying the aggregate must downgrade the
/// person to `away`, not disappear them to `offline` — the away tab is
/// still open, and every viewer should see that immediately rather than
/// waiting out its next heartbeat.
#[tokio::test]
async fn closing_the_more_present_tab_downgrades_rather_than_disconnects() {
    let home = home();
    let state = state(home.path()).await;

    let (status, _) = call(
        &state,
        "PUT",
        "/presence",
        Some(json!({"status": "online", "consoleId": "tab-online"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &state,
        "PUT",
        "/presence",
        Some(json!({"status": "away", "consoleId": "tab-away"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, listed) = call(&state, "GET", "/presence", None, true).await;
    let people = listed["people"].as_array().expect("people");
    assert_eq!(people.len(), 1);
    assert_eq!(people[0]["status"], "online");

    let (status, _) = call(
        &state,
        "DELETE",
        "/presence?consoleId=tab-online",
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, listed) = call(&state, "GET", "/presence", None, true).await;
    let people = listed["people"].as_array().expect("people");
    assert_eq!(people.len(), 1, "the away tab keeps them present");
    assert_eq!(people[0]["status"], "away");
}
