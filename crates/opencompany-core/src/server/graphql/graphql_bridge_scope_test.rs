use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::server::test_support::{fixed_cookie, seed_fixed_admin};
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

/// Two companies on one host, each with its own signed-in admin — the shape
/// a per-page scope question needs, since a single-company host cannot tell
/// "narrowed correctly" from "there was nothing else to reach".
async fn state_with_two_companies(home: &std::path::Path) -> AppState {
    let store = FsCompanyStore::new(home.to_path_buf());
    let state = AppState::new(AppConfig::default()).with_home(home.to_path_buf());
    for name in ["acme", "globex"] {
        let id = CompanyId::new(name);
        let manifest = super::graphql_test_support_1::manifest();
        store
            .save(&CompanyRecord {
                general_channel: Default::default(),
                overlay_retired_agents: Vec::new(),
                overlay_agent_edits: Vec::new(),
                id: id.clone(),
                manifest: manifest.clone(),
                ledger: Vec::new(),
                lifecycle: "running".to_string(),
                overlay_agents: Vec::new(),
                overlay_desk_members: Vec::new(),
                overlay_desk_hive: Vec::new(),
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
        state.registry().insert(id, Arc::new(runtime));
        seed_fixed_admin(&state, name).await;
    }
    state
}

async fn query_as(app: &axum::Router, cookie: &str, body: &str) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/graphql")
                .header("content-type", "application/json")
                .header("cookie", cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// AUTH. A bridged request carries the operator's full session, so what a
/// page may do is decided entirely here: it may not write, because the
/// schema has no mutation root, and it may not read another company,
/// because [`GqlAuth::authorize`] refuses a user principal any company but
/// their own.
///
/// The mutation is a *syntactically valid* document, so the only thing that
/// can refuse it is the absent mutation root — a parse error would prove
/// nothing.
#[tokio::test]
async fn a_bridged_request_can_neither_write_nor_read_another_company() {
    let home_dir = tempfile::tempdir().unwrap();
    let state = state_with_two_companies(home_dir.path()).await;
    let app = router(state);
    let acme = fixed_cookie("acme");

    let value = query_as(&app, &acme, r#"{"query":"mutation { __typename }"}"#).await;
    assert!(
        value["data"].is_null(),
        "a bridged page executed a mutation: {value}"
    );
    let errors = value["errors"].as_array().expect("an errors array");
    assert!(
        errors.iter().any(|e| e["message"]
            .as_str()
            .is_some_and(|m| m.to_ascii_lowercase().contains("mutation"))),
        "the refusal must be the absent mutation root, got {value}"
    );

    let value = query_as(
        &app,
        &acme,
        r#"{"query":"{ company(id: \"globex\") { id } }"}"#,
    )
    .await;
    assert!(
        value["data"]["company"].is_null(),
        "a bridged page read another company: {value}"
    );
    assert_eq!(
        value["errors"][0]["extensions"]["code"], "forbidden",
        "reaching another company must be refused as forbidden, got {value}"
    );

    let value = query_as(&app, &acme, r#"{"query":"{ companies { id } }"}"#).await;
    let ids: Vec<&str> = value["data"]["companies"]
        .as_array()
        .expect("a companies array")
        .iter()
        .map(|c| c["id"].as_str().expect("an id"))
        .collect();
    assert_eq!(
        ids,
        vec!["acme"],
        "the roster must not disclose that another company exists: {value}"
    );
}

/// CONC. The schema is built once at startup and shared by every request
/// ([`build_schema`]); only the principal is per-request, injected as
/// request data by [`graphql_handler`]. So a bridged page opened by one
/// operator and a console open as another are executing against the same
/// object at the same time, and the thing that must not be shared is the
/// principal.
///
/// Interleaved on purpose: alternating callers, all in flight together,
/// each of which must see only its own company.
#[tokio::test]
async fn concurrent_requests_never_borrow_another_callers_principal() {
    let home_dir = tempfile::tempdir().unwrap();
    let state = state_with_two_companies(home_dir.path()).await;
    let app = router(state);

    let pending = (0..24).map(|i| {
        let company = if i % 2 == 0 { "acme" } else { "globex" };
        let app = app.clone();
        async move {
            let value = query_as(
                &app,
                &fixed_cookie(company),
                r#"{"query":"{ companies { id } }"}"#,
            )
            .await;
            (company, value)
        }
    });

    for (company, value) in futures::future::join_all(pending).await {
        let ids: Vec<&str> = value["data"]["companies"]
            .as_array()
            .expect("a companies array")
            .iter()
            .map(|c| c["id"].as_str().expect("an id"))
            .collect();
        assert_eq!(
            ids,
            vec![company],
            "a concurrent request answered under another caller's principal: {value}"
        );
    }
}
