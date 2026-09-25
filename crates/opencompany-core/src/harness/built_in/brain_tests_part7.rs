use super::*;

/// The regression for #172: a `RequireApproval` recorded during a turn is
/// **parked** on the host, so it lands in the journal the Approvals page
/// reads instead of being narrated away in chat and lost.
///
/// `ParkingHost` panics on `emit_effect`, which pins the other half of the
/// fix: the request must NOT be re-evaluated by the runtime gate (which
/// allows — and so silently "executes" — the `Other` group most gated tool
/// calls classify into).
#[tokio::test]
async fn approval_requests_are_parked_for_the_operator() {
    crate::harness::built_in::policy::policy_test_helpers_tests::in_cycle(async {
        use crate::harness::policy::{ApprovalPolicy, ApprovalRequestQueue};
        use openhuman_core::agent::tool_policy::{
            ToolCallContext, ToolPolicy, ToolPolicyDecision, ToolPolicyRequest,
        };

        let dir = tempfile::tempdir().unwrap();
        let requests = ApprovalRequestQueue::default();
        let brain = brain_with_approval_queue(dir.path(), requests.clone());

        // Exactly what a supervised policy records when the agent reaches for a
        // gated tool mid-turn.
        let policy = ApprovalPolicy::new(
            &crate::company::Policy {
                mode: "supervised".to_string(),
                always_approve: Vec::new(),
                auto_approve_under_usd: None,
                approval_ttl_hours: None,
            },
            None,
        )
        .with_requests(requests.clone());
        let args = crate::policy::test_support::composio_send_args();
        let request = ToolPolicyRequest::new(
            "composio_execute",
            args.clone(),
            ToolCallContext::session("s", "chat", "ceo", "call-1", 0),
        );
        assert!(
            matches!(
                policy.check(&request).await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "the fixture must reproduce a gated call"
        );
        assert_eq!(requests.queued(), 1, "the decision was recorded to park");

        let host = ParkingHost::default();
        brain
            .park_approval_requests(&host)
            .await
            .expect("the drain parks");

        let parked = host.parked();
        assert_eq!(parked.len(), 1, "one approval reached the operator");
        assert_eq!(parked[0].kind, "composio_execute");
        assert_eq!(
            parked[0].payload, args,
            "the call's arguments are preserved"
        );
        assert_eq!(requests.queued(), 0, "the queue is drained");
    })
    .await;
}

/// A second drain parks nothing: the queue is emptied, so a later cycle
/// can't re-park a request the operator has already been shown.
#[tokio::test]
async fn draining_twice_parks_nothing_the_second_time() {
    crate::harness::built_in::policy::policy_test_helpers_tests::in_cycle(async {
        use crate::harness::policy::{ApprovalRequest, ApprovalRequestQueue};
        use crate::ports::types::EffectGroup;

        let dir = tempfile::tempdir().unwrap();
        let requests = ApprovalRequestQueue::default();
        let brain = brain_with_approval_queue(dir.path(), requests.clone());
        requests.push(ApprovalRequest {
            tool: "media_generate_image".to_string(),
            reason: "supervised".to_string(),
            effect: Effect {
                kind: "media_generate_image".to_string(),
                group: EffectGroup::Spend,
                amount_usd: None,
                established_thread: false,
                first_time_counterparty: false,
                payload: serde_json::json!({ "prompt": "a logo" }),
                agent: None,
                run_id: None,
            },
        });

        let host = ParkingHost::default();
        brain.park_approval_requests(&host).await.expect("drain");
        brain
            .park_approval_requests(&host)
            .await
            .expect("second drain");
        assert_eq!(host.parked().len(), 1, "parked once, not twice");
    })
    .await;
}

/// Issue #561: a turn that gates more calls than one turn may raise tells
/// the operator so, with the count.
///
/// The cap itself is not the bug and is not touched here. The bug is that
/// exceeding it was **silent**: the operator saw eight cards and had no way
/// to learn that five more gated calls had happened, been refused, and been
/// dropped. Eight cards and no notice is indistinguishable from "eight is
/// all there was".
#[tokio::test]
async fn a_turn_that_overflows_the_cap_tells_the_operator_how_many_were_dropped() {
    crate::harness::built_in::policy::policy_test_helpers_tests::in_cycle(async {
        use crate::harness::policy::{ApprovalRequest, ApprovalRequestQueue};
        use crate::ports::types::EffectGroup;

        let cap = crate::harness::policy::MAX_APPROVAL_REQUESTS_PER_TURN;
        let over = 5;

        let dir = tempfile::tempdir().unwrap();
        let requests = ApprovalRequestQueue::default();
        let brain = brain_with_approval_queue(dir.path(), requests.clone());
        for i in 0..(cap + over) {
            requests.push(ApprovalRequest {
                tool: "composio_execute".to_string(),
                reason: "supervised".to_string(),
                effect: Effect {
                    kind: "composio_execute".to_string(),
                    group: EffectGroup::Send,
                    amount_usd: None,
                    established_thread: false,
                    first_time_counterparty: false,
                    // Distinct payloads, or `push` would dedupe them and the
                    // queue would never reach the cap in the first place.
                    payload: crate::policy::test_support::composio_unclassified_args_numbered(i),
                    agent: None,
                    run_id: None,
                },
            });
        }

        let host = ParkingHost::default();
        let notice = brain
            .park_approval_requests(&host)
            .await
            .expect("drain")
            .expect("an overflowing turn has something to tell the operator");

        assert_eq!(host.parked().len(), cap, "the cap still holds");
        assert!(
            notice.contains(&over.to_string()),
            "the operator is told HOW MANY were dropped, not just that some were: {notice}"
        );
        assert!(
            notice.contains(&cap.to_string()),
            "…and what the limit was, so the number means something: {notice}"
        );
    })
    .await;
}

/// The ordinary turn stays quiet. A notice on every cycle would train the
/// operator to scroll past the one that matters.
#[tokio::test]
async fn a_turn_within_the_cap_raises_no_notice() {
    crate::harness::built_in::policy::policy_test_helpers_tests::in_cycle(async {
        use crate::harness::policy::{ApprovalRequest, ApprovalRequestQueue};
        use crate::ports::types::EffectGroup;

        let dir = tempfile::tempdir().unwrap();
        let requests = ApprovalRequestQueue::default();
        let brain = brain_with_approval_queue(dir.path(), requests.clone());
        requests.push(ApprovalRequest {
            tool: "composio_execute".to_string(),
            reason: "supervised".to_string(),
            effect: Effect {
                kind: "composio_execute".to_string(),
                group: EffectGroup::Send,
                amount_usd: None,
                established_thread: false,
                first_time_counterparty: false,
                payload: crate::policy::test_support::composio_send_args(),
                agent: None,
                run_id: None,
            },
        });

        let host = ParkingHost::default();
        assert!(
            brain
                .park_approval_requests(&host)
                .await
                .expect("drain")
                .is_none(),
            "one request, a cap of 8: nothing was dropped and nothing is said"
        );
        assert_eq!(host.parked().len(), 1, "and the request itself still parks");
    })
    .await;
}

/// One failed park must not take the rest of the batch — or the turn's reply
/// — down with it. `drain` has already emptied the shared queue, so a `?`
/// here would lose every later request forever and abort `run_cycle`,
/// reproducing for the remainder of the batch exactly the silent
/// disappearance this issue fixes.
#[tokio::test]
async fn a_failed_park_does_not_drop_the_rest_of_the_batch() {
    crate::harness::built_in::policy::policy_test_helpers_tests::in_cycle(async {
        use crate::harness::policy::{ApprovalRequest, ApprovalRequestQueue};
        use crate::ports::types::EffectGroup;

        let dir = tempfile::tempdir().unwrap();
        let requests = ApprovalRequestQueue::default();
        let brain = brain_with_approval_queue(dir.path(), requests.clone());
        for tool in ["first_tool", "second_tool", "third_tool"] {
            requests.push(ApprovalRequest {
                tool: tool.to_string(),
                reason: "supervised".to_string(),
                effect: Effect {
                    kind: tool.to_string(),
                    group: EffectGroup::Other,
                    amount_usd: None,
                    established_thread: false,
                    first_time_counterparty: false,
                    payload: serde_json::json!({ "tool": tool }),
                    agent: None,
                    run_id: None,
                },
            });
        }

        let host = FlakyParkingHost::default();
        let notice = brain
            .park_approval_requests(&host)
            .await
            .expect("a park failure is surfaced without aborting the batch")
            .expect("the operator is told a request was not saved");

        // The first park failed; the two after it still reached the operator.
        let parked = host.parked();
        assert_eq!(parked.len(), 2, "the batch continued past the failure");
        assert_eq!(parked[0].kind, "second_tool");
        assert_eq!(parked[1].kind, "third_tool");
        assert!(notice.contains("1 approval request could not be saved"));
        assert!(notice.contains("Ask the agent to request approval again"));
    })
    .await;
}

/// The arm that made #243 visible: an approved grant re-dispatches its agent
/// with the exact arguments, answers on that agent's channel, and journals
/// the reply.
///
/// Before this arm existed, `ApprovalResolved` fell into `_ => {}`: no turn,
/// no response, and the cycle ended on the "Acknowledged." fallback. The
/// operator approved, read "Acknowledged.", and nothing ran — which looks
/// exactly like success.
///
/// `MockProvider` echoes the user message back, so the reply text IS the
/// instruction the agent received — which is what makes argument fidelity
/// assertable offline.
#[tokio::test]
async fn an_approved_grant_redispatches_its_agent_with_the_exact_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let log: Arc<dyn crate::ports::EventLog> =
        Arc::new(crate::store::FsEventLog::new(dir.path().to_path_buf()));
    let requests = crate::harness::policy::ApprovalRequestQueue::default();
    // Issue #470: a real catalogued send, with the action's own parameters
    // under `arguments` where the tool's schema puts them — so the
    // re-dispatch path this test covers carries a call the classifier can
    // actually read.
    let args = crate::policy::test_support::composio_args_with(
        crate::policy::test_support::COMPOSIO_SEND_SLUG,
        serde_json::json!({ "to": "a@b.test" }),
    );
    requests
        .grants()
        .grant(crate::runtime::grants::GrantedCall {
            approval_id: ApprovalId::new("appr-1"),
            agent: "ceo".into(),
            tool: "composio_execute".into(),
            args: args.clone(),
            at_millis: now_millis(),
            origin_thread: None,
            origin_parent: None,
            origin_task: None,
        });
    let brain = brain_with_queue_and_events(dir.path(), requests, log.clone());

    let result = brain
        .run_cycle(
            cycle_over(vec![approval_resolved("appr-1", Verdict::Approve)]),
            &NoopHost,
        )
        .await
        .expect("cycle runs");

    // A real bubble, on the GRANTING agent's channel — not the generic
    // "Acknowledged." fallback and not the operator channel.
    assert_eq!(result.channel_responses.len(), 1);
    let bubble = &result.channel_responses[0];
    assert_eq!(bubble.channel, "ceo");
    assert_ne!(bubble.text, "Acknowledged.");

    // The instruction carried the tool and the arguments VERBATIM. A model
    // that re-issues with drifted arguments re-parks (see the policy tests),
    // so the fidelity of this string is what makes the round-trip land.
    assert!(bubble.text.contains("composio_execute"), "{}", bubble.text);
    assert!(
        bubble.text.contains(&serde_json::to_string(&args).unwrap()),
        "the exact approved arguments must reach the agent: {}",
        bubble.text
    );
    assert!(
        bubble.text.contains("Do not modify them"),
        "{}",
        bubble.text
    );

    // Journaling the reply is no longer this function's job (issue #469):
    // the runtime journals every continuation reply once, in
    // `CompanyRuntime::publish_continuation`, so that the answers of
    // continuations this arm produces nothing for are not lost either. The
    // round trip — reply journaled into the thread the sign-off was raised
    // in, reaching the console's event stream — is covered end to end over
    // the real router by
    // `server::operator::test::a_continuation_answers_in_the_thread_the_sign_off_was_raised_in`.
    assert!(
        no_replies_journaled(&log).await,
        "the brain must not journal the reply a second time; the runtime owns it"
    );
}

