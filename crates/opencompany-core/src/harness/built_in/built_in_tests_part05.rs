//! `built_in`'s own inline tests, part 5 of 10. Split out of the
//! single inline `mod tests` block because it exceeded the 750-line file
//! limit; grouped in the original file's order, not by topic (the block
//! covered dozens of unrelated issues with no existing topical boundaries).
//! Shared setup lives in [`super::built_in_test_fixtures`] and
//! [`super::built_in_test_fixtures_2`].

use super::built_in_test_fixtures::*;
use super::*;

/// Codex review (PR #2053) — **the regression.** A ledger write is a
/// separate concern from what the turn itself did, and a failure in it
/// must not also swallow the OTHER outcome-side-effect this same code
/// block performs: retiring a stale re-issue marker once an agent's turn
/// succeeds again, proving the account that blocked it now has budget.
/// Before this fix, `turn_result_after_metering`'s `?` ran BEFORE this
/// retire logic, so a ledger write that failed for an UNRELATED reason
/// left the stale marker — and its stale "Add credits & resend" CTA —
/// parked indefinitely, able to later re-dispatch the OLD message a
/// second time.
///
/// Same fixture as `a_successful_turn_retires_a_stale_reissue_marker_for_the_same_agent`
/// — a stale marker parked directly, then one ordinary successful `run`
/// for the same agent in the same thread — except this provider's reply
/// carries real usage, so `turn_costs` is nonzero and `meter_turn_costs`
/// actually attempts (and, against `FailingLedgerStore`, fails) a ledger
/// write. Reverting the reordering in `run_inner` makes the final `peek`
/// below find the marker still parked instead of `None`.
#[tokio::test]
async fn a_metering_failure_does_not_swallow_a_stale_marker_retirement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let company = CompanyId::new("acme-budget-meter-fail-retire-regress");
    let mut rec = record();
    rec.id = company.clone();
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        // A single, ordinary, non-blank reply — the same shape
        // `a_successful_turn_retires_a_stale_reissue_marker_for_the_same_agent`
        // scripts, just with usage attached so this turn's spend is
        // nonzero and `meter_turn_costs` has something to write.
        provider: Arc::new(
            ScriptedProvider::new(vec![Ok("Here's today's standup summary.".to_string()); 4])
                .reporting_usage(tinyinference::Usage {
                    input_tokens: 800,
                    output_tokens: 200,
                    total_tokens: 1_000,
                    ..Default::default()
                }),
        ),
        provider_slug: "scripted".to_string(),
        serves: None,
        context: Arc::new(MockContext::default()),
        store: Arc::new(FailingLedgerStore),
        meter: None,
        workspace_root: dir.path().to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.path().to_path_buf(),
        model_override: None,
        tasks: None,
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events: None,
        delegations: DelegationQueue::default(),
        workflow_runner: crate::harness::orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_runs: None,
        deep_trace: None,
        workflow_revisions: None,
        approval_requests: ApprovalRequestQueue::default(),
        approval_parker: None,
        secrets: None,
        web_allowed_domains: Vec::new(),
        capabilities: crate::harness::toolbelt::CapabilityFilter::AllowAll,
        workflow_source_dir: None,
        plan: None,
        media: None,
        composio: None,
        #[cfg(feature = "chargebee")]
        chargebee: None,
        #[cfg(feature = "paypal")]
        paypal: None,
        hosting: None,
        steer: crate::company::steer::InflightRegistry::default(),
        run_supervisor: crate::runtime::RunSupervisor::default(),
        delivery: None,
        search: None,
        tenant_search: None,
        workspace: None,
    };

    let pool = HarnessPool::new();
    pool.ensure(&rec, &deps).await.expect("pool ensures");

    // Park the stale marker directly — standing in for an earlier turn
    // that genuinely paused, exactly as the sibling retire test does.
    crate::runtime::grants::budget_pauses_for(&company).park(
        "ceo",
        Some("general".to_string()),
        "Please summarize today's standup notes.",
        "Paused — ceo's turn ran out of inference budget/credits.",
        crate::ports::now_millis(),
        crate::runtime::grants::RedeemContext::default(),
    );
    assert!(
        crate::runtime::grants::budget_pauses_for(&company)
            .peek("ceo")
            .is_some(),
        "the stale marker must be parked before the run this test exercises"
    );

    let result = pool
        .run(
            &company,
            "ceo",
            "Please summarize today's standup notes.",
            &deps,
            crate::runtime::delegation::ChatTarget::channel(Some("general")),
        )
        .await;

    assert!(
        result.is_err(),
        "the turn itself succeeded, so the ledger failure is the only failure there is, \
         and it still propagates — turn_result_after_metering's own documented contract: \
         {result:?}"
    );

    // The retirement must have happened regardless — read off the turn's
    // OWN outcome, before the metering error ever had a chance to short
    // circuit it.
    assert!(
        crate::runtime::grants::budget_pauses_for(&company)
            .peek("ceo")
            .is_none(),
        "the stale marker must be retired even though the ledger write for THIS turn \
         failed — the ledger is a separate concern from what the turn itself did, and \
         leaving it parked would let its stale CTA re-dispatch the old message again"
    );
}

