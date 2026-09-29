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

use super::tests_p1_1_legacy_managed::{GRANTED_KEY, state_param, state_with_hub};
use super::tests_the_key_round_trips::slot_outcome;

/// A value long and opaque enough that a leak would be unmistakable in a body.
pub(super) const KEY: &str = "th_company_credential_SECRET_do_not_echo_me";

/// A company that grants Composio, so the status route has something to report
/// a credential tier *for*.
pub(super) const GRANTED: &str = "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
     [tools]\nallow = [\"composio\"]\n[tools.composio]\ntoolkits = [\"gmail\"]\n";

pub(super) fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-company-key-")
        .tempdir()
        .expect("tempdir")
}

pub(super) async fn state_with_manifest(
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

pub(super) async fn send_as(
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

pub(super) async fn send(
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

async fn set_custom_search_key(state: &AppState, company: &str) {
    let id = CompanyId::new(company);
    state
        .registry()
        .get(&id)
        .expect("registered")
        .secrets()
        .set(
            &id,
            crate::company::search::MANAGED_KEY_SECRET,
            crate::ports::types::SecretValue("custom-search-key".to_string()),
        )
        .await
        .unwrap();
}

/// M2 by route: sending a model with the same save adds the row and, since
/// none was set, the default.
#[tokio::test]
async fn put_credential_with_a_model_adds_the_row_and_default() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "fanm2", GRANTED).await;

    let (status, resp, raw) = send(
        &state,
        "fanm2",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY, "model": "acme/test-model" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(*slot_outcome(&resp, "provider"), json!("filled"));
    assert_eq!(*slot_outcome(&resp, "default"), json!("filled"));
    assert_eq!(resp["needsModel"], false);

    let (_, inference, raw) = send(&state, "fanm2", "GET", "/api/v1/company/inference", None).await;
    let providers = inference["providers"].as_array().unwrap();
    let row = providers
        .iter()
        .find(|p| p["slug"] == "tinyhumans")
        .unwrap_or_else(|| panic!("no tinyhumans row: {raw}"));
    assert_eq!(row["origin"], "indexed");
    assert_eq!(inference["defaultChoice"]["provider"], "tinyhumans");
    assert_eq!(inference["defaultChoice"]["model"], "acme/test-model");
}

/// Q6: a probe classified `auth` rolls the LLM copy back and never touches
/// the account key or the Composio copy.
#[tokio::test]
async fn put_credential_auth_probe_rolls_back_the_llm_copy() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "fanauth", GRANTED).await;
    super::prober_override::set(
        "fanauth",
        Err(crate::company::inference::probe::ProbeClass::Auth),
    );

    let (status, resp, raw) = send(
        &state,
        "fanauth",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(*slot_outcome(&resp, "inference"), json!("rolledBack"));
    assert_eq!(
        *slot_outcome(&resp, "composio"),
        json!("filled"),
        "the Composio copy is kept even though the LLM copy is not"
    );
    assert_eq!(resp["needsModel"], false);
    assert_eq!(
        resp["status"]["configured"], true,
        "the account key itself is kept"
    );

    let (_, composio, _) = send(&state, "fanauth", "GET", "/api/v1/company/composio", None).await;
    assert_eq!(composio["credentialSource"], "static");
}

/// The key never appears anywhere on the wire — not in the mutation response,
/// not in either status read it feeds.
#[tokio::test]
async fn put_credential_never_echoes_the_key() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "fanleak", GRANTED).await;

    let (_, _, raw) = send(
        &state,
        "fanleak",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY, "model": "acme/test-model" })),
    )
    .await;
    assert!(!raw.contains(KEY), "{raw}");

    let (_, _, raw) = send(&state, "fanleak", "GET", "/api/v1/company/inference", None).await;
    assert!(!raw.contains(KEY), "{raw}");
    let (_, _, raw) = send(&state, "fanleak", "GET", "/api/v1/company/composio", None).await;
    assert!(!raw.contains(KEY), "{raw}");
}

