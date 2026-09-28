use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use super::tests_redeem_replays_the_markers::FailingRedispatchBrain;
use super::*;
use crate::company::CompanyManifest;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::RuntimeBuilder;
use crate::runtime::grants::RedeemContext;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("opencompany-budget-pause-")
        .tempdir()
        .expect("tempdir")
}

fn manifest() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap()
}

/// Builds an [`AppState`] for a single company, its lone registered
/// runtime running on `brain`. Same shape as `operator.rs`'s
/// `build_state_with_brain` — a fresh `company` id per test, never the
/// shared `"acme"` other files' budget-pause tests use, so this file's
/// `BudgetPauseSet` (keyed globally by company id) never collides with a
/// concurrently-running test elsewhere in the same binary.
async fn state_with_brain(
    home: &std::path::Path,
    company: &str,
    brain: Arc<dyn crate::ports::brain::Brain>,
) -> AppState {
    let m = manifest();
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new(company);
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: m.clone(),
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: Vec::new(),
            overlay_desk_members: Vec::new(),
            overlay_desk_order: Vec::new(),
            overlay_desks: Vec::new(),
            overlay_workflows: Vec::new(),
            overlay_budgets: Vec::new(),
            overlay_policy: None,
            overlay_desk_tools: Default::default(),
            disabled_workflows: Vec::new(),
            template_provenance: None,
            setup: None,
            overlay_tool_grants: None,
            name_confirmed: false,
            activation_completed_at: None,
            created_at_millis: None,
        })
        .await
        .unwrap();

    let runtime = RuntimeBuilder::new(home.to_path_buf(), m)
        .with_id(id.clone())
        .with_brain(brain)
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, company).await;
    state
}

