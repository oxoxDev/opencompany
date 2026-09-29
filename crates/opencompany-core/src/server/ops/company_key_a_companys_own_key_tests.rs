//! Route tests for the company's TinyHumans credential (issue #586).

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

use super::tests_p1_1_legacy_managed::state_with_hub;

/// A value long and opaque enough that a leak would be unmistakable in a body.
const KEY: &str = "th_company_credential_SECRET_do_not_echo_me";

/// A company that grants Composio, so the status route has something to report
/// a credential tier *for*.
const GRANTED: &str = "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
     [tools]\nallow = [\"composio\"]\n[tools.composio]\ntoolkits = [\"gmail\"]\n";

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-company-key-")
        .tempdir()
        .expect("tempdir")
}

async fn state_with_manifest(
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
    // Keys rework (#2306), slice 4a: `PUT …/credential` fans the account key
    // out to the LLM TinyHumans slot and probes it before any row or default
    // write (Q6). Forcing this here — rather than per test — is what keeps
    // every test in this file from dialing `api.tinyhumans.ai`; a test that
    // wants a different answer (a rejection, an endpoint failure) overrides
    // it again after this call.
    super::prober_override::set(company, Ok(vec!["acme/test-model".to_string()]));
    state
}

async fn send_as(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: String,
) -> (StatusCode, Value, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie);
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

async fn send(
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
        crate::server::test_support::fixed_cookie(company),
    )
    .await
}

