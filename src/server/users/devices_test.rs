//! Security tests for device pairing.
//!
//! A paired device is a year-long credential handed to a machine, so each of
//! these pins something that would be a vulnerability rather than a bug: a
//! pairing code redeemable twice, a magic link convertible into a device, a
//! compromised desktop enrolling more desktops, or a suspended user whose
//! laptop keeps working.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::ports::{CompanyStore, SessionKind, SessionRecord, UserRecord, UserRole, UserStatus};
use crate::runtime::RuntimeBuilder;
use crate::server::graphql::auth::{GqlAuth, resolve_principal};
use crate::server::router;
use crate::server::users::cookie::{SESSION_HEADER, session_cookie_name};
use crate::server::users::token::{OsTokens, mint_session_token, sha256_hex};
use crate::{AppConfig, AppState};

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-devices-")
        .tempdir()
        .expect("tempdir")
}

fn manifest() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap()
}

async fn state_with(home: &std::path::Path, companies: &[&str]) -> AppState {
    let store = crate::store::FsCompanyStore::new(home.to_path_buf());
    let state = AppState::new(AppConfig::default()).with_home(home.to_path_buf());
    for name in companies {
        let id = CompanyId::new(*name);
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
                overlay_budgets: Vec::new(),
                overlay_policy: None,
                overlay_desk_tools: Default::default(),
                disabled_workflows: Vec::new(),
                template_provenance: None,
                setup: None,
            })
            .await
            .unwrap();
        let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest())
            .with_id(id.clone())
            .build()
            .await
            .unwrap();
        state.registry().insert(id, Arc::new(runtime));
    }
    state
}

/// Seeds an active user with a live *browser* session, as if they had signed in.
async fn seed_signed_in(state: &AppState, company: &str, status: UserStatus) -> String {
    let id = CompanyId::new(company);
    let runtime = state.registry().get(&id).unwrap();
    let now = crate::ports::now_millis();
    runtime
        .users()
        .upsert_user(
            &id,
            &UserRecord {
                id: "u1".into(),
                email: "ada@example.com".into(),
                display_name: None,
                role: UserRole::Member,
                status,
                password_hash: None,
                must_change_password: false,
                created_at_millis: now,
                last_seen_at_millis: None,
                updated_at_millis: now,
            },
        )
        .await
        .unwrap();
    let token = mint_session_token(&OsTokens);
    runtime
        .sessions()
        .create(
            &id,
            &SessionRecord {
                id: "s1".into(),
                token_hash: sha256_hex(&token),
                user_id: "u1".into(),
                created_at_millis: now,
                expires_at_millis: now + 60_000,
                user_agent: None,
                kind: SessionKind::Browser,
                label: None,
            },
        )
        .await
        .unwrap();
    token
}

fn cookie_header(company: &str, token: &str) -> String {
    format!(
        "{}={token}",
        session_cookie_name(&CompanyId::new(company)).unwrap()
    )
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// Signs in as a browser and asks for a pairing code.
async fn start_pairing(state: &AppState, session: &str) -> (StatusCode, serde_json::Value) {
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/devices")
                .header("cookie", cookie_header("acme", session))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

/// Redeems a pairing code, anonymously, as a desktop would.
async fn claim(state: &AppState, code: &str, label: &str) -> (StatusCode, serde_json::Value) {
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/devices/claim")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "code": code, "label": label }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

#[tokio::test]
async fn a_paired_device_can_authenticate_with_the_session_header() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, &["acme"]).await;
    let session = seed_signed_in(&state, "acme", UserStatus::Active).await;

    let (status, pairing) = start_pairing(&state, &session).await;
    assert_eq!(status, StatusCode::OK, "pairing: {pairing}");
    let code = pairing["code"]
        .as_str()
        .expect("a pairing code")
        .to_string();

    let (status, claimed) = claim(&state, &code, "Ada's MacBook").await;
    assert_eq!(status, StatusCode::OK, "claim: {claimed}");
    let device_token = claimed["token"].as_str().expect("a device token");
    assert_eq!(claimed["company"], "acme");

    // The end-to-end property: that token, in the header carrier, is Ada.
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        SESSION_HEADER,
        format!("acme.{device_token}").parse().unwrap(),
    );
    let acme = CompanyId::new("acme");
    match resolve_principal(&headers, &state, Some(&acme), None)
        .await
        .expect("the device authenticates")
    {
        GqlAuth::User(user) => {
            assert_eq!(user.user_id, "u1");
            assert!(user.is_device(), "must resolve as a device, not a browser");
        }
        other => panic!("expected a user, got {other:?}"),
    }

    // And it is a device, with its label, on the owner's device list.
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/company/devices")
                .header("cookie", cookie_header("acme", &session))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let listed = body_json(response).await;
    assert_eq!(listed.as_array().expect("a list").len(), 1);
    assert_eq!(listed[0]["label"], "Ada's MacBook");
    // The browser session Ada is holding must not appear here.
    assert_eq!(listed[0]["id"], claimed["deviceId"]);
}

#[tokio::test]
async fn a_pairing_code_is_single_use() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, &["acme"]).await;
    let session = seed_signed_in(&state, "acme", UserStatus::Active).await;

    let (_, pairing) = start_pairing(&state, &session).await;
    let code = pairing["code"].as_str().unwrap().to_string();

    let (first, _) = claim(&state, &code, "laptop").await;
    assert_eq!(first, StatusCode::OK);
    let (second, body) = claim(&state, &code, "attacker").await;
    assert_ne!(
        second,
        StatusCode::OK,
        "a redeemed code must not enrol a second device: {body}"
    );
}

