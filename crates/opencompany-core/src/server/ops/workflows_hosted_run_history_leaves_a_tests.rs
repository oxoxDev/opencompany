use super::workflows_test_support::hosted_mode::*;
use super::*;
use crate::server::router;

/// A row journaled before issue #371 carries no `run_id`, so the
/// `(run, node)` join has no key at all.
///
/// It must therefore be left exactly as it read before #1189. Calling
/// its gates stranded on the strength of a *missing field* would retire
/// live work — the failure mode that is strictly worse than the one this
/// issue closes, because an operator cannot argue with a run the console
/// says is over.
#[tokio::test]
async fn run_history_leaves_a_pre_371_row_unreconciled() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, id) = hosted_state(&home).await;
    let runtime = state.registry().get(&id).expect("registered");
    runtime
        .events()
        .append(
            &id,
            CompanyEvent::WorkflowRunFinished {
                workflow_id: "sports_blog".to_string(),
                scheduled: true,
                // The pre-#371 shape: no correlation id, and so no start
                // row to pair with either.
                run_id: None,
                deliveries: Vec::new(),
                pending_approvals: vec!["fetch_bbc".to_string()],
                error: None,
                cancelled: false,
                notices: Vec::new(),
                board: Vec::new(),
                blocked_nodes: Vec::new(),
                approvals: Vec::new(),
            },
        )
        .await
        .expect("append");

    let response = router(state)
        .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
        .await
        .unwrap();
    let body = json_body(response).await;
    assert!(
        body["runs"][0].get("strandedApprovals").is_none(),
        "a row with no run id cannot be joined, so it must report nothing: {body}"
    );
    assert_eq!(
        body["runs"][0]["verdict"], "awaiting-approval",
        "an unjoinable row keeps the reading it had before #1189: {body}"
    );
}

/// A run the process is genuinely executing — registered on the
/// supervisor, so its id is in `live()` — folds as `running: true` with
/// the nodes it has completed so far. Since #1009 a start with no finish
/// whose id is NOT live is settled on the read instead
/// ([`a_run_absent_from_the_live_set_is_settled_by_the_read`]); this pins
/// the still-running half of that split, with the guard held across the
/// request so the registration stays live for the whole read.
#[tokio::test]
async fn run_history_reports_an_unsettled_run_as_running() {
    let home_dir = home();
    let (state, _store, id) = hosted_state(home_dir.path()).await;

    let runtime = state.registry().get(&id).expect("registered");
    let (ctx, _guard) = runtime
        .run_supervisor()
        .begin("digest", false)
        .expect("under the default cap");
    journal_start(&state, &id, "digest", &ctx.run_id, false).await;
    journal_node(
        &state,
        &id,
        "digest",
        &ctx.run_id,
        "ceo",
        WorkflowNodeStatus::Ok,
    )
    .await;

    let response = router(state.clone())
        .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
        .await
        .unwrap();
    let body = json_body(response).await;
    assert_eq!(body["runs"][0]["running"], true, "{body}");
    assert_eq!(body["runs"][0]["nodes"].as_array().unwrap().len(), 1);
    assert!(body["runs"][0].get("error").is_none(), "{body}");
}

