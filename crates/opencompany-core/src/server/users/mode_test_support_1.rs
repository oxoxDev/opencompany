//! End-to-end tests for the three sign-in modes.
//!
//! The mode is configuration, and configuration that silently does not take
//! effect is the failure this file exists to prevent. Three properties matter
//! more than the happy paths:
//!
//! 1. **A mode's routes are the only ones that answer.** A wallet company must
//!    not also accept a magic link, or the roster has a second door nobody
//!    configured. A `none` company must not accept either.
//! 2. **`none` really admits somebody.** A mode that turns the login off and
//!    then leaves every request unauthenticated is not "no sign-in", it is a
//!    bricked console — and it would look identical from the outside.
//! 3. **A wallet signature is checked against the challenge the host issued**,
//!    not against anything the caller supplied.

use crate::app::config::AuthMode;
use crate::company::CompanyManifest;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord, SecretValue};
use crate::runtime::RuntimeBuilder;
use crate::server::ops::ConnectionsRuntime;
use crate::server::ops::mailer::{MailCredentials, RecordingMailSender};
use crate::server::ops::smtp::{SmtpCredentials, SmtpSecurity};
use crate::{AppConfig, AppState};
use axum::body::{Body, to_bytes};
use axum::http::Request;
use ed25519_dalek::SigningKey;
use std::sync::Arc;

pub(super) fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-authmode-")
        .tempdir()
        .expect("tempdir")
}

/// A wallet keypair, from a fixed seed so a failure is reproducible.
pub(super) fn wallet(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

pub(super) fn address(key: &SigningKey) -> String {
    bs58::encode(key.verifying_key().to_bytes()).into_string()
}

/// Builds a host serving one company in `mode`, bootstrapping `bootstrap` as an
/// admin through whichever manifest list that mode reads.
pub(super) async fn state_in_mode(
    home: &std::path::Path,
    mode: AuthMode,
    bootstrap: Option<&str>,
) -> AppState {
    state_in_mode_on(
        home,
        mode,
        bootstrap,
        AppConfig::default(),
        ConnectionsRuntime::new(),
    )
    .await
}

/// The same host, over an explicit config and connection set — for the
/// questions whose answer is a property of the *deployment* rather than the
/// mode: whether the bind is routable, and whether mail is wired.
pub(super) async fn state_in_mode_on(
    home: &std::path::Path,
    mode: AuthMode,
    bootstrap: Option<&str>,
    config: AppConfig,
    connections: ConnectionsRuntime,
) -> AppState {
    let toml_src = match (mode, bootstrap) {
        (AuthMode::Email, Some(who)) => {
            format!("[company]\nname = \"Acme\"\n[users]\nmode = \"email\"\nadmins = [\"{who}\"]\n")
        }
        (AuthMode::Wallet, Some(who)) => {
            format!(
                "[company]\nname = \"Acme\"\n[users]\nmode = \"wallet\"\nwallets = [\"{who}\"]\n"
            )
        }
        (mode, _) => format!(
            "[company]\nname = \"Acme\"\n[users]\nmode = \"{}\"\n",
            mode.as_str()
        ),
    };
    let manifest: CompanyManifest = toml::from_str(&toml_src).expect("valid manifest");
    assert!(
        manifest.validate().is_empty(),
        "the test manifest must be valid: {:?}",
        manifest.validate()
    );

    let store = crate::store::FsCompanyStore::new(home.to_path_buf());
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
    let state = AppState::new(config)
        .with_home(home.to_path_buf())
        .with_connections(connections);
    state.registry().insert(id, Arc::new(runtime));
    state
}

/// A routable bind: nothing is echoed back to the caller here, so a magic link
/// is only usable if it can genuinely be mailed.
pub(super) fn routable() -> AppConfig {
    AppConfig {
        bind: "0.0.0.0:8080".to_string(),
        ..AppConfig::default()
    }
}

/// A wired mail transport, so a link is actually sent.
pub(super) fn mail_connections() -> ConnectionsRuntime {
    ConnectionsRuntime::new()
        .with_mail(Arc::new(RecordingMailSender::new()))
        .with_mail_credentials(MailCredentials::Smtp(SmtpCredentials {
            host: "smtp.test".into(),
            port: 587,
            security: SmtpSecurity::Starttls,
            username: "u".into(),
            password: SecretValue("p".into()),
            from_name: "Acme".into(),
            from_email: "noreply@acme.test".into(),
        }))
}

pub(super) fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

pub(super) fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

pub(super) async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

pub(super) fn post_with_cookie(uri: &str, body: serde_json::Value, cookie: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Extracts the session cookie's `name=value` pair from a `Set-Cookie` header.
pub(super) fn session_cookie(response: &axum::response::Response) -> String {
    let set = response
        .headers()
        .get("set-cookie")
        .expect("a session response must set a cookie")
        .to_str()
        .unwrap();
    set.split(';').next().unwrap().to_string()
}
