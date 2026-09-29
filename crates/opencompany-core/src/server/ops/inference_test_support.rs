//! Shared fixtures and helpers for the `inference` test clusters.
//!
//! These were originally private items of a single inline `mod tests {
//! .. }` in `inference.rs`. The mechanical split that pulled the tests out
//! into sibling `inference_*_tests.rs` files duplicated several of them
//! verbatim across those files; this module keeps one copy of each so the
//! split files can share it via `use super::inference_test_support::*;`.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use super::*;

use crate::company::CompanyManifest;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

pub(super) const TOKEN: &str = "sk-super-secret-inference-token-XYZ";

pub(super) fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-inference-")
        .tempdir()
        .expect("tempdir")
}

pub(super) fn manifest() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap()
}

/// A manifest whose **only** inference lives in the default harness's
/// `[harness.inference]` — no company-level `[inference]` section at all.
/// `openhuman`-gated like its only caller: under the default build the
/// harness-wiring path the fix targets is compiled out, and an unused
/// helper would trip `clippy -D warnings`.
#[cfg(feature = "openhuman")]
pub(super) fn manifest_with_harness_inference() -> CompanyManifest {
    toml::from_str(
        r#"[company]
name = "Acme"
[policy]
mode = "full"

[[harness]]
id = "embedded"
kind = "built_in"
default = true

[harness.inference]
provider = "openai_compatible"
base_url = "https://byo.example/v1"
"#,
    )
    .unwrap()
}

/// Commits `manifest` as `id`'s record — what `manifest_inference` reads.
pub(super) async fn save_record(
    home: &std::path::Path,
    id: &CompanyId,
    manifest: &CompanyManifest,
) {
    use crate::ports::CompanyStore;
    FsCompanyStore::new(home.to_path_buf())
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
}

pub(super) async fn state_with_company(home: &std::path::Path) -> AppState {
    state_with_company_named(home, "acme").await
}

/// A company under a caller-chosen id.
///
/// Almost every test here can share `acme`, but the catalog cache is
/// process-global and keyed on the company, and storing a key now evicts
/// that company's authenticated entries (Codex review on #2045). A test that
/// rotates a credential therefore wipes the seeded fixtures of every sibling
/// running beside it under the same id — libtest runs these in parallel — so
/// it needs an id of its own rather than an ordering assumption that cannot
/// hold.
pub(super) async fn state_with_company_named(home: &std::path::Path, name: &str) -> AppState {
    let id = CompanyId::new(name);
    save_record(home, &id, &manifest()).await;
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, name).await;
    state
}

/// [`state_with_company`] over a caller-supplied manifest.
///
/// The strict `validate()` only runs on a first boot with no persisted
/// record (`src/runtime/builder.rs`), and `save_record` writes one first —
/// so this can plant a manifest a fresh company would now be refused. That
/// is the point: an endpoint stored before the refusal existed is exactly
/// the case the redaction half of the rule is for.
pub(super) async fn state_with_manifest(
    home: &std::path::Path,
    name: &str,
    manifest_toml: &str,
) -> AppState {
    let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
    let id = CompanyId::new(name);
    save_record(home, &id, &manifest).await;
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, name).await;
    state
}

/// [`state_with_company`] over a harness-only-inference manifest. The
/// routes read `manifest_inference` from the saved record, so the company
/// boots on the echo brain here (no pool attached) while the record it
/// reads still carries the harness's `[harness.inference]` — exactly the
/// shape of company the fix targets.
#[cfg(feature = "openhuman")]
pub(super) async fn state_with_harness_inference(home: &std::path::Path) -> AppState {
    let id = CompanyId::new("acme");
    save_record(home, &id, &manifest_with_harness_inference()).await;
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest_with_harness_inference())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

/// A rebuilder that rebuilds over the handover, as the binary's does.
pub(super) struct Working {
    pub(super) home: std::path::PathBuf,
}

#[async_trait::async_trait]
impl crate::runtime::RuntimeRebuilder for Working {
    async fn rebuild(
        &self,
        _state: &AppState,
        request: crate::runtime::RebuildRequest,
    ) -> crate::Result<crate::company::runtime::CompanyRuntime> {
        RuntimeBuilder::new(self.home.clone(), request.manifest)
            .with_id(request.id)
            .with_handover(request.handover)
            .build()
            .await
    }
}

pub(super) async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value, String) {
    send_as(state, "acme", method, uri, body).await
}

/// `send` against a company other than `acme`, for the tests that need an id
/// of their own — see `state_with_company_named`.
pub(super) async fn send_as(
    state: &AppState,
    company: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", crate::server::test_support::fixed_cookie(company));
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

/// Seed one endpoint's catalog cache so the route answers offline, and
/// deterministically: each test uses a base URL of its own, because the
/// registry is process-wide and a shared key would let one test's positive
/// entry decide another's outcome.
///
/// Seeded in **`acme`'s** scope, because an authenticated read is
/// partitioned per company — every route test here drives the `acme`
/// company from [`state_with_company`], and a seed in the shared/keyless
/// slot would no longer be the entry the route reads.
/// Seed the authenticated catalog cache for a named company — the scope the
/// route reads under.
///
/// Every caller names its own company rather than sharing one: eviction is
/// company-wide, so a fixture seeded under an id another test saves a key
/// for is thrown away at random.
pub(super) fn seed_catalog_for(company: &str, base_url: &str, ids: &[&str]) {
    crate::server::inference_models::catalog_cache_scoped(base_url, Some(company)).store(
        ids.iter()
            .map(|id| crate::server::inference_models::InferenceModel {
                id: (*id).to_string(),
                name: Some(format!("{id} (display)")),
                context_length: Some(128_000),
            })
            .collect(),
        std::time::Instant::now(),
    );
}

/// The slug the status reports as the default, if any.
pub(super) fn default_slug(dto: &Value) -> Option<String> {
    dto["providers"]
        .as_array()?
        .iter()
        .find(|p| p["isDefault"] == true)
        .and_then(|p| p["slug"].as_str())
        .map(str::to_string)
}

/// The discard port on loopback: a connection refused immediately, with no
/// DNS lookup and no wait. Loopback is a permitted probe target here because
/// the local-runtime category exists, which is exactly what makes it usable
/// as a test endpoint.
pub(super) const UNREACHABLE: &str = "http://127.0.0.1:9/v1";

/// Where a staging deployment is pointed with `OPENCOMPANY_INFERENCE_URL`.
pub(super) const STAGING_URL: &str = "https://staging-api.tinyhumans.ai/openai/v1";

/// The platform default a staging tenant is injected with.
pub(super) fn staging_platform() -> EnvDefault {
    EnvDefault {
        base_url: STAGING_URL.to_string(),
        credential: crate::company::credentials::Credential::from_value(
            "platform-token".to_string(),
        ),
    }
}

/// A built runtime whose committed manifest is `manifest_toml`.
pub(super) async fn runtime_with(home: &std::path::Path, manifest_toml: &str) -> CompanyRuntime {
    let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
    let id = CompanyId::new("acme");
    save_record(home, &id, &manifest).await;
    RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id)
        .build()
        .await
        .unwrap()
}

pub(super) const NO_INFERENCE: &str = "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n";

pub(super) const MANAGED_MANIFEST: &str = "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
     [inference]\nprovider = \"managed\"\n";
