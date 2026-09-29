use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

const MANIFEST: &str = "[company]\nname = \"Acme\"\n\
     [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n\
     [[mcp_server]]\nname = \"notion\"\n\
     endpoint = \"https://mcp.notion.com/mcp\"\n\
     description = \"Notion workspace\"\n\
     allowed_tools = [\"search\"]\n\
     timeout_secs = 45\n\
     enabled = true\n";

async fn state(home: &std::path::Path) -> AppState {
    let manifest: CompanyManifest = toml::from_str(MANIFEST).unwrap();
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

/// `GET …/mcp/config` over the real router: the manifest's `[[mcp_server]]`
/// entry must come back as a `mcpServers` document entry, not merely as
/// something [`effective_mcp_servers`] agrees with in isolation.
///
/// Every helper `declared`/`read_config` calls (`manifest_servers`,
/// `load_runtime_index`, `effective_mcp_servers`) has its own unit
/// coverage; this is the one test that proves the handler actually wires
/// them together behind the HTTP route the console calls, with the
/// auth-matrix's admitted request reaching a real manifest-backed runtime
/// rather than stopping at the authorization verdict.
#[tokio::test]
async fn read_config_reports_a_manifest_declared_server() {
    let home = tempfile::tempdir().unwrap();
    let state = state(home.path()).await;

    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/company/mcp/config")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::empty())
        .unwrap();
    let response = router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let notion = &body["mcpServers"]["notion"];

    assert_eq!(notion["url"], "https://mcp.notion.com/mcp");
    assert_eq!(notion["description"], "Notion workspace");
    assert_eq!(notion["enabled"], true);
    assert_eq!(notion["allowedTools"], serde_json::json!(["search"]));
    assert_eq!(notion["timeoutSecs"], 45);
    assert_eq!(
        notion["source"], "manifest",
        "a company.toml-declared server reports its real provenance"
    );
    assert_eq!(
        notion["authConfigured"], false,
        "no auth_secret was declared and none was stored"
    );
}
