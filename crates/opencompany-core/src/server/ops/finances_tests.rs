use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::types::{CompanyId, CompanyRecord, LedgerEntry};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-finances-")
        .tempdir()
        .expect("tempdir")
}

/// Builds a runtime under `manifest_toml`, then appends `ledger` through the
/// runtime's own store (post-build, so `RuntimeBuilder` can't clobber it —
/// the same append path the harness cost hook uses).
async fn state_with_ledger(
    home: &std::path::Path,
    manifest_toml: &str,
    ledger: Vec<LedgerEntry>,
) -> AppState {
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
    for entry in ledger {
        runtime.store().append_ledger(&id, entry).await.unwrap();
    }
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

async fn get_finances(state: &AppState) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/company/finances")
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

/// An empty ledger projects an all-zero finances DTO, with the budget cap
/// still read from the manifest `[budget]`.
#[tokio::test]
async fn empty_ledger_projects_zeroes_with_budget() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_ledger(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[budget]\nmonthly_usd = 2000.0\n",
        Vec::new(),
    )
    .await;

    let (status, dto) = get_finances(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["budgetUsd"], 2000.0, "{dto}");
    assert_eq!(dto["spentUsd"], 0.0);
    assert_eq!(dto["revenueUsd"], 0.0);
    assert_eq!(dto["netUsd"], 0.0);
    assert!(dto["byCategory"].as_array().unwrap().is_empty(), "{dto}");
    assert!(dto["transactions"].as_array().unwrap().is_empty(), "{dto}");
}

/// A manifest with no `[budget]` answers `budgetUsd: null` — "no cap set" —
/// so the console can tell it apart from an explicit zero-dollar cap.
#[tokio::test]
async fn no_budget_projects_null_budget_usd() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_ledger(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n",
        Vec::new(),
    )
    .await;

    let (status, dto) = get_finances(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["budgetUsd"], Value::Null, "{dto}");
}

/// A ledger with a spend and a revenue entry this month projects the
/// expected budget / spend / revenue / net, by-category, and journal.
#[tokio::test]
async fn ledger_projects_spend_revenue_and_journal() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    // Timestamp entries "now" so they land in the current UTC month.
    let now = crate::ports::now_millis();
    let ledger = vec![
        LedgerEntry {
            at_millis: now,
            kind: "inference.spend".to_string(),
            amount_usd: -12.0,
            memo: "ceo turn".to_string(),
        },
        LedgerEntry {
            at_millis: now,
            kind: "x402.in".to_string(),
            amount_usd: 30.0,
            memo: "a2a sale".to_string(),
        },
    ];
    let state = state_with_ledger(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[budget]\nmonthly_usd = 2000.0\n",
        ledger,
    )
    .await;

    let (status, dto) = get_finances(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["budgetUsd"], 2000.0, "{dto}");
    assert!(
        (dto["spentUsd"].as_f64().unwrap() - 12.0).abs() < 1e-9,
        "{dto}"
    );
    assert!(
        (dto["revenueUsd"].as_f64().unwrap() - 30.0).abs() < 1e-9,
        "{dto}"
    );
    assert!(
        (dto["netUsd"].as_f64().unwrap() - 18.0).abs() < 1e-9,
        "{dto}"
    );

    // Spend rolls up under the "Inference" category.
    let by_category = dto["byCategory"].as_array().unwrap();
    assert_eq!(by_category.len(), 1, "{dto}");
    assert_eq!(by_category[0]["category"], "Inference");
    assert!((by_category[0]["amount"].as_f64().unwrap() - 12.0).abs() < 1e-9);

    // Both monetary entries are in the journal, newest first, directional.
    let txns = dto["transactions"].as_array().unwrap();
    assert_eq!(txns.len(), 2, "{dto}");
    let dirs: Vec<&str> = txns
        .iter()
        .map(|t| t["direction"].as_str().unwrap())
        .collect();
    assert!(dirs.contains(&"in") && dirs.contains(&"out"), "{dto}");
}
