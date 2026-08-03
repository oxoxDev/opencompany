//! End-to-end tests for `POST …/auth/handoff`.
//!
//! Every case runs against [`MockConsoleHandoffExchange`], so the route's whole
//! decision tree — spend the code, check the slug, check eligibility, mint a
//! session — is exercised in a build that links no HTTP crate. The mock models
//! single use faithfully (redeeming removes the code), which is what makes the
//! replay case prove anything.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::server::handoff::MockConsoleHandoffExchange;
use crate::server::router;
use crate::{AppConfig, AppState};

/// The slug the hub knows this instance by. Deliberately *not* the company id
/// (`acme`, derived from the manifest name) — the two differ in practice, and a
/// test that let them coincide would not notice the route comparing the wrong
/// one.
const HUB_SLUG: &str = "acme-instance";

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-handoff-")
        .tempdir()
        .expect("tempdir")
}

/// `Ada` is a manifest-bootstrapped admin, spelled with capitals so the route's
/// email normalization is exercised against a hub that returns a different case.
fn manifest() -> CompanyManifest {
    toml::from_str(
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [users]\nadmins = [\"Ada@Example.com\"]\n",
    )
    .unwrap()
}

/// A host with `exchange` wired and its hub slug set.
async fn state_with(
    home: &std::path::Path,
    exchange: Option<Arc<MockConsoleHandoffExchange>>,
    tenant_slug: Option<&str>,
) -> AppState {
    let store = crate::store::FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            id: id.clone(),
            manifest: manifest(),
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: Vec::new(),
            overlay_desk_members: Vec::new(),
            overlay_desk_order: Vec::new(),
            overlay_desks: Vec::new(),
            overlay_workflows: Vec::new(),
            template_provenance: None,
        })
        .await
        .unwrap();
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let mut state = AppState::new(AppConfig {
        tenant_slug: tenant_slug.map(str::to_string),
        ..AppConfig::default()
    })
    .with_home(home.to_path_buf());
    if let Some(exchange) = exchange {
        state = state.with_console_handoff(exchange);
    }
    state.registry().insert(id, Arc::new(runtime));
    state
}

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get_with_cookie(uri: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap()
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn session_cookie(response: &axum::response::Response) -> String {
    let set = response
        .headers()
        .get("set-cookie")
        .expect("a session response must set a cookie")
        .to_str()
        .unwrap();
    set.split(';').next().unwrap().to_string()
}

async fn handoff(state: &AppState, code: &str) -> axum::response::Response {
    router(state.clone())
        .oneshot(post(
            "/api/v1/companies/acme/auth/handoff",
            serde_json::json!({ "code": code }),
        ))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_live_code_for_an_eligible_address_mints_a_session() {
    let home = home();
    let exchange = Arc::new(MockConsoleHandoffExchange::new().with_code(
        "code-1",
        "Ada@Example.com",
        HUB_SLUG,
    ));
    let state = state_with(home.path(), Some(exchange), Some(HUB_SLUG)).await;

    let response = handoff(&state, "code-1").await;

    assert_eq!(response.status(), StatusCode::OK);
    let cookie = session_cookie(&response);
    assert!(
        cookie.starts_with("oc_session_acme="),
        "the cookie must be named for this company: {cookie}"
    );
    let body = body_json(response).await;
    // The hub's casing is normalized on the way in, so this address cannot end
    // up holding a second account alongside a magic-link sign-in.
    assert_eq!(body["email"], "ada@example.com");
    assert_eq!(body["role"], "admin");
    assert_eq!(body["company"], "acme");

    // And the session actually works.
    let me = router(state.clone())
        .oneshot(get_with_cookie("/api/v1/companies/acme/auth/me", &cookie))
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);
}

#[tokio::test]
async fn an_address_this_company_does_not_know_is_refused() {
    let home = home();
    let exchange = Arc::new(MockConsoleHandoffExchange::new().with_code(
        "code-1",
        "stranger@example.com",
        HUB_SLUG,
    ));
    let state = state_with(home.path(), Some(exchange), Some(HUB_SLUG)).await;

    let response = handoff(&state, "code-1").await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.headers().get("set-cookie").is_none());
    assert_eq!(body_json(response).await["code"], "not_a_member");
}

