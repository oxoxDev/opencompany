//! `built_in`'s own inline tests, part 4 of 10. Split out of the
//! single inline `mod tests` block because it exceeded the 750-line file
//! limit; grouped in the original file's order, not by topic (the block
//! covered dozens of unrelated issues with no existing topical boundaries).
//! Shared setup lives in [`super::built_in_test_fixtures`] and
//! [`super::built_in_test_fixtures_2`].

use super::built_in_test_fixtures::*;
use super::built_in_test_fixtures_2::*;
use super::*;

/// Codex review (PR #2053): an earlier version of the reused-agent fix
/// above compared each `read_turn_usage` against the value seen before
/// that attempt, and zeroed a read that came back unchanged — which is
/// wrong for a genuinely NEW finalized total that happens to numerically
/// equal the immediately preceding one. Two separate, single-attempt,
/// fully successful calls on the SAME agent, both scripted with the exact
/// same usage, must each report their own real spend in full — neither
/// one is a retry, neither one errors, and a coincidental value match is
/// not evidence that the second call spent nothing.
#[tokio::test]
async fn a_second_successful_turn_is_trusted_even_when_its_total_matches_the_first() {
    let (agent, _deps) = scripted_agent_over(
        ScriptedProvider::new(vec![Ok("turn one".to_string()), Ok("turn two".to_string())])
            .reporting_usage(tinyinference::Usage {
                input_tokens: 500,
                output_tokens: 100,
                total_tokens: 600,
                ..Default::default()
            }),
    );

    let (first_outcome, first_usages) = agent.run("turn one").await;
    first_outcome.expect("turn one succeeds in a single attempt");
    let first_tokens: u64 = first_usages
        .iter()
        .map(|u| u.input_tokens + u.output_tokens)
        .sum();
    assert_eq!(
        first_tokens, 600,
        "turn one's own real spend: {first_usages:?}"
    );

    let (second_outcome, second_usages) = agent.run("turn two").await;
    second_outcome.expect("turn two also succeeds in a single attempt");
    let second_tokens: u64 = second_usages
        .iter()
        .map(|u| u.input_tokens + u.output_tokens)
        .sum();
    assert_eq!(
        second_tokens, 600,
        "turn two's finalized total happens to equal turn one's — that coincidence must \
         not zero it out: {second_usages:?}"
    );
}

/// The tally is **cumulative**, so the last frame is the whole attempt's
/// spend — summing the frames would multiply it, and taking the first would
/// report only the opening call of a fifty-step run.
#[test]
fn the_observed_turn_cost_is_the_last_tally_not_the_first_or_the_sum() {
    let frame = |iteration: u32, input: u64, output: u64, usd: f64| {
        oh::agent::progress::AgentProgress::TurnCostUpdated {
            model: "scripted".to_string(),
            iteration,
            input_tokens: input,
            output_tokens: output,
            cached_input_tokens: 0,
            total_usd: usd,
        }
    };
    let events = vec![
        frame(1, 100, 10, 0.001),
        oh::agent::progress::AgentProgress::TextDelta {
            delta: "thinking".to_string(),
            iteration: 2,
        },
        frame(2, 900, 250, 0.019),
    ];

    let observed = progress_pump::last_observed_turn_cost(&events).expect("a tally was published");

    assert_eq!(observed.input_tokens, 900);
    assert_eq!(observed.output_tokens, 250);
    assert!((observed.cost_usd - 0.019).abs() < f64::EPSILON);
    assert_eq!(
        progress_pump::last_observed_turn_cost(&[]),
        None,
        "a turn that made no metered model call has no tally to report"
    );
}