/// Issue #1846 review (Codex #3869193105) — **the regression.** A
/// BYO/custom-provider budget error can carry a credential-bearing URL
/// (the account's own endpoint, with an API key riding in the query
/// string) baked into the provider's response body, which becomes the
/// raw `anyhow` error chain `budget_paused_summary` formats into
/// `summary`.
///
/// Before this fix, only the copy returned as the turn's authored REPLY
/// was scrubbed (`Ok(mcp_probe::scrub(&summary, &[]))`); the copy stored
/// into the `budget_pause_summary` mutex slot — which becomes
/// `TurnOutcome::budget_paused.summary`, and from there the durable
/// `BudgetPauseMarker.summary` AND the chat notice text
/// `budget_pause_notice` renders from it — was the RAW, unscrubbed
/// `summary`. A secret that never should have left the reply bubble was
/// therefore persisted on the marker and shown in the chat notice, both
/// operator-visible and durable, independent of whatever the reply itself
/// said.
///
/// Same fixture and scenario as the test above; the only difference is
/// what the scripted provider's error body contains. Proof this pins the
/// actual fix and not a coincidence: reverting the `scrub` call in
/// either `BudgetPaused` arm of `classify_turn`'s caller makes this
/// assertion fail while leaving the sibling test above green.
#[tokio::test]
async fn a_budget_pause_summary_is_scrubbed_before_it_is_persisted_anywhere() {
    let dir = tempfile::tempdir().expect("tempdir");
    let company = CompanyId::new("acme-budget-scrub-regress");
    let mut rec = record();
    rec.id = company.clone();
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(ScriptedProvider::new(vec![
            Err(
                "insufficient budget: BYO provider request to \
                 https://api.byo-provider.example/v1/chat?api_key=sk-live-topsecret123 \
                 failed with 400"
                    .to_string(),
            );
            10
        ])),
        provider_slug: "scripted".to_string(),
        serves: None,
        context: Arc::new(MockContext::default()),
        store: Arc::new(RecordingStore::default()),
        meter: None,
        workspace_root: dir.path().to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.path().to_path_buf(),
        model_override: None,
        tasks: None,
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events: None,
        delegations: DelegationQueue::default(),
        workflow_runner: crate::harness::orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_runs: None,
        deep_trace: None,
        workflow_revisions: None,
        approval_requests: ApprovalRequestQueue::default(),
        approval_parker: None,
        secrets: None,
        web_allowed_domains: Vec::new(),
        capabilities: crate::harness::toolbelt::CapabilityFilter::AllowAll,
        workflow_source_dir: None,
        plan: None,
        media: None,
        composio: None,
        #[cfg(feature = "chargebee")]
        chargebee: None,
        #[cfg(feature = "paypal")]
        paypal: None,
        hosting: None,
        steer: crate::company::steer::InflightRegistry::default(),
        run_supervisor: crate::runtime::RunSupervisor::default(),
        delivery: None,
        search: None,
        tenant_search: None,
        workspace: None,
    };

    let pool = HarnessPool::new();
    pool.ensure(&rec, &deps).await.expect("pool ensures");

    let outcome = pool
        .run(
            &company,
            "ceo",
            "Please summarize today's standup notes.",
            &deps,
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await
        .expect("a budget pause is a graceful stop, not an error");

    let pause = outcome
        .budget_paused
        .as_ref()
        .expect("the scripted body matches the budget-exhausted classifier");

    assert!(
        !pause.summary.contains("api_key=sk-live-topsecret123"),
        "the credential must not survive into TurnOutcome::budget_paused.summary: {}",
        pause.summary
    );
    assert!(
        !outcome.reply.contains("api_key=sk-live-topsecret123"),
        "the credential must not survive into the authored reply either: {}",
        outcome.reply
    );

    // The durable marker — what the chat notice (`budget_pause_notice`)
    // and the console's `GET …/budget-pause` both read — carries the SAME
    // scrubbed summary, not a second, unscrubbed copy of the raw error.
    let marker = crate::runtime::grants::budget_pauses_for(&company)
        .peek("ceo")
        .expect("a re-issue marker must be parked for the paused agent");
    assert!(
        !marker.summary.contains("api_key=sk-live-topsecret123"),
        "the persisted marker must not carry the credential either: {}",
        marker.summary
    );
    assert_eq!(marker.summary, pause.summary);
}

