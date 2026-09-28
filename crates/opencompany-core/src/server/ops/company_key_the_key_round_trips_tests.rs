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

/// The core round trip: an admin sets the key, the read plane reports it as the
/// company's own identity, and the value never comes back out.
#[tokio::test]
async fn the_key_round_trips_write_only_and_reports_the_company_tier() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "acme", GRANTED).await;

    // Nothing set, and the test process carries no platform identity: the
    // honest degraded state, not a broken picker.
    let (status, dto, raw) = send(&state, "acme", "GET", "/api/v1/company/credential", None).await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(dto["configured"], false);
    assert_eq!(dto["source"], "none");
    let degraded = dto["notice"].as_str().unwrap_or_default();
    assert!(
        degraded.contains("no provider can be connected"),
        "the degraded state has to say what is unavailable: {dto}"
    );
    // …and must not overstate it. "Providers cannot be connected or used" read
    // as "nothing works", and a company whose LLM page holds a key of its own
    // goes on thinking perfectly well without this credential — that key
    // outranks it in the managed chain, and a provider of its own never
    // consults it. Overstating the breakage sends that operator to fix
    // something that is not broken.
    assert!(
        degraded.contains("can still think while this is unset"),
        "the degraded state must not claim the whole company has stopped: {dto}"
    );
    assert!(dto.get("key").is_none(), "status must never carry the key");

    // Set it.
    let (status, resp, raw) = send(
        &state,
        "acme",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(resp["status"]["configured"], true);
    assert_eq!(resp["status"]["source"], "company");
    assert!(!raw.contains(KEY), "PUT response leaked the key: {raw}");

    // The consequence is stated, because it is the thing an admin most needs to
    // understand before pasting.
    let notice = resp["status"]["notice"].as_str().unwrap_or_default();
    assert!(notice.contains("spend"), "{notice}");
    assert!(notice.contains("company"), "{notice}");
    // …and so is the distinction from the model-provider key. These two cards
    // sit next to each other and both read "configured"; the copy is the only
    // thing standing between an admin and pasting an OpenRouter key here.
    assert!(
        notice.contains("LLM page"),
        "the notice must say which key this is NOT: {notice}"
    );

    // The fan-out consequence, stated before the save rather than discovered on
    // the next invoice — and stated *conditionally*, because it is conditional
    // twice. This same notice comes back from the paste route and the grant
    // route, and they do different amounts: a paste fans the key out to
    // Composio and the LLM TinyHumans slot and stops there, while `finish_link`
    // runs the same fan-out and declares no provider of its own (Q10). And the
    // managed chain has two rungs above the copy this fan-out makes (a key
    // pasted for TinyHumans on the LLM page directly, then the legacy
    // `inference/key`), either of which goes on answering after this one is
    // set — #2266. A flat "this moves every agent turn onto the account" would
    // be false on both counts.
    assert!(
        notice.contains("no key of their own"),
        "the notice must say the copy never overwrites a key set on its own page: {notice}"
    );
    assert!(
        notice.contains("only when no default is set"),
        "the notice must not promise a default move a higher rung would prevent: {notice}"
    );

    // And it must not overshoot the other way. "It is not the model-provider
    // key" was false: this credential very often IS what the agents think on,
    // and denying it sends an admin hunting for a second key they do not need.
    // The distinction that survives is narrower — not a *provider's* key.
    assert!(
        !notice.contains("not the model-provider key"),
        "the notice must not claim this key has nothing to do with models: {notice}"
    );

    // GET reflects it and still never carries the key.
    let (_, dto, raw) = send(&state, "acme", "GET", "/api/v1/company/credential", None).await;
    assert_eq!(dto["configured"], true);
    assert_eq!(dto["source"], "company");
    assert!(!raw.contains(KEY), "GET status leaked the key: {raw}");
}