#[tokio::test]
async fn a_login_code_cannot_be_redeemed_as_a_device() {
    // The domain-separation property. Both credentials live in one store, and
    // the only thing keeping them apart is the hash prefix. If that were ever
    // dropped, an intercepted magic link would convert into a year-long device
    // credential.
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, &["acme"]).await;
    seed_signed_in(&state, "acme", UserStatus::Active).await;

    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();
    let now = crate::ports::now_millis();
    let login_code = crate::server::users::token::mint_login_code(&OsTokens);
    runtime
        .login_codes()
        .create(
            &id,
            &crate::ports::LoginCodeRecord {
                id: "c1".into(),
                // Hashed the way a *login* code is hashed: no domain prefix.
                code_hash: sha256_hex(&login_code),
                email: "ada@example.com".into(),
                created_at_millis: now,
                expires_at_millis: now + 60_000,
                consumed_at_millis: None,
            },
        )
        .await
        .unwrap();

    let (status, body) = claim(&state, &login_code, "attacker").await;
    assert_ne!(
        status,
        StatusCode::OK,
        "a login code must not claim a device: {body}"
    );
}

#[tokio::test]
async fn a_pairing_code_cannot_be_redeemed_as_a_login() {
    // The converse direction, so the separation is pinned from both sides.
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, &["acme"]).await;
    let session = seed_signed_in(&state, "acme", UserStatus::Active).await;

    let (_, pairing) = start_pairing(&state, &session).await;
    let code = pairing["code"].as_str().unwrap();

    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/auth/verify")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({ "code": code }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a pairing code must not mint a browser session"
    );
}

#[tokio::test]
async fn a_device_cannot_pair_another_device() {
    // Otherwise the credential reproduces: revoking the compromised desktop
    // would leave behind whatever it enrolled, and revocation would stop being
    // a lever at all.
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, &["acme"]).await;
    let session = seed_signed_in(&state, "acme", UserStatus::Active).await;

    let (_, pairing) = start_pairing(&state, &session).await;
    let code = pairing["code"].as_str().unwrap().to_string();
    let (_, claimed) = claim(&state, &code, "laptop").await;
    let device_token = claimed["token"].as_str().unwrap();

    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/devices")
                .header(SESSION_HEADER, format!("acme.{device_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        response.status(),
        StatusCode::OK,
        "a device must not be able to mint a pairing code"
    );
}

#[tokio::test]
async fn suspending_a_user_kills_their_paired_devices() {
    // The reason a device is a SessionRecord rather than a separate port: the
    // existing revocation lever has to reach it with no device-specific code.
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, &["acme"]).await;
    let session = seed_signed_in(&state, "acme", UserStatus::Active).await;

    let (_, pairing) = start_pairing(&state, &session).await;
    let code = pairing["code"].as_str().unwrap().to_string();
    let (_, claimed) = claim(&state, &code, "laptop").await;
    let device_token = claimed["token"].as_str().unwrap().to_string();

    let acme = CompanyId::new("acme");
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        SESSION_HEADER,
        format!("acme.{device_token}").parse().unwrap(),
    );
    assert!(
        resolve_principal(&headers, &state, Some(&acme), None)
            .await
            .is_ok(),
        "the device should work before suspension"
    );

    // Suspend exactly as the admin route does.
    let runtime = state.registry().get(&acme).unwrap();
    let mut user = runtime
        .users()
        .get_user(&acme, "u1")
        .await
        .unwrap()
        .unwrap();
    user.status = UserStatus::Suspended;
    runtime.users().upsert_user(&acme, &user).await.unwrap();

    assert!(
        resolve_principal(&headers, &state, Some(&acme), None)
            .await
            .is_err(),
        "a suspended user's device must stop working at once"
    );
}

#[tokio::test]
async fn a_device_belonging_to_someone_else_cannot_be_revoked() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, &["acme"]).await;
    let session = seed_signed_in(&state, "acme", UserStatus::Active).await;

    // A device owned by a different user of the same company.
    let acme = CompanyId::new("acme");
    let runtime = state.registry().get(&acme).unwrap();
    let now = crate::ports::now_millis();
    runtime
        .sessions()
        .create(
            &acme,
            &SessionRecord {
                id: "someone-elses-device".into(),
                token_hash: sha256_hex("not-ours"),
                user_id: "u2".into(),
                created_at_millis: now,
                expires_at_millis: now + 60_000,
                user_agent: None,
                kind: SessionKind::Device,
                label: Some("Bob's ThinkPad".into()),
            },
        )
        .await
        .unwrap();

    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/company/devices/someone-elses-device")
                .header("cookie", cookie_header("acme", &session))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        response.status(),
        StatusCode::OK,
        "revocation must be scoped to the caller's own devices"
    );
    assert!(
        runtime
            .sessions()
            .find_by_token_hash(&acme, &sha256_hex("not-ours"))
            .await
            .unwrap()
            .is_some(),
        "the other user's device must survive"
    );
}

#[tokio::test]
async fn an_unsigned_in_caller_cannot_mint_a_pairing_code() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, &["acme"]).await;
    seed_signed_in(&state, "acme", UserStatus::Active).await;

    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/devices")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_device_session_outlives_a_browser_session() {
    // Not a round number check — the point is that the two TTLs are actually
    // different, so a desktop is not silently logged out on the browser's
    // 14-day schedule.
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, &["acme"]).await;
    let session = seed_signed_in(&state, "acme", UserStatus::Active).await;

    let (_, pairing) = start_pairing(&state, &session).await;
    let code = pairing["code"].as_str().unwrap().to_string();
    let (_, claimed) = claim(&state, &code, "laptop").await;

    let expires = claimed["expiresAtMillis"].as_u64().expect("an expiry");
    let now = crate::ports::now_millis();
    assert!(
        expires > now + crate::server::users::token::SESSION_TTL_MILLIS,
        "a device must live longer than a browser session"
    );
}
