use super::*;

/// Issue #1846 review (Codex #3869277640) — **the regression.** A budget
/// pause from a confined workflow-copilot turn must NOT read as the
/// copilot's own answer.
///
/// Before this fix, `confined_bubble` folded `outcome.reply` — the
/// budget-paused placeholder text, per `classify_turn`'s
/// `AttemptOutcome::BudgetPaused` handling — straight into an ordinary
/// bubble authored by `CONFINED_AGENT_ID`, exactly the #885/#966
/// author-vs-channel conflation this file exists to prevent, just for a
/// pause instead of an authored reply. `confined_turn_bubble` is the
/// fixed boundary: this asserts it routes a paused outcome to
/// `system_notice` (unauthored, `SYSTEM_AUTHOR`) instead.
#[test]
fn a_confined_turns_budget_pause_is_a_system_notice_not_a_copilot_reply() {
    let outcome = crate::harness::TurnOutcome {
        // What `classify_turn`'s `AttemptOutcome::BudgetPaused` arm
        // actually leaves in `reply` — irrelevant to the notice text,
        // which is built fresh from `budget_paused` below, but present
        // here so this fixture matches what `run_confined` really
        // returns rather than an idealised one.
        reply: "Paused — copilot's turn ran out of inference budget/credits.".to_string(),
        steps: Vec::new(),
        hit_iteration_cap: false,
        abnormal_stop: None,
        halted_for_spend: None,
        budget_paused: Some(crate::harness::BudgetPause {
            agent: confine::CONFINED_AGENT_ID.to_string(),
            summary: "Add credits to your account, then resend your message.".to_string(),
        }),
    };

    let bubble = confined_turn_bubble(outcome);

    assert_eq!(
        bubble.agent.as_deref(),
        Some(crate::ports::SYSTEM_AUTHOR),
        "a budget pause is never something the copilot said — it must be unauthored, not \
         attributed to CONFINED_AGENT_ID like an ordinary reply: {:?}",
        bubble.agent
    );
    assert_ne!(
        bubble.agent.as_deref(),
        Some(confine::CONFINED_AGENT_ID),
        "the pre-fix defect: falling through to confined_bubble would attribute the pause \
         notice to the copilot itself"
    );
    // Issue #1846 review (Codex #3870562586): the NO-RESEND prefix. This
    // assertion used to require `BUDGET_PAUSE_NOTICE_PREFIX`, which is
    // precisely what the console keys its "Add credits & resend" button
    // off — and `run_confined` never parks a marker, so that button could
    // only ever 404. Asserting the negative too: the whole defect is the
    // two prefixes being conflated, and a test that only checked the new
    // one would still pass if the redeemable prefix were ever made a
    // prefix of it.
    assert!(
        bubble
            .text
            .starts_with(BUDGET_PAUSE_NOTICE_NO_RESEND_PREFIX),
        "a confined pause parks no marker, so it must carry the non-redeemable prefix: {}",
        bubble.text
    );
    assert!(
        !bubble.text.starts_with(BUDGET_PAUSE_NOTICE_PREFIX),
        "the pre-fix defect: this prefix renders an Add-Credits CTA whose GET returns null \
         and whose POST 404s, because CONFINED_AGENT_ID never has a marker: {}",
        bubble.text
    );
    assert!(
        bubble.text.to_ascii_lowercase().contains("add credits"),
        "the actionable ask survives into the notice: {}",
        bubble.text
    );
}

/// Issue #1846 review (Codex #3870562590) — **the regression.** An approval
/// continuation that pauses for credits must not advertise a redeem the
/// server refuses.
///
/// The continuation runs through `run_steered_background`, so `run_inner`
/// parks its marker with `background: true`, and `redeem_budget_pause`
/// rejects exactly that shape with a 400 (`src/server/ops/budget_pause.rs`).
/// Emitting `BUDGET_PAUSE_NOTICE_PREFIX` therefore put a button on screen
/// that reserved the marker, restored it, and failed — every single click.
///
/// Issue #1906: this pins the notice BUILDER only, and its name now says
/// so. It calls `budget_pause_notice_no_resend` directly and asserts the
/// result starts with the constant that function formats with — a
/// tautology over `format!`. Revert the continuation arm at
/// `run_steered_background`'s tail to `budget_pause_notice` and this test
/// still passes, so the name it used to carry — "an approval continuation
/// pause offers no redeem CTA" — promised coverage it does not provide.
/// That coverage is real and lives in
/// `a_budget_paused_approval_continuation_surfaces_the_notice_and_parks_a_marker`,
/// which drives the continuation and reads the bubble it emits. Kept under
/// the honest name anyway: it is the cheap guard on the builder itself,
/// which is what the console branches on.
#[test]
fn the_no_resend_notice_builder_uses_the_non_redeemable_prefix() {
    let pause = crate::harness::BudgetPause {
        agent: "maya".to_string(),
        summary: "Add credits to your account, then start this again.".to_string(),
    };

    let notice = budget_pause_notice_no_resend(&pause);

    assert!(
        notice.starts_with(BUDGET_PAUSE_NOTICE_NO_RESEND_PREFIX),
        "{notice}"
    );
    assert!(
        !notice.starts_with(BUDGET_PAUSE_NOTICE_PREFIX),
        "the pre-fix defect: a background-parked marker is refused by the redeem route, so \
         this prefix's CTA can only ever return 400: {notice}"
    );
    assert!(
        notice.to_ascii_lowercase().contains("add credits"),
        "the operator still has to be told the lever: {notice}"
    );
    assert!(
        notice.contains(&pause.summary),
        "the provider's own summary survives into the notice: {notice}"
    );
}

