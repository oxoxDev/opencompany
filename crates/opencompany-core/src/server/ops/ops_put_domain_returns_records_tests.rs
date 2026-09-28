//! HTTP-level tests for the ops write plane (domain, SMTP, inbox ingest).
//!
//! Every networked seam is exercised offline through injected mocks: a
//! [`StaticDnsResolver`](crate::company::dns::StaticDnsResolver) for domain
//! verify and a [`RecordingMailSender`](super::smtp::RecordingMailSender) for
//! the SMTP test send. The default build links no network crate.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::company::dns::StaticDnsResolver;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord, SecretValue};
use crate::runtime::RuntimeBuilder;
use crate::server::ops::ConnectionsRuntime;
use crate::server::ops::mailer::RecordingMailSender;
use crate::server::router;
#[cfg(not(feature = "webhooks"))]
use crate::server::webhook::DefaultHashSigner;
use crate::server::webhook::WebhookSigner;
use crate::{AppConfig, AppState};

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("opencompany-ops-")
        .tempdir()
        .expect("tempdir")
}

fn manifest() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap()
}

/// Builds state holding one running company `acme`, with `connections` injected.
async fn state_with(home: &std::path::Path, connections: ConnectionsRuntime) -> AppState {
    state_with_secrets(home, connections, None).await
}

/// [`state_with`], optionally over an injected
/// [`SecretStore`](crate::ports::SecretStore).
///
/// The seam exists for the tests that have to observe *when* the SMTP secrets
/// are read and written, which the on-disk store gives no way to see.
async fn state_with_secrets(
    home: &std::path::Path,
    connections: ConnectionsRuntime,
    secrets: Option<Arc<dyn crate::ports::SecretStore>>,
) -> AppState {
    let store = crate::store::FsCompanyStore::new(home.to_path_buf());
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
    let mut builder = RuntimeBuilder::new(home.to_path_buf(), manifest()).with_id(id.clone());
    if let Some(secrets) = secrets {
        builder = builder.with_secrets(secrets);
    }
    let runtime = builder.build().await.unwrap();
    let state = AppState::new(AppConfig::default())
        .with_home(home.to_path_buf())
        .with_connections(connections);
    state.registry().insert(id, Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn put_domain_returns_records() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, ConnectionsRuntime::new()).await;
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/domain")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"domain":"acme.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    assert_eq!(value["domain"], "acme.com");
    assert_eq!(value["verified"], false);
    assert_eq!(value["records"].as_array().unwrap().len(), 5);
}

#[tokio::test]
async fn verify_without_resolver_is_404_not_wired() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, ConnectionsRuntime::new()).await;
    let app = router(state);

    // Configure a domain first.
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/domain")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"domain":"acme.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/domain/verify")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let value = body_json(response).await;
    assert_eq!(value["code"], "not_wired");
}

/// The other refusal on this route, and the one nothing here had driven yet:
/// a resolver *is* wired, but no domain was ever `PUT`. `verify_without_resolver_is_404_not_wired`
/// covers the missing-dependency branch and `verify_with_resolver_marks_verified`
/// covers the happy path; this is `run_verify`'s own `stored.is_none()` guard,
/// which is neither of those.
#[tokio::test]
async fn verify_with_no_domain_configured_is_400() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let resolver = Arc::new(StaticDnsResolver::fully_verifying("acme.com"));
    let state = state_with(&home, ConnectionsRuntime::new().with_dns(resolver)).await;
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/domain/verify")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let value = body_json(response).await;
    assert_eq!(value["code"], "invalid_request");
    assert_eq!(value["error"], "invalid request: no domain configured");
}

#[tokio::test]
async fn verify_with_resolver_marks_verified() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let resolver = Arc::new(StaticDnsResolver::fully_verifying("acme.com"));
    let state = state_with(&home, ConnectionsRuntime::new().with_dns(resolver)).await;
    let app = router(state);

    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/domain")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"domain":"acme.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/domain/verify")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    assert_eq!(value["verified"], true);
}