/// §3.5: one journal line per slot that actually changed, never for the
/// health slot.
#[tokio::test]
async fn put_credential_journals_one_line_per_changed_slot() {
    use crate::ports::types::CompanyEvent;

    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "fanjournal", GRANTED).await;
    send(
        &state,
        "fanjournal",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY, "model": "acme/test-model" })),
    )
    .await;

    let id = CompanyId::new("fanjournal");
    let runtime = state.registry().get(&id).expect("registered");
    let events = runtime
        .events()
        .read_from(&id, crate::ports::types::EventSeq::new(0), 200)
        .await
        .expect("events");
    let changes: Vec<String> = events
        .iter()
        .filter_map(|stored| match &stored.event {
            CompanyEvent::ToolAccessChanged { change, .. } => Some(change.clone()),
            _ => None,
        })
        .collect();
    for expected in [
        "company_key_set",
        "company_key_composio_filled",
        "company_key_inference_filled",
        "company_key_provider_filled",
        "company_key_default_filled",
    ] {
        assert!(
            changes.contains(&expected.to_string()),
            "missing {expected}: {changes:?}"
        );
    }
    assert!(
        !changes.iter().any(|c| c.contains("health")),
        "the health slot never journals: {changes:?}"
    );
}

/// The grant runs the same fan-out and, per Q10, declares no provider of its
/// own: no entry zero appears, and `managed.source` reads as a plain
/// provider-key credential rather than a declared runtime config.
#[tokio::test]
async fn finish_link_runs_the_fan_out_and_writes_no_inference_config() {
    let home_dir = home();
    let state = state_with_hub(home_dir.path(), "fanlink").await;

    let (_, resp, _) = send(
        &state,
        "fanlink",
        "POST",
        "/api/v1/company/credential/link/start",
        Some(json!({})),
    )
    .await;
    let url = resp["authorizeUrl"].as_str().unwrap().to_string();
    let state_value = state_param(&url);
    let verifier = state
        .hub_links()
        .peek_verifier(&state_value)
        .expect("the start parked a pending link");
    let state = state.with_hub_identity(std::sync::Arc::new(
        crate::server::hub_identity::MockHubIdentityExchange::new().with_grant(
            "grant-code",
            &verifier,
            GRANTED_KEY,
        ),
    ));

    let (status, _, raw) = send(
        &state,
        "fanlink",
        "POST",
        "/api/v1/company/credential/link/finish",
        Some(json!({ "state": state_value, "code": "grant-code" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");

    let (_, inference, raw) =
        send(&state, "fanlink", "GET", "/api/v1/company/inference", None).await;
    assert!(
        !inference["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["origin"] == "entryZero"),
        "a grant declares no provider of its own: {raw}"
    );
    assert_eq!(inference["managed"]["source"], "provider_key", "{raw}");
}

/// The in-use guard (new scope beyond phase-4a, from
/// `docs/key-reworks/in-use-guards.md`): clearing the account key while it
/// backs a `tinyhumans` row and resolves both the LLM and Composio slots is
/// refused without confirmation, naming both surfaces.
#[tokio::test]
async fn clearing_the_account_key_when_it_backs_the_llm_row_is_refused_without_confirmation() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "fanguard", GRANTED).await;
    send(
        &state,
        "fanguard",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY, "model": "acme/test-model" })),
    )
    .await;

    let (status, body, raw) = send(
        &state,
        "fanguard",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{raw}");
    assert_eq!(body["code"], "in_use", "{body}");
    let surfaces: Vec<&str> = body["usedBy"]["surfaces"]
        .as_array()
        .expect("surfaces")
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert!(surfaces.contains(&"llm"), "{body}");
    assert!(surfaces.contains(&"composio"), "{body}");

    // Refused, so nothing changed.
    let (_, dto, _) = send(
        &state,
        "fanguard",
        "GET",
        "/api/v1/company/credential",
        None,
    )
    .await;
    assert_eq!(
        dto["configured"], true,
        "a refused clear must not have stored anything"
    );
}

/// The same clear, confirmed: proceeds exactly as §6's C1 describes (the row
/// and the default survive; only the key copies clear) and echoes the
/// `usedBy` it would have refused with.
#[tokio::test]
async fn a_confirmed_clear_of_the_account_key_proceeds_and_echoes_used_by() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "fanconfirm", GRANTED).await;
    send(
        &state,
        "fanconfirm",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY, "model": "acme/test-model" })),
    )
    .await;

    let (status, resp, raw) = send(
        &state,
        "fanconfirm",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": "", "confirmInUse": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    let surfaces: Vec<&str> = resp["usedBy"]["surfaces"]
        .as_array()
        .expect("usedBy echoed on a confirmed clear")
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert!(surfaces.contains(&"llm"), "{resp}");
    assert!(surfaces.contains(&"composio"), "{resp}");

    assert_eq!(*slot_outcome(&resp, "composio"), json!("cleared"));
    assert_eq!(*slot_outcome(&resp, "inference"), json!("cleared"));
    assert_eq!(*slot_outcome(&resp, "provider"), json!("skipped"));
    assert_eq!(*slot_outcome(&resp, "default"), json!("skipped"));

    let (_, inference, raw) = send(
        &state,
        "fanconfirm",
        "GET",
        "/api/v1/company/inference",
        None,
    )
    .await;
    assert!(
        inference["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["slug"] == "tinyhumans"),
        "the row stays: {raw}"
    );
    assert_eq!(
        inference["defaultChoice"]["provider"], "tinyhumans",
        "the default stays: {raw}"
    );
}

/// Custom keys everywhere (matrix shape M6/C2): the account key's clear
/// touches nothing either derived slot still resolves through, so it needs no
/// confirmation and echoes no `usedBy` at all.
#[tokio::test]
async fn clearing_when_nothing_depends_on_it_needs_no_confirmation() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "fanclean", GRANTED).await;
    send(
        &state,
        "fanclean",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    set_custom_search_key(&state, "fanclean").await;
    // A custom key pasted directly on both the Composio and LLM pages, which
    // the fan-out never overwrites (Q7) and which the guard must not treat as
    // still depending on the account key.
    send(
        &state,
        "fanclean",
        "PUT",
        "/api/v1/company/composio/token",
        Some(json!({ "token": "custom-composio-token" })),
    )
    .await;
    send(
        &state,
        "fanclean",
        "PUT",
        "/api/v1/company/inference/managed/key",
        Some(json!({ "key": "custom-llm-key" })),
    )
    .await;

    let (status, resp, raw) = send(
        &state,
        "fanclean",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert!(
        resp.get("usedBy").is_none(),
        "nothing depends on the account key any more: {resp}"
    );
    assert_eq!(*slot_outcome(&resp, "composio"), json!("kept"));
    assert_eq!(*slot_outcome(&resp, "inference"), json!("kept"));
}

/// KR-L3-01: `GET …/credential` carries the same `usedBy` a clear would be
/// refused with, computed the moment the page loads rather than only after a
/// stale-UI 409 — the Remove-key dialog's first-open text.
#[tokio::test]
async fn status_reports_used_by_when_a_clear_would_strand_dependents() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "statususedby", GRANTED).await;
    send(
        &state,
        "statususedby",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY, "model": "acme/test-model" })),
    )
    .await;

    let (_, dto, raw) = send(
        &state,
        "statususedby",
        "GET",
        "/api/v1/company/credential",
        None,
    )
    .await;
    let surfaces: Vec<&str> = dto["usedBy"]["surfaces"]
        .as_array()
        .expect("usedBy on the status DTO")
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert!(surfaces.contains(&"llm"), "{raw}");
    assert!(surfaces.contains(&"composio"), "{raw}");
    assert!(surfaces.contains(&"search"), "{raw}");
}

