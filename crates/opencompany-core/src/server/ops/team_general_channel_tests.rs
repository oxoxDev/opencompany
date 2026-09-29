use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::ports::CompanyStore;
use crate::ports::types::{CompanyEvent, CompanyId, EventSeq};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

const ROSTER: &str = "[company]\nname = \"Acme\"\n\
     [[agent]]\nid = \"analyst\"\nrole = \"Analyst\"\n\
     [[agent]]\nid = \"writer\"\nrole = \"Writer\"\n";

async fn state(home: &std::path::Path) -> AppState {
    let id = CompanyId::new("acme");
    let runtime = RuntimeBuilder::new(home.to_path_buf(), toml::from_str(ROSTER).unwrap())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", crate::server::test_support::fixed_cookie("acme"));
    let request = match body {
        Some(value) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&value).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn general_members(home: &std::path::Path) -> Vec<String> {
    FsCompanyStore::new(home.to_path_buf())
        .load(&CompanyId::new("acme"))
        .await
        .unwrap()
        .unwrap()
        .general_channel
        .members
}

async fn general_changes(state: &AppState) -> Vec<(Vec<String>, Vec<String>)> {
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    runtime
        .events()
        .read_from(&CompanyId::new("acme"), EventSeq::new(0), usize::MAX)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|stored| match stored.event {
            CompanyEvent::DeskMembersChanged {
                desk_id,
                added,
                removed,
                ..
            } if desk_id == "general" => Some((added, removed)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn adding_a_teammate_seats_it_in_general_and_removing_it_unseats_it() {
    let home = tempfile::tempdir().unwrap();
    let state = state(home.path()).await;
    let before = general_members(home.path()).await;
    assert!(before.starts_with(&["analyst".to_string(), "writer".to_string()]));

    let (status, body) = send(
        &state,
        "POST",
        "/api/v1/company/team",
        Some(json!({ "name": "Designer", "role": "Designer" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let hired = body["id"].as_str().unwrap().to_string();
    let mut expected = before.clone();
    expected.push(hired.clone());
    assert_eq!(general_members(home.path()).await, expected);

    let (status, _) = send(
        &state,
        "DELETE",
        &format!("/api/v1/company/team/{hired}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(general_members(home.path()).await, before);

    assert_eq!(
        general_changes(&state).await,
        vec![(vec![hired.clone()], Vec::new()), (Vec::new(), vec![hired]),]
    );
}

#[tokio::test]
async fn retiring_a_blueprint_teammate_unseats_it_from_general() {
    let home = tempfile::tempdir().unwrap();
    let state = state(home.path()).await;

    let mut expected = general_members(home.path()).await;
    expected.retain(|id| id != "writer");

    let (status, _) = send(&state, "DELETE", "/api/v1/company/team/writer", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert_eq!(general_members(home.path()).await, expected);
    assert_eq!(
        general_changes(&state).await,
        vec![(Vec::new(), vec!["writer".to_string()])]
    );
}
