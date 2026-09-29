//! Setup tests for the onboarding redesign's two credential paths: the managed
//! branch's account key (slice 4a) and the self-managed branch's provider
//! (slice 4b-i).
//!
//! Its own group because both are about what the **apply** does after the seed,
//! which is a different question from every other group here — those ask what
//! the wizard collects and what the manifest ends up saying, and these ask
//! which host function ran and what it left in the company's own stores.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[cfg(feature = "openhuman")]
use std::sync::Arc;

#[cfg(feature = "openhuman")]
use async_trait::async_trait;

use crate::company::runtime::CompanyRuntime;
use crate::ports::types::{CompanyId, SecretValue};
#[cfg(feature = "openhuman")]
use crate::runtime::builder::RuntimeBuilder;
#[cfg(feature = "openhuman")]
use crate::runtime::rebuild::{RebuildRequest, RuntimeRebuilder};
use crate::server::router;
use crate::{AppConfig, AppState};

use super::setup_test_support_1::*;

/// Reads one of a company's secrets, or `None` when it holds nothing.
async fn secret(runtime: &CompanyRuntime, key: &str) -> Option<String> {
    runtime
        .secrets()
        .get(runtime.id(), key)
        .await
        .unwrap()
        .map(|SecretValue(value)| value)
}

/// A key shaped like a real one and worth nothing.
const ACCOUNT_KEY: &str = "th-not-a-real-key";

/// The wizard's managed branch collects the **company's** TinyHumans account
/// key, and the apply runs it through the same fan-out `PUT …/credential`
/// does.
///
/// This used to be two features wearing one name. Connecting TinyHumans on the
/// Account page wrote the account key and fanned it out — the Composio copy,
/// the LLM copy, the `tinyhumans` row, the default. Connecting TinyHumans in
/// onboarding wrote the instance-wide `tinyhumans_api_key` setting, which
/// fills none of those and is read once at boot. The console cannot make the
/// real call itself (`PUT …/credential` is admin-scoped to a company, and
/// during first run there is neither a company nor anyone signed in), so the
/// key rides the apply and the fan-out runs here, right after the company
/// exists.
///
/// The rebuild-in-place half is not asserted here and cannot be: this build
/// has no harness compiled in, so `harness_reachable` is false and there is
/// genuinely nothing to rebuild a company onto. It is reached through the same
/// `rebuild_if_pending` the Account page's save calls, which
/// `put_credential_that_configures_inference_rebuilds_the_runtime_in_place`
/// covers on a fixture
/// that does have a pool.
#[tokio::test]
async fn the_wizards_account_key_fans_out_onto_the_company_it_seeds() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    // Keeps the fan-out's health probe off `api.tinyhumans.ai`. Keyed on the
    // id the chosen name mints, which is the one this apply is about to seed.
    crate::server::ops::company_key::prober_override::set(
        "acme",
        Ok(vec!["acme/test-model".to_string()]),
    );

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "name": "Acme",
            "tinyhumans_key": ACCOUNT_KEY,
            "tinyhumans_model": "acme/test-model",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["seeded_company"], "acme", "{body}");
    assert!(
        !body.to_string().contains(ACCOUNT_KEY),
        "the apply response must never echo the key: {body}"
    );

    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("the seeded company is registered");

    assert_eq!(
        secret(&runtime, crate::company::company_key::KEY_KEY).await,
        Some(ACCOUNT_KEY.to_string()),
        "the key belongs to the company, in the slot the Account page writes"
    );
    // The load-bearing one: only the fan-out writes this. A wizard that stored
    // the key and stopped there leaves it empty, which is the state where
    // "connected to TinyHumans" buys the operator no integrations at all.
    assert_eq!(
        secret(&runtime, crate::company::composio::TINYHUMANS_KEY_KEY).await,
        Some(ACCOUNT_KEY.to_string()),
        "the Composio copy must have been filled from it"
    );
    assert_eq!(
        secret(
            &runtime,
            &crate::company::inference::store::provider_key_key(
                crate::company::inference::MANAGED_SLUG
            )
        )
        .await,
        Some(ACCOUNT_KEY.to_string()),
        "and the LLM copy with it"
    );

    // The note is the host's own account of all of that, for the completion
    // screen to show verbatim rather than flatten into "you're set up".
    let note = body["credential_note"]
        .as_str()
        .unwrap_or_else(|| panic!("the fan-out's own words must come back: {body}"));
    assert!(note.contains("acme/test-model"), "{note}");
}