/// **A run that finishes while the read is folding must not be buried.**
///
/// The window: `list_runs` snapshots the journal, folds it, and only
/// then asks the supervisor what is live. A run that appends its finish
/// after the snapshot and drops its guard before the `live()` call is
/// missing from both — so it looks exactly like a run that died.
///
/// Settling it is not a harmless duplicate, and the reason is ORDER. The
/// rebuild false positive the cross-check knowingly accepts is
/// self-correcting: that run's truthful finish lands *after* the
/// synthetic one, and the fold settles an entry from the last finish it
/// sees. Here the truthful finish lands *first* and loses — so a
/// successful run reads `INTERRUPTED_BY_RESTART` for good, and its
/// `deliveries` are replaced by the synthetic row's empty list. That is
/// a silent, permanent misreport of a run that worked, and of what it
/// sent.
///
/// So the read confirms against the journal tail before it writes.
#[tokio::test]
async fn a_run_that_settles_during_the_read_is_not_buried_by_a_synthetic_finish() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let id = CompanyId::new("acme");
    FsCompanyStore::new(home.clone())
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: empty_manifest(),
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

    // The run's REAL finish: a clean success carrying a delivery, which
    // is the record a spurious settle would replace with nothing.
    let real_finish = CompanyEvent::WorkflowRunFinished {
        workflow_id: "digest".to_string(),
        scheduled: false,
        run_id: Some("run-racing".to_string()),
        deliveries: vec![crate::ports::DeliveryReport {
            node: "send".to_string(),
            kind: "email".to_string(),
            target: Some("ada@example.com".to_string()),
            status: crate::ports::DeliveryStatus::Sent,
            detail: "sent".to_string(),
            reason: crate::ports::DeliveryReason::RecipientEmailed,
        }],
        pending_approvals: Vec::new(),
        error: None,
        cancelled: false,
        notices: Vec::new(),
        board: Vec::new(),
        blocked_nodes: Vec::new(),
        approvals: Vec::new(),
    };

    // Built with the interposer UNARMED. Arming it before boot would let
    // the one-shot fire inside `RuntimeBuilder::build`, whose own
    // journal read runs `sweep_interrupted_runs` — and the two finishes
    // that produced would be the boot sweep's doing, not this route's.
    // Likewise the start is journaled after boot, so the sweep (which is
    // boot-only, and correctly so) never sees it.
    let racer = std::sync::Arc::new(FinishesDuringTheRead {
        inner: std::sync::Arc::new(crate::store::FsEventLog::new(home.clone())),
        finish: std::sync::Mutex::new(None),
    });
    let events: std::sync::Arc<dyn crate::ports::EventLog> = racer.clone();

    let runtime = RuntimeBuilder::new(home.clone(), empty_manifest())
        .with_id(id.clone())
        .with_events(events.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state
        .registry()
        .insert(id.clone(), std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;

    journal_start(&state, &id, "digest", "run-racing", false).await;

    // Armed now: the next `read_from` — `list_runs`' own snapshot — is
    // the one the run finishes underneath.
    *racer.finish.lock().expect("poisoned") = Some((id.clone(), real_finish));

    // Nothing is registered on the supervisor: the run has already
    // dropped its guard, which is the second half of the interleaving.
    let body = json_body(
        router(state.clone())
            .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
            .await
            .unwrap(),
    )
    .await;

    // Exactly one finish in the journal — the truthful one. A synthetic
    // second row here is the bug: it lands after the real finish and so
    // wins the fold for good.
    let finishes: Vec<_> = events
        .read_from(&id, crate::ports::types::EventSeq::new(0), usize::MAX)
        .await
        .expect("read")
        .into_iter()
        .filter(|stored| matches!(stored.event, CompanyEvent::WorkflowRunFinished { .. }))
        .collect();
    assert_eq!(
        finishes.len(),
        1,
        "the read buried a run that had just finished under a synthetic \
         interrupted-by-restart finish: {body}"
    );

    // This response cannot show the finish it did not read — the row is
    // still the snapshot's, `running: true`. That is honest: the run WAS
    // in flight when the journal was sampled. What matters is that it is
    // not stamped dead.
    let rows = body["runs"].as_array().expect("array");
    assert_eq!(rows.len(), 1, "{body}");
    assert!(
        rows[0].get("error").is_none(),
        "the run must not be reported as interrupted: {body}"
    );

    // And the next read — the poll 2s later — sees the truth: a
    // successful run, with the delivery a synthetic finish would have
    // replaced with an empty list.
    let next = json_body(
        router(state)
            .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
            .await
            .unwrap(),
    )
    .await;
    assert!(next["runs"][0].get("running").is_none(), "settled: {next}");
    assert!(
        next["runs"][0].get("error").is_none(),
        "a successful run: {next}"
    );
    assert_eq!(next["runs"][0]["deliveries"][0]["status"], "sent", "{next}");
}

/// **A settled row keeps one identity across reads.** The response that
/// performs the settle and every response after it carry the same `seq`
/// and `atMillis` — the appended finish's, not the start's.
///
/// Not cosmetic. `RunHistoryPanel` keys its rows on `run.seq`
/// (`key={run.seq}`) and compares that same field against
/// `selectedRunSeq`, `fixingRunSeq` and `fixReason.seq`. A row whose
/// `seq` changed between two polls therefore remounts and loses its
/// selection — in the window where that is most likely to be noticed,
/// since the 2s recovery poll is running precisely because somebody is
/// watching this run.
#[tokio::test]
async fn a_settled_dead_run_keeps_its_seq_and_time_across_reads() {
    let home_dir = home();
    let (state, _store, id) = hosted_state(home_dir.path()).await;

    // Nothing registered this id, so the read settles it.
    journal_start(&state, &id, "digest", "run-dead", false).await;

    let first = json_body(
        router(state.clone())
            .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
            .await
            .unwrap(),
    )
    .await;
    let second = json_body(
        router(state)
            .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
            .await
            .unwrap(),
    )
    .await;

    assert!(
        first["runs"][0].get("running").is_none(),
        "settled: {first}"
    );
    assert_eq!(
        first["runs"][0]["seq"], second["runs"][0]["seq"],
        "the settling read and the one after it must agree on the row's \
         identity: {first} then {second}"
    );
    assert_eq!(
        first["runs"][0]["atMillis"], second["runs"][0]["atMillis"],
        "…and on when it settled: {first} then {second}"
    );
    // Specifically the FINISH's row, which is what the next fold uses —
    // not the start's, which is the only other candidate.
    assert!(
        first["runs"][0]["atMillis"].as_u64().unwrap()
            >= first["runs"][0]["startedAtMillis"].as_u64().unwrap(),
        "the settle cannot predate the start: {first}"
    );
}

/// **The compatibility claim, pinned.** A journal written before #371
/// carries finished rows with no run id and no starts. Those fold
/// exactly as they always did — one row in, one entry out, no `nodes`
/// key, no `running` key, no `startedAtMillis`.
#[tokio::test]
async fn run_history_folds_pre_371_rows_unchanged() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, id) = hosted_state(&home).await;

    journal_run(&state, &id, "digest", true, Vec::new(), None).await;

    let response = router(state)
        .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
        .await
        .unwrap();
    let body = json_body(response).await;
    let rows = body["runs"].as_array().expect("array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["workflowId"], "digest");
    assert!(rows[0].get("nodes").is_none(), "{body}");
    assert!(rows[0].get("running").is_none(), "{body}");
    assert!(rows[0].get("startedAtMillis").is_none(), "{body}");
    assert!(rows[0].get("runId").is_none(), "{body}");
}