/// The other half: nothing set, or nothing left depending on the account key
/// (matrix M6/C2's shape) — `usedBy` is absent, never an empty object, so a
/// plain `"usedBy" in dto` check on the console reads false.
#[tokio::test]
async fn status_reports_no_used_by_when_nothing_depends_on_it() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "statusnousedby", GRANTED).await;

    let (_, empty_dto, raw) = send(
        &state,
        "statusnousedby",
        "GET",
        "/api/v1/company/credential",
        None,
    )
    .await;
    assert!(empty_dto.get("usedBy").is_none(), "no key set yet: {raw}");

    send(
        &state,
        "statusnousedby",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    set_custom_search_key(&state, "statusnousedby").await;
    send(
        &state,
        "statusnousedby",
        "PUT",
        "/api/v1/company/composio/token",
        Some(json!({ "token": "custom-composio-token" })),
    )
    .await;
    send(
        &state,
        "statusnousedby",
        "PUT",
        "/api/v1/company/inference/managed/key",
        Some(json!({ "key": "custom-llm-key" })),
    )
    .await;

    let (_, dto, raw) = send(
        &state,
        "statusnousedby",
        "GET",
        "/api/v1/company/credential",
        None,
    )
    .await;
    assert!(
        dto.get("usedBy").is_none(),
        "all derived slots hold their own key, not the account key's copy: {raw}"
    );
}