/// Issue #111 retry-guard edge: when a steer already pends and the first
/// attempt is the transient empty class, the one-shot retry is SKIPPED — so a
/// cancel/pause issued before any text can't silently restart the work. The
/// steered-empty turn therefore makes EXACTLY ONE attempt.
#[tokio::test]
async fn steered_empty_turn_makes_exactly_one_attempt() {
    // Attempt 1 is empty; a normal `run` would retry and consume the second
    // script entry. With a steer pending, the retry must not fire.
    let (agent, _deps) = scripted_agent(vec![Ok(String::new()), Ok("second".into())]);
    let control = SteerControl::new();
    control.request(SteerAction::Cancel);
    let (_outcome, usages) = agent
        .run_with_steer(
            "hi",
            Some(&control),
            None,
            None,
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await;
    let _outcome = _outcome.expect("runs");
    assert_eq!(
        usages.len(),
        1,
        "a steered empty turn does NOT retry — exactly one attempt"
    );
}

/// The empty-retry guard above proves steer does not silently *restart*
/// work. This proves the other half: a steer requested before a turn whose
/// first attempt already produced a real reply must not discard it. Only
/// the *next* iteration is where `SteerStopHook` is meant to intervene —
/// nothing here may drop output the model already returned.
#[tokio::test]
async fn a_steer_pending_before_a_successful_attempt_does_not_drop_its_reply() {
    let (agent, _deps) = scripted_agent(vec![Ok("here is the answer".into())]);
    let control = SteerControl::new();
    control.request(SteerAction::Cancel);
    let (outcome, usages) = agent
        .run_with_steer(
            "hi",
            Some(&control),
            None,
            None,
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await;
    let outcome = outcome.expect("runs");
    assert_eq!(usages.len(), 1, "one attempt, and it already succeeded");
    assert_eq!(
        outcome.reply, "here is the answer",
        "a pending steer must not discard a reply the model already produced"
    );
}

/// Empty twice → a graceful, non-error reply (chat never shows "Couldn't
/// send" for a transient hiccup), still two attempts.
#[tokio::test]
#[ignore = "TODO(hive-desks follow-up): scripts the exact model-call sequence of the previous in-crate agent loop (its empty-reply retry, its iteration-cap wrap-up call, its provider-outage failure). Since plan hive-desks Phase 2 the loop is OpenHuman's own, with its own empty/cap/outage protocol; re-base the expectations on that loop once its protocol is pinned."]
async fn turn_wrapper_empty_twice_is_graceful() {
    let (agent, _deps) = scripted_agent(vec![Ok(String::new()), Ok(String::new())]);
    let (outcome, usages) = agent.run("hi").await;
    let outcome = outcome.expect("graceful, not an Err");
    assert!(
        outcome
            .reply
            .to_lowercase()
            .contains("temporary model hiccup"),
        "got {:?}",
        outcome.reply
    );
    assert_eq!(usages.len(), 2);
}

/// The Empty-vs-Hard split: only the transient empty-response class is
/// retried/softened; every other error is `Hard` and propagates loudly (no
/// blanket swallow). Driven at the classifier so it's deterministic — the
/// live agent internally retries provider errors, which would make a scripted
/// "hard error" non-deterministic.
#[test]
fn transient_empty_response_is_recognised_but_hard_errors_are_not() {
    let empty = anyhow::anyhow!("The model returned an empty response. Please try again.");
    assert!(
        is_transient_empty_response(&empty),
        "empty-response is transient"
    );

    let hard = anyhow::anyhow!("daily budget exceeded for agent 'ceo'");
    assert!(
        !is_transient_empty_response(&hard),
        "a budget error is NOT the transient empty class — it must propagate"
    );
}

/// A chain must not lose its leaf. `{err}` renders only the outermost
/// context — `tinyagents harness run failed` — which drops both the
/// remaining-budget figure and the call that was in flight, the two things
/// the message promises to keep. `{err:#}` renders the whole chain.
#[test]
fn a_chained_ceiling_error_keeps_its_leaf() {
    let err = chained_ceiling_error();
    assert_eq!(
        format!("{err}"),
        "tinyagents harness run failed",
        "the premise: the outermost context alone says nothing useful"
    );
    assert!(
        is_wall_clock_ceiling(&err),
        "a chained ceiling hit is still a ceiling hit"
    );

    let msg = wall_clock_ceiling_message("product_manager", Duration::from_millis(601_000), &err);
    assert!(
        msg.contains("56636 ms"),
        "the remaining-budget figure survives the chain: {msg}"
    );
    assert!(
        msg.contains("model call for run 'agent_turn'"),
        "and so does the call that was in flight: {msg}"
    );
}

#[test]
fn wall_clock_ceiling_is_recognised_and_other_timeouts_are_not() {
    assert!(
        is_wall_clock_ceiling(&ceiling_error()),
        "the harness's own ceiling leaf must be recognised"
    );
    assert!(
        !is_wall_clock_ceiling(&anyhow::anyhow!(
            "request timed out after 30s connecting to the provider"
        )),
        "an ordinary provider timeout is NOT the turn ceiling — it keeps its own text"
    );
    assert!(
        !is_wall_clock_ceiling(&anyhow::anyhow!("daily budget exceeded for agent 'ceo'")),
        "a SPEND budget is not a wall-clock budget"
    );
}

/// A provider's response body reaches this chain verbatim — `provider.rs`
/// raises `InferenceError::Model("hosted inference returned {status}: {text}")`
/// — so an endpoint with a wall-clock budget of its own must not be read as
/// OpenHuman's per-turn ceiling. That misdiagnosis is worse than the plain
/// wrapper: it would report the second the request took as if it were a
/// ten-minute turn, and tell the operator to raise a ceiling that was never
/// reached.
#[test]
fn a_provider_body_that_mentions_a_wall_clock_budget_is_not_the_ceiling() {
    for body in [
        "hosted inference returned 429: {\"error\":\"wall-clock budget for this key is exhausted\"}",
        "hosted inference returned 400: your per-request wall-clock budget must be positive",
    ] {
        assert!(
            !is_wall_clock_ceiling(&anyhow::anyhow!("{body}")),
            "a provider body is not the turn ceiling: {body}"
        );
    }
    // Both phrasings the vendored harness actually raises still match,
    // including the deadline spelling the old three-word search covered
    // only by accident.
    for leaf in [
        "model call for run 'agent_turn' exceeded its remaining wall-clock budget (56636 ms)",
        "tool call for run 'agent_turn' exceeded its wall-clock deadline",
    ] {
        assert!(
            is_wall_clock_ceiling(&anyhow::anyhow!("{leaf}")),
            "the harness's own leaf must still be recognised: {leaf}"
        );
    }
}

#[test]
fn elapsed_reads_as_an_operator_reads_a_clock() {
    assert_eq!(humanise_elapsed(Duration::from_millis(9_400)), "9s");
    assert_eq!(humanise_elapsed(Duration::from_secs(90)), "1m 30s");
    assert_eq!(humanise_elapsed(Duration::from_millis(601_000)), "10m 01s");
}

/// The whole point of #1680: the operator is told what the turn actually
/// spent, and told that the harness's own number is the remainder rather
/// than a limit. The old text said neither.
#[test]
fn ceiling_message_reports_elapsed_and_reframes_the_harness_number() {
    let err = ceiling_error();
    let msg = wall_clock_ceiling_message("product_manager", Duration::from_millis(601_000), &err);

    assert!(msg.contains("product_manager"), "names the agent: {msg}");
    assert!(
        msg.contains("10m 01s"),
        "states what the turn actually spent: {msg}"
    );
    assert!(
        msg.contains("REMAINED"),
        "says the harness's figure is the remainder, not a limit: {msg}"
    );
    assert!(
        msg.contains("OPENHUMAN_AGENT_TURN_TIMEOUT_SECS"),
        "names the knob that moves the ceiling: {msg}"
    );
    assert!(
        msg.contains("56636 ms"),
        "keeps the underlying diagnostic verbatim: {msg}"
    );
    // The ceiling's own value is private to the vendored crate, so it is
    // deliberately not restated — a stale copy would be worse than none.
    assert!(
        !msg.contains("600"),
        "must not hardcode a ceiling it cannot read: {msg}"
    );
}

/// At the classifier, where the retry wrapper actually reads it: a ceiling
/// hit stays HARD (a ten-minute failure must not be retried into twenty),
/// and carries the honest message rather than the bare `turn for 'x': …`.
/// The other spelling the harness raises carries **no** figure — `run
/// `agent_turn` exceeded its wall-clock deadline` — so the message must not
/// point the operator at one. Being accurate but unfollowable is the exact
/// defect #1680 was filed on; reintroducing it one spelling over would be a
/// poor way to close it.
#[test]
fn the_figure_less_spelling_is_not_promised_a_figure() {
    let err = anyhow::anyhow!(
        "tinyagents harness run failed: run timed out: run `agent_turn` exceeded its \
         wall-clock deadline"
    );
    assert!(
        is_wall_clock_ceiling(&err),
        "it is still the ceiling, and still classified here"
    );

    let msg = wall_clock_ceiling_message("ceo", Duration::from_secs(600), &err);
    assert!(
        !msg.contains("the figure below"),
        "there is no figure below to point at: {msg}"
    );
    assert!(
        msg.contains("10m 00s"),
        "the measured elapsed still leads: {msg}"
    );
    assert!(
        msg.contains("OPENHUMAN_AGENT_TURN_TIMEOUT_SECS"),
        "and the knob is still named: {msg}"
    );
    assert!(
        msg.contains("exceeded its wall-clock deadline"),
        "the leaf survives verbatim: {msg}"
    );
}

#[tokio::test]
async fn classify_turn_reframes_a_ceiling_hit_and_keeps_it_hard() {
    let (agent, _deps) = scripted_agent(vec![]);

    let outcome = agent.classify_turn(Err(ceiling_error()), Duration::from_millis(601_000));
    let AttemptOutcome::Hard(err) = outcome else {
        panic!("a ceiling hit is not retryable and must classify Hard");
    };
    let text = err.to_string();
    assert!(
        text.contains("per-turn wall-clock ceiling after 10m 01s"),
        "got {text}"
    );

    // Every other hard error keeps the plain wrapper, unchanged.
    let other = agent.classify_turn(
        Err(anyhow::anyhow!("provider refused the request")),
        Duration::from_secs(3),
    );
    let AttemptOutcome::Hard(err) = other else {
        panic!("an unrelated failure is still Hard");
    };
    assert!(
        err.to_string().contains("provider refused the request"),
        "an unrelated failure keeps its own text: {err}"
    );
    assert!(
        !err.to_string().contains("wall-clock"),
        "and gains no ceiling prose: {err}"
    );
}

/// Drift-coupling: `is_top_level_budget_exhausted` must be a thin wrapper
/// over `oh::api::classify::is_budget_exhausted_message`, never a
/// second, independently-maintained phrase list. Computes both sides for
/// a spread of real and synthetic bodies and asserts they never disagree,
/// so an edit that "helps" by hardcoding a phrase here fails CI instead of
/// silently drifting from the shared source (the deferred-classifier-arm
/// trap this repo has been bitten by before).
#[test]
fn top_level_budget_classifier_never_drifts_from_the_shared_source() {
    for body in [
        "hosted inference returned 400: insufficient budget for this account",
        "hosted inference returned 402: budget exceeded for this key",
        "anthropic API error (400 Bad Request): {\"error\":{\"code\":\"invalid_request_error\",\
         \"message\":\"Your credit balance is too low to access the Anthropic API. Please go \
         to Plans & Billing to upgrade or purchase credits.\",\"type\":\"invalid_request_error\"}}",
        "hosted inference returned 402: {\"success\": false, \"error\": \"You have no \
         remaining credits to use the LLM apis.\"}",
        "hosted inference returned 429: quota exceeded — add credits to continue",
        "hosted inference returned 500: internal server error",
        "provider refused the request",
        "request timed out after 30s connecting to the provider",
        "",
    ] {
        let err = anyhow::anyhow!("{body}");
        assert_eq!(
            is_top_level_budget_exhausted(&err),
            oh::api::classify::is_budget_exhausted_message(&format!("{err:#}")),
            "top-level classifier drifted from the shared source for: {body}"
        );
    }
}

/// The headline unit test: every known budget-exhausted wire shape
/// classifies `BudgetPaused`, not `Hard` — proving the asymmetry this
/// issue closes at the one place it was introduced. A non-budget `Err`
/// keeps classifying `Hard`, unchanged.
#[tokio::test]
async fn classify_turn_recognises_every_known_budget_wire_shape_as_paused_not_hard() {
    let (agent, _deps) = scripted_agent(vec![]);

    let wire_shapes = [
        (
            "managed backend 400 (USER_INSUFFICIENT_CREDITS-style)",
            "hosted inference returned 400: {\"error\":\"USER_INSUFFICIENT_CREDITS: \
             insufficient budget for this account\"}",
        ),
        (
            "Anthropic BYO 400",
            "anthropic API error (400 Bad Request): {\"error\":{\"code\":\"invalid_request_error\",\
             \"message\":\"Your credit balance is too low to access the Anthropic API. Please \
             go to Plans & Billing to upgrade or purchase credits.\",\"type\":\"invalid_request_error\"}}",
        ),
        (
            "abacus/OpenRouter-style no-remaining-credits 402",
            "hosted inference returned 402: {\"success\": false, \"error\": \"You have no \
             remaining credits to use the LLM apis.\"}",
        ),
        (
            "quota exceeded",
            "hosted inference returned 429: quota exceeded — add credits to continue",
        ),
    ];

    for (label, body) in wire_shapes {
        let outcome = agent.classify_turn(Err(anyhow::anyhow!("{body}")), Duration::from_secs(1));
        let AttemptOutcome::BudgetPaused { summary } = outcome else {
            panic!("{label}: must classify BudgetPaused for wire body: {body}");
        };
        assert!(summary.starts_with("Paused —"), "{label}: {summary}");
        assert!(
            summary.to_ascii_lowercase().contains("add credits"),
            "{label}: must carry the actionable ask: {summary}"
        );
    }

    // Non-budget Err → still Hard, byte-for-byte the pre-#1846 behaviour.
    let other = agent.classify_turn(
        Err(anyhow::anyhow!(
            "hosted inference returned 500: internal server error"
        )),
        Duration::from_secs(1),
    );
    let AttemptOutcome::Hard(err) = other else {
        panic!("a non-budget failure must still classify Hard");
    };
    assert!(
        err.to_string().contains("internal server error"),
        "an unrelated failure keeps its own text: {err}"
    );
}

/// The halt copy shares the delegated sub-agent halt's ACTIONABLE framing
/// — "add credits" / "top up that provider's own account" / an explicit
/// next step for the operator — never the harness's own error vocabulary.
/// Byte-identity with the vendored `terminal_inference_halt_summary` is
/// not achievable (that function is private to `tinyagents` and phrased
/// per-tool, "the `{tool}` step failed", which has no analogue at the top
/// level), so this asserts the shared phrases survive instead of a
/// whole-string match. The top-level copy's own next step is "resend your
/// message" rather than the delegated halt's "try again" — a turn, unlike
/// a tool call, has no retry to invite; both say the SAME thing in the
/// vocabulary that fits their own layer.
#[test]
fn budget_paused_copy_shares_the_add_credits_framing_with_the_delegated_halt() {
    let err = anyhow::anyhow!("hosted inference returned 400: insufficient budget");
    let summary = budget_paused_summary("ceo", &err);
    let lower = summary.to_ascii_lowercase();
    for phrase in [
        "add credits",
        "top up that provider's own account",
        "resend your message",
    ] {
        assert!(
            lower.contains(phrase),
            "missing shared framing {phrase:?}: {summary}"
        );
    }
    assert!(summary.contains("ceo"), "names the teammate: {summary}");
}

/// The coarse proximity threshold (issue #1846): fires at/above 90% of the
/// cap, never below it, and never on a non-positive/non-finite cap — the
/// guard that keeps a company with no ceiling configured (`cap == 0` is
/// unreachable in practice, but the function must not divide by it) from
/// ever firing.
#[test]
fn budget_proximity_threshold_fires_at_ninety_percent_and_not_below() {
    assert!(!is_approaching_budget_ceiling(89, 100), "89% is not near");
    assert!(is_approaching_budget_ceiling(90, 100), "90% is near");
    assert!(
        is_approaching_budget_ceiling(100, 100),
        "100% is near (though callers gate this out via total_exhausted first)"
    );
    assert!(
        !is_approaching_budget_ceiling(50, 0),
        "a zero cap must never divide-by-zero into true"
    );

    assert!(
        !is_approaching_budget_ceiling_f64(4.49, 5.0),
        "89.8% rounds down to not-near"
    );
    assert!(is_approaching_budget_ceiling_f64(4.5, 5.0), "90% is near");
    assert!(
        !is_approaching_budget_ceiling_f64(4.5, f64::NAN),
        "a non-finite cap must never read as near"
    );
    assert!(
        !is_approaching_budget_ceiling_f64(4.5, 0.0),
        "a non-positive cap must never read as near"
    );
}

/// **The regression.** The bug this issue closes: a top-level orchestrator
/// turn whose own inference call fails with a budget-exhausted body — no
/// delegated-tool envelope marker anywhere in the chain, because nothing
/// was delegated — must terminate `Ok(TurnOutcome { budget_paused: Some(_), .. })`
/// and park a re-issue marker, NOT propagate `Err(OpenCompanyError::Harness(_))`.
///
/// Before this issue's fix, `classify_turn` had no arm between the
/// wall-clock-ceiling check and the generic `Hard` catch-all, so this
/// exact scenario fell through to `Hard(OpenCompanyError::Harness(format!("turn
/// for '{agent}': {err}")))` and `HarnessPool::run` returned that `Err` to
/// every caller — the silent mid-task break the issue is named for. This
/// test's premise is provable by inspection: `classify_turn`'s match arms
/// are ordered ceiling → budget → generic, and removing the budget arm
/// (i.e. reverting this diff) makes this body fall through to the generic
/// arm, which this test would then catch as an `Err` instead of the
/// expected `Ok`. The `ScriptedProvider` returns the error DIRECTLY from
/// `ChatModel::invoke` (no tool call, no delegation, no envelope) — the
/// "simple non-delegating task" the issue specifies.
#[tokio::test]
async fn a_top_level_budget_exhaustion_pauses_gracefully_and_parks_a_reissue_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let company = CompanyId::new("acme-budget-regress");
    let mut rec = record();
    rec.id = company.clone();
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        // Scripted 10 deep, not once: whether the vendored harness retries
        // a model error internally before ever handing `agent.turn()`'s
        // caller an `Err` is not this crate's contract to assume — the
        // real-world case this mirrors (an exhausted account) fails the
        // SAME way on every retry regardless, so scripting depth instead
        // of count-exactness is both safer and truer to the scenario.
        provider: Arc::new(ScriptedProvider::new(vec![
            Err(
                "USER_INSUFFICIENT_CREDITS: insufficient budget for this account — add \
                 credits to continue"
                    .to_string(),
            );
            10
        ])),
        provider_slug: "scripted".to_string(),
        serves: None,
        context: Arc::new(MockContext::default()),
        store: Arc::new(RecordingStore::default()),
        // No meter: the pre-flight budget gates (total ceiling, per-agent
        // cap) fail OPEN with none configured, so this dispatch reaches
        // the model call rather than being refused pre-flight — the
        // scenario under test is the model call itself failing, not a
        // pre-flight refusal.
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
        .expect(
            "a budget pause is a graceful stop, not an error — it must return Ok, which is \
             the whole regression this test proves",
        );

    let pause = outcome.budget_paused.as_ref().expect(
        "the top-level inference call failed on a budget-exhausted body; the turn must \
         report the pause",
    );
    assert_eq!(pause.agent, "ceo");
    assert!(
        pause.summary.contains("add credits"),
        "the actionable ask survives to the outcome: {}",
        pause.summary
    );

    // And a durable re-issue marker is parked, keyed on the agent, naming
    // the ORIGINAL message — the grant-reissue precedent this issue reuses.
    let marker = crate::runtime::grants::budget_pauses_for(&company)
        .peek("ceo")
        .expect("a re-issue marker must be parked for the paused agent");
    assert_eq!(marker.agent, "ceo");
    assert_eq!(marker.message, "Please summarize today's standup notes.");
    assert_eq!(marker.summary, pause.summary);
}
