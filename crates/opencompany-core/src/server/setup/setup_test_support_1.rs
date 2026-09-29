//! HTTP-level tests for the first-run setup surface.
//!
//! The two things worth guarding hardest are the ones that are invisible from a
//! running host if they break: that an env-owned field cannot be "configured"
//! into a file nothing will read, and that the open-while-unconfigured access
//! gate closes the moment either of its two conditions stops holding.

use crate::company::CompanyManifest;
use crate::company::runtime::CompanyRuntime;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord, SecretValue};
use crate::runtime::{RebuildRequest, RuntimeBuilder, RuntimeRebuilder};
use crate::server::ops::ConnectionsRuntime;
use crate::server::ops::mailer::{MailCredentials, RecordingMailSender};
use crate::server::ops::smtp::{SmtpCredentials, SmtpSecurity};
use crate::server::router;
use crate::{AppConfig, AppState};
use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

pub(super) fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("opencompany-setup-")
        .tempdir()
        .expect("tempdir")
}

pub(super) fn manifest() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n").unwrap()
}

/// A loopback-bound host with an empty registry: a genuine first run.
pub(super) fn fresh_state(home: &std::path::Path) -> AppState {
    AppState::new(AppConfig {
        bind: "127.0.0.1:8080".to_string(),
        ..AppConfig::default()
    })
    .with_home(home.to_path_buf())
}

/// A loopback host with a mail transport wired — the shape where a magic link
/// is genuinely mailed rather than handed back in the response.
pub(super) fn state_with_mail(home: &std::path::Path) -> AppState {
    let connections = ConnectionsRuntime::new()
        .with_mail(Arc::new(RecordingMailSender::new()))
        .with_mail_credentials(MailCredentials::Smtp(SmtpCredentials {
            host: "smtp.test".into(),
            port: 587,
            security: SmtpSecurity::Starttls,
            username: "u".into(),
            password: SecretValue("p".into()),
            from_name: "Acme".into(),
            from_email: "noreply@acme.test".into(),
        }));
    fresh_state(home).with_connections(connections)
}

/// A routable host, where the anonymous gate must never open.
pub(super) fn routable_state(home: &std::path::Path) -> AppState {
    AppState::new(AppConfig {
        bind: "0.0.0.0:8080".to_string(),
        ..AppConfig::default()
    })
    .with_home(home.to_path_buf())
}

/// Registers `acme`, as a host that has already been used would have.
pub(super) async fn with_company(state: &AppState, home: &std::path::Path) -> CompanyId {
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
            overlay_tool_grants: None,
            overlay_desk_tools: std::collections::BTreeMap::new(),
            overlay_desk_order: Vec::new(),
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
        .unwrap();
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    state.registry().insert(id.clone(), Arc::new(runtime));
    id
}

pub(super) async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

pub(super) async fn get_setup(state: AppState) -> (StatusCode, serde_json::Value) {
    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/api/v1/setup")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

/// Reads the payload with an admin session, for the routable host — where the
/// anonymous gate is (correctly) shut and there is no other way in.
pub(super) async fn get_setup_as_admin(state: AppState) -> serde_json::Value {
    let cookie =
        crate::server::test_support::seed_session(&state, "acme", crate::ports::UserRole::Admin)
            .await;
    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/api/v1/setup")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    body_json(response).await
}

pub(super) async fn post_setup(
    state: AppState,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/setup")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

pub(super) fn field<'a>(dto: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    dto["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .find(|f| f["key"] == key)
        .unwrap_or_else(|| panic!("no field `{key}` in the payload"))
}

/// A rebuilder that fails for exactly the company ids named in `fails`, and
/// otherwise behaves like the production one.
pub(super) struct SelectiveRebuilder {
    pub(super) home: std::path::PathBuf,
    pub(super) fails: Vec<String>,
}

#[async_trait]
impl RuntimeRebuilder for SelectiveRebuilder {
    async fn rebuild(
        &self,
        state: &AppState,
        request: RebuildRequest,
    ) -> crate::Result<CompanyRuntime> {
        if self.fails.iter().any(|id| id == request.id.as_ref()) {
            return Err(crate::error::OpenCompanyError::Config(
                "simulated rebuild failure".to_string(),
            ));
        }
        // The auth-mode override is carried the way the boot rebuilder carries
        // it, or a company rebuilt here keeps the manifest default and the
        // mode the request chose is silently dropped.
        RuntimeBuilder::new(self.home.clone(), request.manifest)
            .with_id(request.id)
            .with_handover(request.handover)
            .with_auth_mode_override(state.auth_mode_override())
            .build()
            .await
    }
}

// ---------------------------------------------------------------------------
// The roster proposal, before any company exists
// ---------------------------------------------------------------------------
pub(super) async fn post_roster(
    state: AppState,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/setup/roster")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

// ---------------------------------------------------------------------------
// Applying a company the wizard designed
// ---------------------------------------------------------------------------

/// The manifest as it was persisted. `CompanyRuntime` exposes no accessor, and
/// the record is the thing a restart would read back anyway.
pub(super) async fn seeded_manifest(home: &std::path::Path, id: &str) -> CompanyManifest {
    let store = crate::store::FsCompanyStore::new(home.to_path_buf());
    store
        .load(&CompanyId::new(id))
        .await
        .expect("load")
        .expect("the seeded company has a record")
        .manifest
}

pub(super) fn designed_company(email: Option<&str>) -> serde_json::Value {
    let mut company = serde_json::json!({
        "industry": "E-commerce — I sell homeware online",
        "automate": "Meta ads, order dispatch",
        "agents": [
            { "name": "Meta Ads", "role": "Meta Ads Specialist", "description": "Campaigns and budgets." },
            { "name": "Dispatch", "role": "Order Dispatch Coordinator", "description": "Paid to delivered." },
            { "name": "Accounts", "role": "Accountant", "description": "Margins and spend." },
            { "name": "Ops", "role": "Operations Lead", "description": "Unblocks the team." }
        ]
    });
    if let Some(email) = email {
        company["adminEmail"] = serde_json::Value::String(email.to_string());
    }
    company
}