/// A key sent with nothing to attach it to is dropped, not guessed at.
///
/// A host that already has companies seeds none, and there is no one of its
/// existing companies this wizard can claim the operator meant — writing the
/// key onto whichever happened to be first would hand one company another's
/// wallet.
#[tokio::test]
async fn an_account_key_with_no_company_to_own_it_is_not_written_anywhere() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    let existing = with_company(&state, home_dir.path()).await;

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "tinyhumans_key": ACCOUNT_KEY,
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["seeded_company"].is_null(), "{body}");
    assert!(
        body["credential_note"].is_null(),
        "nothing happened, so nothing is claimed: {body}"
    );

    let runtime = state.registry().get(&existing).expect("still registered");
    assert_eq!(
        secret(&runtime, crate::company::company_key::KEY_KEY).await,
        None,
        "a company this wizard did not create must not be given a wallet"
    );
}

/// A key shaped like a real one and worth nothing.
const PROVIDER_KEY: &str = "sk-not-a-real-key";

/// A local endpoint nothing is listening on, so the add's probe fails the
/// non-destructive way rather than dialling anyone.
const DRAFT_ENDPOINT: &str = "http://127.0.0.1:1/v1";

/// The wizard's self-managed branch connects a provider, and the apply runs it
/// through the same `add_provider` the LLM page's own add runs.
///
/// The row and the key are the visible half. The **default** is the half that
/// says which function wrote them: decision X1 makes the first provider a
/// company ever connects its default, and it lives inside `add_provider` — a
/// hand-written flush of `put_provider` plus a secret set would land the row
/// and the key exactly as below and leave the company with no default at all,
/// which is a company whose agents have nothing to route to.
#[tokio::test]
async fn the_wizards_connected_provider_is_added_through_the_real_add() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "name": "Acme",
            "provider_draft": {
                "kind": "custom",
                "label": "Acme Models",
                "baseUrl": DRAFT_ENDPOINT,
                "key": PROVIDER_KEY,
                "model": "acme/test-model",
                "addAnyway": true,
            },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["seeded_company"], "acme", "{body}");
    assert!(
        !body.to_string().contains(PROVIDER_KEY),
        "the apply response must never echo the key: {body}"
    );

    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("the seeded company is registered");
    let secrets = runtime.secrets();

    let providers =
        crate::company::inference::store::list_providers(runtime.id(), secrets.as_ref())
            .await
            .unwrap();
    let row = providers
        .iter()
        .find(|p| p.slug == "acme-models")
        .unwrap_or_else(|| panic!("no row was added: {providers:?}"));
    assert!(
        matches!(
            row.model(),
            crate::company::inference::store::ModelOnRow::One(ref model)
                if model == "acme/test-model"
        ),
        "the row carries the model the operator chose: {:?}",
        row.model()
    );
    assert_eq!(
        secret(
            &runtime,
            &crate::company::inference::store::provider_key_key(&row.slug)
        )
        .await,
        Some(PROVIDER_KEY.to_string()),
        "the credential belongs to the row, in the slot the LLM page writes"
    );

    // The load-bearing one. Only `add_provider` decides this (decision X1), so
    // a flush that wrote the row itself leaves it `Unset`.
    let default = crate::company::inference::store::load_default(runtime.id(), secrets.as_ref())
        .await
        .unwrap();
    assert_eq!(
        default
            .full()
            .map(|choice| (choice.provider.as_str(), choice.model.as_str())),
        Some(("acme-models", "acme/test-model")),
        "the first provider a company ever connects becomes its default: {default:?}"
    );

    let note = body["provider_note"]
        .as_str()
        .unwrap_or_else(|| panic!("the add's own words must come back: {body}"));
    assert!(note.contains("Acme Models"), "{note}");
}

