use super::*;
use serde_json::json;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::types::ApprovalId;
use crate::ports::types::CompanyRecord;
use crate::ports::users::{UserRecord, UserRole, UserStatus};
use crate::ports::{CompanyStore, SessionKind, SessionRecord};
use crate::server::users::cookie::session_cookie_name;
use crate::server::users::token::{OsTokens, mint_session_token, sha256_hex};
use crate::store::FsCompanyStore;

use super::test_support::*;

#[test]
fn a_parked_turn_carries_an_approval_notification_but_still_ends_the_turn() {
    let report = crate::runtime::CycleReport {
        cycle_id: "c1".to_string(),
        responses: Vec::new(),
        executed_effects: Vec::new(),
        parked: vec![ApprovalId::from("appr-1".to_string())],
        persisted_seq: None,
        input_seqs: Vec::new(),
    };
    let result = prompt_result(report);
    assert_eq!(result["stopReason"], "end_turn");
    let updates = result["updates"].as_array().expect("updates array");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["_meta"]["opencompany/approval"]["id"], "appr-1");
}

#[test]
fn a_clean_turn_carries_no_approval_notification() {
    let report = crate::runtime::CycleReport {
        cycle_id: "c1".to_string(),
        responses: Vec::new(),
        executed_effects: Vec::new(),
        parked: Vec::new(),
        persisted_seq: None,
        input_seqs: Vec::new(),
    };
    let result = prompt_result(report);
    assert_eq!(result["stopReason"], "end_turn");
    assert!(
        result["updates"]
            .as_array()
            .expect("updates array")
            .is_empty()
    );
}

// -----------------------------------------------------------------
// PLAT-057 / PLAT-060: the `call` HTTP handler itself. Every test above
// this point calls `open_session`/`prompt`/etc. directly, bypassing the
// axum extraction, method dispatch and JSON-RPC envelope that only `call`
// (mounted by `router()`) actually implements.
// -----------------------------------------------------------------

/// PLAT-060 (AUTH): a `none`-mode company's local owner is reachable over
/// the real HTTP `call` handler with zero credentials — no cookie, no
/// bearer — same as every other credential-less surface that mode grants.
#[tokio::test]
async fn call_handler_authenticates_a_credential_less_none_mode_request() {
    let home = tempfile::Builder::new()
        .prefix("oc-acp-call-none-")
        .tempdir()
        .expect("tempdir");
    let state = acp_state_none_mode(home.path()).await;
    let app = router().with_state(state);

    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    let response = app.oneshot(acp_call_request(body)).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["jsonrpc"], "2.0");
    assert_eq!(value["id"], 1);
    assert_eq!(value["result"]["protocolVersion"], 1);
}

/// PLAT-057 (AUTH): a company with real sign-in refuses an unauthenticated
/// `call`, over HTTP — not just at the level of the extractor unit tests.
#[tokio::test]
async fn call_handler_refuses_an_unauthenticated_request() {
    let home = tempfile::Builder::new()
        .prefix("oc-acp-call-auth-")
        .tempdir()
        .expect("tempdir");
    let state = acp_state(home.path()).await;
    let app = router().with_state(state);

    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    let response = app.oneshot(acp_call_request(body)).await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// PLAT-057 (STATE): a user who must change their password is refused
/// *before* any ACP method runs, over the real HTTP path — the same
/// boundary `ScopedCompany` enforces for the operator API.
#[tokio::test]
async fn call_handler_refuses_a_temporary_password_user_before_running_any_method() {
    let home = tempfile::Builder::new()
        .prefix("oc-acp-call-temp-pw-")
        .tempdir()
        .expect("tempdir");
    let state = acp_state(home.path()).await;
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).expect("company");
    let now = crate::ports::now_millis();
    runtime
        .users()
        .upsert_user(
            &company,
            &UserRecord {
                id: "u-temp".to_string(),
                email: "temp@example.test".to_string(),
                display_name: None,
                avatar: None,
                role: UserRole::Admin,
                status: UserStatus::Active,
                password_hash: None,
                must_change_password: true,
                created_at_millis: now,
                last_seen_at_millis: None,
                updated_at_millis: now,
            },
        )
        .await
        .expect("seed temp-password user");
    let token = mint_session_token(&OsTokens);
    runtime
        .sessions()
        .create(
            &company,
            &SessionRecord {
                id: "s-temp".to_string(),
                token_hash: sha256_hex(&token),
                user_id: "u-temp".to_string(),
                created_at_millis: now,
                expires_at_millis: now + 60_000,
                user_agent: None,
                kind: SessionKind::Browser,
                label: None,
            },
        )
        .await
        .expect("seed session");
    let cookie_name = session_cookie_name(&company).expect("cookie name");

    let app = router().with_state(state);
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    let mut request = acp_call_request(body);
    request.headers_mut().insert(
        axum::http::header::COOKIE,
        format!("{cookie_name}={token}").parse().unwrap(),
    );
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// PLAT-057 (FAIL): an unsupported ACP method comes back as a `-32602`
/// JSON-RPC error envelope, over the real HTTP path — not a raw error, not
/// an HTTP-level 4xx/5xx.
#[tokio::test]
async fn call_handler_reports_an_unsupported_method_as_a_json_rpc_error() {
    let home = tempfile::Builder::new()
        .prefix("oc-acp-call-fail-")
        .tempdir()
        .expect("tempdir");
    let state = acp_state_none_mode(home.path()).await;
    let app = router().with_state(state);

    let body = json!({
        "jsonrpc": "2.0",
        "id": "req-9",
        "method": "session/frobnicate",
        "params": {},
    });
    let response = app.oneshot(acp_call_request(body)).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["id"], "req-9");
    assert_eq!(value["error"]["code"], -32602);
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("session/frobnicate")
    );
}