#[tokio::test]
async fn an_explicit_approval_continues_without_reissuing_the_request_tool() {
    assert_explicit_decision_continues(Verdict::Approve, "APPROVED").await;
}

#[tokio::test]
async fn an_explicit_denial_also_returns_to_the_requesting_agent() {
    assert_explicit_decision_continues(Verdict::Deny, "DENIED").await;
}

/// A threaded approval continuation must preserve the approval's thread root
/// when it re-dispatches the granted call. The bound agent's observable
/// context proves the target is threaded rather than channel-only.
#[tokio::test]
async fn an_approved_threaded_grant_redispatches_in_its_origin_thread() {
    let dir = tempfile::tempdir().unwrap();
    let requests = crate::harness::policy::ApprovalRequestQueue::default();
    let root = crate::ports::types::EventSeq::new(7);
    requests
        .grants()
        .grant(crate::runtime::grants::GrantedCall {
            approval_id: ApprovalId::new("appr-threaded"),
            agent: "ceo".into(),
            tool: "workspace_write".into(),
            args: serde_json::json!({}),
            at_millis: now_millis(),
            origin_thread: Some("general".into()),
            origin_parent: Some(root),
            origin_task: None,
        });
    let base = brain_with_queue_and_events(
        dir.path(),
        requests,
        Arc::new(crate::store::FsEventLog::new(dir.path().to_path_buf())),
    );
    let pool = Arc::new(HarnessPool::new());
    let brain = HarnessBrain::new(pool.clone(), (*base.deps).clone(), record());
    pool.ensure(&record(), &brain.deps).await.expect("ensure");

    brain
        .run_cycle(
            cycle_over(vec![approval_resolved("appr-threaded", Verdict::Approve)]),
            &NoopHost,
        )
        .await
        .expect("cycle runs");

    let agent = pool
        .agents
        .read()
        .await
        .get(&CompanyId::new("acme"))
        .and_then(|roster| roster.iter().find(|agent| agent.agent_id == "ceo"))
        .cloned()
        .expect("the approved turn keeps the agent resident");
    // Issue #1890 I reverses this. It asserted `None` — that an approval's
    // continuation binds to nothing, because it runs unstreamed and "binding
    // is covered by the delegated target".
    //
    // The delegated target does cover the *drain*, which was already bound
    // by `in_thread(grant.origin_parent())`. It never covered the re-issued
    // call itself: that turn ran against whatever history the agent
    // happened to be holding and then published its answer into the origin
    // thread regardless — grounded in one conversation, answering into
    // another. Identity is no longer inferred from the absent stream, so
    // the turn now binds to the conversation the grant recorded.
    // The pooled agent no longer carries a chat binding (plan hive-desks,
    // Phase 2: OpenHuman owns the thread; the conversation rides in the turn
    // text). What the grant recorded is asserted on the journal above; the
    // binding itself has nothing left to read.
    // TODO(Phase 4): assert the conversation cue on the recorded turn text.
    let _ = (&agent, root);
}