/// A provider with nothing to attach it to is dropped, not guessed at — the
/// same rule the account key follows, for the same reason: on a host that
/// already had companies there is none of them this wizard can claim the
/// operator meant.
#[tokio::test]
async fn a_provider_draft_with_no_company_to_own_it_is_not_written_anywhere() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    let existing = with_company(&state, home_dir.path()).await;

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "provider_draft": {
                "kind": "custom",
                "label": "Acme Models",
                "baseUrl": DRAFT_ENDPOINT,
                "key": PROVIDER_KEY,
                "model": "acme/test-model",
                "addAnyway": true,
            },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["seeded_company"].is_null(), "{body}");
    assert!(
        body["provider_note"].is_null(),
        "nothing happened, so nothing is claimed: {body}"
    );

    let runtime = state.registry().get(&existing).expect("still registered");
    let providers =
        crate::company::inference::store::list_providers(runtime.id(), runtime.secrets().as_ref())
            .await
            .unwrap();
    assert!(
        providers.is_empty(),
        "a company this wizard did not create must not be given a provider: {providers:?}"
    );
}

/// No draft, no write. An apply that carries none must leave the seeded
/// company's provider list exactly as the seed left it, and claim nothing.
#[tokio::test]
async fn an_apply_with_no_provider_draft_adds_nothing_and_says_nothing() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({ "fields": {}, "template": "law_firm", "name": "Acme" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["provider_note"].is_null(), "{body}");

    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("the seeded company is registered");
    let providers =
        crate::company::inference::store::list_providers(runtime.id(), runtime.secrets().as_ref())
            .await
            .unwrap();
    assert!(providers.is_empty(), "{providers:?}");
}

/// TinyHumans picked on the **self-managed** branch goes through the same add
/// as anything else, including its slot guard.
///
/// `provider/tinyhumans/key` is shared with the account-key fan-out, so the add
/// reads whatever is there before it writes and puts it back on any rollback.
/// This pins that a wizard-side add lands in that slot rather than beside it.
#[tokio::test]
async fn tinyhumans_connected_on_the_self_managed_branch_lands_in_the_shared_slot() {
    let home_dir = home();
    // The TinyHumans row's endpoint is the configured proxy, so this points it
    // at a closed local port: the add's probe then fails as transport, which is
    // not a class that rolls a cloud provider back, and no test ever dials the
    // real hub.
    let state = AppState::new(AppConfig {
        bind: "127.0.0.1:8080".to_string(),
        api_url: "http://127.0.0.1:1".to_string(),
        ..AppConfig::default()
    })
    .with_home(home_dir.path().to_path_buf());
    // The account key's own fan-out runs first and writes the same slot, which
    // is the state the add has to read and replace rather than write beside.
    crate::server::ops::company_key::prober_override::set(
        "acme",
        Ok(vec!["acme/test-model".to_string()]),
    );

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "name": "Acme",
            "tinyhumans_key": ACCOUNT_KEY,
            "provider_draft": {
                "kind": crate::company::inference::MANAGED_SLUG,
                "key": PROVIDER_KEY,
                "model": "acme/test-model",
                "addAnyway": true,
            },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("the seeded company is registered");

    assert_eq!(
        secret(
            &runtime,
            &crate::company::inference::store::provider_key_key(
                crate::company::inference::MANAGED_SLUG
            )
        )
        .await,
        Some(PROVIDER_KEY.to_string()),
        "the row's key belongs in the slot the fan-out shares, not beside it"
    );
    // One slot, one occupant. The add read the fan-out's copy as its
    // `previous_key` and replaced it; a wizard-side write that missed the slot
    // would leave the account key here and the row's credential nowhere.
    assert_ne!(
        secret(
            &runtime,
            &crate::company::inference::store::provider_key_key(
                crate::company::inference::MANAGED_SLUG
            )
        )
        .await,
        Some(ACCOUNT_KEY.to_string()),
    );
    // And it is a row, not the bare key the deprecated managed-key route
    // leaves behind — the difference between a provider the LLM page can show
    // and a credential nothing owns.
    let providers =
        crate::company::inference::store::list_providers(runtime.id(), runtime.secrets().as_ref())
            .await
            .unwrap();
    assert!(
        providers
            .iter()
            .any(|p| p.slug == crate::company::inference::MANAGED_SLUG),
        "{providers:?}"
    );
}