/// The two prefixes must stay genuinely distinct: the console decides
/// whether to render an actionable button by `startsWith`, so if the
/// redeemable prefix were ever edited to become a prefix of the
/// non-redeemable one, every no-resend notice would silently regain the
/// broken CTA. Cheap coupling test, mirrored on the frontend by
/// `budget-pause-notice.test.ts`'s "does not match the NO-RESEND sibling
/// prefix" fixture — which asserts the negative against the real
/// `BUDGET_PAUSE_NOTICE_NO_RESEND_PREFIX` string rather than an invented
/// near-miss (issue #1906: the claim was made here before that fixture
/// existed).
#[test]
fn the_redeemable_and_no_resend_prefixes_are_not_prefixes_of_each_other() {
    assert!(
        !BUDGET_PAUSE_NOTICE_NO_RESEND_PREFIX.starts_with(BUDGET_PAUSE_NOTICE_PREFIX),
        "a no-resend notice would match `isBudgetPauseNotice` and regain the CTA"
    );
    assert!(
        !BUDGET_PAUSE_NOTICE_PREFIX.starts_with(BUDGET_PAUSE_NOTICE_NO_RESEND_PREFIX),
        "a redeemable notice would stop matching and lose its working CTA"
    );
}

/// Issue #966: a bubble the runtime wrote names a non-agent author.
///
/// Asserts it is not `"operator"` specifically, rather than only that it
/// equals the constant. The whole defect is that a host-authored notice and
/// a reply whose author was overwritten stored the *same* value, so a test
/// that checked equality alone would still pass if `SYSTEM_AUTHOR` were ever
/// redefined to the channel name.
#[test]
fn a_host_authored_notice_is_not_authored_by_the_operator_channel() {
    let bubble = system_notice("Acknowledged.".to_string());
    assert_eq!(bubble.channel, "operator", "the destination is unchanged");
    assert_eq!(bubble.agent.as_deref(), Some(crate::ports::SYSTEM_AUTHOR));
    assert_ne!(
        bubble.agent.as_deref(),
        Some("operator"),
        "a notice must not store the author a destination-overwrite produces"
    );
}

/// The acceptance case: a dispatch that died on a model id the provider
/// rejects is answerable — somebody can set a real one — so it parks and
/// the card lands `paused` carrying the question, instead of dropping back
/// into To-do indistinguishable from work nobody started.
#[tokio::test]
async fn a_rejected_model_id_parks_a_blocker_rather_than_settling_failed() {
    crate::harness::built_in::policy::policy_test_helpers_tests::in_cycle(async {
        use crate::harness::policy::ApprovalRequestQueue;
        use crate::ports::blockers::{BlockerKind, BlockerPayload, BlockerSource, BlockerStep};

        let dir = tempfile::tempdir().unwrap();
        let requests = ApprovalRequestQueue::default();
        let brain = brain_with_approval_queue(dir.path(), requests.clone());

        let reason = "dispatch failed: the model `gpt-nonexistent` does not exist or you do not \
                  have access to it";
        let end = brain.settle_as_blocker_or_failure("t-1", reason, Some("run-1"));

        assert_eq!(end, TaskRunEnd::Blocked);
        assert_eq!(
            lifecycle::landing_column(end),
            crate::ports::tasks::COLUMN_PAUSED,
            "a card with an open question on it has not failed — it is waiting"
        );

        let drained = requests.drain(8);
        assert_eq!(drained.requests.len(), 1, "exactly one question is asked");
        let effect = &drained.requests[0].effect;
        assert_eq!(effect.kind, "blocker.infrastructure");
        assert_eq!(effect.run_id.as_deref(), Some("run-1"));

        let payload: BlockerPayload =
            serde_json::from_value(effect.payload.clone()).expect("the payload round-trips");
        assert_eq!(payload.kind, BlockerKind::Infrastructure);
        assert_eq!(payload.source, BlockerSource::Provider);
        assert_eq!(
            payload.step,
            Some(BlockerStep::Task {
                task_id: "t-1".to_string()
            })
        );
        assert!(
            !payload.needed.trim().is_empty(),
            "a question that does not say what would answer it wastes the asking"
        );
    })
    .await;
}