/// Once the company sets its own key, billing reads that key's standing from
/// the hub — the intended path this route exists for.
#[tokio::test]
async fn a_companys_own_key_reads_its_own_billing_summary() {
    use crate::server::hub_identity::{BillingSummary, MockHubIdentityExchange};

    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "acme", GRANTED)
        .await
        .with_hub_identity(std::sync::Arc::new(
            MockHubIdentityExchange::new().with_billing(
                KEY,
                BillingSummary {
                    balance_usd: 12.5,
                    plan: "pro".to_string(),
                    active_subscription: true,
                    ..Default::default()
                },
            ),
        ));

    let (status, _, raw) = send_as(
        &state,
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
        crate::server::test_support::fixed_cookie("acme"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");

    let (status, dto, raw) = send(
        &state,
        "acme",
        "GET",
        "/api/v1/company/credential/billing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(dto["configured"], true, "{raw}");
    assert_eq!(dto["summary"]["balanceUsd"], 12.5, "{raw}");
    assert_eq!(dto["summary"]["plan"], "pro", "{raw}");
}

/// A member — not just an admin — can read the balance: nobody should have to
/// ask an admin why their agents stopped working this afternoon.
#[tokio::test]
async fn a_member_can_read_billing_without_admin_rights() {
    let home_dir = home();
    let state = state_with_hub(home_dir.path(), "acme").await;
    crate::server::test_support::seed_fixed_member(&state, "acme").await;
    let member = crate::server::test_support::member_cookie("acme");

    let (status, dto, raw) = send_as(
        &state,
        "GET",
        "/api/v1/company/credential/billing",
        None,
        member,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(dto["configured"], false, "{raw}");
}

/// A key the hub refuses is reported as refused — by a `reason` the console
/// switches on and a `code`, never by the hub's response body.
#[tokio::test]
async fn a_refused_key_travels_as_a_reason_and_a_code_never_as_the_hubs_body() {
    let home_dir = home();
    let state = state_with_hub(home_dir.path(), "acme").await;

    let (status, _, raw) = send_as(
        &state,
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
        crate::server::test_support::fixed_cookie("acme"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");

    let (status, dto, raw) = send(
        &state,
        "acme",
        "GET",
        "/api/v1/company/credential/billing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(dto["configured"], true, "{raw}");
    assert!(dto["summary"].is_null(), "{raw}");
    assert_eq!(dto["unavailableReason"], "rejected", "{raw}");
    assert_eq!(dto["unavailableCode"], "http_401", "{raw}");
    assert!(
        !raw.contains("did not recognize"),
        "the hub's own words reached the wire: {raw}"
    );
}

/// A hub that cannot be reached is never reported as a refused key: the two
/// call for opposite actions.
#[tokio::test]
async fn an_unreachable_hub_is_not_a_refused_key() {
    use crate::server::hub_identity::MockHubIdentityExchange;

    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "acme", GRANTED)
        .await
        .with_hub_identity(std::sync::Arc::new(MockHubIdentityExchange::unreachable()));

    let (status, _, raw) = send_as(
        &state,
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
        crate::server::test_support::fixed_cookie("acme"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");

    let (status, dto, raw) = send(
        &state,
        "acme",
        "GET",
        "/api/v1/company/credential/billing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(dto["configured"], true, "{raw}");
    assert_eq!(dto["unavailableReason"], "unreachable", "{raw}");
    assert_ne!(dto["unavailableReason"], "rejected", "{raw}");
}

/// A build with no hub blames the build, not the key.
#[tokio::test]
async fn a_host_with_no_hub_says_so_rather_than_blaming_the_key() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "acme", GRANTED).await;

    let (status, _, raw) = send_as(
        &state,
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
        crate::server::test_support::fixed_cookie("acme"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");

    let (status, dto, raw) = send(
        &state,
        "acme",
        "GET",
        "/api/v1/company/credential/billing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(dto["unavailableReason"], "noHub", "{raw}");
    assert!(dto["unavailableCode"].is_null(), "{raw}");
}

/// No key means no verdict about a key: every reason field is absent.
#[tokio::test]
async fn a_company_with_no_key_offers_no_reason_to_judge_one_by() {
    let home_dir = home();
    let state = state_with_hub(home_dir.path(), "acme").await;

    let (status, dto, raw) = send(
        &state,
        "acme",
        "GET",
        "/api/v1/company/credential/billing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(dto["configured"], false, "{raw}");
    assert!(dto["unavailable"].is_null(), "{raw}");
    assert!(dto["unavailableReason"].is_null(), "{raw}");
    assert!(dto["unavailableCode"].is_null(), "{raw}");
}

/// The classification table itself, away from any route: `401` alone earns
/// `rejected`, every other hub code waits with the outages.
#[test]
fn only_a_401_is_a_refused_key() {
    use crate::error::OpenCompanyError;

    let tinyhumans = |code: &str| OpenCompanyError::TinyHumans {
        code: code.to_string(),
        message: "{\"success\":false,\"error\":\"Invalid API key\"}".to_string(),
    };

    let (reason, sentence, code) = super::billing_unavailable(&tinyhumans("http_401"));
    assert_eq!(reason, "rejected");
    assert_eq!(code.as_deref(), Some("http_401"));
    assert!(!sentence.contains("success"), "{sentence}");
    assert!(!sentence.contains('{'), "{sentence}");

    for code in ["unreachable", "decode", "http_403", "http_429", "http_502"] {
        let (reason, _, echoed) = super::billing_unavailable(&tinyhumans(code));
        assert_eq!(reason, "unreachable", "{code}");
        assert_eq!(echoed.as_deref(), Some(code));
    }

    let (reason, _, _) = super::billing_unavailable(&tinyhumans("teapot"));
    assert_eq!(reason, "unknown");

    let (reason, _, code) =
        super::billing_unavailable(&OpenCompanyError::Store("store unreadable".to_string()));
    assert_eq!(reason, "unknown");
    assert!(code.is_none());
}

// ---------------------------------------------------------------------------
// KR-ACCT-01 (2026-09-15): the wire spellings `frontend/src/api/credential.ts`
// reads — `CompanyCredentialMutation.restartRequired` and
// `CompanyCredentialStatus.inferenceHasModel` — pinned directly against the
// DTOs' own `Serialize` impl, independent of any route or runtime.
// ---------------------------------------------------------------------------

#[test]
fn restart_required_serializes_camel_case_and_is_omitted_when_false() {
    let mut response = super::MutationResponse {
        status: minimal_status(),
        note: "note".to_string(),
        slots: Vec::new(),
        needs_model: false,
        sets_default: false,
        models: Vec::new(),
        used_by: None,
        restart_required: true,
    };
    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(
        json["restartRequired"],
        Value::Bool(true),
        "must match frontend/src/api/credential.ts's CompanyCredentialMutation.restartRequired: {json}"
    );

    // Never serialized as `false` — the console reads an absent field as "did
    // not say" and a present `false` would be a second, contradictory way to
    // say the same thing.
    response.restart_required = false;
    let json = serde_json::to_value(&response).unwrap();
    assert!(
        !json.as_object().unwrap().contains_key("restartRequired"),
        "restartRequired must be omitted rather than sent as false: {json}"
    );
}

#[test]
fn inference_has_model_serializes_camel_case() {
    let mut status = minimal_status();
    status.inference_has_model = true;
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(
        json["inferenceHasModel"],
        Value::Bool(true),
        "must match frontend/src/api/credential.ts's CompanyCredentialStatus.inferenceHasModel: {json}"
    );

    status.inference_has_model = false;
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(
        json["inferenceHasModel"],
        Value::Bool(false),
        "unlike restartRequired, this field is a plain bool and is always present: {json}"
    );
}

/// The smallest [`super::CredentialStatusDto`] that serializes without
/// panicking — every field the two tests above don't care about set to its
/// most inert value.
fn minimal_status() -> super::CredentialStatusDto {
    super::CredentialStatusDto {
        configured: false,
        source: crate::company::credentials::CredentialSource::None,
        notice: String::new(),
        account: None,
        hub_link: false,
        inference_has_own_key: false,
        composio_has_own_key: false,
        search_has_own_key: false,
        default_set: false,
        inference_has_model: false,
        used_by: None,
    }
}

// ---------------------------------------------------------------------------
// Where the grant comes back to
// ---------------------------------------------------------------------------

mod callback_base {
    use super::super::{callback_base, is_loopback_origin};
    use crate::{AppConfig, AppState};
    use axum::http::{HeaderMap, HeaderValue, header::ORIGIN};

    fn state_with(public_url: Option<&str>) -> AppState {
        AppState::new(AppConfig {
            bind: "127.0.0.1:8080".to_string(),
            public_url: public_url.map(str::to_string),
            ..AppConfig::default()
        })
    }

    fn headers_from(origin: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_str(origin).expect("header"));
        headers
    }

    #[test]
    fn a_stated_public_url_wins_over_the_browser() {
        // A deployment that names its origin has said where its console is, and
        // that is not something a request gets to move.
        let state = state_with(Some("https://acme.opencompany.example/"));
        let base = callback_base(&state, &headers_from("http://localhost:5173"));
        assert_eq!(base, "https://acme.opencompany.example/");
    }

    #[test]
    fn the_dev_console_gets_its_own_port_back() {
        // The failure this exists to stop: with nothing configured, the callback
        // was `http://127.0.0.1:8080`, where a dev host serves no page — so the
        // approval landed on a 404 holding a spent code.
        let state = state_with(None);
        let base = callback_base(&state, &headers_from("http://localhost:5173"));
        assert_eq!(base, "http://localhost:5173/");
    }

    #[test]
    fn a_remote_origin_is_ignored_for_the_bind_address() {
        // A header is attacker-controllable. A stolen code redeems nothing
        // without this host's verifier, but a callback is not somewhere to take
        // an arbitrary address on a request's say-so.
        let state = state_with(None);
        let base = callback_base(&state, &headers_from("https://evil.example"));
        assert_eq!(base, "http://127.0.0.1:8080/auth/key/callback");
    }

    #[test]
    fn no_origin_header_returns_to_the_hosts_own_route() {
        let state = state_with(None);
        assert_eq!(
            callback_base(&state, &HeaderMap::new()),
            "http://127.0.0.1:8080/auth/key/callback"
        );
    }

    #[test]
    fn an_empty_public_url_is_not_an_origin() {
        // A launcher that exported the variable with nothing in it has said
        // nothing, and must not produce a callback of `/?company=…`.
        let state = state_with(Some("   "));
        let base = callback_base(&state, &headers_from("http://127.0.0.1:5173"));
        assert_eq!(base, "http://127.0.0.1:5173/");
    }

    #[test]
    fn loopback_is_the_hub_gates_own_shape() {
        // Accepting an origin the hub would refuse would only move the failure
        // one leg later, into a 400 nobody can act on.
        assert!(is_loopback_origin("http://localhost:5173"));
        assert!(is_loopback_origin("http://127.0.0.1:8080"));
        assert!(is_loopback_origin("http://[::1]:5173"));
        assert!(!is_loopback_origin("https://localhost:5173"));
        assert!(!is_loopback_origin("http://localhost.evil.example"));
        assert!(!is_loopback_origin("http://127.0.0.1:5173/steal"));
        assert!(!is_loopback_origin("not a url"));
    }
}
