use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord, SecretValue};
use crate::runtime::RuntimeBuilder;
use crate::server::ops::ConnectionsRuntime;
use crate::server::ops::mailer::{MailCredentials, RecordingMailSender};
use crate::server::ops::smtp::{SmtpCredentials, SmtpSecurity};
use crate::server::router;
use crate::{AppConfig, AppState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

use super::routes_test_support_1::*;

#[tokio::test]
async fn the_header_carrier_returns_a_session_that_authenticates() {
    // The whole point of the carrier: a console on another origin gets a
    // credential it can actually present, because a cookie would never be sent.
    let home = home();
    let (state, sender) = state_with_mail(home.path()).await;
    let (_, json) = login_wanting_header_carrier(&state, &sender, "ada@example.com").await;

    let session = json["session"]
        .as_str()
        .expect("the header carrier must return a session")
        .to_string();
    assert!(
        session.starts_with("acme."),
        "the value must name its company so a client need not know how the \
         addressed company was resolved: {session}"
    );

    let app = router(state.clone());
    let response = app
        .oneshot(get_with_session_header(
            "/api/v1/companies/acme/auth/me",
            &session,
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the returned session must authenticate the next request"
    );
    assert_eq!(body_json(response).await["email"], "ada@example.com");
}

#[tokio::test]
async fn the_header_carrier_sets_no_cookie() {
    // One session, one carrier. Setting both would leave the cookie half as a
    // third-party cookie some browsers keep and others drop, so whether logging
    // out actually ended the session would vary by browser.
    let home = home();
    let (state, sender) = state_with_mail(home.path()).await;
    let (headers, _) = login_wanting_header_carrier(&state, &sender, "ada@example.com").await;
    assert!(
        headers.get("set-cookie").is_none(),
        "a client that asked to carry the session must not also be given a cookie"
    );
}

#[tokio::test]
async fn a_login_that_asks_for_nothing_is_unchanged() {
    // The carrier is opt-in, and every existing console is the opt-out case:
    // it must still get its HttpOnly cookie and must never see a token in the
    // body, which is precisely what it has no way to store safely.
    let home = home();
    let (state, sender) = state_with_mail(home.path()).await;
    let code = request_code(&state, &sender, "ada@example.com").await;
    let app = router(state.clone());
    let response = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/verify",
            serde_json::json!({ "code": code }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let set = response
        .headers()
        .get("set-cookie")
        .expect("the default carrier is still the cookie")
        .to_str()
        .unwrap()
        .to_string();
    assert!(set.contains("HttpOnly"), "{set}");
    let json = body_json(response).await;
    assert!(
        json.get("session").is_none(),
        "a cookie client must never be handed the raw token: {json}"
    );
    // And the body it always returned is still there, unflattened by the change.
    assert_eq!(json["email"], "ada@example.com");
}

#[tokio::test]
async fn a_setup_link_carries_the_requested_landing_fragment() {
    // Setup's hand-off asks the mailed link to land on the roster, so a
    // production operator who finishes setup and follows the email reaches the
    // company the wizard just built rather than the Overview graph. The
    // fragment is appended after the code so the magic-link landing strips the
    // credential and keeps the destination.
    let home = home();
    let (state, sender) = state_with_mail(home.path()).await;
    let app = router(state.clone());
    app.oneshot(post(
        "/api/v1/companies/acme/auth/request",
        serde_json::json!({
            "email": "ada@example.com",
            "redirect": "#/company?from=setup",
        }),
    ))
    .await
    .unwrap();
    let sent = sender.sent();
    let body = &sent.last().expect("no mail was sent").1.body;
    assert!(
        body.contains("/login?company=acme&code=") && body.contains("#/company?from=setup"),
        "the mailed link must carry the setup destination: {body}"
    );
}

#[tokio::test]
async fn a_malformed_redirect_is_dropped_not_obeyed() {
    // The fragment is mailed, so a value that could break the link out of the
    // body — or name something that is not a console route — must be ignored,
    // and it must not stop the sign-in mail from going out at all.
    let home = home();
    let (state, sender) = state_with_mail(home.path()).await;
    let app = router(state.clone());
    app.oneshot(post(
        "/api/v1/companies/acme/auth/request",
        serde_json::json!({
            "email": "ada@example.com",
            "redirect": "https://evil.example\n#/company",
        }),
    ))
    .await
    .unwrap();
    let sent = sender.sent();
    let body = &sent.last().expect("no mail was sent").1.body;
    assert!(!body.contains("evil.example"), "{body}");
    assert!(
        body.contains("/login?company=acme&code="),
        "the sign-in link itself must survive: {body}"
    );
}

#[tokio::test]
async fn a_header_carried_session_can_be_logged_out() {
    // Revocation has to reach the session however it was carried, or a hub
    // console's "sign out" would clear its own storage and leave a live token
    // on the server for the rest of its TTL.
    let home = home();
    let (state, sender) = state_with_mail(home.path()).await;
    let (_, json) = login_wanting_header_carrier(&state, &sender, "ada@example.com").await;
    let session = json["session"].as_str().unwrap().to_string();

    let app = router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/companies/acme/auth/logout")
                .header(super::cookie::SESSION_HEADER, &session)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let app = router(state.clone());
    let after = app
        .oneshot(get_with_session_header(
            "/api/v1/companies/acme/auth/me",
            &session,
        ))
        .await
        .unwrap();
    assert_eq!(
        after.status(),
        StatusCode::UNAUTHORIZED,
        "the token must be dead server-side, not merely dropped by the client"
    );
}

// ---------------------------------------------------------------------------
// The generic-failure rule
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_request_answers_identically_for_everyone() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, sender) = state_with_mail(&home).await;

    // An eligible admin, a stranger, and malformed input must be
    // indistinguishable from outside. Anything else is a membership oracle.
    for email in ["ada@example.com", "nobody@example.com", "not-an-email", ""] {
        let app = router(state.clone());
        let response = app
            .oneshot(post(
                "/api/v1/companies/acme/auth/request",
                serde_json::json!({ "email": email }),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "status varied for {email:?}"
        );
        assert_eq!(
            body_json(response).await,
            serde_json::json!({ "sent": true }),
            "the body varied for {email:?} — that is an enumeration oracle"
        );
    }

    // Only the eligible address actually got mail.
    let sent = sender.sent();
    assert_eq!(sent.len(), 1, "mail went to someone it shouldn't have");
    assert_eq!(sent[0].1.to, "ada@example.com");
}

#[tokio::test]
async fn every_verify_failure_is_the_same_401() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _) = state_with_mail(&home).await;

    let mut seen = Vec::new();
    for code in [
        "",
        "not-a-real-code",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        let app = router(state.clone());
        let response = app
            .oneshot(post(
                "/api/v1/companies/acme/auth/verify",
                serde_json::json!({ "code": code }),
            ))
            .await
            .unwrap();
        let status = response.status();
        seen.push((status, body_json(response).await));
    }
    let first = seen[0].clone();
    for entry in &seen {
        assert_eq!(entry.0, StatusCode::UNAUTHORIZED);
        assert_eq!(*entry, first, "verify failures must be byte-identical");
        assert_eq!(entry.1["code"], "invalid_login");
    }
}