/// The conservative default, pinned: a failure the classifier does not
/// recognise keeps today's behaviour exactly and asks nobody. Being wrong
/// in this direction costs a `Failed` that #1865 already surfaces; being
/// wrong the other way spends an operator's attention on a question they
/// cannot answer.
#[tokio::test]
async fn an_unrecognised_failure_still_fails_and_asks_nobody() {
    use crate::harness::policy::ApprovalRequestQueue;

    let dir = tempfile::tempdir().unwrap();
    let requests = ApprovalRequestQueue::default();
    let brain = brain_with_approval_queue(dir.path(), requests.clone());

    let end =
        brain.settle_as_blocker_or_failure("t-1", "dispatch failed: index out of bounds", None);

    assert_eq!(end, TaskRunEnd::Failed);
    assert_eq!(
        lifecycle::landing_column(end),
        crate::ports::tasks::COLUMN_TODO
    );
    assert!(
        requests.drain(8).requests.is_empty(),
        "an unrecognised failure must not reach the operator as a question"
    );
}

/// Recognising a transient stop is how we know **not** to ask: a rate limit
/// resolves itself, so it settles like any other failure and nothing is
/// parked.
#[tokio::test]
async fn a_rate_limit_settles_without_asking_anybody() {
    use crate::harness::policy::ApprovalRequestQueue;

    let dir = tempfile::tempdir().unwrap();
    let requests = ApprovalRequestQueue::default();
    let brain = brain_with_approval_queue(dir.path(), requests.clone());

    let end = brain.settle_as_blocker_or_failure(
        "t-1",
        "dispatch failed: hosted inference returned 429: rate limit exceeded",
        None,
    );

    assert_eq!(end, TaskRunEnd::Failed);
    assert!(requests.drain(8).requests.is_empty());
}

/// Approving a blocker must do **nothing** in this issue — the answer is
/// carried back into the stopped turn by #1863, and until then an approve
/// that half-executed something would be worse than one that does not.
///
/// `perform_effect` acts on three things: an `amount_usd` (writes a ledger
/// entry), a `channel`+`text` pair in the payload (sends a message), and
/// the email kind. This pins that a blocker effect carries none of them, so
/// the no-op is a property of the shape rather than a coincidence somebody
/// could break by adding a field.
#[tokio::test]
async fn a_parked_blocker_carries_nothing_an_executor_would_act_on() {
    crate::harness::built_in::policy::policy_test_helpers_tests::in_cycle(async {
        use crate::harness::policy::ApprovalRequestQueue;

        let dir = tempfile::tempdir().unwrap();
        let requests = ApprovalRequestQueue::default();
        let brain = brain_with_approval_queue(dir.path(), requests.clone());

        brain.settle_as_blocker_or_failure(
            "t-1",
            "tool call failed: could not connect to mcp server `slack`",
            None,
        );

        let drained = requests.drain(8);
        let effect = &drained.requests[0].effect;
        assert!(effect.amount_usd.is_none(), "a question costs nothing");
        assert!(
            effect.payload.get("channel").is_none() && effect.payload.get("text").is_none(),
            "a `channel`+`text` payload would make approving a blocker post a message"
        );
        assert!(
            effect.agent.is_none(),
            "stamping an agent would mint a grant and re-dispatch the turn, which would \
         call the escalation again and park a second time"
        );
    })
    .await;
}

/// A caller with nothing published must not mint a card describing a
/// deliverable that does not exist. Every known caller already filters this
/// out before reaching `record_conversation_publishes`, so this pins the
/// defensive guard for whichever caller does not: with both a task board and
/// an artifact store wired, the pre-guard code would otherwise mint an
/// orphaned in-review card.
#[tokio::test]
async fn record_conversation_publishes_rejects_an_empty_batch() {
    use crate::runtime::delegation::ChatTarget;

    let dir = tempfile::tempdir().unwrap();
    let (brain, tasks) = brain_with_artifacts(dir.path());

    let error = brain
        .record_conversation_publishes("maya", ChatTarget::in_thread(None, None), Vec::new())
        .await
        .expect_err("an empty batch must not mint a card");
    assert!(error.to_string().contains("nothing published"), "{error}");
    assert!(
        tasks
            .list(&CompanyId::new("acme"))
            .await
            .expect("list")
            .is_empty(),
        "no card must be minted for an empty batch"
    );
}