// ---------------------------------------------------------------------------
// P1-1 (keys rework #2306, KR-L3-01 review): the composio/mode gate and the
// legacy managed entry-zero decision, pinned by dedicated tests.
// ---------------------------------------------------------------------------

/// P1-1 (a): BYOK mode with an account key equal to the Composio copy — no
/// row exists either, so nothing at all could be stranded, and clearing needs
/// no confirmation. This isolates the composio/mode gate specifically:
/// `composio/tinyhumans/key` still equals the account key (`decide_copy`
/// would `Clear` it), but a company on `byok` has nothing live resolving
/// through that slot (`in-use-guards.md` §2's `"composio"` row).
#[tokio::test]
async fn p1_1_byok_mode_with_a_matching_composio_copy_needs_no_confirmation() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "p11byok", GRANTED).await;
    let id = CompanyId::new("p11byok");
    // No model: no `tinyhumans` row is created, so nothing can make "llm"
    // appear either.
    send(
        &state,
        "p11byok",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    // Switch Composio to BYOK directly at the store, bypassing
    // `PUT …/composio/api-key` and the real network probe it would run in
    // this feature build — this test is about the mode gate, not the probe.
    // `composio/tinyhumans/key` is untouched by this — it still holds the
    // fan-out's own copy of the account key.
    let runtime = state.registry().get(&id).expect("registered");
    runtime
        .secrets()
        .set(
            &id,
            crate::company::composio::MODE_KEY,
            crate::ports::types::SecretValue("byok".to_string()),
        )
        .await
        .unwrap();
    set_custom_search_key(&state, "p11byok").await;

    let (status, resp, raw) = send(
        &state,
        "p11byok",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert!(
        resp.get("usedBy").is_none(),
        "byok has nothing live resolving through composio/tinyhumans/key: {resp}"
    );
}

/// P1-1 (b): the mirror image of (a) — managed mode (the default), same
/// matching Composio copy, same absence of a row. Refused with `409`,
/// naming `"composio"` and nothing else.
#[tokio::test]
async fn p1_1_managed_mode_with_a_matching_composio_copy_is_refused() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "p11managed", GRANTED).await;
    send(
        &state,
        "p11managed",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    set_custom_search_key(&state, "p11managed").await;

    let (status, body, raw) = send(
        &state,
        "p11managed",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{raw}");
    assert_eq!(body["code"], "in_use", "{body}");
    let surfaces: Vec<&str> = body["usedBy"]["surfaces"]
        .as_array()
        .expect("surfaces")
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert_eq!(surfaces, vec!["composio"], "{body}");
}
