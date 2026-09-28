//! The first-admin claim: `POST …/auth/claim` and the `claimable` cue on
//! `GET …/auth/config`.

use crate::app::config::AuthMode;
use crate::server::ops::ConnectionsRuntime;
use crate::server::router;
use crate::{AppConfig, AppState};
use axum::http::StatusCode;
use tower::ServiceExt;

use super::mode_test_support_1::*;

const PASSWORD: &str = "correct horse battery staple";

fn get_with_cookie(uri: &str, cookie: &str) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::builder()
        .uri(uri)
        .header("cookie", cookie)
        .body(axum::body::Body::empty())
        .unwrap()
}

fn claim(email: &str, password: &str) -> axum::http::Request<axum::body::Body> {
    post(
        "/api/v1/company/auth/claim",
        serde_json::json!({ "email": email, "password": password }),
    )
}

/// A host the docker image boots: routable, no mail, a company nobody has
/// joined. The claim is the only way in, and it has to work from the sign-in
/// screen without a shell.
async fn fresh_routable_host(home: &std::path::Path) -> AppState {
    state_in_mode_on(
        home,
        AuthMode::Email,
        None,
        routable(),
        ConnectionsRuntime::new(),
    )
    .await
}

/// The whole point: the first person in picks a login, gets an admin session,
/// and the same password signs them in again tomorrow.
#[tokio::test]
async fn the_first_visitor_claims_the_admin_account_and_is_signed_in() {
    let dir = home();
    let state = fresh_routable_host(dir.path()).await;
    let app = router(state);

    let response = app
        .clone()
        .oneshot(claim("Ada@Example.com", PASSWORD))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = session_cookie(&response);
    let body = body_json(response).await;
    assert_eq!(
        body["email"], "ada@example.com",
        "normalized like every login"
    );
    assert_eq!(body["role"], "admin");
    assert_eq!(body["hasPassword"], true);
    assert_eq!(
        body["mustChangePassword"], false,
        "they chose it themselves"
    );

    // The session is real: the admin routes answer it.
    let response = app
        .clone()
        .oneshot(get_with_cookie("/api/v1/company/users", &cookie))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // And the password is the ordinary one.
    let response = app
        .clone()
        .oneshot(post(
            "/api/v1/company/auth/login",
            serde_json::json!({ "email": "ada@example.com", "password": PASSWORD }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // The door is now shut, and the config says so.
    let response = app
        .clone()
        .oneshot(get("/api/v1/company/auth/config"))
        .await
        .unwrap();
    assert_eq!(body_json(response).await["claimable"], false);
}

/// The claim admits exactly one person. The second to arrive is told to sign
/// in, not handed a second admin account.
#[tokio::test]
async fn a_claimed_company_refuses_a_second_claim() {
    let dir = home();
    let state = fresh_routable_host(dir.path()).await;
    let app = router(state);
    let response = app
        .clone()
        .oneshot(claim("ada@example.com", PASSWORD))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(claim("mallory@example.com", PASSWORD))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(response).await["code"], "already_claimed");
}

/// A login need not be a mailbox. On a host with no mail there is nothing to
/// send to, and `admin` is a perfectly good login for the one person running
/// it.
#[tokio::test]
async fn a_plain_username_is_an_acceptable_login() {
    let dir = home();
    let state = fresh_routable_host(dir.path()).await;
    let app = router(state);
    let response = app.clone().oneshot(claim("admin", PASSWORD)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .oneshot(post(
            "/api/v1/company/auth/login",
            serde_json::json!({ "email": "admin", "password": PASSWORD }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// Where the deployment already named its first admin — here through
/// `OPENCOMPANY_ADMIN_EMAIL`, exactly as a provisioned tenant is booted — a
/// stranger who reaches the sign-in screen first cannot take the instance;
/// only the named address may claim.
#[tokio::test]
async fn a_named_admin_keeps_the_claim_from_a_stranger() {
    let dir = home();
    let state = state_in_mode_on(
        dir.path(),
        AuthMode::Email,
        None,
        AppConfig {
            admin_email: Some("Owner@Example.com".to_string()),
            ..routable()
        },
        ConnectionsRuntime::new(),
    )
    .await;
    let app = router(state);

    let response = app
        .clone()
        .oneshot(claim("mallory@example.com", PASSWORD))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_json(response).await["code"], "not_the_named_admin");

    let response = app
        .clone()
        .oneshot(claim("owner@example.com", PASSWORD))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// The same holds for a manifest `[users].admins` entry.
#[tokio::test]
async fn a_manifest_admin_keeps_the_claim_from_a_stranger() {
    let dir = home();
    let state = state_in_mode(dir.path(), AuthMode::Email, Some("ada@example.com")).await;
    let app = router(state);

    let response = app
        .clone()
        .oneshot(claim("mallory@example.com", PASSWORD))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = app
        .oneshot(claim("ada@example.com", PASSWORD))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// The password policy is the ordinary one: a short password is refused
/// before anything is written, and the company stays claimable.
#[tokio::test]
async fn a_weak_password_is_refused_and_the_company_stays_claimable() {
    let dir = home();
    let state = fresh_routable_host(dir.path()).await;
    let app = router(state);
    let response = app
        .clone()
        .oneshot(claim("ada@example.com", "short"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = app
        .oneshot(get("/api/v1/company/auth/config"))
        .await
        .unwrap();
    assert_eq!(body_json(response).await["claimable"], true);
}

/// One mode, one door: a company that does not sign in by email has no first
/// admin to claim this way.
#[tokio::test]
async fn other_modes_refuse_the_claim() {
    for mode in [AuthMode::Wallet, AuthMode::None] {
        let dir = home();
        let state = state_in_mode(dir.path(), mode, None).await;
        let response = router(state)
            .oneshot(claim("ada@example.com", PASSWORD))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT, "{}", mode.as_str());
        let body = body_json(response).await;
        assert_eq!(body["code"], "auth_mode", "{}", mode.as_str());
    }
}
