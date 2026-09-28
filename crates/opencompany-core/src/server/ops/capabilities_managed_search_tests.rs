//! Tests for the managed Search credential verdict on the capability card:
//! it walks the same two tiers as the request path (company key, then
//! deployment fallback), and a secret-store outage omits the verdict rather
//! than reporting a false negative (split out of `capabilities_tests.rs`).

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use super::tests_a_company_on_the::BrokenSecrets;
use crate::company::CompanyManifest;
use crate::ports::types::CompanyId;
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::{AppConfig, AppState};

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-capabilities-")
        .tempdir()
        .expect("tempdir")
}

async fn state_with_manifest(home: &std::path::Path, manifest_toml: &str) -> AppState {
    use crate::ports::CompanyStore;
    use crate::ports::types::CompanyRecord;
    use crate::store::FsCompanyStore;

    let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
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
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

async fn get_capabilities(state: &AppState) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/company/capabilities")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::empty())
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// The capability card asks the same question as the request path: a
/// company-owned managed key is sufficient even when this test process has
/// no deployment search credential.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn reports_company_managed_search_key_as_configured() {
    let home_dir = home();
    let state = state_with_manifest(
        home_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"search\"]\n",
    )
    .await;
    let company = CompanyId::new("acme");
    state
        .registry()
        .get(&company)
        .unwrap()
        .secrets()
        .set(
            &company,
            crate::company::search::MANAGED_KEY_SECRET,
            crate::ports::types::SecretValue("company-search-key".to_string()),
        )
        .await
        .unwrap();

    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["searchCredentialConfigured"], true, "{dto}");
}

/// A secret-store outage is neither "no credential" nor a reason to lose
/// the endpoint's unrelated budget and grant data.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn omits_managed_search_verdict_when_the_secret_store_is_unreadable() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let manifest_toml =
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"search\"]\n";
    let state = state_with_manifest(&home, manifest_toml).await;
    let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
    let id = CompanyId::new("acme");
    let runtime = RuntimeBuilder::new(home, manifest)
        .with_id(id.clone())
        .with_secrets(std::sync::Arc::new(BrokenSecrets))
        .build()
        .await
        .unwrap();
    state.registry().insert(id, std::sync::Arc::new(runtime));

    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK, "{dto}");
    assert_eq!(dto["searchGranted"], true, "{dto}");
    assert!(dto.get("searchCredentialConfigured").is_none(), "{dto}");
}