#[tokio::test]
async fn put_smtp_hides_password() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, ConnectionsRuntime::new()).await;
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/smtp")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"host":"smtp.acme.test","port":587,"security":"starttls","username":"u","password":"top-secret","from_email":"ceo@acme.test"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!text.contains("top-secret"), "password leaked: {text}");
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["configured"], true);
    assert_eq!(value["host"], "smtp.acme.test");
}

#[tokio::test]
async fn smtp_test_without_sender_is_404() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, ConnectionsRuntime::new()).await;
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/smtp/test")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// The sender is wired (unlike the 404 case above) but nothing was ever
/// `PUT` to `/smtp`, so there are no credentials to send with. This is a
/// different refusal from "not wired" — a 400 naming the missing
/// configuration, not a 404 saying the feature does not exist on this host.
#[tokio::test]
async fn smtp_test_with_a_sender_but_no_stored_credentials_is_400() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let sender = Arc::new(RecordingMailSender::new());
    let state = state_with(&home, ConnectionsRuntime::new().with_mail(sender.clone())).await;
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/smtp/test")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        text.contains("no SMTP credentials configured"),
        "the refusal must name what is missing, not just fail: {text}"
    );
    assert!(
        sender.sent().is_empty(),
        "a refused test-send must never reach the sender"
    );
}

#[tokio::test]
async fn smtp_test_sends_and_records_outbound() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let sender = Arc::new(RecordingMailSender::new());
    let state = state_with(&home, ConnectionsRuntime::new().with_mail(sender.clone())).await;
    let app = router(state);

    // Store credentials.
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/smtp")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"host":"smtp.acme.test","port":587,"username":"u","password":"pw","from_email":"ceo@acme.test"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/smtp/test")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"to":"ops@acme.test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    assert_eq!(value["ok"], true);
    assert_eq!(sender.sent().len(), 1);
    assert_eq!(sender.sent()[0].1.to, "ops@acme.test");
}

#[tokio::test]
async fn ingest_bad_hmac_is_401_and_no_mail() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, ConnectionsRuntime::new()).await;
    // Seed the ingest secret.
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    runtime
        .secrets()
        .set(
            runtime.id(),
            super::INGEST_SECRET_KEY,
            SecretValue("s3cret".into()),
        )
        .await
        .unwrap();
    let app = router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/inboxes/ingest")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .header("x-opencompany-signature", "kh1=deadbeef")
                .body(Body::from(
                    r#"{"from":"a@x.test","to":"ceo@acme.test","subject":"hi","body":"yo"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // No mail was filed.
    assert!(
        runtime
            .inbox()
            .messages(runtime.id(), "ceo", usize::MAX, 0)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn ingest_good_hmac_files_mail() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, ConnectionsRuntime::new()).await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    runtime
        .secrets()
        .set(
            runtime.id(),
            super::INGEST_SECRET_KEY,
            SecretValue("s3cret".into()),
        )
        .await
        .unwrap();
    let app = router(state.clone());

    let payload = r#"{"from":"a@x.test","to":"ceo@acme.test","subject":"hi","body":"yo"}"#;
    // Sign with whatever signer this build actually verifies with, mirroring
    // `inbox::signer()`. Hardcoding DefaultHashSigner made this test pass only
    // in the default build and 401 under `--features webhooks`, where the route
    // verifies with HmacSha256Signer.
    #[cfg(feature = "webhooks")]
    let signature = crate::server::webhook::HmacSha256Signer.sign("s3cret", payload.as_bytes());
    #[cfg(not(feature = "webhooks"))]
    let signature = DefaultHashSigner.sign("s3cret", payload.as_bytes());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/inboxes/ingest")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .header("x-opencompany-signature", signature)
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let value = body_json(response).await;
    assert_eq!(value["inbox"], "ceo");
    let mail = runtime
        .inbox()
        .messages(runtime.id(), "ceo", usize::MAX, 0)
        .await
        .unwrap();
    assert_eq!(mail.len(), 1);
    assert_eq!(mail[0].from_email, "a@x.test");
    assert!(!mail[0].outbound);
}

// -- GET domain -------------------------------------------------------------

#[tokio::test]
async fn get_domain_is_null_before_one_is_configured() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, ConnectionsRuntime::new()).await;
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/company/domain")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // `null`, not a synthesized empty status — the same nullability
    // `Company.domain` reports over GraphQL, so the console has one shape to
    // branch on rather than two.
    assert_eq!(body_json(response).await, serde_json::Value::Null);
}