/// PLAT-057 (BOUND): the JSON-RPC `id` round-trips exactly, including the
/// boundary case of a request that omits it entirely (must answer `null`,
/// not fail or invent one).
#[tokio::test]
async fn call_handler_echoes_the_request_id_including_when_absent() {
    let home = tempfile::Builder::new()
        .prefix("oc-acp-call-bound-")
        .tempdir()
        .expect("tempdir");
    let state = acp_state_none_mode(home.path()).await;
    let app = router().with_state(state);

    let body = json!({ "jsonrpc": "2.0", "id": 42, "method": "initialize", "params": {} });
    let response = app.clone().oneshot(acp_call_request(body)).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["id"], 42);

    let body = json!({ "jsonrpc": "2.0", "method": "initialize", "params": {} });
    let response = app.oneshot(acp_call_request(body)).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["id"], Value::Null);
    assert_eq!(value["result"]["protocolVersion"], 1);
}

/// PLAT-057 (INPUT): a body that is not valid JSON at all must not panic
/// or hang the handler — it is rejected before `call`'s body even runs.
#[tokio::test]
async fn call_handler_rejects_a_body_that_is_not_valid_json() {
    let home = tempfile::Builder::new()
        .prefix("oc-acp-call-input-")
        .tempdir()
        .expect("tempdir");
    let state = acp_state_none_mode(home.path()).await;
    let app = router().with_state(state);

    let request = Request::builder()
        .method("POST")
        .uri("/acp")
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(b"{ this is not json".to_vec()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();

    assert!(
        response.status().is_client_error(),
        "a malformed JSON body must be rejected, got {:?}",
        response.status()
    );
}

/// PLAT-057 (LIMIT): the per-connection session cap
/// (`MAX_SESSIONS_PER_CONNECTION`) is enforced through the real HTTP `call`
/// handler, not just the `open_session` inner function every test above
/// this point calls directly — the handler's own JSON-RPC dispatch and
/// envelope sit between a caller and that cap in production, and neither
/// was ever exercised together with it.
#[tokio::test]
async fn call_handler_refuses_session_new_once_the_per_connection_cap_is_hit() {
    let home = tempfile::Builder::new()
        .prefix("oc-acp-call-limit-")
        .tempdir()
        .expect("tempdir");
    let state = acp_state(home.path()).await;
    let company = CompanyId::new("acme");
    let cookie = seed_admin_session_cookie(&state, &company, "u-cap-admin").await;
    let app = router().with_state(state);

    for i in 0..crate::server::acp::session::MAX_SESSIONS_PER_CONNECTION {
        let response = app
            .clone()
            .oneshot(session_new_request(&cookie, "conn-http-cap", i as u64))
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            value.get("result").is_some(),
            "open #{i} must succeed: {value:?}"
        );
    }

    let response = app
        .oneshot(session_new_request(&cookie, "conn-http-cap", 999))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "still a JSON-RPC 200");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        value.get("error").is_some(),
        "the session past the cap must come back a JSON-RPC error: {value:?}"
    );
}