/// Issue #1846 review (Codex #3868962381) — **the regression.** The
/// budget-pause notice's own copy gives the operator TWO ways to recover:
/// click "Add credits & resend" (the CTA, which redeems the marker), or
/// add credits and resend the message themselves from the composer. Only
/// the first path used to retire the parked marker — `redeem`/
/// `redeem_matching` are the sole consumers of `BudgetPauseSet`'s entries.
/// A manual resend that succeeds bypasses both entirely, so the marker
/// (and the stale "Add credits & resend" CTA on the old notice) stayed
/// parked indefinitely. Clicking that stale CTA later would silently
/// re-dispatch the OLD message a second time.
///
/// Proof this pins the fix and not a coincidence: the marker parked
/// (directly, bypassing the turn machinery entirely so this test does not
/// depend on how many attempts the vendored harness's own internal retry
/// consumes before a budget-exhausted body reaches `classify_turn` — see
/// the sibling regression tests' "scripted 10 deep" comments for why that
/// count is not this crate's contract to assume) must be gone after ONE
/// ordinary successful `pool.run` call for the SAME agent — reverting the
/// retire branch in `run_inner` (this file) makes the final `peek` below
/// find the marker still parked instead of `None`.
#[tokio::test]
async fn a_successful_turn_retires_a_stale_reissue_marker_for_the_same_agent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let company = CompanyId::new("acme-budget-retire-regress");
    let mut rec = record();
    rec.id = company.clone();
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        // Always succeeds — the "operator manually added credits and
        // resent" half of the scenario, NOT the redeem route, which is
        // the whole point: nothing here ever calls `redeem`/
        // `redeem_matching`.
        provider: Arc::new(ScriptedProvider::new(vec![
            Ok(
                "Here's today's standup summary.".to_string()
            );
            4
        ])),
        provider_slug: "scripted".to_string(),
        serves: None,
        context: Arc::new(MockContext::default()),
        store: Arc::new(RecordingStore::default()),
        meter: None,
        workspace_root: dir.path().to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.path().to_path_buf(),
        model_override: None,
        tasks: None,
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events: None,
        delegations: DelegationQueue::default(),
        workflow_runner: crate::harness::orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_runs: None,
        deep_trace: None,
        workflow_revisions: None,
        approval_requests: ApprovalRequestQueue::default(),
        approval_parker: None,
        secrets: None,
        web_allowed_domains: Vec::new(),
        capabilities: crate::harness::toolbelt::CapabilityFilter::AllowAll,
        workflow_source_dir: None,
        plan: None,
        media: None,
        composio: None,
        #[cfg(feature = "chargebee")]
        chargebee: None,
        #[cfg(feature = "paypal")]
        paypal: None,
        hosting: None,
        steer: crate::company::steer::InflightRegistry::default(),
        run_supervisor: crate::runtime::RunSupervisor::default(),
        delivery: None,
        search: None,
        tenant_search: None,
        workspace: None,
    };

    let pool = HarnessPool::new();
    pool.ensure(&rec, &deps).await.expect("pool ensures");

    // Park the stale marker directly — standing in for an earlier turn
    // that genuinely paused. `redeem`/`redeem_matching` are the only
    // consumers this fix's `else` branch is NOT one of, so parking it
    // this way exercises the exact same retire path a real pause would.
    crate::runtime::grants::budget_pauses_for(&company).park(
        "ceo",
        Some("general".to_string()),
        "Please summarize today's standup notes.",
        "Paused — ceo's turn ran out of inference budget/credits.",
        crate::ports::now_millis(),
        crate::runtime::grants::RedeemContext::default(),
    );
    assert!(
        crate::runtime::grants::budget_pauses_for(&company)
            .peek("ceo")
            .is_some(),
        "the stale marker must be parked before the run this test exercises"
    );

    // The "manually add credits and resend" half of the scenario: an
    // ordinary successful `run`, NOT the redeem route — nothing here
    // ever calls `redeem`/`redeem_matching` on the marker parked above.
    // Same thread ("general") the marker itself parked with (issue
    // #1846 review, Codex #3869968949): a genuine resend runs in the
    // SAME conversation, and the widened context match this test is
    // pinned against would otherwise (correctly) treat a different
    // thread as a different request.
    let outcome = pool
        .run(
            &company,
            "ceo",
            "Please summarize today's standup notes.",
            &deps,
            crate::runtime::delegation::ChatTarget::channel(Some("general")),
        )
        .await
        .expect("this run succeeds against the scripted reply");
    assert!(
        outcome.budget_paused.is_none(),
        "this attempt must NOT pause — this scenario is about a resend that succeeds"
    );

    assert!(
        crate::runtime::grants::budget_pauses_for(&company)
            .peek("ceo")
            .is_none(),
        "the stale marker parked above must be retired once this agent has a successful \
         turn again, even though nothing ever redeemed it"
    );
}

