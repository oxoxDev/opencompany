//! Route tests for the account-key save rebuilding the company's runtime in
//! place when it completes inference configuration, and for the
//! `PUT …/credential/model` companion route that finishes a key the console
//! cannot resend (split out of `company_key_put_credential_with_a_tests.rs`
//! once it passed the 750-line cap).

use axum::http::StatusCode;
use serde_json::json;

#[cfg(feature = "openhuman")]
use crate::company::CompanyManifest;
#[cfg(feature = "openhuman")]
use crate::ports::types::{CompanyId, CompanyRecord};
#[cfg(feature = "openhuman")]
use crate::runtime::RuntimeBuilder;
#[cfg(feature = "openhuman")]
use crate::store::FsCompanyStore;
#[cfg(feature = "openhuman")]
use crate::{AppConfig, AppState};

use super::tests_put_credential_with_a::{GRANTED, KEY, home, send, send_as, state_with_manifest};
use super::tests_the_key_round_trips::slot_outcome;

/// A brain that reports the harness cognition path, so the successor a rebuild
/// installs reads as "thinking" rather than "echo" (the observable
/// `restart_pending` keys on). Mirrors the stub in `ops::inference`'s tests.
#[cfg(feature = "openhuman")]
struct RebuiltBrain;

#[cfg(feature = "openhuman")]
#[async_trait::async_trait]
impl crate::ports::brain::Brain for RebuiltBrain {
    async fn run_cycle(
        &self,
        _req: crate::ports::types::CycleRequest,
        _host: &dyn crate::ports::brain::CycleHost,
    ) -> crate::Result<crate::ports::types::CycleResult> {
        Ok(crate::ports::types::CycleResult {
            channel_responses: Vec::new(),
            new_traces: Vec::new(),
            ledger_deltas: Vec::new(),
            token_usage: crate::ports::types::TokenUsage::default(),
        })
    }

    fn cognition(&self) -> crate::ports::Cognition {
        crate::ports::Cognition {
            path: crate::ports::brain::HARNESS_PATH,
            provider: "stub",
            model: None,
            metering: crate::ports::UsageMetering::PerTurn,
        }
    }
}

#[cfg(feature = "openhuman")]
struct StubRebuilder {
    home: std::path::PathBuf,
}

#[cfg(feature = "openhuman")]
#[async_trait::async_trait]
impl crate::runtime::RuntimeRebuilder for StubRebuilder {
    async fn rebuild(
        &self,
        _state: &AppState,
        request: crate::runtime::RebuildRequest,
    ) -> crate::Result<crate::company::runtime::CompanyRuntime> {
        RuntimeBuilder::new(self.home.clone(), request.manifest)
            .with_id(request.id)
            .with_harness(std::sync::Arc::new(crate::harness::HarnessPool::new()))
            .with_brain(std::sync::Arc::new(RebuiltBrain))
            .with_handover(request.handover)
            .build()
            .await
    }
}