/// The draft probe is behind the same gate every other setup route is.
///
/// It widens the first-run surface by one outward dial, so the gate is the
/// whole of what keeps it honest: a routable host must refuse it anonymously,
/// exactly as it refuses the read and the apply.
#[tokio::test]
async fn the_draft_probe_is_refused_on_a_routable_host() {
    let home_dir = home();
    let response = router(routable_state(home_dir.path()))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/setup/inference/probe")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "baseUrl": DRAFT_ENDPOINT }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(
        response.status(),
        StatusCode::OK,
        "a routable host must not let an anonymous caller dial an address it names"
    );
}

/// And it keeps its own refusal, which is not about authority at all: an
/// endpoint carrying userinfo is refused before any request is made, because
/// this host would otherwise put a basic-auth credential on the wire to an
/// address the caller chose.
#[tokio::test]
async fn the_draft_probe_refuses_an_endpoint_carrying_a_credential() {
    let home_dir = home();
    let response = router(fresh_state(home_dir.path()))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/setup/inference/probe")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "baseUrl": "http://alice:pw@127.0.0.1:1/v1" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// A refused add is **reported**, not raised.
///
/// The company is already seeded by the time the add runs, so failing the apply
/// over a draft would leave the operator with a built company behind an error
/// screen and no way back into the wizard. The refusal rides `provider_note`
/// instead, and the rest of the apply stands.
///
/// Driven through the one refusal reachable without a network: a draft with no
/// model, which `store::check_model_id` refuses before any write.
#[tokio::test]
async fn a_refused_provider_is_reported_rather_than_failing_the_apply() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "name": "Acme",
            "provider_draft": {
                "kind": "custom",
                "label": "Acme Models",
                "baseUrl": DRAFT_ENDPOINT,
                "key": PROVIDER_KEY,
            },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "the apply still succeeds: {body}");
    assert_eq!(body["seeded_company"], "acme", "{body}");
    let note = body["provider_note"]
        .as_str()
        .unwrap_or_else(|| panic!("the refusal must be said: {body}"));
    assert!(
        !note.contains("invalid request"),
        "the envelope's own vocabulary is not for a person: {note}"
    );

    // And nothing half-applied: the add refuses before any write.
    let runtime = state
        .registry()
        .get(&CompanyId::new("acme"))
        .expect("the seeded company is registered");
    let providers =
        crate::company::inference::store::list_providers(runtime.id(), runtime.secrets().as_ref())
            .await
            .unwrap();
    assert!(providers.is_empty(), "{providers:?}");
    assert_eq!(
        secret(
            &runtime,
            &crate::company::inference::store::provider_key_key("acme-models")
        )
        .await,
        None,
        "a refused add must leave no orphaned credential"
    );
}

/// A brain on the harness cognition path, so a rebuilt company reads as
/// "thinking" rather than "echo".
#[cfg(feature = "openhuman")]
struct RebuiltBrain;

#[cfg(feature = "openhuman")]
#[async_trait]
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

/// Records which companies it was asked to rebuild, and hands back a
/// successor on the harness path.
#[cfg(feature = "openhuman")]
struct RecordingRebuilder {
    home: std::path::PathBuf,
    rebuilt: Arc<std::sync::Mutex<Vec<String>>>,
}

