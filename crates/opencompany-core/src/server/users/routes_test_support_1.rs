//! End-to-end tests for the login and admin routes.

use crate::company::CompanyManifest;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord, SecretValue};
use crate::runtime::RuntimeBuilder;
use crate::server::ops::ConnectionsRuntime;
use crate::server::ops::mailer::{MailCredentials, RecordingMailSender};
use crate::server::ops::smtp::{SmtpCredentials, SmtpSecurity};
use crate::server::router;
use crate::{AppConfig, AppState};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

pub(super) fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-routes-")
        .tempdir()
        .expect("tempdir")
}

/// A manifest whose `[users] admins` bootstraps `ada` — deliberately spelled
/// with capitals, so normalization is exercised end to end.
pub(super) fn manifest() -> CompanyManifest {
    toml::from_str(
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [users]\nadmins = [\"Ada@Example.com\"]\n",
    )
    .unwrap()
}

/// A manifest with **no** `[users] admins` — the shape a company the platform
/// provisions boots with, and the reason issue #321 exists: nobody is eligible
/// and there is no operator token to send the first invite with.
pub(super) fn manifest_without_admins() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap()
}

pub(super) async fn state_with(
    home: &std::path::Path,
    connections: ConnectionsRuntime,
) -> AppState {
    state_bound_to(home, &AppConfig::default().bind, connections).await
}

/// State on an explicit bind, for the tests that turn on whether the host looks
/// reachable from anywhere but this machine.
pub(super) async fn state_bound_to(
    home: &std::path::Path,
    bind: &str,
    connections: ConnectionsRuntime,
) -> AppState {
    state_from(
        home,
        manifest(),
        AppConfig {
            bind: bind.to_string(),
            ..AppConfig::default()
        },
        connections,
    )
    .await
}