/// The account-key save that creates the `tinyhumans` row and default for a
/// company that booted with no inference source rebuilds that company in
/// place — the same thing `PUT …/inference` does (issue #290) — instead of
/// answering `restartRequired` and leaving every turn on the echo brain
/// until someone finds the toast's "Restart now" action. Found by the
/// umbrella e2e (`workflow-opencompany/scripts/e2e-tinyhumans-key.sh`):
/// key saved, model chosen, default set, and the next chat still said
/// `You said: …`.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn put_credential_that_configures_inference_rebuilds_the_runtime_in_place() {
    use crate::ports::CompanyStore;

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let manifest: CompanyManifest = toml::from_str(GRANTED).unwrap();
    let id = CompanyId::new("fanrb");
    FsCompanyStore::new(home.clone())
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
    // Booted with a harness pool but no inference source: the echo brain.
    let runtime = RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .with_harness(std::sync::Arc::new(crate::harness::HarnessPool::new()))
        .build()
        .await
        .unwrap();
    assert_eq!(runtime.cognition().path, "echo");
    let outgoing = std::sync::Arc::new(runtime);
    let state = AppState::new(AppConfig::default())
        .with_rebuilder(std::sync::Arc::new(StubRebuilder { home: home.clone() }));
    state.registry().insert(id.clone(), outgoing.clone());
    crate::server::test_support::seed_fixed_admin(&state, "fanrb").await;
    super::prober_override::set("fanrb", Ok(vec!["acme/test-model".to_string()]));

    // Step one: key only. Nothing about inference is configured yet (no row,
    // no default), so nothing is rebuilt and the company keeps its runtime.
    let (status, resp, raw) = send(
        &state,
        "fanrb",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(resp["needsModel"], true, "{raw}");
    let registered = state.registry().get(&id).expect("still registered");
    assert!(std::sync::Arc::ptr_eq(&registered, &outgoing));

    // Step two: the model. The row and default land, and the company is
    // rebuilt onto the harness rather than told to restart.
    let (status, resp, raw) = send(
        &state,
        "fanrb",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY, "model": "acme/test-model" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(*slot_outcome(&resp, "provider"), json!("filled"));
    assert_eq!(*slot_outcome(&resp, "default"), json!("filled"));
    assert!(
        resp.get("restartRequired").is_none(),
        "the rebuilt company must not ask for a restart: {raw}"
    );
    assert!(!raw.contains(KEY), "PUT response leaked the key: {raw}");

    let registered = state.registry().get(&id).expect("still registered");
    assert!(!std::sync::Arc::ptr_eq(&registered, &outgoing));
    assert!(outgoing.is_quiesced());
    assert!(!registered.is_quiesced());

    let (_, dto, raw) = send(&state, "fanrb", "GET", "/api/v1/company/inference", None).await;
    assert_eq!(dto["cognition"], "harness", "{raw}");
    assert_eq!(dto["restartRequired"], false, "{raw}");
}

/// `PUT …/credential/model` finishes a key the console cannot resend — the
/// key-grant case, where `finish_link` stored it and answered `needsModel`.
/// The row and the default land off the stored key; nothing else moves.
#[tokio::test]
async fn put_credential_model_completes_the_row_from_the_stored_key() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "fanmodel", GRANTED).await;

    // Nothing stored yet: nothing to finish.
    let (status, _, raw) = send(
        &state,
        "fanmodel",
        "PUT",
        "/api/v1/company/credential/model",
        Some(json!({ "model": "acme/test-model" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{raw}");

    // The key arrives without a model (what a grant does).
    let (status, resp, raw) = send(
        &state,
        "fanmodel",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(resp["needsModel"], true, "{raw}");

    let (status, resp, raw) = send(
        &state,
        "fanmodel",
        "PUT",
        "/api/v1/company/credential/model",
        Some(json!({ "model": "acme/test-model" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(*slot_outcome(&resp, "composio"), json!("kept"), "{raw}");
    assert_eq!(*slot_outcome(&resp, "inference"), json!("kept"), "{raw}");
    assert_eq!(*slot_outcome(&resp, "provider"), json!("filled"), "{raw}");
    assert_eq!(*slot_outcome(&resp, "default"), json!("filled"), "{raw}");
    assert_eq!(resp["needsModel"], false);
    assert!(!raw.contains(KEY), "response leaked the key: {raw}");

    let (_, inference, raw) =
        send(&state, "fanmodel", "GET", "/api/v1/company/inference", None).await;
    assert_eq!(
        inference["defaultChoice"]["provider"], "tinyhumans",
        "{raw}"
    );
    assert_eq!(
        inference["defaultChoice"]["model"], "acme/test-model",
        "{raw}"
    );
}

/// A member may not finish the model any more than set the key.
#[tokio::test]
async fn a_member_cannot_finish_the_model() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "fanmodelm", GRANTED).await;
    let cookie = crate::server::test_support::seed_session(
        &state,
        "fanmodelm",
        crate::ports::UserRole::Member,
    )
    .await;
    let (status, _, _) = send_as(
        &state,
        "PUT",
        "/api/v1/company/credential/model",
        Some(json!({ "model": "acme/test-model" })),
        cookie,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