#[cfg(feature = "openhuman")]
#[async_trait]
impl RuntimeRebuilder for RecordingRebuilder {
    async fn rebuild(
        &self,
        _state: &AppState,
        request: RebuildRequest,
    ) -> crate::Result<CompanyRuntime> {
        self.rebuilt
            .lock()
            .unwrap()
            .push(request.id.as_ref().to_string());
        RuntimeBuilder::new(self.home.clone(), request.manifest)
            .with_id(request.id)
            .with_harness(Arc::new(crate::harness::HarnessPool::new()))
            .with_brain(Arc::new(RebuiltBrain))
            .with_handover(request.handover)
            .build()
            .await
    }
}

/// The wizard's account key rebuilds the company the same apply just seeded,
/// so the operator's first chat thinks rather than echoing behind a
/// "restart required" notice nobody asked for.
///
/// Only under `openhuman` does the seed attach a harness pool, so only there
/// is the rebuild the thing that moves the company off the echo brain.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn the_wizards_account_key_rebuilds_the_company_it_just_seeded() {
    let home_dir = home();
    let rebuilt: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let state = fresh_state(home_dir.path()).with_rebuilder(Arc::new(RecordingRebuilder {
        home: home_dir.path().to_path_buf(),
        rebuilt: rebuilt.clone(),
    }));
    crate::server::ops::company_key::prober_override::set(
        "acme",
        Ok(vec!["acme/test-model".to_string()]),
    );

    let (status, body) = post_setup(
        state.clone(),
        serde_json::json!({
            "fields": {},
            "template": "law_firm",
            "name": "Acme",
            "tinyhumans_key": ACCOUNT_KEY,
            "tinyhumans_model": "acme/test-model",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["seeded_company"], "acme", "{body}");

    assert_eq!(
        rebuilt.lock().unwrap().as_slice(),
        ["acme".to_string()],
        "the apply must rebuild the company it seeded. If this is the only assertion \
         failing, check the environment: OPENCOMPANY_INFERENCE_KEY, TINYHUMANS_API_KEY \
         or an existing TINYHUMANS_TOKEN_FILE each boot that company already configured \
         on the harness, which genuinely owes no rebuild — the test is wrong about the \
         shell, not about the code. They are not cleared here because `set_var` is \
         process-global and would race every other test in this binary."
    );

    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/company/inference")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let dto = body_json(response).await;
    assert_eq!(dto["cognition"], "harness", "{dto}");
    assert_eq!(dto["restartRequired"], false, "{dto}");
    assert_eq!(dto["defaultChoice"]["provider"], "tinyhumans", "{dto}");
}

/// A wizard company that brought its own provider writes `[inference].provider`
/// before the seed, so it boots already configured instead of onto echo — and
/// owes no rebuild.
#[tokio::test]
async fn a_wizard_company_that_brought_its_own_provider_boots_ready() {
    let home_dir = home();
    let state = fresh_state(home_dir.path());
    let mut company = designed_company(None);
    company["inference"] = serde_json::json!({
        "provider": "openrouter",
        "model": "acme/test-model",
        "key": ACCOUNT_KEY,
    });

    let (status, body) = post_setup(state.clone(), serde_json::json!({ "company": company })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let seeded = body["seeded_company"].as_str().expect("seeded");

    assert_eq!(
        seeded_manifest(home_dir.path(), seeded)
            .await
            .inference
            .provider
            .as_deref(),
        Some("openrouter"),
        "the manifest this company booted from must already name its provider"
    );

    let runtime = state
        .registry()
        .get(&CompanyId::new(seeded))
        .expect("the seeded company is registered");
    assert!(
        crate::company::inference::key_configured(runtime.id(), runtime.secrets().as_ref(), None)
            .await
            .unwrap(),
        "and hold the key the wizard collected for it"
    );
}
