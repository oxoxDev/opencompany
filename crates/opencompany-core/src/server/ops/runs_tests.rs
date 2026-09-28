use super::*;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::runs::{NewRun, RunOutcome, RunStore};
use crate::ports::types::{CompanyId, CompanyRecord, EventSeq, TurnStep};
use crate::ports::{CompanyStore, now_millis};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

/// The read side must flag a capped trace with the same number the writer
/// stops at. The sink is `openhuman`-only; on a build that has it, the two
/// constants must agree.
#[cfg(feature = "openhuman")]
#[test]
fn the_cap_matches_the_trace_sink() {
    assert_eq!(MAX_RUN_STEPS, crate::harness::run_trace::MAX_RUN_STEPS);
}

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-ops-runs-")
        .tempdir()
        .expect("tempdir")
}

fn manifest() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap()
}

/// A running company `acme` with a real (fs-backed) run store.
async fn state_with_company(home: &std::path::Path) -> (AppState, CompanyId) {
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: manifest(),
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: Vec::new(),
            overlay_desk_members: Vec::new(),
            overlay_desk_order: Vec::new(),
            overlay_desks: Vec::new(),
            overlay_budgets: Vec::new(),
            overlay_policy: None,
            overlay_tool_grants: None,
            overlay_desk_tools: Default::default(),
            disabled_workflows: Vec::new(),
            overlay_workflows: Vec::new(),
            template_provenance: None,
            setup: None,
            name_confirmed: false,
            activation_completed_at: None,
            created_at_millis: None,
        })
        .await
        .unwrap();
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default()).with_home(home.to_path_buf());
    state
        .registry()
        .insert(id.clone(), std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    (state, id)
}

fn request(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::empty())
        .unwrap()
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// The company's run store, straight from the registry — how these tests
/// seed attempts. `doctor` on a dev workstation reports no inference
/// credential, so a dispatched card boots onto the echo brain and produces
/// no rich trace; seeding through the port exercises the same rows the
/// dispatch path writes, without a model.
fn runs_of(state: &AppState, id: &CompanyId) -> std::sync::Arc<dyn RunStore> {
    std::sync::Arc::clone(state.registry().get(id).expect("registered").runs())
}

async fn mint(
    runs: &std::sync::Arc<dyn RunStore>,
    id: &CompanyId,
    run_id: &str,
    task_id: &str,
) -> RunRecord {
    runs.create_run(id, NewRun::for_task(run_id, task_id, "ceo"))
        .await
        .expect("mint")
}

fn step(seq: u32, kind: TurnStepKind, status: TurnStepStatus, label: &str) -> TurnStep {
    let _ = seq;
    TurnStep {
        kind,
        status,
        label: label.to_string(),
        elapsed_ms: matches!(kind, TurnStepKind::ToolCall).then_some(42),
        ..TurnStep::default()
    }
}

async fn push_step(
    runs: &std::sync::Arc<dyn RunStore>,
    id: &CompanyId,
    run_id: &str,
    seq: u32,
    kind: TurnStepKind,
    status: TurnStepStatus,
    label: &str,
) {
    runs.append_run_step(
        id,
        &RunStepRecord {
            run_id: run_id.to_string(),
            step_seq: seq,
            at_millis: now_millis(),
            step: step(seq, kind, status, label),
        },
    )
    .await
    .expect("append step");
}