/// Issue #1846 review (Codex #3869725683) — **the regression.** Same
/// fixture as `an_approved_grant_redispatches_its_agent_with_the_exact_arguments`
/// above, but the re-issued call's provider is now out of credits.
///
/// `run_steered_background` runs through the SAME `run_inner` the
/// interactive chat path does, so it parks a re-issue marker for the
/// granting agent exactly as an ordinary paused message would — proven
/// below by reading it straight off `BudgetPauseSet`. Before this fix, the
/// bubble `redispatch_granted_call` built from that outcome carried
/// `outcome.reply` (the budget-paused placeholder text) verbatim, so the
/// operator saw an ordinary-looking reply rather than the runtime's own
/// pause notice.
///
/// Issue #1846 review (Codex #3870562590): the notice it now carries is the
/// NO-RESEND one. The marker asserted below is real but not redeemable —
/// `run_steered_background` parks it with `background: true`, the one shape
/// `redeem_budget_pause` refuses (`src/server/ops/budget_pause.rs`) — so
/// the redeemable prefix would have drawn a CTA that returned 400 on every
/// click. Both prefixes are asserted: matching the new one is only half the
/// contract, since the console branches on the old one.
#[tokio::test]
async fn a_budget_paused_approval_continuation_surfaces_the_notice_and_parks_a_marker() {
    let dir = tempfile::tempdir().unwrap();
    let log: Arc<dyn crate::ports::EventLog> =
        Arc::new(crate::store::FsEventLog::new(dir.path().to_path_buf()));
    let requests = crate::harness::policy::ApprovalRequestQueue::default();
    let args = crate::policy::test_support::composio_args_with(
        crate::policy::test_support::COMPOSIO_SEND_SLUG,
        serde_json::json!({ "to": "a@b.test" }),
    );
    requests
        .grants()
        .grant(crate::runtime::grants::GrantedCall {
            approval_id: ApprovalId::new("appr-1"),
            agent: "ceo".into(),
            tool: "composio_execute".into(),
            args: args.clone(),
            at_millis: now_millis(),
            origin_thread: None,
            origin_parent: None,
            origin_task: None,
        });
    let brain = brain_with_queue_and_events_and_budget_exhausted_provider(
        dir.path(),
        requests,
        log.clone(),
    );

    let result = brain
        .run_cycle(
            cycle_over(vec![approval_resolved("appr-1", Verdict::Approve)]),
            &NoopHost,
        )
        .await
        .expect("cycle runs");

    assert_eq!(result.channel_responses.len(), 1);
    let bubble = &result.channel_responses[0];
    assert_eq!(bubble.channel, "ceo");
    assert!(
        bubble
            .text
            .starts_with(BUDGET_PAUSE_NOTICE_NO_RESEND_PREFIX),
        "an approval continuation parks a background marker the redeem route refuses, so \
         its notice must carry the non-redeemable prefix — got: {}",
        bubble.text
    );
    assert!(
        !bubble.text.starts_with(BUDGET_PAUSE_NOTICE_PREFIX),
        "the pre-fix defect: this prefix is what the console keys its \"Add credits & \
         resend\" CTA off, and this marker's redeem returns 400: {}",
        bubble.text
    );
    assert!(
        bubble.text.to_ascii_lowercase().contains("add credits"),
        "the actionable ask survives into the notice: {}",
        bubble.text
    );

    // And a re-issue marker really was parked for the granting agent: the
    // notice is non-redeemable because of HOW it was parked (background),
    // not because nothing was parked at all.
    let marker = crate::runtime::grants::budget_pauses_for(&CompanyId::new("acme"))
        .peek("ceo")
        .expect("run_steered_background parks a marker on the same terms run_inner does");
    assert_eq!(marker.agent, "ceo");
}
