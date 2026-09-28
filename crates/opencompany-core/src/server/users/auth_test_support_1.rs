//! Security tests for the user principal.
//!
//! These exist to pin the properties that make session cookies safe to accept
//! at all. Each one is a thing that, if it broke, would be a vulnerability
//! rather than a bug: a user reaching the operator write plane, a session
//! working against the wrong company, a suspended user still being served.

use crate::company::CompanyManifest;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::ports::{CompanyStore, SessionKind, SessionRecord, UserRecord, UserRole, UserStatus};
use crate::runtime::RuntimeBuilder;
use crate::server::users::cookie::session_cookie_name;
use crate::server::users::token::{OsTokens, mint_session_token, sha256_hex};
use crate::{AppConfig, AppState};
use std::sync::Arc;

pub(super) fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-userauth-")
        .tempdir()
        .expect("tempdir")
}

pub(super) fn manifest() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap()
}

/// Builds state holding the named running companies.
pub(super) async fn state_with(home: &std::path::Path, companies: &[&str]) -> AppState {
    let store = crate::store::FsCompanyStore::new(home.to_path_buf());
    let state = AppState::new(AppConfig::default()).with_home(home.to_path_buf());
    for name in companies {
        let id = CompanyId::new(*name);
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
        let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest())
            .with_id(id.clone())
            .build()
            .await
            .unwrap();
        state.registry().insert(id, Arc::new(runtime));
    }
    state
}

/// Seeds an active user with a live session in `company`, returning the
/// plaintext session token the browser would hold.
pub(super) async fn seed_session(
    state: &AppState,
    company: &str,
    role: UserRole,
    status: UserStatus,
) -> String {
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
                avatar: None,
                role,
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
                // Only the hash is stored — the plaintext goes to the browser.
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

pub(super) fn cookie_header(company: &str, token: &str) -> String {
    format!(
        "{}={token}",
        session_cookie_name(&CompanyId::new(company)).unwrap()
    )
}

pub(super) fn headers_with_cookie(company: &str, token: &str) -> axum::http::HeaderMap {
    let mut h = axum::http::HeaderMap::new();
    h.insert(
        axum::http::header::COOKIE,
        cookie_header(company, token).parse().unwrap(),
    );
    h
}

// ---------------------------------------------------------------------------
// The header carrier
//
// A desktop client is cross-site with every server it talks to, and a
// `SameSite=Lax` cookie is never sent cross-site — so the same session has to
// be presentable as a header. These mirror the cookie tests above one for one:
// the carrier changed, so every property the cookie tests pin has to be
// re-pinned rather than assumed to carry over. The dangerous outcome is not the
// header failing, it is the header succeeding somewhere the cookie would not.
// ---------------------------------------------------------------------------
pub(super) fn session_header_value(company: &str, token: &str) -> String {
    format!("{company}.{token}")
}

pub(super) fn headers_with_session_header(company: &str, token: &str) -> axum::http::HeaderMap {
    let mut h = axum::http::HeaderMap::new();
    h.insert(
        crate::server::users::cookie::SESSION_HEADER,
        session_header_value(company, token).parse().unwrap(),
    );
    h
}
