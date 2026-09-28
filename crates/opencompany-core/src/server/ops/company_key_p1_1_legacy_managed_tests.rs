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

/// P1-1 (c): a company still on the legacy managed entry zero — no indexed
/// `tinyhumans` row at all, but `inference/config` already names
/// `tinyhumans` — counts as `"llm"` in use too (the KR-L3-01 review decision
/// recorded in `docs/key-reworks/in-use-guards.md` §2's `"llm"` row).
/// `row_exists` alone only ever sees indexed rows; this pins that the
/// entry-zero path is deliberately counted as well, with an explicit
/// assertion rather than leaving the decision undertested.
#[tokio::test]
async fn p1_1_legacy_managed_entry_zero_counts_as_llm_in_use() {
    use crate::company::inference::RuntimeInference;

    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "p11entryzero", GRANTED).await;
    let id = CompanyId::new("p11entryzero");
    let runtime = state.registry().get(&id).expect("registered");

    // The account key, and the SAME value at the legacy flat slot
    // `inference/key` — the address `load_managed_key`'s own entry-zero
    // fallback reads, so the guard's `decide_copy` sees a copy that still
    // equals the account key.
    runtime
        .secrets()
        .set(
            &id,
            crate::company::company_key::KEY_KEY,
            crate::ports::types::SecretValue(KEY.to_string()),
        )
        .await
        .unwrap();
    runtime
        .secrets()
        .set(
            &id,
            crate::company::inference::KEY_KEY,
            crate::ports::types::SecretValue(KEY.to_string()),
        )
        .await
        .unwrap();
    crate::company::inference::save_runtime_config(
        &id,
        runtime.secrets().as_ref(),
        &RuntimeInference {
            provider: crate::company::inference::MANAGED_SLUG.to_string(),
            base_url: None,
            models: Default::default(),
        },
    )
    .await
    .unwrap();

    let (status, body, raw) = send(
        &state,
        "p11entryzero",
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
    assert!(
        surfaces.contains(&"llm"),
        "entry-zero legacy managed config counts as llm in use: {body}"
    );
}

// ---------------------------------------------------------------------------
// `slot_facts` — the account-key dialog's own-key booleans (keys rework
// #2306, slice 4b). See `docs/key-reworks/phase-4b-account-dialog.md` §3.1/§6.
// ---------------------------------------------------------------------------

/// A fresh company reports no own keys anywhere and no default — the
/// dialog's starting state, where saving would fill both derived slots.
#[tokio::test]
async fn status_reports_no_own_keys_on_an_empty_company() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "factsempty", GRANTED).await;

    let (_, dto, raw) = send(
        &state,
        "factsempty",
        "GET",
        "/api/v1/company/credential",
        None,
    )
    .await;
    assert_eq!(dto["inferenceHasOwnKey"], false, "{raw}");
    assert_eq!(dto["composioHasOwnKey"], false, "{raw}");
    assert_eq!(dto["searchHasOwnKey"], false, "{raw}");
    assert_eq!(dto["defaultSet"], false, "{raw}");
}

/// A copy that merely equals the account key is not "its own" — Q7's whole
/// point is that such a copy is filled again on the next rotation.
#[tokio::test]
async fn a_copy_equal_to_the_account_key_is_not_an_own_key() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "factscopy", GRANTED).await;
    send(
        &state,
        "factscopy",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;

    let (_, dto, raw) = send(
        &state,
        "factscopy",
        "GET",
        "/api/v1/company/credential",
        None,
    )
    .await;
    assert_eq!(
        dto["inferenceHasOwnKey"], false,
        "the fan-out's own copy is not an own key: {raw}"
    );
    assert_eq!(
        dto["composioHasOwnKey"], false,
        "the fan-out's own copy is not an own key: {raw}"
    );
    assert_eq!(
        dto["searchHasOwnKey"], false,
        "the fan-out's own Search copy is not an own key: {raw}"
    );
}

/// A key pasted directly on the LLM page is that slot's own key, and the
/// dialog must be able to tell.
#[tokio::test]
async fn a_key_set_on_the_llm_page_is_an_own_key() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "factsllm", GRANTED).await;
    send(
        &state,
        "factsllm",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    send(
        &state,
        "factsllm",
        "PUT",
        "/api/v1/company/inference/managed/key",
        Some(json!({ "key": "th-not-a-real-key-custom" })),
    )
    .await;

    let (_, dto, raw) = send(
        &state,
        "factsllm",
        "GET",
        "/api/v1/company/credential",
        None,
    )
    .await;
    assert_eq!(dto["inferenceHasOwnKey"], true, "{raw}");
}