/// Acceptance: a company with its key set can connect a provider without any
/// per-tenant provider app — the fan-out (keys rework #2306, slice 4a) copies
/// the account key straight into `composio/tinyhumans/key`, so the Composio
/// plane reports the stored-token tier rather than falling through to the
/// shared brokered-credential seam.
#[tokio::test]
async fn setting_the_key_fills_the_composio_tinyhumans_key() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "brokered", GRANTED).await;

    // No Composio token, no platform identity in this process → nothing.
    let (_, dto, _) = send(&state, "brokered", "GET", "/api/v1/company/composio", None).await;
    assert_eq!(dto["credentialSource"], "none");

    send(
        &state,
        "brokered",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;

    // The company key alone credentials Composio. This is the issue in one
    // assertion: no Composio token pasted anywhere, no provider app, still
    // connectable — via the fan-out's own copy, which reads back as the
    // stored-token tier (`static`), not the fallback tier (`company`).
    let (_, dto, raw) = send(&state, "brokered", "GET", "/api/v1/company/composio", None).await;
    assert_eq!(dto["credentialSource"], "static", "{raw}");
    assert!(
        !raw.contains(KEY),
        "the Composio status leaked the key: {raw}"
    );
}

/// Acceptance: clearing is real, and reverts to the honest degraded state
/// rather than stranding the console on a stale "connected" claim.
///
/// The first `PUT` fans the key out to `composio/tinyhumans/key`, so the
/// clear below is exactly what the in-use guard exists for (its Composio copy
/// still equals the account key) — hence `confirmInUse: true`. Phase-4a's own
/// plan predates that guard and called this test "unchanged"; applying
/// `docs/key-reworks/in-use-guards.md` on top is this dispatch's added scope,
/// and this is the one place it changes what an already-passing test has to
/// send.
#[tokio::test]
async fn clearing_the_key_reverts_to_the_degraded_state() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "cleared", GRANTED).await;
    send(
        &state,
        "cleared",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;

    let (_, resp, _) = send(
        &state,
        "cleared",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": "", "confirmInUse": true })),
    )
    .await;
    assert_eq!(resp["status"]["configured"], false);
    assert_eq!(resp["status"]["source"], "none");

    let (_, dto, _) = send(&state, "cleared", "GET", "/api/v1/company/composio", None).await;
    assert_eq!(dto["credentialSource"], "none");
}

/// A company's own Composio token still outranks the company key — the BYO
/// escape hatch is not taken away by this change.
#[tokio::test]
async fn a_pasted_composio_token_still_outranks_the_company_key() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "byo", GRANTED).await;
    send(
        &state,
        "byo",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    send(
        &state,
        "byo",
        "PUT",
        "/api/v1/company/composio/token",
        Some(json!({ "token": "byo-composio-token" })),
    )
    .await;

    let (_, dto, _) = send(&state, "byo", "GET", "/api/v1/company/composio", None).await;
    assert_eq!(
        dto["credentialSource"], "static",
        "a company that pasted its own Composio token keeps it: {dto}"
    );

    // The company key is still stored — the two are separate slots, and
    // clearing the Composio token falls back to the company's own identity
    // rather than to nothing. Guarded while the company is on the managed
    // route (in-use-guards.md §2); this test is about the fallback tier,
    // not the guard, so it confirms.
    send(
        &state,
        "byo",
        "PUT",
        "/api/v1/company/composio/token",
        Some(json!({ "token": "", "confirmInUse": true })),
    )
    .await;
    let (_, dto, _) = send(&state, "byo", "GET", "/api/v1/company/composio", None).await;
    assert_eq!(dto["credentialSource"], "company", "{dto}");
}

/// The two read planes answer **different questions**, and a company holding
/// both credentials is where that stops being pedantry.
///
/// `GET …/credential` reports whose identity the company *has*; `GET …/composio`
/// reports what a Composio call *presents*, which its BYO token overrides. Both
/// are correct simultaneously, and any refactor that "unifies" them would have
/// to break one of these two assertions.
#[tokio::test]
async fn the_credential_plane_and_the_composio_plane_may_honestly_disagree() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "disagree", GRANTED).await;

    send(
        &state,
        "disagree",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    send(
        &state,
        "disagree",
        "PUT",
        "/api/v1/company/composio/token",
        Some(json!({ "token": "byo-composio-token" })),
    )
    .await;

    let (_, credential, _) = send(
        &state,
        "disagree",
        "GET",
        "/api/v1/company/credential",
        None,
    )
    .await;
    let (_, composio, _) = send(&state, "disagree", "GET", "/api/v1/company/composio", None).await;

    assert_eq!(
        credential["source"], "company",
        "the company's own identity is set, whatever Composio presents: {credential}"
    );
    assert_eq!(
        composio["credentialSource"], "static",
        "a Composio call presents the BYO token, whatever identity the company holds: {composio}"
    );
    assert_eq!(credential["configured"], true);
}

