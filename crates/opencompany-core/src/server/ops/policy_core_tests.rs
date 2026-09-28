use super::*;

use axum::body::{Body, to_bytes};
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::CompanyStore;
use crate::ports::types::CompanyId;
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

const MANIFEST: &str = "[company]\nname = \"Acme\"\n\
     [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n\
     [policy]\nmode = \"supervised\"\n\
     always_approve = [\"payment.send\", \"filing.submit\"]\n\
     auto_approve_under_usd = 5.0\n\
     approval_ttl_hours = 24\n";

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-policy-")
        .tempdir()
        .expect("tempdir")
}

async fn state(home: &std::path::Path) -> AppState {
    let manifest: CompanyManifest = toml::from_str(MANIFEST).unwrap();
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

/// Same fixture as [`state`], but with an injected gate that has NOT been
/// built through the production path's `.with_policy_hitl_disabled()` —
/// the only way this suite can exercise `PolicyDto.policyHitlEnabled: true`
/// before any code path sets it live.
async fn state_with_policy_hitl_enabled(home: &std::path::Path) -> AppState {
    let manifest: CompanyManifest = toml::from_str(MANIFEST).unwrap();
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
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
            overlay_desk_hive: Vec::new(),
            disabled_workflows: Vec::new(),
            template_provenance: None,
            setup: None,
            name_confirmed: false,
            activation_completed_at: None,
            created_at_millis: None,
        })
        .await
        .unwrap();
    let gate = std::sync::Arc::new(crate::policy::gate::ManifestApprovalGate::new(
        manifest.policy.clone(),
    ));
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .with_approvals(gate)
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

async fn call(state: &AppState, method: &str, body: Option<Value>) -> (StatusCode, Value) {
    call_as(
        state,
        method,
        body,
        Some(crate::server::test_support::fixed_cookie("acme")),
    )
    .await
}

/// [`call`], with the caller's cookie under the test's own control —
/// `None` for no session at all — instead of always the fixed admin.
async fn call_as(
    state: &AppState,
    method: &str,
    body: Option<Value>,
    cookie: Option<String>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri("/api/v1/company/policy")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let request = match body {
        Some(value) => request.body(Body::from(value.to_string())).unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// `GET` reports the manifest's policy when nothing is overridden, and says
/// so — `overridden` is what the console keys the reset control off.
#[tokio::test]
async fn get_reports_the_manifest_policy_when_nothing_is_overridden() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, body) = call(&state, "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], "supervised");
    assert_eq!(body["manifestMode"], "supervised");
    assert_eq!(body["autoApproveUnderUsd"], 5.0);
    assert_eq!(body["manifestAutoApproveUnderUsd"], 5.0);
    assert_eq!(body["approvalTtlHours"], 24);
    assert_eq!(body["manifestApprovalTtlHours"], 24);
    assert_eq!(body["overridden"], false);
    assert!(body["setBy"].is_null());
    assert!(!body["tiers"].as_array().unwrap().is_empty());
}

/// `GET` also answers the console's "is this a real tool?" note: the
/// complete gateable registry, not the workflow-authorable subset. A wired
/// agent tool that cannot be a workflow node (`publish_artifact`,
/// `hosting_launch_site`) is still a fence the gate matches, so it must be
/// present — otherwise the console would call a working entry a mistake.
#[tokio::test]
async fn get_serves_the_complete_gateable_tool_registry() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, body) = call(&state, "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    let known_tools: &Vec<Value> = body["knownTools"]
        .as_array()
        .expect("knownTools is an array");
    assert!(
        known_tools.iter().any(|tool| tool == "publish_artifact"),
        "a wired agent tool the workflow catalog does not carry must be known"
    );
    assert!(
        known_tools.iter().any(|tool| tool == "shell"),
        "the registry still carries the workflow tools"
    );
}

/// `policyHitlEnabled` reports the live gate's own state, and every
/// production build reports it `false` — the console's honest "disabled"
/// copy must be reading a real fact, not a hardcoded one.
#[tokio::test]
async fn get_reports_policy_hitl_as_disabled_on_the_production_path() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, body) = call(&state, "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["policyHitlEnabled"], false);
}

/// The other half of the same fact: a gate that has NOT been built through
/// `.with_policy_hitl_disabled()` must report `true`, so the field really
/// does track the gate rather than always answering `false`.
#[tokio::test]
async fn get_reports_policy_hitl_as_enabled_when_the_gate_has_it_on() {
    let dir = home();
    let state = state_with_policy_hitl_enabled(dir.path()).await;
    let (status, body) = call(&state, "GET", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["policyHitlEnabled"], true);
}

