use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::Request;
use tower::ServiceExt;

use super::*;
use crate::AppConfig;
use crate::company::CompanyManifest;
use crate::ports::types::CompanyId;
use crate::runtime::RuntimeBuilder;
use crate::server::router;

fn manifest() -> CompanyManifest {
    toml::from_str(
        r#"
        [company]
        name = "Acme"
        handle = "acme"
        [policy]
        mode = "full"
        "#,
    )
    .unwrap()
}

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("opencompany-admin-")
        .tempdir()
        .expect("tempdir")
}

async fn state_with(home: &std::path::Path) -> AppState {
    let id = CompanyId::new("acme");
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, Arc::new(runtime));
    state
}

async fn get_with_cookie(app: axum::Router, uri: &str, cookie: &str) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri(uri)
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

/// `require_admin` gates every route in this module. Prove it holds the
/// same temporary-password boundary `ScopedCompany` already enforces for
/// member writes (`platform_auth::refuse_until_password_changed`) — an
/// admin-issued temporary password must be good for exactly one thing:
/// replacing it, not for administering the company it was issued on.
#[tokio::test]
async fn a_temporary_password_admin_cannot_reach_the_roster() {
    let home_dir = home();
    let state = state_with(home_dir.path()).await;
    let cookie = crate::server::test_support::seed_temp_password_admin(&state, "acme").await;

    let app = router(state);
    let response = get_with_cookie(app, "/api/v1/companies/acme/users", &cookie).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["code"], "password_change_required");
}

/// The positive control: an admin whose password is not temporary reaches
/// the same route fine. Without this, the denial above could pass for the
/// wrong reason (e.g. the route always refuses in this harness).
#[tokio::test]
async fn an_ordinary_admin_reaches_the_roster() {
    let home_dir = home();
    let state = state_with(home_dir.path()).await;
    let cookie = crate::server::test_support::seed_admin(&state, "acme").await;

    let app = router(state);
    let response = get_with_cookie(app, "/api/v1/companies/acme/users", &cookie).await;

    assert_eq!(response.status(), StatusCode::OK);
}

/// Seeds an active admin with a known id and a live session, so a
/// concurrency test can address it by id and authenticate as it — unlike
/// `test_support::seed_session`, which mints a random id the caller cannot
/// recover.
async fn seed_admin_with_id(state: &AppState, company: &str, id: &str) -> String {
    use crate::ports::{SessionKind, SessionRecord};
    use crate::server::users::cookie::session_cookie_name;
    use crate::server::users::token::{OsTokens, mint_session_token, sha256_hex};

    let cid = CompanyId::new(company);
    let runtime = state
        .registry()
        .get(&cid)
        .expect("seed_admin_with_id: company is not registered");
    let now = now_millis();
    runtime
        .users()
        .upsert_user(
            &cid,
            &UserRecord {
                id: id.to_string(),
                email: format!("{id}@example.test"),
                display_name: None,
                avatar: None,
                role: UserRole::Admin,
                status: UserStatus::Active,
                password_hash: None,
                must_change_password: false,
                created_at_millis: now,
                last_seen_at_millis: None,
                updated_at_millis: now,
            },
        )
        .await
        .expect("seed_admin_with_id: upsert user");

    let token = mint_session_token(&OsTokens);
    runtime
        .sessions()
        .create(
            &cid,
            &SessionRecord {
                id: generate_id(),
                token_hash: sha256_hex(&token),
                user_id: id.to_string(),
                created_at_millis: now,
                expires_at_millis: now + 60 * 60 * 1000,
                user_agent: None,
                kind: SessionKind::Browser,
                label: None,
            },
        )
        .await
        .expect("seed_admin_with_id: create session");

    format!(
        "{}={token}",
        session_cookie_name(&cid).expect("seed_admin_with_id: cookie name")
    )
}