/// PLAT-057 (CONC): two different principals racing `session/new` on the
/// same `connectionId`, over the real HTTP handler concurrently rather than
/// sequentially. `a_caller_cannot_open_a_session_on_a_connection_it_does_
/// not_own` proves the inner function's ownership rule one call at a time;
/// this proves the lock the handler sits on top of actually serializes two
/// requests that land at the same time, rather than both racing past a
/// check-then-insert and one silently losing its own connection.
#[tokio::test]
async fn call_handler_lets_only_one_of_two_concurrent_openers_win_a_connection() {
    let home = tempfile::Builder::new()
        .prefix("oc-acp-call-conc-")
        .tempdir()
        .expect("tempdir");
    let state = acp_state(home.path()).await;
    let company = CompanyId::new("acme");
    let cookie_a = seed_admin_session_cookie(&state, &company, "u-conc-a").await;
    let cookie_b = seed_admin_session_cookie(&state, &company, "u-conc-b").await;
    let app = router().with_state(state);

    let request_a = session_new_request(&cookie_a, "conn-http-race", 1);
    let request_b = session_new_request(&cookie_b, "conn-http-race", 2);
    let app_a = app.clone();
    let app_b = app.clone();
    let (response_a, response_b) = tokio::join!(app_a.oneshot(request_a), app_b.oneshot(request_b));

    let value_a: Value = serde_json::from_slice(
        &axum::body::to_bytes(response_a.unwrap().into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let value_b: Value = serde_json::from_slice(
        &axum::body::to_bytes(response_b.unwrap().into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();

    let winners = [
        value_a.get("result").is_some(),
        value_b.get("result").is_some(),
    ]
    .into_iter()
    .filter(|ok| *ok)
    .count();
    assert_eq!(
        winners, 1,
        "exactly one of two concurrent openers may win a shared connection: \
         {value_a:?} / {value_b:?}"
    );
}

/// PLAT-060 (STATE): `local_owner` falls back to `registry().sole()` when
/// `/acp` cannot name an addressed company (it has no `{id}` path param).
/// Once a second company is registered, `sole()` no longer resolves — the
/// credential-less request must come back unauthorized, not silently
/// authorized against whichever company happens to be `none`-mode.
#[tokio::test]
async fn call_handler_none_mode_owner_is_unreachable_once_a_second_company_exists() {
    let home = tempfile::Builder::new()
        .prefix("oc-acp-call-state-")
        .tempdir()
        .expect("tempdir");
    let state = acp_state_none_mode(home.path()).await;

    // Control, proven first on this exact state: with only the sole
    // `none`-mode company registered, the credential-less request
    // succeeds — the same claim `call_handler_authenticates_a_credential_
    // less_none_mode_request` makes, repeated here so the 401 asserted
    // below is shown to depend on the second company, not on some other
    // difference between the two tests' fixtures.
    let control_app = router().with_state(state.clone());
    let control_body = json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {} });
    let control_response = control_app
        .oneshot(acp_call_request(control_body))
        .await
        .unwrap();
    assert_eq!(
        control_response.status(),
        StatusCode::OK,
        "control: the sole none-mode company must still answer credential-less \
         before a second company is registered"
    );

    let manifest: CompanyManifest = toml::from_str("[company]\nname = \"Globex\"\n").unwrap();
    let store = FsCompanyStore::new(home.path().to_path_buf());
    let globex = CompanyId::new("globex");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: globex.clone(),
            manifest: manifest.clone(),
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: Vec::new(),
            overlay_desk_members: Vec::new(),
            overlay_desk_order: Vec::new(),
            overlay_desk_hive: Vec::new(),
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
    let globex_runtime = crate::runtime::RuntimeBuilder::new(home.path().to_path_buf(), manifest)
        .with_id(globex.clone())
        .with_brain(std::sync::Arc::new(SilentBrain))
        .build()
        .await
        .unwrap();
    state
        .registry()
        .insert(globex, std::sync::Arc::new(globex_runtime));

    let app = router().with_state(state);
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    let response = app.oneshot(acp_call_request(body)).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a none-mode company sharing a host with a second company must not \
         silently authorize a credential-less /acp request against either one"
    );
}

/// PLAT-060 (FAIL): the same `none`-mode gate `local_owner` applies
/// everywhere else — a request carrying a forwarding header is refused
/// outright, never degraded to a session/bearer check — proven here over
/// the real HTTP `call` handler.
#[tokio::test]
async fn call_handler_none_mode_refuses_a_forwarded_request() {
    let home = tempfile::Builder::new()
        .prefix("oc-acp-call-fail2-")
        .tempdir()
        .expect("tempdir");
    let state = acp_state_none_mode(home.path()).await;
    let app = router().with_state(state);

    // Control: the identical request, minus the forwarding header,
    // succeeds on this exact app — so the refusal below is shown to
    // depend on the header, not on some other difference in the fixture.
    let control_body = json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {} });
    let control_response = app
        .clone()
        .oneshot(acp_call_request(control_body))
        .await
        .unwrap();
    assert_eq!(
        control_response.status(),
        StatusCode::OK,
        "control: the same none-mode host must answer without the forwarding header"
    );

    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    let mut request = acp_call_request(body);
    request
        .headers_mut()
        .insert("x-forwarded-for", "203.0.113.5".parse().unwrap());
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a none-mode company must refuse a forwarded request outright, not \
         treat it as its own local owner"
    );
}