#[tokio::test]
async fn every_password_login_failure_is_the_same_401() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, sender) = state_with_mail(&home).await;
    // Give ada an account and a password first.
    let cookie = login_via_link(&state, &sender, "ada@example.com").await;
    let app = router(state.clone());
    let response = app
        .oneshot(post_with_cookie(
            "/api/v1/companies/acme/auth/password",
            serde_json::json!({ "password": "correct horse battery" }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mut seen = Vec::new();
    for (email, pw) in [
        // Wrong password for a real account.
        ("ada@example.com", "wrong password here"),
        // Unknown address entirely.
        ("nobody@example.com", "correct horse battery"),
        // Empty address.
        ("", "correct horse battery"),
    ] {
        let app = router(state.clone());
        let response = app
            .oneshot(post(
                "/api/v1/companies/acme/auth/login",
                serde_json::json!({ "email": email, "password": pw }),
            ))
            .await
            .unwrap();
        let status = response.status();
        seen.push((status, body_json(response).await));
    }
    let first = seen[0].clone();
    for entry in &seen {
        assert_eq!(entry.0, StatusCode::UNAUTHORIZED);
        assert_eq!(*entry, first, "login failures must be byte-identical");
    }
}

// ---------------------------------------------------------------------------
// Happy paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_manifest_admin_can_log_in_and_is_an_admin() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, sender) = state_with_mail(&home).await;
    // The manifest spells it "Ada@Example.com"; normalization must match.
    let cookie = login_via_link(&state, &sender, "ada@example.com").await;

    let app = router(state.clone());
    let response = app
        .oneshot(get_with_cookie("/api/v1/companies/acme/auth/me", &cookie))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let me = body_json(response).await;
    assert_eq!(me["email"], "ada@example.com");
    assert_eq!(me["role"], "admin", "the manifest bootstraps an admin");
    assert_eq!(me["company"], "acme");
    assert_eq!(me["hasPassword"], false);
}

#[tokio::test]
async fn a_link_is_single_use() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, sender) = state_with_mail(&home).await;
    let code = request_code(&state, &sender, "ada@example.com").await;

    let app = router(state.clone());
    let first = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/verify",
            serde_json::json!({ "code": code }),
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    // The same link again buys nothing — a forwarded mail is not a credential.
    let app = router(state.clone());
    let second = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/verify",
            serde_json::json!({ "code": code }),
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_second_link_within_the_throttle_window_is_not_sent() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, sender) = state_with_mail(&home).await;
    let first_code = request_code(&state, &sender, "ada@example.com").await;

    // Immediately ask again: same acknowledgement, no second mail. Otherwise
    // this route is a mail cannon pointed at an invited mailbox.
    let app = router(state.clone());
    let response = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/request",
            serde_json::json!({ "email": "ada@example.com" }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        serde_json::json!({ "sent": true }),
        "a throttled request must answer exactly like a sent one"
    );
    assert_eq!(sender.sent().len(), 1, "a second mail went out");

    // The live link still works: throttling must not let anyone invalidate
    // someone else's link on demand.
    let app = router(state.clone());
    let response = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/verify",
            serde_json::json!({ "code": first_code }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn once_the_window_passes_a_new_link_invalidates_the_previous_one() {
    use crate::ports::LoginCodeRecord;

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, sender) = state_with_mail(&home).await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();
    let now = crate::ports::now_millis();

    // Seed a code minted well outside the throttle window, standing in for
    // "they asked a few minutes ago". Clock control is not available here, so
    // the elapsed time is expressed in the record rather than by waiting.
    let old_plaintext =
        crate::server::users::token::mint_login_code(&crate::server::users::token::OsTokens);
    runtime
        .login_codes()
        .create(
            &id,
            &LoginCodeRecord {
                id: "old".into(),
                code_hash: crate::server::users::token::sha256_hex(&old_plaintext),
                email: "ada@example.com".into(),
                created_at_millis: now - 5 * 60 * 1000,
                expires_at_millis: now + 5 * 60 * 1000,
                consumed_at_millis: None,
            },
        )
        .await
        .unwrap();

    // Past the window, so a fresh link is minted and mailed.
    let new_code = request_code(&state, &sender, "ada@example.com").await;
    assert_ne!(new_code, old_plaintext);

    let app = router(state.clone());
    let response = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/verify",
            serde_json::json!({ "code": old_plaintext }),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "an abandoned link must not work once a newer one exists"
    );

    let app = router(state.clone());
    let response = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/verify",
            serde_json::json!({ "code": new_code }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn setting_a_password_enables_password_login() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, sender) = state_with_mail(&home).await;
    let cookie = login_via_link(&state, &sender, "ada@example.com").await;

    let app = router(state.clone());
    let response = app
        .oneshot(post_with_cookie(
            "/api/v1/companies/acme/auth/password",
            serde_json::json!({ "password": "correct horse battery" }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["hasPassword"], true);

    let app = router(state.clone());
    let response = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/login",
            serde_json::json!({
                "email": "ada@example.com",
                "password": "correct horse battery",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!session_cookie(&response).is_empty());
}

#[tokio::test]
async fn a_weak_password_is_refused() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, sender) = state_with_mail(&home).await;
    let cookie = login_via_link(&state, &sender, "ada@example.com").await;

    let app = router(state.clone());
    let response = app
        .oneshot(post_with_cookie(
            "/api/v1/companies/acme/auth/password",
            serde_json::json!({ "password": "short" }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn setting_a_password_requires_a_session() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _) = state_with_mail(&home).await;
    let app = router(state.clone());
    let response = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/password",
            serde_json::json!({ "password": "correct horse battery" }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_session_cookie_is_defended() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, sender) = state_with_mail(&home).await;
    let code = request_code(&state, &sender, "ada@example.com").await;
    let app = router(state.clone());
    let response = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/verify",
            serde_json::json!({ "code": code }),
        ))
        .await
        .unwrap();
    let set = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(set.starts_with("oc_session_acme="), "{set}");
    assert!(set.contains("HttpOnly"), "{set}");
    assert!(set.contains("SameSite=Lax"), "{set}");
    assert!(set.contains("Path=/"), "{set}");
    // Default config has no https public_url, so this is loopback dev.
    assert!(
        !set.contains("Secure"),
        "http dev must not set Secure or the cookie is dropped: {set}"
    );
}