/// A tier `PUT` moves the tier and leaves the always-ask list on the
/// manifest's value — the independence the console's two controls rely on.
#[tokio::test]
async fn putting_a_tier_leaves_the_always_ask_list_alone() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, body) = call(&state, "PUT", Some(json!({ "mode": "full" }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], "full");
    assert_eq!(body["overridden"], true);
    assert_eq!(
        body["alwaysApprove"],
        json!(["payment.send", "filing.submit"]),
        "setting the tier must not discard the manifest's always-ask list"
    );
    // And it persisted, rather than only being reflected in the response.
    let (_, reread) = call(&state, "GET", None).await;
    assert_eq!(reread["mode"], "full");
}

/// An emptied always-ask list is stored as empty, not resolved back to the
/// manifest's three defaults.
#[tokio::test]
async fn an_emptied_always_ask_list_is_stored_as_empty() {
    let dir = home();
    let state = state(dir.path()).await;
    let (status, body) = call(&state, "PUT", Some(json!({ "alwaysApprove": [] }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["alwaysApprove"], json!([]));
    assert_eq!(body["mode"], "supervised", "the tier must not have moved");
}

/// The spend cap and deadline use the same field-wise write behaviour as
/// the tier: changing either leaves every other policy setting alone.
#[tokio::test]
async fn putting_a_cap_or_deadline_overrides_only_that_field() {
    let dir = home();
    let state = state(dir.path()).await;

    let (status, body) = call(
        &state,
        "PUT",
        Some(json!({ "autoApproveUnderUsd": null, "approvalTtlHours": 72 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["autoApproveUnderUsd"].is_null());
    assert_eq!(body["approvalTtlHours"], 72);
    assert_eq!(body["mode"], "supervised");
    assert_eq!(
        body["alwaysApprove"],
        json!(["payment.send", "filing.submit"])
    );

    let (_, reread) = call(&state, "GET", None).await;
    assert!(reread["autoApproveUnderUsd"].is_null());
    assert_eq!(reread["approvalTtlHours"], 72);
}

/// The saved override reaches the live gate on the documented schedule
/// (issue #1455): the deadline immediately — a parked card is re-checked
/// against the current TTL each time it is displayed or swept — and the
/// tier/cap/always-ask half at the next turn boundary, applied by
/// `run_locked` so an in-flight turn finishes under the snapshot it started
/// with.
#[tokio::test]
async fn a_policy_put_applies_the_deadline_immediately_and_the_cap_next_turn() {
    use crate::ports::types::CompanyEvent;

    let dir = home();
    let state = state(dir.path()).await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).expect("registered").clone();
    assert_eq!(runtime.approval_gate.ttl_millis(), 24 * 60 * 60 * 1000);

    // Deadline-only PUT: the live gate's TTL moves without waiting for a turn.
    let (status, body) = call(&state, "PUT", Some(json!({ "approvalTtlHours": 72 }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["approvalTtlHours"], 72);
    assert_eq!(runtime.approval_gate.ttl_millis(), 72 * 60 * 60 * 1000);

    // Cap PUT: the snapshot must NOT move mid-turn...
    let (status, _) = call(&state, "PUT", Some(json!({ "autoApproveUnderUsd": 50 }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        runtime.approval_gate.policy().auto_approve_under_usd,
        Some(5.0),
        "the evaluation snapshot must not move mid-turn"
    );

    // ...and the next turn applies the effective policy snapshot. Policy
    // HITL is disabled, but the stored cap still moves on its documented
    // schedule for reporting and any future opt-in policy mode.
    runtime
        .run_cycle(vec![CompanyEvent::ScheduleFired {
            cron: "* * * * *".to_string(),
            prompt: "status".to_string(),
        }])
        .await
        .expect("the next turn runs");
    assert_eq!(
        runtime.approval_gate.policy().auto_approve_under_usd,
        Some(50.0)
    );
}

/// The test above proves the cap reaches the live policy snapshot "for
/// reporting", per its own comment. This proves the other half: on the
/// gate this route's `state()` fixture actually builds — through
/// `RuntimeBuilder`, exactly as production does, policy HITL disabled —
/// setting `autoApproveUnderUsd` and applying it does not make a spend
/// over that cap require approval. `PUT {scope}/policy` is admin-gated,
/// validates the value as non-negative and finite, persists it, and
/// carries it to the next turn's snapshot — every one of those steps
/// works — but nothing in the currently-shipped evaluation path ever
/// reads the snapshot's `auto_approve_under_usd` to decide anything,
/// because the disabled-HITL arm of `evaluate` returns before reaching the
/// mode dispatch that would consult it (`policy::gate`). A console
/// showing "capped at $50" is not currently describing an enforced limit.
#[tokio::test]
async fn the_persisted_cap_does_not_gate_a_spend_on_the_production_gate() {
    use crate::ports::ApprovalGate;
    use crate::ports::types::{CompanyEvent, Effect, EffectGroup};

    let dir = home();
    let state = state(dir.path()).await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).expect("registered").clone();
    assert!(
        !runtime.approval_gate.policy_hitl_enabled(),
        "this fixture must build the gate the way production does"
    );

    let (status, _) = call(&state, "PUT", Some(json!({ "autoApproveUnderUsd": 1.0 }))).await;
    assert_eq!(status, StatusCode::OK);
    runtime
        .run_cycle(vec![CompanyEvent::ScheduleFired {
            cron: "* * * * *".to_string(),
            prompt: "status".to_string(),
        }])
        .await
        .expect("the next turn applies the snapshot");
    assert_eq!(
        runtime.approval_gate.policy().auto_approve_under_usd,
        Some(1.0),
        "the cap did reach the live snapshot"
    );

    let over_cap = Effect {
        kind: "payment.send".to_string(),
        group: EffectGroup::Spend,
        amount_usd: Some(1_000_000.0),
        established_thread: false,
        first_time_counterparty: false,
        payload: serde_json::Value::Null,
        agent: None,
        run_id: None,
    };
    let decision = runtime
        .approval_gate
        .evaluate(&id, &over_cap)
        .await
        .unwrap();
    assert_eq!(
        decision,
        crate::ports::types::PolicyDecision::Allow,
        "a $1,000,000 spend against a $1 cap is allowed on the production gate today — \
         the persisted cap is not currently enforced"
    );
}

/// A deadline `null` releases that one override while preserving the cap,
/// just as `mode: null` releases only the tier override.
#[tokio::test]
async fn null_deadline_stops_overriding_the_deadline() {
    let dir = home();
    let state = state(dir.path()).await;
    call(
        &state,
        "PUT",
        Some(json!({ "autoApproveUnderUsd": 10, "approvalTtlHours": 72 })),
    )
    .await;

    let (status, body) = call(&state, "PUT", Some(json!({ "approvalTtlHours": null }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["autoApproveUnderUsd"], 10.0);
    assert_eq!(body["approvalTtlHours"], 24);
}

/// A body that sets nothing is refused rather than stored, and an unknown
/// tier is refused rather than silently downgraded to `supervised`.
#[tokio::test]
async fn an_empty_body_and_an_unknown_tier_are_both_refused() {
    let dir = home();
    let state = state(dir.path()).await;

    let (status, _) = call(&state, "PUT", Some(json!({}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = call(&state, "PUT", Some(json!({ "mode": "supervized" }))).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unknown tier must be refused — accepting it would leave the console \
         showing a tier the gate was not running"
    );

    for hours in [0, 8_761, u64::MAX] {
        let (status, _) = call(&state, "PUT", Some(json!({ "approvalTtlHours": hours }))).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "an invalid approval deadline must be refused before persistence"
        );
    }

    // Neither refusal stored anything.
    let (_, body) = call(&state, "GET", None).await;
    assert_eq!(body["overridden"], false);
}

/// `alwaysApprove` is admin-gated and attributed like every other field
/// here, but nothing bounded its size: an operator (or a script acting as
/// one) could grow the stored list without limit, a standing cost on every
/// `GET` from then on. A refusal here, not silent truncation, matching how
/// every other invalid field on this route is handled.
#[tokio::test]
async fn an_oversized_always_approve_list_is_refused() {
    let dir = home();
    let state = state(dir.path()).await;

    let too_many: Vec<String> = (0..=MAX_ALWAYS_APPROVE_ENTRIES)
        .map(|i| format!("tool.{i}"))
        .collect();
    let (status, _) = call(&state, "PUT", Some(json!({ "alwaysApprove": too_many }))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Refused, not truncated and stored.
    let (_, body) = call(&state, "GET", None).await;
    assert_eq!(body["overridden"], false);

    // Exactly at the cap is accepted.
    let at_cap: Vec<String> = (0..MAX_ALWAYS_APPROVE_ENTRIES)
        .map(|i| format!("tool.{i}"))
        .collect();
    let (status, body) = call(&state, "PUT", Some(json!({ "alwaysApprove": at_cap }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["alwaysApprove"].as_array().unwrap().len(),
        MAX_ALWAYS_APPROVE_ENTRIES
    );
}