async fn send(
    state: &AppState,
    company: &str,
    method: &str,
    uri: &str,
) -> (StatusCode, Value, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", crate::server::test_support::fixed_cookie(company))
        .body(Body::empty())
        .unwrap();
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

#[async_trait::async_trait]
impl crate::ports::brain::Brain for FailingRedispatchBrain {
    async fn run_cycle(
        &self,
        _req: crate::ports::types::CycleRequest,
        _host: &dyn crate::ports::brain::CycleHost,
    ) -> crate::Result<crate::ports::types::CycleResult> {
        Err(OpenCompanyError::InvalidRequest(
            "redispatch refused".to_string(),
        ))
    }
}

/// Issue #1846 review (Codex #3865812411): a redispatch that returns
/// `Err` must restore the reservation `redeem` took, not leave the
/// operator's saved payload gone for good — the failure branch the
/// spawn-based fix has to keep reachable.
#[tokio::test]
async fn a_failed_redispatch_restores_the_reservation() {
    let home = home();
    let company = "acme-redeem-restore";
    let state = state_with_brain(home.path(), company, Arc::new(FailingRedispatchBrain)).await;
    let id = CompanyId::new(company);

    budget_pauses_for(&id).park(
        "ceo",
        None,
        "ship the API",
        "paused",
        1_000,
        RedeemContext::default(),
    );

    let (status, _resp, raw) = send(
        &state,
        company,
        "POST",
        "/api/v1/company/agents/ceo/budget-pause/redeem",
    )
    .await;
    assert!(
        status.is_client_error() || status.is_server_error(),
        "a refused redispatch must not report success: {raw}"
    );

    assert!(
        budget_pauses_for(&id).peek("ceo").is_some(),
        "a failed redispatch must restore the reservation so the operator's saved \
         payload survives for a retry, rather than being thrown away over a redispatch \
         that never happened"
    );
}

/// A brain that stalls mid-cycle so the test can drop the connection
/// while the redispatch is still in flight, then release it and prove
/// the redispatch ran to completion anyway. Same shape as
/// `operator.rs`'s `StalledContinuationBrain`.
struct StalledRedispatchBrain {
    /// Fires once the redispatch cycle is under way — the moment a
    /// dropped connection would have cancelled it under the pre-fix
    /// direct `.await`.
    entered: std::sync::Arc<tokio::sync::Notify>,
    /// The test's permission for the cycle to finish.
    release: std::sync::Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl crate::ports::brain::Brain for StalledRedispatchBrain {
    async fn run_cycle(
        &self,
        req: crate::ports::types::CycleRequest,
        _host: &dyn crate::ports::brain::CycleHost,
    ) -> crate::Result<crate::ports::types::CycleResult> {
        if req
            .events
            .iter()
            .any(|e| matches!(e, CompanyEvent::OperatorMessage { .. }))
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(crate::ports::types::CycleResult {
            channel_responses: Vec::new(),
            new_traces: Vec::new(),
            ledger_deltas: Vec::new(),
            token_usage: crate::ports::types::TokenUsage::default(),
        })
    }
}

/// Issue #1846 review (Codex #3865812411) — the keystone test for the
/// cancellation-safety fix. This host is plain
/// `axum::serve(listener, router(state))`; hyper drops a handler's
/// future the moment the peer disconnects, and a reverse proxy in front
/// of a hosted tenant closes it the moment it decides the upstream is
/// too slow. Before this fix, `redeem_budget_pause` awaited
/// `run_cycle` directly in its own future, so that drop cancelled the
/// redispatch mid-flight: `restore_if_absent` never ran, the reservation
/// `redeem` took was gone for good, and the operator's saved payload
/// vanished with no redispatch ever having completed.
///
/// `Router::oneshot` reproduces that drop faithfully rather than by
/// analogy — same mechanism hyper uses, since the handler future is
/// owned by the future the caller polls.
#[tokio::test]
async fn a_dropped_connection_does_not_cancel_the_redispatch() {
    let home = home();
    let company = "acme-redeem-drop";
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let state = state_with_brain(
        home.path(),
        company,
        Arc::new(StalledRedispatchBrain {
            entered: entered.clone(),
            release: release.clone(),
        }),
    )
    .await;
    let id = CompanyId::new(company);

    budget_pauses_for(&id).park(
        "ceo",
        None,
        "ship the API",
        "paused",
        1_000,
        RedeemContext::default(),
    );

    let uri = "/api/v1/company/agents/ceo/budget-pause/redeem";
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("cookie", crate::server::test_support::fixed_cookie(company))
        .body(Body::empty())
        .unwrap();
    let mut redeeming = Box::pin(router(state.clone()).oneshot(request));
    tokio::select! {
        _ = &mut redeeming => panic!("the redeem answered before the redispatch began"),
        _ = entered.notified() => {}
    }
    drop(redeeming);

    // The reservation is gone — exactly the state a client sees the
    // instant a real proxy gives up mid-redispatch.
    assert!(
        budget_pauses_for(&id).peek("ceo").is_none(),
        "the marker was reserved before the connection dropped"
    );

    // So the redispatch the reservation exists for must still run to
    // completion, not die with the dropped connection.
    release.notify_one();
    let recorded = recording_settles(&id, "ceo").await;
    assert!(
        recorded,
        "the redispatch died with the dropped connection: the reservation is spent and \
         the redispatch never ran to completion"
    );
}

/// Polls until the marker for `agent` is gone-and-stays-gone (redeemed
/// and never restored) or the timeout expires, so the drop test above
/// does not need a bespoke completion channel through `StalledRedispatchBrain`.
/// `run_cycle` returns `Ok` on release, so a marker that is STILL absent
/// after a settle window means the background redispatch ran to
/// completion without erroring — an error would have restored it.
async fn recording_settles(id: &CompanyId, agent: &str) -> bool {
    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if budget_pauses_for(id).peek(agent).is_none() {
            // Give the spawned task's own `Ok` branch a moment past the
            // notify to finish; then confirm it stayed absent rather than
            // having been an in-between read racing a restore.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            return budget_pauses_for(id).peek(agent).is_none();
        }
    }
    false
}

/// Same shape as [`StalledRedispatchBrain`], but its cycle FAILS once
/// released instead of succeeding — so the spawned redispatch owes a
/// restore, not just a settle.
struct StalledThenFailingRedispatchBrain {
    entered: std::sync::Arc<tokio::sync::Notify>,
    release: std::sync::Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl crate::ports::brain::Brain for StalledThenFailingRedispatchBrain {
    async fn run_cycle(
        &self,
        req: crate::ports::types::CycleRequest,
        _host: &dyn crate::ports::brain::CycleHost,
    ) -> crate::Result<crate::ports::types::CycleResult> {
        if req
            .events
            .iter()
            .any(|e| matches!(e, CompanyEvent::OperatorMessage { .. }))
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Err(OpenCompanyError::InvalidRequest(
            "redispatch refused".to_string(),
        ))
    }
}

/// Polls until the marker for `agent` is parked again (restored) or the
/// timeout expires — the mirror image of [`recording_settles`], for a
/// redispatch that owes a restore rather than a settle.
async fn recording_restores(id: &CompanyId, agent: &str) -> bool {
    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if budget_pauses_for(id).peek(agent).is_some() {
            return true;
        }
    }
    false
}

/// Issue #1846 review (Codex #3866802276) — the keystone test for the
/// guard-in-the-detached-task fix. Combines the drop-safety scenario
/// [`a_dropped_connection_does_not_cancel_the_redispatch`] proves with a
/// FAILING redispatch: the connection drops while the redispatch is
/// in-flight (so nothing is left awaiting `redeem_budget_pause`'s own
/// `redispatch.await`/its `match` arms), and the redispatch then fails.
///
/// Before this fix, `restore_if_absent` sat in the `Ok(Err(_))` arm of
/// that `match` — code that lived in THIS handler's own future, which
/// the drop above already cancelled. The spawned task still ran
/// `run_cycle` to completion (that half was already fixed by
/// #3865812411's spawn), but its `Err` reached nobody: the reservation
/// stayed gone forever, indistinguishable from a successful redeem to
/// anything reading the marker set afterward. `RestoreGuard` lives
/// inside the spawned task itself, so its restore does not depend on
/// this handler's future still being polled.
#[tokio::test]
async fn a_dropped_connection_still_restores_the_reservation_when_the_redispatch_fails() {
    let home = home();
    let company = "acme-redeem-drop-then-fail";
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let state = state_with_brain(
        home.path(),
        company,
        Arc::new(StalledThenFailingRedispatchBrain {
            entered: entered.clone(),
            release: release.clone(),
        }),
    )
    .await;
    let id = CompanyId::new(company);

    budget_pauses_for(&id).park(
        "ceo",
        None,
        "ship the API",
        "paused",
        1_000,
        RedeemContext::default(),
    );

    let uri = "/api/v1/company/agents/ceo/budget-pause/redeem";
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("cookie", crate::server::test_support::fixed_cookie(company))
        .body(Body::empty())
        .unwrap();
    let mut redeeming = Box::pin(router(state.clone()).oneshot(request));
    tokio::select! {
        _ = &mut redeeming => panic!("the redeem answered before the redispatch began"),
        _ = entered.notified() => {}
    }
    // Drops `redeem_budget_pause`'s own future — including its
    // `match redispatch.await { ... }` and every restore call that used
    // to live inside it. Nothing is left polling the `JoinHandle`.
    drop(redeeming);

    assert!(
        budget_pauses_for(&id).peek("ceo").is_none(),
        "the marker was reserved before the connection dropped"
    );

    // The spawned task's own `run_cycle` still runs to completion, and
    // now fails.
    release.notify_one();
    let restored = recording_restores(&id, "ceo").await;
    assert!(
        restored,
        "a failed redispatch must restore the reservation even when the connection that \
         triggered it dropped before the redispatch finished — otherwise the operator's \
         saved payload is lost for good with no redispatch ever having completed"
    );
}