#[tokio::test]
async fn a_replayed_code_is_refused() {
    let home = home();
    let exchange = Arc::new(MockConsoleHandoffExchange::new().with_code(
        "code-1",
        "Ada@Example.com",
        HUB_SLUG,
    ));
    let state = state_with(home.path(), Some(exchange), Some(HUB_SLUG)).await;

    assert_eq!(handoff(&state, "code-1").await.status(), StatusCode::OK);
    let replay = handoff(&state, "code-1").await;

    // The hub spent it on the first redemption, so the second is indistinguishable
    // from a code that never existed — and is reported as a dead credential,
    // not as the hub being down.
    assert_eq!(replay.status(), StatusCode::FORBIDDEN);
    assert!(replay.headers().get("set-cookie").is_none());
    assert_eq!(body_json(replay).await["code"], "handoff_invalid");
}

#[tokio::test]
async fn a_code_minted_for_another_company_is_refused() {
    let home = home();
    // The address is a perfectly good admin here — only the slug is wrong. This
    // is the relay attack: a hostile tenant passing on a neighbour's code.
    let exchange = Arc::new(MockConsoleHandoffExchange::new().with_code(
        "code-1",
        "Ada@Example.com",
        "someone-else",
    ));
    let state = state_with(home.path(), Some(exchange), Some(HUB_SLUG)).await;

    let response = handoff(&state, "code-1").await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.headers().get("set-cookie").is_none());
    assert_eq!(body_json(response).await["code"], "handoff_wrong_company");
}

#[tokio::test]
async fn an_expired_or_unknown_code_is_refused() {
    let home = home();
    // A mock that knows no codes is exactly a hub that has already expired or
    // reaped this one: same refusal, same shape.
    let state = state_with(
        home.path(),
        Some(Arc::new(MockConsoleHandoffExchange::new())),
        Some(HUB_SLUG),
    )
    .await;

    let response = handoff(&state, "expired-code").await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.headers().get("set-cookie").is_none());
    assert_eq!(body_json(response).await["code"], "handoff_invalid");
}

#[tokio::test]
async fn a_host_with_no_hub_slug_refuses_without_spending_the_code() {
    let home = home();
    let exchange = Arc::new(MockConsoleHandoffExchange::new().with_code(
        "code-1",
        "Ada@Example.com",
        HUB_SLUG,
    ));
    let state = state_with(home.path(), Some(exchange.clone()), None).await;

    let response = handoff(&state, "code-1").await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_json(response).await["code"], "handoff_unavailable");
    // And the code was never presented to the hub. A host that could not have
    // honored the sign-in must not burn the user's one chance at it.
    assert!(exchange.attempts().is_empty());
}

#[tokio::test]
async fn a_self_hosted_host_with_no_exchange_refuses() {
    let home = home();
    let state = state_with(home.path(), None, Some(HUB_SLUG)).await;

    let response = handoff(&state, "code-1").await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_json(response).await["code"], "handoff_unavailable");
}

#[tokio::test]
async fn the_mock_spends_a_code_exactly_once() {
    // Guards the guard: if the mock ever stopped removing redeemed codes, the
    // replay test above would pass for the wrong reason.
    use crate::server::handoff::ConsoleHandoffExchange;
    let exchange =
        MockConsoleHandoffExchange::new().with_code("code-1", "ada@example.com", HUB_SLUG);

    assert!(exchange.redeem("code-1").await.is_ok());
    assert!(exchange.redeem("code-1").await.is_err());
    assert_eq!(exchange.attempts(), vec!["code-1", "code-1"]);
}