#[tokio::test]
async fn a_https_deployment_marks_the_cookie_secure() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let sender = RecordingMailSender::new();
    let store = crate::store::FsCompanyStore::new(home.clone());
    let id = CompanyId::new("acme");
    store
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
    let runtime = RuntimeBuilder::new(home.clone(), manifest())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    // The hosted shape: the manager injects an https public URL.
    let state = AppState::new(AppConfig {
        public_url: Some("https://acme.example".into()),
        ..AppConfig::default()
    })
    .with_home(home.clone())
    .with_connections(
        ConnectionsRuntime::new()
            .with_mail(Arc::new(sender.clone()))
            .with_mail_credentials(MailCredentials::Smtp(SmtpCredentials {
                host: "smtp.test".into(),
                port: 587,
                security: SmtpSecurity::Starttls,
                username: "u".into(),
                password: SecretValue("p".into()),
                from_name: "Acme".into(),
                from_email: "noreply@acme.test".into(),
            })),
    );
    state.registry().insert(id, Arc::new(runtime));

    let code = request_code(&state, &sender, "ada@example.com").await;
    let app = router(state.clone());
    let response = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/verify",
            serde_json::json!({ "code": code }),
        ))
        .await
        .unwrap();
    let set = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        set.contains("Secure"),
        "an https host must set Secure: {set}"
    );
}

#[tokio::test]
async fn logout_revokes_the_session_not_just_the_cookie() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, sender) = state_with_mail(&home).await;
    let cookie = login_via_link(&state, &sender, "ada@example.com").await;

    let app = router(state.clone());
    let response = app
        .oneshot(post_with_cookie(
            "/api/v1/companies/acme/auth/logout",
            serde_json::json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );

    // The token must be dead server-side: clearing a cookie does nothing to a
    // copy of the token held anywhere else.
    let app = router(state.clone());
    let response = app
        .oneshot(get_with_cookie("/api/v1/companies/acme/auth/me", &cookie))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
