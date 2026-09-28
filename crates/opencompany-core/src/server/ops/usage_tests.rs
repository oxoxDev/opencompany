use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::ports::usage::{SampleKind, UsageSample};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-usage-")
        .tempdir()
        .expect("tempdir")
}

async fn state_with_manifest(home: &std::path::Path, manifest_toml: &str) -> AppState {
    use crate::ports::CompanyStore;
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

async fn get_usage(state: &AppState, query: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/company/usage{query}"))
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

/// An empty meter zero-fills the series to the range length and totals zero.
#[tokio::test]
async fn empty_meter_projects_a_zero_filled_series() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n",
    )
    .await;

    let (status, dto) = get_usage(&state, "?range=7d").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["series"].as_array().unwrap().len(), 7, "{dto}");
    assert_eq!(dto["totals"]["tokens"], 0.0);
    assert_eq!(dto["totals"]["connections"], 0);
    assert!(dto["byAgent"].as_array().unwrap().is_empty(), "{dto}");
    assert!(dto["byProvider"].as_array().unwrap().is_empty(), "{dto}");
}

/// A couple of recorded samples project the expected DTO: totals sum, the
/// series carries the day's tokens, and `byAgent` resolves the roster role.
#[tokio::test]
async fn recorded_samples_project_totals_series_and_by_desk() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[agent]]\nid = \"ceo\"\nrole = \"Chief Executive\"\n",
    )
    .await;

    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();
    let now = crate::ports::now_millis();
    for (input, output) in [(100u64, 40u64), (20, 5)] {
        runtime
            .usage()
            .record(
                &id,
                &UsageSample {
                    at_millis: now,
                    agent: "ceo".into(),
                    provider: "managed".into(),
                    input_tokens: input,
                    output_tokens: output,
                    cached_input_tokens: 0,
                    cost_usd: 0.25,
                    kind: SampleKind::Inference,
                    run_id: None,
                    model: None,
                },
            )
            .await
            .unwrap();
    }

    let (status, dto) = get_usage(&state, "?range=30d").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["series"].as_array().unwrap().len(), 30, "{dto}");
    assert_eq!(dto["totals"]["inputTokens"], 120.0, "{dto}");
    assert_eq!(dto["totals"]["outputTokens"], 45.0, "{dto}");
    assert_eq!(dto["totals"]["tokens"], 165.0, "{dto}");
    assert!(
        (dto["totals"]["costUsd"].as_f64().unwrap() - 0.5).abs() < 1e-9,
        "{dto}"
    );

    // The most recent bucket (today) carries the whole burn.
    let series = dto["series"].as_array().unwrap();
    let today = series.last().unwrap();
    assert_eq!(today["inputTokens"], 120.0, "{today}");
    assert_eq!(today["outputTokens"], 45.0, "{today}");

    // byAgent resolves the manifest role as the display name.
    let by_agent = dto["byAgent"].as_array().unwrap();
    assert_eq!(by_agent.len(), 1, "{dto}");
    assert_eq!(by_agent[0]["name"], "Chief Executive");
    assert_eq!(by_agent[0]["tokens"], 165.0);
}

/// Metered web searches (issue #238) reach the console read: they count in
/// their own `searchCalls` field and their cost rolls into the window
/// total, while leaving the connected-accounts numbers alone.
///
/// That last clause is the point. A search is a priced call on the managed
/// platform, not a third-party account the company connected, so folding it
/// into `oauthCalls` / `byProvider` (whose row count **is** `connections`)
/// would report an integration the operator never set up.
#[tokio::test]
async fn search_calls_surface_separately_from_connected_accounts() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[agent]]\nid = \"ceo\"\nrole = \"Chief Executive\"\n",
    )
    .await;

    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();
    let now = crate::ports::now_millis();
    for _ in 0..3 {
        runtime
            .usage()
            .record(
                &id,
                &crate::metering::search_call_sample("ceo", "Exa", 0.01, now),
            )
            .await
            .unwrap();
    }

    let (status, dto) = get_usage(&state, "?range=7d").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["totals"]["searchCalls"], 3, "{dto}");
    assert!(
        (dto["totals"]["costUsd"].as_f64().unwrap() - 0.03).abs() < 1e-9,
        "search cost must roll into the window total: {dto}"
    );
    assert_eq!(
        dto["totals"]["connections"], 0,
        "a search is not a connected account: {dto}"
    );
    assert_eq!(dto["totals"]["oauthCalls"], 0, "{dto}");
    assert!(dto["byProvider"].as_array().unwrap().is_empty(), "{dto}");
    // And no tokens, so the teammate chart stays a token chart.
    assert_eq!(dto["totals"]["tokens"], 0.0, "{dto}");
    assert!(dto["byAgent"].as_array().unwrap().is_empty(), "{dto}");
}

/// An absent / unknown `?range=` defaults to the 30-day window.
#[tokio::test]
async fn range_defaults_to_thirty_days() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n",
    )
    .await;

    let (_, dto) = get_usage(&state, "").await;
    assert_eq!(dto["series"].as_array().unwrap().len(), 30, "{dto}");
    let (_, dto_bad) = get_usage(&state, "?range=nonsense").await;
    assert_eq!(dto_bad["series"].as_array().unwrap().len(), 30, "{dto_bad}");
}