/// Two runs interleaving on one journal — the shape two concurrent
/// workflows produce — attach their nodes to the right entry. This is
/// why the fold groups on run id rather than on row adjacency.
#[tokio::test]
async fn run_history_keeps_interleaved_runs_apart() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, id) = hosted_state(&home).await;

    journal_start(&state, &id, "a", "run-a", false).await;
    journal_start(&state, &id, "b", "run-b", false).await;
    journal_node(&state, &id, "b", "run-b", "b1", WorkflowNodeStatus::Ok).await;
    journal_node(&state, &id, "a", "run-a", "a1", WorkflowNodeStatus::Ok).await;
    journal_finish(&state, &id, "a", "run-a", false, None).await;
    journal_finish(&state, &id, "b", "run-b", false, None).await;

    let response = router(state)
        .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
        .await
        .unwrap();
    let body = json_body(response).await;
    let rows = body["runs"].as_array().expect("array");
    assert_eq!(rows.len(), 2, "{body}");
    let by_id = |run: &str| {
        rows.iter()
            .find(|r| r["runId"] == run)
            .unwrap_or_else(|| panic!("{run} missing: {body}"))
            .clone()
    };
    assert_eq!(by_id("run-a")["nodes"][0]["nodeId"], "a1");
    assert_eq!(by_id("run-a")["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(by_id("run-b")["nodes"][0]["nodeId"], "b1");
    assert_eq!(by_id("run-b")["nodes"].as_array().unwrap().len(), 1);
}

/// Issue #1012. Two interleaved runs — `run-a` starts first but
/// `run-b` finishes last — must come back ordered by **finish**, the
/// field every row displays, not by the order they started. The old
/// `runs.reverse()` only flipped the fold's push order (start order),
/// so this exact fixture used to list `run-a` first despite `run-b`
/// carrying the newer `seq`/`atMillis`.
#[tokio::test]
async fn run_history_orders_by_finish_not_start() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, id) = hosted_state(&home).await;

    // Start order: a, then b. Finish order: a, then b — so b is both
    // the last to start AND the last to finish, which alone would not
    // distinguish "ordered by start" from "ordered by finish". Insert
    // a THIRD run, `c`, that starts before `b` but finishes before `a`
    // too, so the two orderings genuinely disagree on where it lands.
    journal_start(&state, &id, "wf", "run-a", false).await;
    journal_start(&state, &id, "wf", "run-c", false).await;
    journal_finish(&state, &id, "wf", "run-c", false, None).await;
    journal_start(&state, &id, "wf", "run-b", false).await;
    journal_finish(&state, &id, "wf", "run-a", false, None).await;
    journal_finish(&state, &id, "wf", "run-b", false, None).await;

    // Start order: a, c, b. Finish order: c, a, b.
    // By-start reversal would read: b, c, a (wrong).
    // By-finish descending must read: b, a, c.
    let response = router(state)
        .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
        .await
        .unwrap();
    let body = json_body(response).await;
    let rows = body["runs"].as_array().expect("array");
    assert_eq!(rows.len(), 3, "{body}");
    let ids: Vec<&str> = rows.iter().map(|r| r["runId"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["run-b", "run-a", "run-c"], "{body}");
}

