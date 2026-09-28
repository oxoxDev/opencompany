use super::*;
use async_trait::async_trait;
use serde_json::json;

use axum::body::Body;
use axum::http::Request;

use crate::company::CompanyManifest;
use crate::ports::types::{CompressedTrace, CycleRequest, CycleResult, TokenUsage};
use crate::ports::users::{UserRecord, UserRole, UserStatus};
use crate::ports::{Brain, CompanyStore, CycleHost, SessionKind, SessionRecord};
use crate::server::graphql::auth::UserPrincipal;
use crate::server::users::cookie::session_cookie_name;
use crate::server::users::token::{OsTokens, mint_session_token, sha256_hex};
use crate::store::FsCompanyStore;
use crate::{AppConfig, ports::types::CompanyRecord};

/// A brain that answers a cycle with nothing, so the ACP `prompt` turn
/// completes without an inference credential. The notification this suite
/// asserts on is filed before the turn runs, so the empty answer is fine.
pub(super) struct SilentBrain;

#[async_trait]
impl Brain for SilentBrain {
    async fn run_cycle(
        &self,
        req: CycleRequest,
        _host: &dyn CycleHost,
    ) -> crate::Result<CycleResult> {
        Ok(CycleResult {
            channel_responses: Vec::new(),
            new_traces: vec![CompressedTrace::now(req.cycle_id, "silent test brain")],
            ledger_deltas: Vec::new(),
            token_usage: TokenUsage::default(),
        })
    }
}

pub(super) async fn seed_user(
    state: &AppState,
    company: &CompanyId,
    id: &str,
    display: &str,
) -> String {
    let runtime = state.registry().get(company).expect("company");
    let now = crate::ports::now_millis();
    runtime
        .users()
        .upsert_user(
            company,
            &UserRecord {
                id: id.to_string(),
                email: format!("{id}@example.test"),
                display_name: Some(display.to_string()),
                avatar: None,
                role: UserRole::Member,
                status: UserStatus::Active,
                password_hash: None,
                must_change_password: false,
                created_at_millis: now,
                last_seen_at_millis: None,
                updated_at_millis: now,
            },
        )
        .await
        .expect("seed_user: upsert");
    id.to_string()
}

/// A host whose registry runtime answers cycles with [`SilentBrain`], on
/// the `acp,runner,tinymemory` lane — the one that executes `server::acp`.
pub(super) async fn acp_state(home: &std::path::Path) -> AppState {
    let manifest: CompanyManifest = toml::from_str(
        r#"
[company]
name = "Acme"

[[agent]]
id = "product_manager"
role = "Product Manager"

[[agent]]
id = "writer"
role = "Writer"

[[group_chat]]
id = "engineering"
name = "Engineering"
members = []

[[group_chat]]
id = "writer"
name = "Writer desk"
members = []

[policy]
mode = "full"
"#,
    )
    .unwrap();
    let store = FsCompanyStore::new(home.to_path_buf());
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
    let runtime = crate::runtime::RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .with_brain(std::sync::Arc::new(SilentBrain))
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    state
}

/// Same as [`acp_state`], but the sole company is `[users] mode = "none"`
/// — the packaged-desktop shape with no sign-in, reachable by anyone who
/// can reach the loopback bind at all.
pub(super) async fn acp_state_none_mode(home: &std::path::Path) -> AppState {
    let manifest: CompanyManifest = toml::from_str(
        r#"
[company]
name = "Acme"

[[agent]]
id = "product_manager"
role = "Product Manager"

[[group_chat]]
id = "engineering"
name = "Engineering"
members = []

[policy]
mode = "full"

[users]
mode = "none"
"#,
    )
    .unwrap();
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
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
            overlay_desk_order: Vec::new(),
            overlay_desk_hive: Vec::new(),
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
    let runtime = crate::runtime::RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .with_brain(std::sync::Arc::new(SilentBrain))
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    state
}

/// Builds a bare JSON-RPC `POST /acp` request with no auth headers.
pub(super) fn acp_call_request(body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/acp")
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

pub(super) fn admin_auth(
    company: &CompanyId,
    user_id: String,
    session_token_hash: &str,
) -> GqlAuth {
    GqlAuth::User(UserPrincipal {
        company: company.clone(),
        user_id,
        email: "admin@example.test".to_string(),
        role: UserRole::Admin,
        must_change_password: false,
        session_token_hash: session_token_hash.to_string(),
        credential: crate::ports::SessionKind::Browser,
    })
}

/// Mints a real, HTTP-carriable admin session for `acp_state`'s "acme"
/// company, returning its `Cookie` header value.
pub(super) async fn seed_admin_session_cookie(
    state: &AppState,
    company: &CompanyId,
    user_id: &str,
) -> String {
    let runtime = state.registry().get(company).expect("company");
    let now = crate::ports::now_millis();
    runtime
        .users()
        .upsert_user(
            company,
            &UserRecord {
                id: user_id.to_string(),
                email: format!("{user_id}@example.test"),
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
        .expect("seed admin");
    let token = mint_session_token(&OsTokens);
    runtime
        .sessions()
        .create(
            company,
            &SessionRecord {
                id: format!("s-{user_id}"),
                token_hash: sha256_hex(&token),
                user_id: user_id.to_string(),
                created_at_millis: now,
                expires_at_millis: now + 60_000,
                user_agent: None,
                kind: SessionKind::Browser,
                label: None,
            },
        )
        .await
        .expect("seed session");
    let cookie_name = session_cookie_name(company).expect("cookie name");
    format!("{cookie_name}={token}")
}

pub(super) fn session_new_request(
    cookie: &str,
    connection_id: &str,
    request_id: u64,
) -> Request<Body> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "session/new",
        "params": {
            "_meta": {
                "opencompany": { "company": "acme" },
                "opencompany/connectionId": connection_id,
            }
        }
    });
    let mut request = acp_call_request(body);
    request
        .headers_mut()
        .insert(axum::http::header::COOKIE, cookie.parse().unwrap());
    request
}
