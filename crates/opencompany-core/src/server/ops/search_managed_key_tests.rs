//! Route tests for the managed-search company key: `PUT
//! …/search/providers/managed/key` writes and clears the company's own
//! TinyHumans copy without ever creating a `managed` provider row (split out
//! of `search_tests.rs`).

use super::*;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::ports::types::CompanyId;

/// A running company whose manifest grants `search` (or does not).
async fn state_with_company(home: &std::path::Path, grant_search: bool) -> AppState {
    use crate::ports::CompanyStore;
    use crate::ports::types::CompanyRecord;

    let id = CompanyId::new("acme");
    let allow = if grant_search {
        "\n[tools]\nallow = [\"search\"]\n"
    } else {
        "\n[tools]\nallow = [\"*\"]\n"
    };
    let manifest: crate::company::CompanyManifest = ::toml::from_str(&format!(
        "[company]\nname = \"Acme\"\n[[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n[policy]\nmode = \"full\"\n{allow}"
    ))
    .expect("manifest");
    crate::store::FsCompanyStore::new(home.to_path_buf())
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
            overlay_tool_grants: None,
            overlay_desk_tools: Default::default(),
            overlay_desks: Vec::new(),
            overlay_workflows: Vec::new(),
            overlay_budgets: Vec::new(),
            overlay_policy: None,
            disabled_workflows: Vec::new(),
            template_provenance: None,
            setup: None,
            name_confirmed: false,
            activation_completed_at: None,
            created_at_millis: None,
        })
        .await
        .expect("save");

    let runtime = crate::runtime::RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .expect("runtime");
    let state = AppState::new(crate::AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    state
}

async fn call(
    state: &AppState,
    method: &str,
    uri: &str,
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie);
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .expect("request");
    let response = crate::server::router(state.clone())
        .oneshot(request)
        .await
        .expect("routed");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn managed_company_key_can_be_replaced_without_becoming_a_provider_row() {
    let home = ::tempfile::tempdir().expect("tempdir");
    let state = state_with_company(home.path(), true).await;
    let admin = crate::server::test_support::seed_admin(&state, "acme").await;

    let (status, body) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search/providers/managed/key",
        &admin,
        Some(json!({"apiKey": "th-company-search-key"})),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["managedConfigured"], true, "{body}");
    assert_eq!(body["managedKeyConfigured"], true, "{body}");
    assert_eq!(body["providers"], json!([]), "{body}");
    assert!(!body.to_string().contains("th-company-search-key"));

    let (status, body) = call(
        &state,
        "PUT",
        "/api/v1/companies/acme/search/providers/managed/key",
        &admin,
        Some(json!({"apiKey": "", "confirmInUse": true})),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["managedKeyConfigured"], false, "{body}");
    assert_eq!(body["providers"], json!([]), "{body}");
}