/// `?limit=` now cuts **runs**, not journal rows — the number the caller
/// was asking about all along. Without the group-aware cut, a limit of 2
/// over three 4-row runs would return fragments.
#[tokio::test]
async fn run_history_limit_counts_runs_not_journal_rows() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, id) = hosted_state(&home).await;

    for i in 0..3 {
        let run = format!("run-{i}");
        journal_start(&state, &id, "digest", &run, false).await;
        journal_node(&state, &id, "digest", &run, "ceo", WorkflowNodeStatus::Ok).await;
        journal_node(&state, &id, "digest", &run, "done", WorkflowNodeStatus::Ok).await;
        journal_finish(&state, &id, "digest", &run, false, None).await;
    }

    let response = router(state)
        .oneshot(request(
            "GET",
            "/api/v1/company/workflows/runs?limit=2",
            None,
        ))
        .await
        .unwrap();
    let body = json_body(response).await;
    let rows = body["runs"].as_array().expect("array");
    assert_eq!(rows.len(), 2, "{body}");
    // Newest first, and each one whole.
    assert_eq!(rows[0]["runId"], "run-2");
    assert_eq!(rows[0]["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(rows[1]["runId"], "run-1");
}

/// `?workflow=` narrows to one graph, and does so BEFORE the limit cut —
/// otherwise asking for one workflow would return "whichever of the last
/// N happen to match", which for a busy company is usually none.
#[tokio::test]
async fn run_history_filters_by_workflow_before_the_limit_cut() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, id) = hosted_state(&home).await;

    journal_run(&state, &id, "digest", true, Vec::new(), None).await;
    for _ in 0..5 {
        journal_run(&state, &id, "greeter", false, Vec::new(), None).await;
    }

    // Only ONE `digest` run exists, and it is the OLDEST of six. A
    // filter applied after a `limit=2` cut would find nothing.
    let response = router(state)
        .oneshot(request(
            "GET",
            "/api/v1/company/workflows/runs?workflow=digest&limit=2",
            None,
        ))
        .await
        .unwrap();
    let body = json_body(response).await;
    let rows = body["runs"].as_array().expect("array");
    assert_eq!(rows.len(), 1, "body: {body}");
    assert_eq!(rows[0]["workflowId"], "digest");
}

/// `?limit=` caps the page from the newest end, defaults when absent or
/// zero, and clamps above the ceiling rather than folding the whole log.
#[tokio::test]
async fn run_history_limit_defaults_caps_and_clamps() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, id) = hosted_state(&home).await;

    for i in 0..25 {
        journal_run(&state, &id, &format!("wf-{i}"), false, Vec::new(), None).await;
    }

    let page = |uri: &'static str| {
        let state = state.clone();
        async move {
            json_body(
                router(state)
                    .oneshot(request("GET", uri, None))
                    .await
                    .unwrap(),
            )
            .await
        }
    };

    // Explicit cap, taken from the newest end.
    let capped = page("/api/v1/company/workflows/runs?limit=3").await;
    let rows = capped["runs"].as_array().expect("array");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["workflowId"], "wf-24", "newest first: {capped}");

    // No `limit` → the default page, not the whole 25.
    let defaulted = page("/api/v1/company/workflows/runs").await;
    assert_eq!(
        defaulted["runs"].as_array().unwrap().len(),
        DEFAULT_RUN_LIMIT,
        "{defaulted}"
    );

    // `limit=0` means "I didn't really mean zero" — an empty page is
    // never what a caller wants, so it falls back to the default.
    let zero = page("/api/v1/company/workflows/runs?limit=0").await;
    assert_eq!(
        zero["runs"].as_array().unwrap().len(),
        DEFAULT_RUN_LIMIT,
        "{zero}"
    );

    // Above the ceiling clamps; with only 25 rows that is all of them.
    let huge = page("/api/v1/company/workflows/runs?limit=100000").await;
    assert_eq!(huge["runs"].as_array().unwrap().len(), 25, "{huge}");
}

/// A company that has never run a workflow gets an empty list, not a
/// 404 — the history panel renders "nothing yet" rather than an error.
#[tokio::test]
async fn run_history_is_empty_before_any_run() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, _store, _id) = hosted_state(&home).await;

    let response = router(state)
        .oneshot(request("GET", "/api/v1/company/workflows/runs", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await["runs"].as_array().unwrap().len(),
        0
    );
}