/// PLAT-062 (STATE): a turn that both parked an approval and produced a
/// reply still reports `end_turn` — the mixed case, not just the two
/// single-field variations above.
#[test]
fn a_mixed_turn_with_a_reply_and_a_park_still_ends_the_turn() {
    let report = crate::runtime::CycleReport {
        cycle_id: "c1".to_string(),
        responses: vec![crate::ports::types::OutboundMessage {
            channel: "operator".to_string(),
            agent: None,
            text: "here's what I found".to_string(),
            steps: Vec::new(),
            reply_to: None,
            task_id: None,
            outputs: Vec::new(),
            message_id: None,
            mentions: Vec::new(),
        }],
        executed_effects: Vec::new(),
        parked: vec![ApprovalId::from("appr-1".to_string())],
        persisted_seq: None,
        input_seqs: Vec::new(),
    };
    let result = prompt_result(report);
    assert_eq!(result["stopReason"], "end_turn");
    let updates = result["updates"].as_array().expect("updates array");
    // Counting alone would pass on two chunks and no notification, which is
    // the shape this case exists to refuse.
    let approvals: Vec<&str> = updates
        .iter()
        .filter_map(|u| u["_meta"]["opencompany/approval"]["id"].as_str())
        .collect();
    assert_eq!(
        approvals,
        vec!["appr-1"],
        "the parked approval must carry its own notification: {updates:?}"
    );
    assert!(
        updates.iter().any(|u| u["content"]["text"]
            .as_str()
            .is_some_and(|t| t.contains("here's what I found"))),
        "the reply chunk must survive alongside it: {updates:?}"
    );
    assert_eq!(updates.len(), 2, "and nothing else: {updates:?}");
}

/// PLAT-062 (BOUND): several parked approvals in one turn each get their
/// own notification — none dropped, none deduplicated, at the boundary of
/// "more than one".
#[test]
fn every_parked_approval_in_a_multi_park_turn_gets_its_own_notification() {
    let parked: Vec<ApprovalId> = (0..5)
        .map(|i| ApprovalId::from(format!("appr-{i}")))
        .collect();
    let report = crate::runtime::CycleReport {
        cycle_id: "c1".to_string(),
        responses: Vec::new(),
        executed_effects: Vec::new(),
        parked: parked.clone(),
        persisted_seq: None,
        input_seqs: Vec::new(),
    };
    let result = prompt_result(report);
    assert_eq!(result["stopReason"], "end_turn");
    let updates = result["updates"].as_array().expect("updates array");
    assert_eq!(updates.len(), 5);
    // Five *unique* ids is not the claim — five ids that are the ones we
    // parked is. A set of unrelated ids satisfies the former.
    let mut ids: Vec<&str> = updates
        .iter()
        .map(|u| u["_meta"]["opencompany/approval"]["id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    let mut expected: Vec<&str> = parked.iter().map(|id| id.as_ref()).collect();
    expected.sort_unstable();
    assert_eq!(
        ids, expected,
        "every parked id must appear exactly once: {updates:?}"
    );
}