/// The write is admin-only, for the same reason the Composio token write is:
/// this key repoints the company's entire brokered surface at whatever account
/// the caller controls, and it is the company's wallet.
#[tokio::test]
async fn a_member_cannot_set_the_companys_credential() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "acme", GRANTED).await;
    let member =
        crate::server::test_support::seed_session(&state, "acme", crate::ports::UserRole::Member)
            .await;

    let (status, body, raw) = send_as(
        &state,
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
        member,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{raw}");
    assert_eq!(body["code"], "forbidden", "{body}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("admin"),
        "the refusal has to say why: {body}"
    );

    // The refusal is real, not merely a different status.
    let (_, dto, _) = send(&state, "acme", "GET", "/api/v1/company/credential", None).await;
    assert_eq!(
        dto["configured"], false,
        "a refused write must not have stored anything: {dto}"
    );
}

/// Setting the credential is journaled, so a change to what the company acts
/// through is never invisible — and a clear is told apart from a set.
#[tokio::test]
async fn setting_and_clearing_are_journaled_with_an_actor() {
    use crate::ports::types::CompanyEvent;

    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "audited", GRANTED).await;
    send(
        &state,
        "audited",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    send(
        &state,
        "audited",
        "PUT",
        "/api/v1/company/credential",
        // The first PUT already fanned the key out to Composio, so this clear
        // is guarded (its Composio copy still equals the account key).
        Some(json!({ "key": "", "confirmInUse": true })),
    )
    .await;

    let id = CompanyId::new("audited");
    let runtime = state.registry().get(&id).expect("registered");
    let events = runtime
        .events()
        .read_from(&id, crate::ports::types::EventSeq::new(0), 200)
        .await
        .expect("events");
    let changes: Vec<(String, bool)> = events
        .iter()
        .filter_map(|stored| match &stored.event {
            CompanyEvent::ToolAccessChanged { change, by, .. } => {
                Some((change.clone(), by.is_some()))
            }
            _ => None,
        })
        .collect();
    assert!(
        changes.contains(&("company_key_set".to_string(), true)),
        "a set must be journaled with who did it: {changes:?}"
    );
    assert!(
        changes.contains(&("company_key_cleared".to_string(), true)),
        "a clear must be told apart from a set: {changes:?}"
    );
    // …and it must not borrow the Composio token's vocabulary. Both routes
    // append `ToolAccessChanged` to one log, so if this route spoke
    // `credential_set` an auditor could not tell a rotation of the company's
    // whole identity from a swap of one integration's token.
    assert!(
        !changes
            .iter()
            .any(|(change, _)| change == "credential_set" || change == "credential_cleared"),
        "no Composio write happened here, so no Composio audit word may appear: {changes:?}"
    );
}

/// An [`EventLog`](crate::ports::events::EventLog) decorator whose `append`
/// can be switched to fail after setup, so a test can build a real company
/// through a working log and then drive a route through the journal-refusal
/// arm. Reads always delegate to a real
/// [`FsEventLog`](crate::store::fs::FsEventLog), so `build()`'s own boot reads
/// are never touched by the failure.
struct FailingAppendLog {
    inner: crate::store::fs::FsEventLog,
    fail_appends: std::sync::atomic::AtomicBool,
}

