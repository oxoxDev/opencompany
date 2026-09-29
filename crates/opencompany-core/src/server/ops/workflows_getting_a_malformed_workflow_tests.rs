// The globals-unaware readers: these tests assert the company's own two
// sources, so they call the form that resolves no baseline.
use super::workflows_test_support::*;

/// FAIL-axis: unlike the list route above (which skips a broken graph and
/// carries on), addressing the broken one directly is a single-resource
/// read on a body that cannot be used, and `OpenCompanyError::DataParse`
/// is centrally mapped to `400` (`server/error.rs`). This is the
/// single-workflow `GET` driven all the way through the router, not just
/// the loader function, so the mapping is proven at the seam the console
/// actually calls.
#[tokio::test]
async fn getting_a_malformed_workflow_by_id_answers_400_data_parse() {
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

    let dir = seed_demo();
    std::fs::write(
        dir.path().join("workflows").join("broken.toml"),
        "id = \"broken\"\nname = \n[[node]] oops",
    )
    .unwrap();

    let manifest: CompanyManifest =
        toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap();
    let store = FsCompanyStore::new(dir.path().to_path_buf());
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
    let runtime = RuntimeBuilder::new(dir.path().to_path_buf(), manifest)
        .with_id(id.clone())
        .with_seed_dir(dir.path().to_path_buf())
        .build()
        .await
        .unwrap();
    assert!(
        runtime.source_dir().is_some(),
        "test setup must give the company a real source tree to read `broken.toml` from"
    );
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;

    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/company/workflows/broken")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::empty())
        .unwrap();
    let response = router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body["code"], "data_parse",
        "the stable error code must name a parse failure, not a generic 500: {body}"
    );
}

// HTTP-level: a hosted tenant has no source directory to scan, so these
// exercise the manifest-enabled union path end to end via the router.