/// The headline read: a card's attempts come back newest first, each with
/// its ordinal and status.
#[tokio::test]
async fn the_run_list_returns_attempts_newest_first() {
    let dir = home();
    let (state, id) = state_with_company(dir.path()).await;
    let runs = runs_of(&state, &id);

    let first = mint(&runs, &id, "run-1", "card-a").await;
    assert_eq!(first.attempt, 1, "the first attempt at a card is 1");
    runs.begin_run(&id, "run-1", EventSeq::new(7))
        .await
        .expect("begin");
    runs.finish_run(
        &id,
        "run-1",
        RunOutcome::new(RunStatus::Failed).with_error("the tool refused"),
    )
    .await
    .expect("settle");

    let second = mint(&runs, &id, "run-2", "card-a").await;
    assert_eq!(second.attempt, 2, "a re-dispatch is a new ordinal");

    let response = router(state)
        .oneshot(request("/api/v1/company/runs"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let rows = body.as_array().expect("array");
    assert_eq!(rows.len(), 2, "body: {body}");

    assert_eq!(rows[0]["id"], "run-2");
    assert_eq!(rows[0]["attempt"], 2);
    assert_eq!(rows[0]["status"], "pending");
    assert_eq!(rows[0]["phase"], "active");
    assert_eq!(rows[1]["id"], "run-1");
    assert_eq!(rows[1]["status"], "failed");
    assert_eq!(rows[1]["phase"], "terminal");
    assert_eq!(rows[1]["error"], "the tool refused");
    assert_eq!(rows[1]["triggerEventSeq"], 7);
}

/// The trap the `phase` projection exists for. A `waiting_approval` attempt
/// is non-terminal, so it has **no** finish time — exactly like a live one.
/// A reader keying off the timestamp would show it as running forever.
#[tokio::test]
async fn a_waiting_attempt_reports_parked_with_no_finish_time() {
    let dir = home();
    let (state, id) = state_with_company(dir.path()).await;
    let runs = runs_of(&state, &id);

    mint(&runs, &id, "run-1", "card-a").await;
    runs.begin_run(&id, "run-1", EventSeq::new(1))
        .await
        .expect("begin");
    runs.finish_run(&id, "run-1", RunOutcome::new(RunStatus::WaitingApproval))
        .await
        .expect("park");

    let response = router(state)
        .oneshot(request("/api/v1/company/runs"))
        .await
        .unwrap();
    let body = json_body(response).await;
    let row = &body.as_array().expect("array")[0];

    assert_eq!(row["status"], "waiting_approval");
    assert_eq!(row["phase"], "parked");
    assert!(
        row.get("finishedAtMillis").is_none(),
        "a parked attempt must carry no finish time: {row}"
    );
    assert!(
        row.get("startedAtMillis").is_some(),
        "…but it did start: {row}"
    );
}

/// Epic #183: a card may enter review many times, so several waits on one
/// card is the expected record, not a bug. The list must show them all.
#[tokio::test]
async fn one_card_can_show_several_waits() {
    let dir = home();
    let (state, id) = state_with_company(dir.path()).await;
    let runs = runs_of(&state, &id);

    mint(&runs, &id, "run-1", "card-a").await;
    let begun = runs
        .begin_run(&id, "run-1", EventSeq::new(1))
        .await
        .expect("begin");
    for _ in 0..3 {
        runs.finish_run(&id, "run-1", RunOutcome::new(RunStatus::WaitingApproval))
            .await
            .expect("park");
        runs.begin_run(&id, "run-1", EventSeq::new(2))
            .await
            .expect("resume");
    }
    let settled = runs
        .finish_run(&id, "run-1", RunOutcome::new(RunStatus::Succeeded))
        .await
        .expect("settle");
    assert!(settled.finished_at_millis.is_some());
    // The attempt's start is the moment it *first* began, not its last leg —
    // so the elapsed figure the console prints spans the whole attempt,
    // waits included, instead of resetting on every resume.
    assert_eq!(settled.started_at_millis, begun.started_at_millis);

    let response = router(state)
        .oneshot(request("/api/v1/company/runs?task=card-a"))
        .await
        .unwrap();
    let body = json_body(response).await;
    let rows = body.as_array().expect("array");
    assert_eq!(rows.len(), 1, "three waits are one attempt: {body}");
    assert_eq!(rows[0]["phase"], "terminal");
}

/// A killed host leaves the in-flight tool call as a `running` step. That is
/// the whole point of an incremental trace, so it must reach the console as
/// in-flight — with its status intact and distinct from `error`.
#[tokio::test]
async fn a_killed_run_keeps_its_in_flight_step() {
    let dir = home();
    let (state, id) = state_with_company(dir.path()).await;
    let runs = runs_of(&state, &id);

    mint(&runs, &id, "run-1", "card-a").await;
    runs.begin_run(&id, "run-1", EventSeq::new(1))
        .await
        .expect("begin");
    push_step(
        &runs,
        &id,
        "run-1",
        0,
        TurnStepKind::Thinking,
        TurnStepStatus::Ok,
        "Thinking",
    )
    .await;
    push_step(
        &runs,
        &id,
        "run-1",
        1,
        TurnStepKind::ToolCall,
        TurnStepStatus::Error,
        "Sending mail",
    )
    .await;
    // …and the one that was in flight when the host died.
    push_step(
        &runs,
        &id,
        "run-1",
        2,
        TurnStepKind::ToolCall,
        TurnStepStatus::Running,
        "Searching",
    )
    .await;
    // What the boot reaper then does to the row.
    runs.finish_run(
        &id,
        "run-1",
        RunOutcome::new(RunStatus::Failed).with_error(crate::ports::runs::ORPHAN_ERROR),
    )
    .await
    .expect("reap");

    let response = router(state)
        .oneshot(request("/api/v1/company/runs/run-1"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;

    assert_eq!(body["run"]["status"], "failed");
    assert_eq!(body["run"]["phase"], "terminal");
    let steps = body["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 3, "body: {body}");
    // The console's `TimelineEntry` contract, widened additively.
    assert_eq!(steps[0]["seq"], 0);
    assert_eq!(steps[0]["kind"], "thinking");
    assert_eq!(steps[0]["status"], "ok");
    assert_eq!(steps[0]["label"], "Thinking");
    assert!(
        steps[0].get("elapsedMs").is_none(),
        "thinking steps report no duration: {body}"
    );
    assert_eq!(steps[1]["kind"], "tool_call");
    assert_eq!(steps[1]["status"], "error");
    assert_eq!(steps[1]["elapsedMs"], 42);
    // The in-flight one — NOT an error.
    assert_eq!(steps[2]["kind"], "tool_call");
    assert_eq!(steps[2]["status"], "running");
}

/// The `?task=` and `?status=` predicates narrow the page, and an unknown
/// status word is a 400 rather than a silent empty page.
#[tokio::test]
async fn the_filters_narrow_and_a_bad_status_is_refused() {
    let dir = home();
    let (state, id) = state_with_company(dir.path()).await;
    let runs = runs_of(&state, &id);

    mint(&runs, &id, "run-1", "card-a").await;
    runs.begin_run(&id, "run-1", EventSeq::new(1))
        .await
        .expect("begin");
    runs.finish_run(&id, "run-1", RunOutcome::new(RunStatus::Succeeded))
        .await
        .expect("settle");
    mint(&runs, &id, "run-2", "card-b").await;

    let by_task = json_body(
        router(state.clone())
            .oneshot(request("/api/v1/company/runs?task=card-b"))
            .await
            .unwrap(),
    )
    .await;
    let rows = by_task.as_array().expect("array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["taskId"], "card-b");

    // Comma-separated, and a status the card does not have is excluded.
    let by_status = json_body(
        router(state.clone())
            .oneshot(request("/api/v1/company/runs?status=succeeded,cancelled"))
            .await
            .unwrap(),
    )
    .await;
    let rows = by_status.as_array().expect("array");
    assert_eq!(rows.len(), 1, "body: {by_status}");
    assert_eq!(rows[0]["id"], "run-1");

    let bad = router(state)
        .oneshot(request("/api/v1/company/runs?status=done"))
        .await
        .unwrap();
    assert_eq!(
        bad.status(),
        StatusCode::BAD_REQUEST,
        "a typo'd filter must not look like 'nothing matched'"
    );
}

/// An unknown id 404s — including one minted in another company, because
/// the store read is company-scoped.
#[tokio::test]
async fn an_unknown_run_is_not_found() {
    let dir = home();
    let (state, _id) = state_with_company(dir.path()).await;
    let response = router(state)
        .oneshot(request("/api/v1/company/runs/nope"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Registers a second company, `beta`, in `state`'s existing registry.
async fn add_second_company(state: &AppState, home: &std::path::Path) -> CompanyId {
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("beta");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: manifest(),
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: Vec::new(),
            overlay_desk_members: Vec::new(),
            overlay_desk_order: Vec::new(),
            overlay_desks: Vec::new(),
            overlay_budgets: Vec::new(),
            overlay_policy: None,
            overlay_tool_grants: None,
            overlay_desk_tools: Default::default(),
            disabled_workflows: Vec::new(),
            overlay_workflows: Vec::new(),
            template_provenance: None,
            setup: None,
            name_confirmed: false,
            activation_completed_at: None,
            created_at_millis: None,
        })
        .await
        .unwrap();
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    state
        .registry()
        .insert(id.clone(), std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(state, "beta").await;
    id
}

/// AUTH-axis: the doc comment right above (`an_unknown_run_is_not_found`)
/// claims a run id minted in another company is what the company-scoped
/// store read keeps out, but that test only ever tries an id nobody
/// minted anywhere. This is the case it describes but never drove: a run
/// that genuinely exists, in a genuinely different company, must be
/// invisible from both the list and the detail route.
#[tokio::test]
async fn a_run_minted_in_another_company_is_invisible_from_this_one() {
    let dir = home();
    let (state, _acme) = state_with_company(dir.path()).await;
    let beta = add_second_company(&state, dir.path()).await;

    let beta_runs = runs_of(&state, &beta);
    mint(&beta_runs, &beta, "beta-run-1", "beta-card").await;

    let list = json_body(
        router(state.clone())
            .oneshot(request("/api/v1/companies/acme/runs"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        list.as_array().expect("array").len(),
        0,
        "a run minted in another company must not appear on this one's list: {list}"
    );

    let response = router(state)
        .oneshot(request("/api/v1/companies/acme/runs/beta-run-1"))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "a run id that resolves in another company must 404 here, not leak the record"
    );
}

/// `stepCount` is a high-water ordinal, capped — so the wire says when the
/// number has stopped meaning "how many steps the agent took".
#[tokio::test]
async fn a_capped_step_count_is_flagged() {
    let dir = home();
    let (state, id) = state_with_company(dir.path()).await;
    let runs = runs_of(&state, &id);

    mint(&runs, &id, "run-1", "card-a").await;
    runs.begin_run(&id, "run-1", EventSeq::new(1))
        .await
        .expect("begin");
    let mut outcome = RunOutcome::new(RunStatus::Succeeded);
    outcome.step_count = MAX_RUN_STEPS;
    runs.finish_run(&id, "run-1", outcome)
        .await
        .expect("settle");

    let body = json_body(
        router(state)
            .oneshot(request("/api/v1/company/runs"))
            .await
            .unwrap(),
    )
    .await;
    let row = &body.as_array().expect("array")[0];
    assert_eq!(row["stepCount"], MAX_RUN_STEPS);
    assert_eq!(row["stepCountCapped"], true);
}

/// Both scope forms answer, like every other route in the ops plane.
#[tokio::test]
async fn both_scope_forms_answer() {
    let dir = home();
    let (state, id) = state_with_company(dir.path()).await;
    let runs = runs_of(&state, &id);
    mint(&runs, &id, "run-1", "card-a").await;

    for uri in [
        "/api/v1/company/runs",
        "/api/v1/companies/acme/runs",
        "/api/v1/company/runs/run-1",
        "/api/v1/companies/acme/runs/run-1",
    ] {
        let response = router(state.clone()).oneshot(request(uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
    }
}

/// Every key on the wire is camelCase — including the two inside `usage`.
///
/// Regression test for a real defect caught only by curling a live host:
/// embedding [`TokenUsage`] directly emitted `cached_input` and `cost_usd`
/// beside an otherwise camelCase object, because that type carries no
/// `rename_all` (its field names are the decode contract for journaled
/// events). Neither `tsc` nor a hand-written console type can catch that —
/// only an assertion on the bytes.
#[tokio::test]
async fn usage_is_camel_case_on_the_wire() {
    let dir = home();
    let (state, id) = state_with_company(dir.path()).await;
    let runs = runs_of(&state, &id);

    mint(&runs, &id, "run-1", "card-a").await;
    runs.begin_run(&id, "run-1", EventSeq::new(1))
        .await
        .expect("begin");
    let mut outcome = RunOutcome::new(RunStatus::Succeeded);
    outcome.usage = TokenUsage {
        input: 100,
        output: 20,
        cached_input: 5,
        cost_usd: 0.25,
    };
    runs.finish_run(&id, "run-1", outcome)
        .await
        .expect("settle");

    let body = json_body(
        router(state)
            .oneshot(request("/api/v1/company/runs"))
            .await
            .unwrap(),
    )
    .await;
    let usage = &body.as_array().expect("array")[0]["usage"];
    assert_eq!(usage["input"], 100);
    assert_eq!(usage["output"], 20);
    assert_eq!(usage["cachedInput"], 5, "usage: {usage}");
    assert_eq!(usage["costUsd"], 0.25, "usage: {usage}");
    assert!(
        usage.get("cached_input").is_none() && usage.get("cost_usd").is_none(),
        "no snake_case may leak through: {usage}"
    );

    // …and nothing else on the row is snake_case either.
    for row in body.as_array().expect("array") {
        for key in row.as_object().expect("object").keys() {
            assert!(!key.contains('_'), "'{key}' is not camelCase");
        }
    }
}

/// `?limit=` clamps, and `0` means "the default" rather than an empty page.
#[test]
fn the_limit_clamps_and_zero_means_default() {
    let filter = |limit: Option<usize>| {
        RunsQuery {
            workflow_run: None,
            task: None,
            agent: None,
            status: None,
            limit,
        }
        .into_filter()
        .expect("filter")
    };
    assert_eq!(filter(None).limit, Some(DEFAULT_RUN_LIMIT));
    assert_eq!(filter(Some(0)).limit, Some(DEFAULT_RUN_LIMIT));
    assert_eq!(filter(Some(5)).limit, Some(5));
    assert_eq!(filter(Some(10_000)).limit, Some(MAX_RUN_LIMIT));
}

/// `?agent=` reaches the store as a predicate rather than being dropped
/// (issue #1573).
///
/// The failure this guards against is silent in the worst way: an
/// unrecognised selector on a `Deserialize` query struct is simply ignored,
/// so the console would ask for one teammate's history, get the *whole
/// company's* newest N attempts back, and render them under that teammate's
/// name. Every row would be real, and the page would still be a lie.
#[test]
fn the_agent_selector_becomes_a_store_predicate() {
    let filter = RunsQuery {
        task: Some("card-7".into()),
        workflow_run: None,
        agent: Some("engineer".into()),
        status: None,
        limit: None,
    }
    .into_filter()
    .expect("filter");
    assert_eq!(filter.agent_id.as_deref(), Some("engineer"));
    assert_eq!(
        filter.task_id.as_deref(),
        Some("card-7"),
        "the desk predicate does not displace the card one"
    );

    assert_eq!(
        RunsQuery {
            task: None,
            workflow_run: None,
            agent: None,
            status: None,
            limit: None,
        }
        .into_filter()
        .expect("filter")
        .agent_id,
        None,
        "no `?agent=` means every desk, not a desk named nothing"
    );
}

/// A seat turn's attempt names its episode and round on the wire (plan
/// hive-desks, Phase 8) — `episodeId` and `roundRevision`, camelCase — and
/// every other attempt omits both rather than writing `null`, so the
/// Observatory's fold can tell "no round" from "round 0".
#[tokio::test]
async fn a_seat_turn_attempt_carries_its_episode_and_round() {
    let dir = home();
    let (state, id) = state_with_company(dir.path()).await;
    let runs = runs_of(&state, &id);

    runs.create_run(
        &id,
        NewRun::for_chat("seat-1", "engineering", "ceo")
            .in_thread(Some(EventSeq::new(4)))
            .in_episode("ep-1", 0),
    )
    .await
    .expect("mint the seat turn");
    mint(&runs, &id, "run-1", "card-a").await;

    let response = router(state)
        .oneshot(request("/api/v1/company/runs"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let rows = body.as_array().expect("array");
    let seat = rows
        .iter()
        .find(|row| row["id"] == "seat-1")
        .expect("the seat turn");
    assert_eq!(seat["episodeId"], "ep-1");
    assert_eq!(seat["roundRevision"], 0);
    assert_eq!(seat["threadRoot"], 4);
    let card = rows
        .iter()
        .find(|row| row["id"] == "run-1")
        .expect("the dispatch");
    assert!(card.get("episodeId").is_none(), "{card}");
    assert!(card.get("roundRevision").is_none(), "{card}");
}