impl FailingAppendLog {
    fn new(inner: crate::store::fs::FsEventLog) -> Self {
        Self {
            inner,
            fail_appends: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn fail_appends_from_now_on(&self) {
        self.fail_appends
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl crate::ports::events::EventLog for FailingAppendLog {
    async fn append(
        &self,
        id: &CompanyId,
        event: crate::ports::types::CompanyEvent,
    ) -> crate::Result<crate::ports::types::EventSeq> {
        if self.fail_appends.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::error::OpenCompanyError::Config(
                "the event journal is unwritable".to_string(),
            ));
        }
        self.inner.append(id, event).await
    }

    async fn read_from(
        &self,
        id: &CompanyId,
        seq: crate::ports::types::EventSeq,
        limit: usize,
    ) -> crate::Result<Vec<crate::ports::types::StoredEvent>> {
        self.inner.read_from(id, seq, limit).await
    }

    fn subscribe(
        &self,
        id: &CompanyId,
    ) -> futures::stream::BoxStream<'static, crate::ports::events::EventStreamItem> {
        self.inner.subscribe(id)
    }
}

/// [`state_with_manifest`], with the company's journal swapped for
/// [`FailingAppendLog`] — armed only after the company finishes booting, so
/// `RuntimeBuilder::build`'s own event reads/writes see a working log and the
/// test controls exactly when the journal starts refusing.
async fn state_with_failing_journal(
    home: &std::path::Path,
    company: &str,
    manifest_toml: &str,
) -> (AppState, std::sync::Arc<FailingAppendLog>) {
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
    let journal = std::sync::Arc::new(FailingAppendLog::new(crate::store::fs::FsEventLog::new(
        home.to_path_buf(),
    )));
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .with_events(journal.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, company).await;
    super::prober_override::set(company, Ok(vec!["acme/test-model".to_string()]));
    (state, journal)
}

/// Issue #403's discipline, the unhappy half: `set_key`'s own doc comment
/// says the journal write is propagated rather than swallowed "so a change to
/// what the company's agents act through is never invisible" — but a
/// propagated error is not the same as an undone one. `store_key` has already
/// landed by the time `journal` runs, so a refused audit line leaves the
/// credential rotated with the caller holding nothing but a 500.
///
/// This is the documented trade, not a bug this test is trying to catch —
/// but until now nothing forced the journal to refuse and checked which side
/// of "propagates rather than swallows" actually happened.
#[tokio::test]
async fn a_journal_failure_after_the_key_is_stored_still_leaves_the_key_stored() {
    let home_dir = home();
    let (state, journal) = state_with_failing_journal(home_dir.path(), "acme", GRANTED).await;
    let app = router(state.clone());
    let cookie = crate::server::test_support::fixed_cookie("acme");

    journal.fail_appends_from_now_on();

    let (status, _, raw) = send_as(
        &state,
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
        cookie.clone(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a refused journal write must not be swallowed into a 200: {raw}"
    );

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/company/credential")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let status_body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        status_body["configured"], true,
        "the key was stored before the journal ever ran, so it stays stored even though the \
         caller was told the request failed: {status_body}"
    );
}

// ---------------------------------------------------------------------------
// The account-key fan-out (keys rework #2306, slice 4a) and its in-use guard.
// ---------------------------------------------------------------------------

/// One slot's `outcome` field, looked up by slot name rather than by
/// position — `company_key::fan_out` promises the order, but a test should
/// not have to remember it to read one entry.
pub(super) fn slot_outcome<'a>(resp: &'a Value, slot: &str) -> &'a Value {
    let entry = resp["slots"]
        .as_array()
        .expect("slots array")
        .iter()
        .find(|s| s["slot"] == slot)
        .unwrap_or_else(|| panic!("no {slot} slot in {resp}"));
    &entry["outcome"]
}

/// M1 by route: a bare key save on an empty company fills Composio and the
/// LLM copy, cannot create a row or a default for want of a model, and says
/// so — the exact JSON shape `phase-4a-account-key-fanout.md` §3.3 quotes.
#[tokio::test]
async fn put_credential_answers_slots_and_needs_model() {
    let home_dir = home();
    let state = state_with_manifest(home_dir.path(), "fanm1", GRANTED).await;

    let (status, resp, raw) = send(
        &state,
        "fanm1",
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");

    assert_eq!(*slot_outcome(&resp, "composio"), json!("filled"));
    assert_eq!(*slot_outcome(&resp, "inference"), json!("filled"));
    assert_eq!(*slot_outcome(&resp, "provider"), json!("skipped"));
    assert_eq!(*slot_outcome(&resp, "default"), json!("skipped"));
    assert_eq!(*slot_outcome(&resp, "health"), json!("ok"));
    assert_eq!(resp["needsModel"], true);
    assert_eq!(resp["setsDefault"], true);
    assert!(
        resp["models"]
            .as_array()
            .unwrap()
            .contains(&json!("acme/test-model")),
        "{resp}"
    );

    let (_, inference, raw) = send(&state, "fanm1", "GET", "/api/v1/company/inference", None).await;
    assert!(
        inference["providers"].as_array().unwrap().is_empty(),
        "no row without a model: {raw}"
    );
}