/// Issue #1846 review (Codex #3869792503) — **the regression.** The
/// sibling test above proves a genuine RESEND retires its own marker;
/// this proves an UNRELATED success for the same agent does not retire
/// somebody else's still-unretried marker.
///
/// An agent has at most one parked marker (`BudgetPauseSet` overwrites by
/// agent id), so two DIFFERENT requests cannot both be "the" pause at
/// once — but a marker parked for request A can still be live when an
/// entirely separate request B for the same agent (an automatic
/// background task, a second chat message) happens to succeed. Before
/// this fix, that success unconditionally retired A's marker too — the
/// operator's original ask (A) was never reissued, yet its CTA would
/// report "nothing to resend" as though it had been.
///
/// Proof this pins the fix and not a coincidence: reverting
/// `retire_if_message_matches`'s match guard back to an unconditional
/// take (this file's `run_inner`, or `BudgetPauseSet::retire_if_message_matches`
/// itself) makes the final `peek` below find `None` instead of the still-
/// parked marker for request A.
#[tokio::test]
async fn an_unrelated_success_does_not_retire_a_different_requests_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let company = CompanyId::new("acme-budget-retire-mismatch-regress");
    let mut rec = record();
    rec.id = company.clone();
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        // Succeeds against a request B's text — deliberately DIFFERENT
        // from request A's, parked below.
        provider: Arc::new(ScriptedProvider::new(vec![
            Ok(
                "Filed under Q3 planning.".to_string()
            );
            4
        ])),
        provider_slug: "scripted".to_string(),
        serves: None,
        context: Arc::new(MockContext::default()),
        store: Arc::new(RecordingStore::default()),
        meter: None,
        workspace_root: dir.path().to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.path().to_path_buf(),
        model_override: None,
        tasks: None,
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events: None,
        delegations: DelegationQueue::default(),
        workflow_runner: crate::harness::orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_runs: None,
        deep_trace: None,
        workflow_revisions: None,
        approval_requests: ApprovalRequestQueue::default(),
        approval_parker: None,
        secrets: None,
        web_allowed_domains: Vec::new(),
        capabilities: crate::harness::toolbelt::CapabilityFilter::AllowAll,
        workflow_source_dir: None,
        plan: None,
        media: None,
        composio: None,
        #[cfg(feature = "chargebee")]
        chargebee: None,
        #[cfg(feature = "paypal")]
        paypal: None,
        hosting: None,
        steer: crate::company::steer::InflightRegistry::default(),
        run_supervisor: crate::runtime::RunSupervisor::default(),
        delivery: None,
        search: None,
        tenant_search: None,
        workspace: None,
    };

    let pool = HarnessPool::new();
    pool.ensure(&rec, &deps).await.expect("pool ensures");

    // Request A: paused, still unretried — parked directly, standing in
    // for a genuine earlier pause.
    crate::runtime::grants::budget_pauses_for(&company).park(
        "ceo",
        Some("general".to_string()),
        "Please summarize today's standup notes.",
        "Paused — ceo's turn ran out of inference budget/credits.",
        crate::ports::now_millis(),
        crate::runtime::grants::RedeemContext::default(),
    );

    // Request B: a completely different ask for the SAME agent, which
    // succeeds — an automatic background task landing before the
    // operator ever gets to A's CTA, per the finding's own example.
    let outcome = pool
        .run(
            &company,
            "ceo",
            "File this under Q3 planning.",
            &deps,
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await
        .expect("request B succeeds against the scripted reply");
    assert!(outcome.budget_paused.is_none(), "request B must not pause");

    let marker = crate::runtime::grants::budget_pauses_for(&company)
        .peek("ceo")
        .expect(
            "request A's marker must survive an unrelated request B succeeding — A was \
             never reissued",
        );
    assert_eq!(
        marker.message, "Please summarize today's standup notes.",
        "the marker still parked must be request A's, untouched by B's success"
    );
}