/// State over an explicit manifest and config — the seam the bootstrap-admin
/// tests need, since they turn on both.
pub(super) async fn state_from(
    home: &std::path::Path,
    manifest: CompanyManifest,
    config: AppConfig,
    connections: ConnectionsRuntime,
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

/// A recording mail sender wired as a transport, so links are "delivered".
pub(super) fn mail_connections() -> (ConnectionsRuntime, RecordingMailSender) {
    let sender = RecordingMailSender::new();
    let connections = ConnectionsRuntime::new()
        .with_mail(Arc::new(sender.clone()))
        .with_mail_credentials(MailCredentials::Smtp(SmtpCredentials {
            host: "smtp.test".into(),
            port: 587,
            security: SmtpSecurity::Starttls,
            username: "u".into(),
            password: SecretValue("p".into()),
            from_name: "Acme".into(),
            from_email: "noreply@acme.test".into(),
        }));
    (connections, sender)
}

/// State with a recording mail sender wired, so links are "delivered".
pub(super) async fn state_with_mail(home: &std::path::Path) -> (AppState, RecordingMailSender) {
    let (connections, sender) = mail_connections();
    (state_with(home, connections).await, sender)
}

/// State with mail wired over an explicit manifest and `OPENCOMPANY_ADMIN_EMAIL`
/// value — `None` being the pre-#321 deployment.
pub(super) async fn state_with_admin_email(
    home: &std::path::Path,
    manifest: CompanyManifest,
    admin_email: Option<&str>,
) -> (AppState, RecordingMailSender) {
    let (connections, sender) = mail_connections();
    let config = AppConfig {
        admin_email: admin_email.map(str::to_string),
        ..AppConfig::default()
    };
    (
        state_from(home, manifest, config, connections).await,
        sender,
    )
}

pub(super) async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

pub(super) fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
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

pub(super) fn get_with_cookie(uri: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("cookie", cookie)
        .body(Body::empty())
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

/// Requests a link for `email` and returns the dev-echoed code, if any.
pub(super) async fn request_dev_code(state: &AppState, email: &str) -> Option<String> {
    let app = router(state.clone());
    let response = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/request",
            serde_json::json!({ "email": email }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["sent"], true, "the response must never vary");
    json["dev_code"].as_str().map(str::to_string)
}

/// The login code from the most recent mail the recorder captured.
///
/// With a transport wired the code is deliberately *not* echoed in the
/// response, so tests read it the way a user would: out of the mail.
pub(super) fn code_from_last_mail(sender: &RecordingMailSender) -> String {
    let sent = sender.sent();
    let body = &sent.last().expect("no mail was sent").1.body;
    body.split("code=")
        .nth(1)
        .expect("the mail must contain a login link")
        .split_whitespace()
        .next()
        .expect("the link must carry a code")
        .to_string()
}

/// Requests a link for `email` and returns the code, read out of the mail.
pub(super) async fn request_code(
    state: &AppState,
    sender: &RecordingMailSender,
    email: &str,
) -> String {
    let echoed = request_dev_code(state, email).await;
    assert_eq!(
        echoed, None,
        "a host with mail wired must never echo the code"
    );
    code_from_last_mail(sender)
}

/// Logs `email` in via the magic link, returning the session cookie.
pub(super) async fn login_via_link(
    state: &AppState,
    sender: &RecordingMailSender,
    email: &str,
) -> String {
    let code = request_code(state, sender, email).await;
    let app = router(state.clone());
    let response = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/verify",
            serde_json::json!({ "code": code }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    session_cookie(&response)
}

// ---------------------------------------------------------------------------
// The header carrier — a hub console that cannot receive a cookie
// ---------------------------------------------------------------------------

/// A login request that asks for a session the client will carry itself.
fn post_wanting_header_carrier(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header(super::cookie::SESSION_CARRIER_HEADER, "header")
        .body(Body::from(body.to_string()))
        .unwrap()
}

pub(super) fn get_with_session_header(uri: &str, session: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(super::cookie::SESSION_HEADER, session)
        .body(Body::empty())
        .unwrap()
}

/// Signs `email` in asking for the header carrier, returning the whole body.
pub(super) async fn login_wanting_header_carrier(
    state: &AppState,
    sender: &RecordingMailSender,
    email: &str,
) -> (axum::http::HeaderMap, serde_json::Value) {
    let code = request_code(state, sender, email).await;
    let app = router(state.clone());
    let response = app
        .oneshot(post_wanting_header_carrier(
            "/api/v1/companies/acme/auth/verify",
            serde_json::json!({ "code": code }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    (headers, body_json(response).await)
}

/// Looks a user's id up through the admin roster.
pub(super) async fn user_id(state: &AppState, admin: &str, email: &str) -> String {
    let app = router(state.clone());
    let response = app
        .oneshot(get_with_cookie("/api/v1/companies/acme/users", admin))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response)
        .await
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["email"] == email)
        .unwrap_or_else(|| panic!("no user {email}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------
// Invite mail (issue #584)
//
// Adding a person used to write a record and mail nobody, while the console
// reported unconditional success. These turn on both halves: that a mail
// actually goes out, and that when it does not the caller is told so rather
// than being congratulated.
// ---------------------------------------------------------------------------

/// A wired transport that refuses mail to one address and delivers the rest.
///
/// "No mail configured" is not the only way an invite fails to arrive, and it
/// is the less dangerous one — it is at least visible in configuration. A
/// wired transport that rejects the message is the case where a success toast
/// is a lie, so it needs a mock of its own.
///
/// Refusal is per-recipient rather than global, for a reason that is not
/// convenience: a sender that failed everything would also fail the admin's own
/// login mail, leaving no way to reach the admin-authenticated route under
/// test. It is also the truer model — a transport rejects a *message*, not a
/// process.
#[derive(Clone)]
struct RefusingMailSender {
    refuse: String,
    accepted: RecordingMailSender,
}

#[async_trait::async_trait]
impl crate::server::ops::mailer::MailSender for RefusingMailSender {
    async fn send(
        &self,
        creds: &MailCredentials,
        email: &crate::server::ops::mailer::OutboundEmail,
    ) -> Result<(), crate::error::OpenCompanyError> {
        if email.to == self.refuse {
            // The same variant the real SMTP sender reports a rejected send
            // with, so this exercises the branch production actually takes.
            return Err(crate::error::OpenCompanyError::Store(
                "smtp send: the transport refused the message".to_string(),
            ));
        }
        self.accepted.send(creds, email).await
    }
}

/// State whose transport works except for mail addressed to `refuse`.
///
/// Returns the recorder of everything it *did* accept, so a test can assert
/// that the refused message is genuinely absent rather than merely unreported.
pub(super) async fn state_refusing_mail_to(
    home: &std::path::Path,
    refuse: &str,
) -> (AppState, RecordingMailSender) {
    let accepted = RecordingMailSender::new();
    let sender = RefusingMailSender {
        refuse: refuse.to_string(),
        accepted: accepted.clone(),
    };
    let connections = ConnectionsRuntime::new()
        .with_mail(Arc::new(sender))
        .with_mail_credentials(MailCredentials::Smtp(SmtpCredentials {
            host: "smtp.test".into(),
            port: 587,
            security: SmtpSecurity::Starttls,
            username: "u".into(),
            password: SecretValue("p".into()),
            from_name: "Acme".into(),
            from_email: "noreply@acme.test".into(),
        }));
    (state_with(home, connections).await, accepted)
}

/// A transport that revokes the invite it is delivering, mid-send.
///
/// The race being modelled is an admin pressing Revoke while the SMTP round
/// trip is still in flight — the exact window the route's post-send stamp sits
/// in. Revoking from inside `send` reproduces that window deterministically:
/// no sleep, no second task, and the route is provably still holding its
/// pre-send copy of the record when the revocation lands.
#[derive(Clone)]
pub(super) struct RevokingMailSender {
    /// Filled once the state exists, since the runtime this revokes through is
    /// the one the state owns — and the state cannot be built until the sender
    /// it borrows is already wired into its connections.
    pub(super) runtime: Arc<std::sync::OnceLock<Arc<crate::runtime::CompanyRuntime>>>,
    pub(super) revoke_for: String,
    pub(super) accepted: RecordingMailSender,
}

#[async_trait::async_trait]
impl crate::server::ops::mailer::MailSender for RevokingMailSender {
    async fn send(
        &self,
        creds: &MailCredentials,
        email: &crate::server::ops::mailer::OutboundEmail,
    ) -> Result<(), crate::error::OpenCompanyError> {
        if email.to == self.revoke_for {
            let runtime = self
                .runtime
                .get()
                .expect("the runtime is wired before any invite is sent");
            let invite = runtime
                .users()
                .find_invite_by_email(runtime.id(), &self.revoke_for)
                .await
                .unwrap()
                .expect("the grant lands before the mail goes out");
            assert!(
                runtime
                    .users()
                    .delete_invite(runtime.id(), &invite.id)
                    .await
                    .unwrap(),
                "the revocation this models must actually remove the invite"
            );
        }
        self.accepted.send(creds, email).await
    }
}

/// Signs an admin in on a host with no mail transport, via the dev echo.
pub(super) async fn login_via_dev_code(state: &AppState, email: &str) -> String {
    let code = request_dev_code(state, email)
        .await
        .expect("a loopback host with no transport echoes the code");
    let app = router(state.clone());
    let response = app
        .oneshot(post(
            "/api/v1/companies/acme/auth/verify",
            serde_json::json!({ "code": code }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    session_cookie(&response)
}

/// Invites `email` as `admin`, returning the status and decoded body.
pub(super) async fn invite_as(
    state: &AppState,
    admin: &str,
    email: &str,
) -> (StatusCode, serde_json::Value) {
    let app = router(state.clone());
    let response = app
        .oneshot(post_with_cookie(
            "/api/v1/companies/acme/users/invites",
            serde_json::json!({ "email": email }),
            admin,
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

// ---------------------------------------------------------------------------
// The profile: naming yourself and choosing your own face
// (docs/spec/runtime/avatars.md)
// ---------------------------------------------------------------------------
pub(super) fn patch_with_cookie(uri: &str, body: serde_json::Value, cookie: &str) -> Request<Body> {
    Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .body(Body::from(body.to_string()))
        .unwrap()
}

pub(super) async fn patch_me(
    state: &AppState,
    cookie: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router(state.clone())
        .oneshot(patch_with_cookie(
            "/api/v1/companies/acme/auth/me",
            body,
            cookie,
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

// ---------------------------------------------------------------------------
// Materialization races (issue #1833)
// ---------------------------------------------------------------------------

/// A runtime over a company with no `[users] admins`, which is all these need:
/// `local_owner_record` answers a mode question nobody asks it, so the manifest
/// only has to produce a store.
pub(super) async fn users_runtime(home: &std::path::Path) -> Arc<crate::CompanyRuntime> {
    let (connections, _sender) = mail_connections();
    let state = state_from(
        home,
        manifest_without_admins(),
        AppConfig::default(),
        connections,
    )
    .await;
    state.registry().get(&CompanyId::new("acme")).unwrap()
}
