//! Shared fixtures for the `composio.rs` test-file split.
//!
//! Every `composio_*_tests.rs` sibling module pulls its request helpers,
//! manifest fixtures, and catalog builders from here rather than redefining
//! them, so the mechanical split into one-file-per-test-group did not leave
//! several divergent copies of the same fixture.

use super::{CatalogEntry, CredentialSource, TinyhumansTokenSource, access_for};
use crate::company::runtime::CompanyRuntime;
use crate::server::error::ApiError;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
#[cfg(feature = "composio")]
use axum::routing::post;
#[cfg(feature = "composio")]
use axum::{Json, Router};
use serde_json::Value;
#[cfg(feature = "composio")]
use serde_json::json;
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

pub(super) const TOKEN: &str = "composio-tenant-bearer-SECRET-xyz";

pub(super) fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-composio-")
        .tempdir()
        .expect("tempdir")
}

/// A loopback Composio authorize endpoint for the feature-enabled route
/// test. Keeping the HTTP boundary real proves the handler reaches the
/// client after the admin guard, without allowing a unit test to dial the
/// production backend.
#[cfg(feature = "composio")]
pub(super) async fn spawn_authorize_backend() -> String {
    let app = Router::new().route(
        "/agent-integrations/composio/authorize",
        post(|Json(body): Json<Value>| async move {
            assert_eq!(body["toolkit"], "gmail");
            Json(json!({
                "success": true,
                "data": {
                    "connectUrl": "https://composio.test/connect/gmail",
                    "connectionId": "gmail-connection"
                }
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

pub(super) async fn state_with_manifest(home: &std::path::Path, manifest_toml: &str) -> AppState {
    state_with_manifest_id(home, "acme", manifest_toml).await
}

/// The same, under an explicit company id.
///
/// The catalog cache is process-wide and keyed by company, so the tests that
/// seed it need ids of their own — two tests sharing `acme` would share an
/// entry and race.
pub(super) async fn state_with_manifest_id(
    home: &std::path::Path,
    company: &str,
    manifest_toml: &str,
) -> AppState {
    use crate::ports::CompanyStore;
    let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new(company);
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
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, company).await;
    state
}

pub(super) async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value, String) {
    send_for(state, "acme", method, uri, body).await
}

/// [`send`] against a named company's session.
pub(super) async fn send_for(
    state: &AppState,
    company: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value, String) {
    send_as(
        state,
        method,
        uri,
        body,
        Auth::Cookie(crate::server::test_support::fixed_cookie(company)),
    )
    .await
}

/// How a request presents itself, so the role boundary can be driven with
/// an admin session, a member session, or a machine credential.
pub(super) enum Auth {
    Cookie(String),
    Bearer(String),
}

pub(super) async fn send_as(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<Value>,
    auth: Auth,
) -> (StatusCode, Value, String) {
    let request = Request::builder().method(method).uri(uri);
    let request = match auth {
        Auth::Cookie(cookie) => request.header("cookie", cookie),
        Auth::Bearer(token) => request.header("authorization", format!("Bearer {token}")),
    };
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let raw = String::from_utf8_lossy(&bytes).to_string();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value, raw)
}

/// The company registered under `company` in `state`.
pub(super) fn runtime_of(state: &AppState, company: &str) -> std::sync::Arc<super::CompanyRuntime> {
    state
        .registry()
        .get(&CompanyId::new(company))
        .expect("company is registered")
}

/// A hundred-provider catalog, the shape the backend actually returns —
/// each entry carrying the display metadata #600 stopped discarding.
pub(super) fn hundred_entries() -> Vec<CatalogEntry> {
    (0..100)
        .map(|i| CatalogEntry {
            slug: format!("provider{i:03}"),
            name: format!("Provider {i:03}"),
            description: format!("Does provider-{i:03} things."),
            logo: Some(format!("https://logos.example.test/provider{i:03}")),
            categories: vec!["productivity".to_string()],
        })
        .collect()
}

/// Just the slugs of [`hundred_entries`], for asserting on the slug list
/// the wire has always carried.
pub(super) fn hundred_slugs() -> Vec<String> {
    hundred_entries().into_iter().map(|e| e.slug).collect()
}

/// Serialises this test against `an_admin_is_unaffected`'s env mutation.
///
/// In composio builds that test repoints `TINYHUMANS_API_URL_ENV` at a
/// loopback backend for its whole body (Composio's own backend-URL override,
/// `OPENCOMPANY_COMPOSIO_BACKEND_URL`, was removed in phase 6a of #2306; the
/// tenant's shared API base is the only repoint left). The cache-seeding
/// tests below derive a cache key that embeds that URL, seed the cache under
/// it, then re-derive the key on the request path — a process-wide override
/// landing between the two reads would change the key and strand the seeded
/// entry, failing the `catalogSource == "backend"` assertion. `EnvVarGuard`
/// serialises guard users against each other, so taking it here closes the
/// race the way the crate documents (unguarded `std::env::var` readers are
/// otherwise fair game). Gated to composio builds, where the mutation
/// exists.
#[cfg(feature = "composio")]
pub(super) fn composio_backend_env_guard() -> crate::test_support::EnvVarGuard {
    crate::test_support::EnvVarGuard::capture(&[crate::company::composio::TINYHUMANS_API_URL_ENV])
}

pub(super) async fn read_slot(
    runtime: &super::CompanyRuntime,
    key: &'static str,
) -> Option<String> {
    runtime
        .secrets()
        .get(runtime.id(), key)
        .await
        .unwrap()
        .map(|crate::ports::types::SecretValue(v)| v)
}

pub(super) async fn credential_source_for(
    runtime: &CompanyRuntime,
    token_source: Option<std::sync::Arc<TinyhumansTokenSource>>,
) -> Result<CredentialSource, ApiError> {
    Ok(access_for(runtime, token_source).await?.1)
}

pub(super) const GRANTED: &str = "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
     [tools]\nallow = [\"composio\"]\n[tools.composio]\ntoolkits = [\"gmail\"]\n";