#[tokio::test]
async fn get_domain_returns_the_records_put_stored() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, ConnectionsRuntime::new()).await;
    let app = router(state);

    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/domain")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"domain":"acme.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/company/domain")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    assert_eq!(value["domain"], "acme.com");
    assert_eq!(value["verified"], false);
    // The records themselves, not just the domain: they are what the operator
    // copies into their DNS panel, and a read that dropped them would send them
    // back to the PUT response they no longer have.
    assert_eq!(value["records"].as_array().unwrap().len(), 5);
}

#[tokio::test]
async fn get_domain_carries_the_last_verify_result() {
    // The load-bearing one. Verification is a server-side pass whose outcome
    // lives only in the secret store; without it on the read, the console's
    // badge resets to Pending on every page reload and an operator who has
    // already published their records is told to publish them again.
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let resolver = Arc::new(StaticDnsResolver::fully_verifying("acme.com"));
    let state = state_with(&home, ConnectionsRuntime::new().with_dns(resolver)).await;
    let app = router(state);

    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/domain")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"domain":"acme.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/domain/verify")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/company/domain")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    assert_eq!(value["verified"], true, "{value}");
    let checks = value["checks"].as_array().expect("per-record checks");
    assert_eq!(checks.len(), 5, "{value}");
    assert!(checks.iter().all(|check| check["found"] == true), "{value}");
}

// -- GET smtp ---------------------------------------------------------------

#[tokio::test]
async fn get_smtp_is_unconfigured_before_any_credentials() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, ConnectionsRuntime::new()).await;
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/company/smtp")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    // An object saying `configured: false`, not `null`: the type carries the
    // flag, and the GraphQL twin is the non-null `SmtpStatus!`.
    assert_eq!(value["configured"], false, "{value}");
    assert!(value["host"].is_null(), "{value}");
}

#[tokio::test]
async fn get_smtp_never_returns_the_password() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, ConnectionsRuntime::new()).await;
    let app = router(state);

    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/smtp")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"host":"smtp.acme.test","port":587,"security":"starttls","username":"mailer","password":"read-back-secret","from_name":"Acme","from_email":"ceo@acme.test"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/company/smtp")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // Asserted on the raw bytes rather than a parsed field, like
    // `put_smtp_hides_password`: a field-by-field check only proves the fields
    // someone thought to name are clean, and a password leaking under a new key
    // would slip past it.
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        !text.contains("read-back-secret"),
        "password leaked: {text}"
    );
    // …while the read is still worth making: the form has something to render.
    assert!(text.contains("smtp.acme.test"), "{text}");
    assert!(text.contains("mailer"), "{text}");
    assert!(text.contains("ceo@acme.test"), "{text}");
}

#[tokio::test]
async fn saving_the_from_name_alone_keeps_the_stored_password() {
    // A patch, not a replace. The password is write-only, so a form can never
    // render it back; without this, correcting a display name would cost the
    // operator a credential they would have to go and look up again.
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let sender = Arc::new(RecordingMailSender::new());
    let state = state_with(&home, ConnectionsRuntime::new().with_mail(sender.clone())).await;
    let app = router(state);

    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/smtp")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"host":"smtp.acme.test","port":587,"security":"starttls","username":"mailer","password":"the-original-pw","from_name":"Acme","from_email":"ceo@acme.test"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // The same body again, minus the password, with only the display name changed.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/smtp")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"host":"smtp.acme.test","port":587,"security":"starttls","username":"mailer","from_name":"Acme Inc","from_email":"ceo@acme.test"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    assert_eq!(value["from_name"], "Acme Inc", "{value}");
    assert_eq!(value["configured"], true, "{value}");

    // The proof is what reaches the transport, not what is stored: a send after
    // the second save must still present the first save's password.
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/smtp/test")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let presented = sender.presented();
    assert_eq!(presented.len(), 1);
    let crate::server::ops::mailer::MailCredentials::Smtp(creds) = &presented[0];
    assert_eq!(creds.password.expose(), "the-original-pw");
    assert_eq!(creds.from_name, "Acme Inc");
}