/// A legacy `composio/token` (the pre-1a address, never mirrored to the new
/// one) still counts as the Composio slot's own key —
/// `load_tinyhumans_key`'s own fallback read. The account key is written
/// **raw** here, not through `PUT …/credential`: that route's own fan-out
/// would fill the new `composio/tinyhumans/key` address and this test needs
/// it to stay empty, exactly the M14 shape (a company whose Composio
/// credential was only ever read through the legacy address).
#[tokio::test]
async fn a_legacy_composio_token_counts_as_the_composio_slot() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "factslegacy", GRANTED).await;

    let id = CompanyId::new("factslegacy");
    let runtime = state.registry().get(&id).expect("registered");
    runtime
        .secrets()
        .set(
            &id,
            crate::company::company_key::KEY_KEY,
            crate::ports::types::SecretValue(KEY.to_string()),
        )
        .await
        .unwrap();
    runtime
        .secrets()
        .set(
            &id,
            crate::company::composio::LEGACY_TOKEN_KEY,
            crate::ports::types::SecretValue("th-not-a-real-key-custom".to_string()),
        )
        .await
        .unwrap();

    let (_, dto, raw) = send(
        &state,
        "factslegacy",
        "GET",
        "/api/v1/company/credential",
        None,
    )
    .await;
    assert_eq!(dto["composioHasOwnKey"], true, "{raw}");
}

/// A bare provider slug (Q1: "provider chosen, model not chosen") still
/// counts as a set default — never overwritten, and the dialog must not
/// promise a default move that would not happen.
#[tokio::test]
async fn default_set_is_true_for_a_bare_slug() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "factsdefault", GRANTED).await;

    let id = CompanyId::new("factsdefault");
    let runtime = state.registry().get(&id).expect("registered");
    runtime
        .secrets()
        .set(
            &id,
            crate::company::inference::store::DEFAULT_PROVIDER_KEY,
            crate::ports::types::SecretValue("openrouter".to_string()),
        )
        .await
        .unwrap();

    let (_, dto, raw) = send(
        &state,
        "factsdefault",
        "GET",
        "/api/v1/company/credential",
        None,
    )
    .await;
    assert_eq!(dto["defaultSet"], true, "{raw}");
}

/// None of the three new booleans ever needs to echo a value to compute —
/// the status body carries neither fake key, whatever is set on either slot.
#[tokio::test]
async fn status_never_carries_a_key_when_reporting_own_key_facts() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "factsleak", GRANTED).await;
    send(
        &state,
        "factsleak",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    send(
        &state,
        "factsleak",
        "PUT",
        "/api/v1/company/inference/managed/key",
        Some(json!({ "key": "th-not-a-real-key-custom" })),
    )
    .await;
    let id = CompanyId::new("factsleak");
    let runtime = state.registry().get(&id).expect("registered");
    runtime
        .secrets()
        .set(
            &id,
            crate::company::composio::LEGACY_TOKEN_KEY,
            crate::ports::types::SecretValue("th-not-a-real-key-custom".to_string()),
        )
        .await
        .unwrap();
    runtime
        .secrets()
        .set(
            &id,
            crate::company::inference::store::DEFAULT_PROVIDER_KEY,
            crate::ports::types::SecretValue("openrouter".to_string()),
        )
        .await
        .unwrap();

    let (_, _, raw) = send(
        &state,
        "factsleak",
        "GET",
        "/api/v1/company/credential",
        None,
    )
    .await;
    assert!(!raw.contains(KEY), "{raw}");
    assert!(!raw.contains("th-not-a-real-key-custom"), "{raw}");
}

// ---------------------------------------------------------------------------
// The one-click key grant (PKCE). See `server::hub_link`.
// ---------------------------------------------------------------------------

/// The key the mock hub mints when a grant is redeemed.
pub(super) const GRANTED_KEY: &str = "tiny_live_granted_by_the_hub_do_not_echo_me";

/// A state with a hub that will mint `GRANTED_KEY` for the right verifier.
///
/// The verifier is not known until the host mints one, so the exchange is
/// seeded *after* `start` — which is also the only way to assert that the host
/// sends the challenge for the verifier it actually kept.
pub(super) async fn state_with_hub(home: &std::path::Path, company: &str) -> AppState {
    state_with_manifest(home, company, GRANTED)
        .await
        .with_hub_identity(std::sync::Arc::new(
            crate::server::hub_identity::MockHubIdentityExchange::new(),
        ))
}

/// Pulls `state=` out of the authorize URL the console is told to navigate to.
pub(super) fn state_param(authorize_url: &str) -> String {
    let (_, after) = authorize_url
        .split_once("state%3D")
        .expect("state in callback");
    after
        .split(['&', '%'])
        .next()
        .expect("state value")
        .to_string()
}