/// Issue #1846 review (Codex #3869968949) — **the regression.** The
/// sibling test above proves a DIFFERENT-text unrelated success does not
/// retire the marker; this proves IDENTICAL text in a DIFFERENT thread
/// does not either — the finding's own example ("review this", posted in
/// two different threads).
///
/// Same fixture and scenario, but request B repeats request A's EXACT
/// text — in a different chat thread. Before the widened match (message
/// text alone), this would have retired A's marker: `marker.message ==
/// candidate_message` was already true for identical text regardless of
/// which thread either ran in.
#[tokio::test]
async fn identical_text_in_a_different_thread_does_not_retire_the_original_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let company = CompanyId::new("acme-budget-retire-same-text-diff-thread");
    let mut rec = record();
    rec.id = company.clone();
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(ScriptedProvider::new(vec![
            Ok("Here it is.".to_string());
            4
        ])),
        provider_slug: "scripted".to_string(),
        serves: None,
        context: Arc::new(MockContext::default()),
        store: Arc::new(RecordingStore::default()),
        meter: None,
        workspace_root: dir.path().to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.path().to_path_buf(),
        model_override: None,
        tasks: None,
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events: None,
        delegations: DelegationQueue::default(),
        workflow_runner: crate::harness::orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_runs: None,
        deep_trace: None,
        workflow_revisions: None,
        approval_requests: ApprovalRequestQueue::default(),
        approval_parker: None,
        secrets: None,
        web_allowed_domains: Vec::new(),
        capabilities: crate::harness::toolbelt::CapabilityFilter::AllowAll,
        workflow_source_dir: None,
        plan: None,
        media: None,
        composio: None,
        #[cfg(feature = "chargebee")]
        chargebee: None,
        #[cfg(feature = "paypal")]
        paypal: None,
        hosting: None,
        steer: crate::company::steer::InflightRegistry::default(),
        run_supervisor: crate::runtime::RunSupervisor::default(),
        delivery: None,
        search: None,
        tenant_search: None,
        workspace: None,
    };

    let pool = HarnessPool::new();
    pool.ensure(&rec, &deps).await.expect("pool ensures");

    // Request A: paused in "general", still unretried.
    crate::runtime::grants::budget_pauses_for(&company).park(
        "ceo",
        Some("general".to_string()),
        "review this",
        "Paused — ceo's turn ran out of inference budget/credits.",
        crate::ports::now_millis(),
        crate::runtime::grants::RedeemContext::default(),
    );

    // Request B: the EXACT same text, but in "sales" — a different
    // conversation entirely — which succeeds.
    let outcome = pool
        .run(
            &company,
            "ceo",
            "review this",
            &deps,
            crate::runtime::delegation::ChatTarget::channel(Some("sales")),
        )
        .await
        .expect("request B succeeds against the scripted reply");
    assert!(outcome.budget_paused.is_none(), "request B must not pause");

    let marker = crate::runtime::grants::budget_pauses_for(&company)
        .peek("ceo")
        .expect(
            "request A's marker must survive B's success — same text, but a DIFFERENT \
             thread, is not the same request",
        );
    assert_eq!(marker.chat_id.as_deref(), Some("general"));
}