/// `ensure_not_last_admin` is a check-then-act: count the other active
/// admins, then write. Six admins, each demoting the next one around a
/// ring, fired at the same instant via a `Barrier`, is the scenario the
/// last-admin rule exists for and the one a sequential test cannot
/// reproduce: every demotion's "am I safe" check can observe five other
/// still-active admins even though every one of them is *also* about to be
/// demoted. Serialized (the fix), the sixth request to actually commit
/// finds nobody else left and is refused, so exactly one admin survives —
/// deterministically, regardless of completion order. Unserialized, every
/// check can pass before any write lands, and the company can end up with
/// zero.
#[tokio::test]
async fn concurrent_demotions_cannot_zero_out_the_admin_roster() {
    const N: usize = 6;
    let home_dir = home();
    let state = state_with(home_dir.path()).await;

    let mut cookies = Vec::with_capacity(N);
    for i in 0..N {
        cookies.push(seed_admin_with_id(&state, "acme", &format!("ring-{i}")).await);
    }

    let app = router(state.clone());
    let barrier = Arc::new(tokio::sync::Barrier::new(N));
    let mut tasks = Vec::with_capacity(N);
    for (i, cookie) in cookies.iter().enumerate() {
        let app = app.clone();
        let cookie = cookie.clone();
        let target = format!("ring-{}", (i + 1) % N);
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            app.oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/v1/companies/acme/users/{target}"))
                    .header("cookie", cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"role":"member"}"#))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        }));
    }

    let mut statuses = Vec::with_capacity(N);
    for task in tasks {
        statuses.push(task.await.expect("a demotion request must not panic"));
    }
    // Exactly one of the six is refused — the request that would have
    // taken the last admin. The rest succeed.
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1,
        "exactly one demotion in the ring must be refused as the last admin: {statuses:?}"
    );

    let cid = CompanyId::new("acme");
    let runtime = state.registry().get(&cid).unwrap();
    let users = runtime.users().list_users(&cid).await.unwrap();
    let active_admins = users
        .iter()
        .filter(|u| u.id.starts_with("ring-") && u.role == UserRole::Admin)
        .count();
    assert_eq!(
        active_admins, 1,
        "the ring must never lose its last admin under concurrent demotions"
    );
}

/// `identity()` applies the shared login rule: a value with whitespace inside
/// it, or one that would parse as the `none`-mode owner's key, is refused
/// rather than accepted as a to-be-normalized address.
#[test]
fn identity_refuses_a_malformed_login() {
    for bad in ["Ada Lovelace", "local:owner"] {
        let body = InviteBody {
            email: bad.to_string(),
            wallet: String::new(),
            role: UserRole::Member,
        };

        let err = body
            .identity(AuthMode::Email)
            .expect_err("an unusable login must be refused");

        assert!(
            matches!(
                err,
                OpenCompanyError::InvalidRequest(ref msg)
                    if msg == "that is not a usable login — an email address or a single word"
            ),
            "unexpected error for {bad:?}: {err:?}"
        );
    }
}

/// A plain username is a login: on a host with no mail there is no mailbox to
/// demand, and the admin hands over a password instead.
#[test]
fn identity_accepts_a_plain_username() {
    let body = InviteBody {
        email: "Ops".to_string(),
        wallet: String::new(),
        role: UserRole::Member,
    };
    assert_eq!(body.identity(AuthMode::Email).unwrap(), "ops");
}

/// The same branch on whitespace-only input — `normalize_email` trims it
/// to empty, which the emptiness half of the check must also catch.
#[test]
fn identity_refuses_an_empty_email() {
    let body = InviteBody {
        email: "   ".to_string(),
        wallet: String::new(),
        role: UserRole::Member,
    };

    let err = body
        .identity(AuthMode::Email)
        .expect_err("whitespace-only input must be refused");

    assert!(
        matches!(
            err,
            OpenCompanyError::InvalidRequest(ref msg)
                if msg == "that is not a usable login — an email address or a single word"
        ),
        "unexpected error: {err:?}"
    );
}

/// The positive control: a well-formed address is accepted and comes back
/// normalized, so the two refusals above are not simply refusing
/// everything.
#[test]
fn identity_accepts_a_well_formed_email() {
    let body = InviteBody {
        email: "Ops@Example.com".to_string(),
        wallet: String::new(),
        role: UserRole::Member,
    };

    let identity = body
        .identity(AuthMode::Email)
        .expect("a real address must be accepted");

    assert_eq!(identity, "ops@example.com");
}