#[tokio::test]
async fn a_host_with_no_hub_offers_no_link_and_refuses_to_start_one() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "acme", GRANTED).await;

    // The status says so, which is what keeps the console from rendering a
    // button whose only possible outcome is a 404.
    let (status, dto, raw) = send(&state, "acme", "GET", "/api/v1/company/credential", None).await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(dto["hubLink"], false);

    let (status, _, raw) = send(
        &state,
        "acme",
        "POST",
        "/api/v1/company/credential/link/start",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{raw}");
}

#[tokio::test]
async fn starting_a_link_sends_the_console_to_the_hub_with_a_challenge_not_a_secret() {
    let home_dir = home();
    let state = state_with_hub(home_dir.path(), "acme").await;

    let (status, dto, raw) = send(&state, "acme", "GET", "/api/v1/company/credential", None).await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(dto["hubLink"], true);

    let (status, resp, raw) = send(
        &state,
        "acme",
        "POST",
        "/api/v1/company/credential/link/start",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");

    let url = resp["authorizeUrl"].as_str().expect("authorizeUrl");
    // Through the site's provider chooser, which forwards to the hub's own
    // `/auth/key` with the provider the person picked. Straight at `/auth/key`
    // would be the hub's `provider=google` default: an account picker naming
    // nobody, for somebody who pressed a button in their own console.
    assert!(
        url.contains("/connect?"),
        "must start the grant flow on the site's chooser: {url}"
    );
    assert!(
        url.contains("code_challenge_method=S256"),
        "plain must never be offered: {url}"
    );
    assert!(
        url.contains("key%3Dlink"),
        "the return leg needs this console's own marker, not the hub's key=auth: {url}"
    );

    // The verifier is the one thing that must not be in a URL the browser
    // follows. Only its SHA-256 goes out, and the response carries neither the
    // verifier nor anything else redeemable.
    let state_value = state_param(url);
    let link = state
        .hub_links()
        .take(&state_value, "acme")
        .expect("the start parked a pending link");
    assert!(
        !url.contains(&link.verifier),
        "the verifier must never leave this host: {url}"
    );
    assert!(url.contains(&crate::server::hub_link::challenge_for(&link.verifier)));
}

/// The scopes ask reaches the wire, from the shared builder rather than here.
///
/// A key minted without `connections` cannot drive
/// `/agent-integrations/composio/*`, and this route is the only way a console
/// that is not a provisioned tenant gets a key at all — so the ask being
/// absent from this URL is the whole third-party integrations surface
/// answering 403, with nothing local to point at.
#[tokio::test]
async fn starting_a_link_asks_the_hub_for_the_connections_scope() {
    let home_dir = home();
    let state = state_with_hub(home_dir.path(), "acme").await;

    let (status, resp, raw) = send(
        &state,
        "acme",
        "POST",
        "/api/v1/company/credential/link/start",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");

    let url = resp["authorizeUrl"].as_str().expect("authorizeUrl");
    let (_, query) = url.split_once('?').expect("a query");

    // Last, and exactly where `key_grant_query` puts it. The route appends
    // nothing of its own to the builder's output, so a `scopes` that arrived
    // by concatenation here would land somewhere else or twice —
    // `hub_identity::both_readers_carry_the_same_built_query` pins the other
    // half of that.
    assert!(
        query.ends_with("&scopes=connections"),
        "managed Composio 403s without the ask: {url}"
    );
    assert_eq!(
        query.matches("scopes=").count(),
        1,
        "one ask, from one builder: {url}"
    );
}

/// The grant runs the same fan-out `PUT …/credential` does, and — per Q10 —
/// declares no provider of its own: `finish_link_runs_the_fan_out_and_writes_no_inference_config`
/// pins the "no entry zero, no `inference/config`" half of that; this test
/// keeps the acceptance shape (one grant, no key echoed, single-use).
#[tokio::test]
async fn finishing_a_link_copies_the_minted_key_without_declaring_a_provider() {
    let home_dir = home();
    let state = state_with_hub(home_dir.path(), "acme").await;

    let (_, resp, _) = send(
        &state,
        "acme",
        "POST",
        "/api/v1/company/credential/link/start",
        Some(json!({})),
    )
    .await;
    let url = resp["authorizeUrl"].as_str().unwrap().to_string();
    let state_value = state_param(&url);

    // Play the hub: it is the one participant that legitimately learns both the
    // verifier (from the challenge it was sent, at redemption) and the code it
    // handed the browser. Peeked rather than taken, so the link is still parked
    // for the route to spend.
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

    let (status, resp, raw) = send(
        &state,
        "acme",
        "POST",
        "/api/v1/company/credential/link/finish",
        Some(json!({ "state": state_value, "code": "grant-code" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");

    // One grant, and the agents already think through it by resolution
    // (`resolve_effective` reads the account key for a managed provider) — no
    // second declaration needed for Connections to report the identity.
    assert_eq!(resp["status"]["configured"], true);
    assert_eq!(resp["status"]["source"], "company");
    assert!(
        !raw.contains(GRANTED_KEY),
        "the minted key must never be echoed to the console: {raw}"
    );

    // Single-use: the same handle and code cannot be spent again.
    let (status, _, raw) = send(
        &state,
        "acme",
        "POST",
        "/api/v1/company/credential/link/finish",
        Some(json!({ "state": state_value, "code": "grant-code" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{raw}");
}

/// The desktop callback redeems the grant itself, with no console session.
#[tokio::test]
async fn the_hosts_own_return_route_redeems_a_grant_without_a_session() {
    let home_dir = home();
    let state = state_with_hub(home_dir.path(), "acme").await;
    let (_, resp, _) = send(
        &state,
        "acme",
        "POST",
        "/api/v1/company/credential/link/start",
        Some(json!({})),
    )
    .await;
    let url = resp["authorizeUrl"].as_str().unwrap().to_string();
    let state_value = state_param(&url);
    let callback = url
        .split_once("callback_url=")
        .map(|(_, rest)| rest.split('&').next().unwrap_or(rest))
        .expect("a callback_url");
    let callback = percent_decode(callback);
    assert!(
        callback.starts_with("http://127.0.0.1:8080/auth/key/callback?"),
        "with no origin to return to, the host's own route is the callback: {callback}"
    );

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
    let request = Request::builder()
        .method("GET")
        .uri(format!(
            "/auth/key/callback?company=acme&key=link&state={state_value}&code=grant-code"
        ))
        .body(Body::empty())
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let page = String::from_utf8_lossy(&bytes).to_string();
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("Connected"), "{page}");
    assert!(
        !page.contains(GRANTED_KEY),
        "the minted key must not be shown: {page}"
    );

    let (_, resp, _) = send(&state, "acme", "GET", "/api/v1/company/credential", None).await;
    assert_eq!(resp["configured"], true);
    assert_eq!(resp["source"], "company");
}

#[tokio::test]
async fn the_hosts_own_return_route_reports_a_declined_grant() {
    let home_dir = home();
    let state = state_with_hub(home_dir.path(), "acme").await;
    let request = Request::builder()
        .method("GET")
        .uri("/auth/key/callback?company=acme&key=link&state=x&error=access_denied")
        .body(Body::empty())
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let page = String::from_utf8_lossy(&bytes);
    assert_eq!(status, StatusCode::BAD_REQUEST, "{page}");
    assert!(page.contains("access_denied"), "{page}");
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

#[tokio::test]
async fn a_replayed_or_unknown_state_is_refused() {
    let home_dir = home();
    let state = state_with_hub(home_dir.path(), "acme").await;

    let (status, _, raw) = send(
        &state,
        "acme",
        "POST",
        "/api/v1/company/credential/link/finish",
        Some(json!({ "state": "never-minted", "code": "whatever" })),
    )
    .await;
    // A `state` this host never minted, one that expired, and one already spent
    // are the same answer on purpose: the remedy is identical, and telling them
    // apart would say which handles had once been real.
    assert_eq!(status, StatusCode::BAD_REQUEST, "{raw}");
}

#[tokio::test]
async fn a_member_cannot_start_or_finish_a_link() {
    let home_dir = home();
    let state = state_with_hub(home_dir.path(), "acme").await;
    crate::server::test_support::seed_fixed_member(&state, "acme").await;
    let member = crate::server::test_support::member_cookie("acme");

    // Same authority `PUT /credential` needs. That the key is minted rather
    // than pasted changes who types it, not what it does.
    let (status, _, raw) = send_as(
        &state,
        "POST",
        "/api/v1/company/credential/link/start",
        Some(json!({})),
        member.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{raw}");

    let (status, _, raw) = send_as(
        &state,
        "POST",
        "/api/v1/company/credential/link/finish",
        Some(json!({ "state": "x", "code": "y" })),
        member,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{raw}");
}

// ---------------------------------------------------------------------------
// `GET .../credential/billing`
// ---------------------------------------------------------------------------

/// A company with no key of its own reports `configured: false` and no
/// figures — never a fallback account's balance.
///
/// This is the negative control for a real regression: `get_billing` once
/// resolved through [`crate::company::company_key::resolve`], which falls
/// through to this instance's platform identity when the company has set
/// nothing. That would report `configured: true` and query billing for the
/// shared host identity — exposing that account's balance and plan to any
/// company member. The route must load the company's own credential only.
#[tokio::test]
async fn a_company_with_no_key_reports_unconfigured_billing_not_a_fallback_balance() {
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
    assert!(dto["summary"].is_null(), "{raw}");
}
