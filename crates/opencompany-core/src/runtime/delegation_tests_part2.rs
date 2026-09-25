use super::tests_core2::*;

/// "By construction" means the card is on the board **while** the delegate
/// works, not reconstructed once they are done. Proven by reading the board
/// from inside the delegate's own turn: it is already there, already theirs,
/// already In progress.
///
/// This is the assertion that distinguishes the fix from a cosmetic one —
/// a card written only after the answer came back would satisfy every
/// count-based test above and still leave the work invisible for the whole
/// time it was actually happening.
#[tokio::test]
async fn the_hand_off_card_is_on_the_board_while_the_delegate_works() {
    let fx = Fixture::new();
    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("done")]);
    let outcome = fx
        .runner(&turns)
        .run_delegation(
            handoff("draft the launch plan"),
            None,
            MessageContext::default(),
        )
        .await
        .expect("delegation runs");
    assert!(outcome.spawned_task.is_some());
    assert_eq!(turns.calls().len(), 1, "the delegate ran exactly once");
    assert_eq!(
        turns.board_at_turn(0),
        vec![("engineer".to_string(), COLUMN_IN_PROGRESS.to_string())],
        "the card is open, assigned and in progress before the delegate starts"
    );
    // …and settles for a person once they are done.
    let cards = fx.cards().await;
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].column, COLUMN_IN_REVIEW);
}

/// A hand-off an operator cancels mid-flight keeps its card and returns it
/// to To-do. The alternative — no card — would erase the fact that the work
/// was ever asked for.
#[tokio::test]
async fn a_cancelled_hand_off_returns_its_card_to_todo() {
    let fx = Fixture::new();
    let turns = ScriptedTurns::new(&fx, vec![Turn::cancelled("half-written")]);
    let outcome = fx
        .runner(&turns)
        .run_delegation(
            handoff("write the migration plan"),
            None,
            MessageContext::default(),
        )
        .await
        .expect("delegation runs");
    assert!(outcome.cancelled);
    let cards = fx.cards().await;
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].column, COLUMN_TODO);
}

/// The constraint that keeps the fix from becoming its own bug, on the
/// hand-off path: relaying a question to a desk is not commissioning work.
#[tokio::test]
async fn a_question_relayed_to_a_desk_mints_no_card() {
    let fx = Fixture::new();
    let turns = ScriptedTurns::new(
        &fx,
        vec![
            Turn::queueing("asking", vec![handoff("what's the status of the build?")]),
            Turn::reply("engineering says it's green"),
            Turn::reply("it's green"),
        ],
    );
    let turn = fx
        .runner(&turns)
        .handle_operator_message("chief", "is the build ok?", None)
        .await
        .expect("operator message handled");
    assert!(fx.cards().await.is_empty(), "a question is not work");
    assert!(turn.spawned_task.is_none());
}

/// A hand-off made from inside a **dispatched card** must not open a second
/// one — that card already is the tracking, and #204 hands it to the
/// delegate.
#[tokio::test]
async fn a_hand_off_inside_a_dispatched_card_opens_no_second_card() {
    let fx = Fixture::new();
    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("done")]);
    let outcome = fx
        .runner(&turns)
        .for_task("card-1")
        .run_delegation(
            handoff("write the migration plan"),
            None,
            MessageContext::default(),
        )
        .await
        .expect("delegation runs");
    assert!(outcome.spawned_task.is_none());
    assert!(fx.cards().await.is_empty());
}

// ── path one: a desk asked directly ─────────────────────────────────────

/// Asking a desk lead directly opens **no** card by itself. Issue #442 carded
/// anything "substantial" said to one here — before the turn, because a
/// non-orchestrator carried no tool that could — and every desk message
/// became a work item nobody had asked for. The board is a tool call now:
/// the message below is exactly what the old detector tracked, and nothing
/// is opened for it, linked to it, or on the board while the desk works.
#[tokio::test]
async fn a_desk_asked_directly_opens_no_card_by_itself() {
    let request = "read the pricing repo and write modules.md";
    assert!(
        is_trackable_work(request),
        "fixture must be a message the old detector carded, or this proves nothing"
    );
    let fx = Fixture::new();
    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("modules.md is written")]);
    let turn = fx
        .runner(&turns)
        .handle_operator_message("engineer", request, Some("eng_desk"))
        .await
        .expect("operator message handled");

    assert!(
        turns.board_at_turn(0).is_empty(),
        "nothing is on the board while the desk works"
    );
    assert!(fx.cards().await.is_empty(), "and nothing after");
    assert_eq!(turn.spawned_task, None, "and the reply links to no card");
}

/// The desk's **own** `spawn_task` is how a direct ask gets tracked: the
/// card it opens is this turn's card, assigned as the agent said, raised in
/// the conversation the ask came from, and reported on the operator bubble.
#[tokio::test]
async fn a_desk_asked_directly_tracks_the_ask_with_its_own_spawn_task() {
    let fx = Fixture::new();
    let turns = ScriptedTurns::new(
        &fx,
        vec![Turn::queueing(
            "on it — tracked",
            vec![Delegation::SpawnTask {
                title: "Write modules.md".to_string(),
                note: Some("read the pricing repo first".to_string()),
                assignee: Some("engineer".to_string()),
            }],
        )],
    );
    let turn = fx
        .runner(&turns)
        .in_thread(Some(EventSeq::new(41)))
        .handle_operator_message(
            "engineer",
            "read the pricing repo and write modules.md",
            Some("eng_desk"),
        )
        .await
        .expect("operator message handled");

    let cards = fx.cards().await;
    assert_eq!(cards.len(), 1, "{cards:?}");
    assert_eq!(cards[0].assignee, "engineer");
    assert_eq!(cards[0].title, "Write modules.md");
    assert_eq!(cards[0].origin_chat_id(), Some("eng_desk"));
    assert_eq!(
        cards[0].origin_parent(),
        Some(EventSeq::new(41)),
        "raised in the thread the ask came from"
    );
    assert_eq!(turn.spawned_task.as_deref(), Some(cards[0].id.as_str()));
}

/// The same, for the card a `spawn_task` queues rather than the one a
/// hand-off opens. Two card-raising sites, one rule — and they are far
/// enough apart in this file that only a test keeps them agreeing.
#[tokio::test]
async fn a_spawned_card_records_the_thread_that_queued_it() {
    let fx = Fixture::new();
    let turns = ScriptedTurns::new(
        &fx,
        vec![Turn::queueing(
            "opening a card",
            vec![Delegation::SpawnTask {
                title: "write the migration plan".to_string(),
                note: None,
                assignee: Some("engineer".to_string()),
            }],
        )],
    );
    fx.runner(&turns)
        .in_thread(Some(EventSeq::new(41)))
        .handle_operator_message("chief", "open a card for the migration", Some("general"))
        .await
        .expect("operator message handled");

    let cards = fx.cards().await;
    assert_eq!(cards.len(), 1, "{cards:?}");
    assert_eq!(cards[0].origin_chat_id(), Some("general"));
    assert_eq!(cards[0].origin_parent(), Some(EventSeq::new(41)));
}

/// **Issue #984, the reported probe.** The message that opened a card on
/// staging, run through the path that opened it.
///
/// `"verifying the Send button responds to a real mouse click. No action
/// needed from anyone."` is 15 words, so it clears
/// [`SMALLTALK_MAX_WORDS`]; it names no [`WORK_VERBS`] entry (`verifying`
/// and `send` are both deliberately absent — `send` is a noun here); and it
/// is not interrogative. So [`is_trackable_work`] falls through to its
/// "anything else is work" rung and returns true, which is how a message
/// that explicitly disclaimed any action became a card assigned to a desk.
///
/// The lexical layer cannot fix this without inverting its own default, so
/// the model is asked — and having been asked, its answer is now used.
#[tokio::test]
async fn a_desk_asked_something_the_model_calls_chatter_opens_no_card() {
    let probe = "verifying the Send button responds to a real mouse click. \
                 No action needed from anyone.";
    assert!(
        crate::company::task_intent::triage_message_detailed(probe).abstained(),
        "fixture must be a message no lexical rule decides"
    );
    assert!(
        is_trackable_work(probe),
        "fixture must be one the card detector would otherwise track — that \
         is the bug this closes"
    );

    let fx = Fixture::new();
    let escalation = ScriptedTriage::new(crate::harness::triage::TriageVerdict::Chatter);
    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("ack")]);
    let turn = fx
        .runner(&turns)
        .with_triage(&escalation)
        .handle_operator_message("engineer", probe, Some("eng_desk"))
        .await
        .expect("operator message handled");

    assert_eq!(
        escalation.asked(),
        vec![probe.to_string()],
        "the abstention is what gets escalated"
    );
    assert!(
        fx.cards().await.is_empty(),
        "a message the model read as conversation opens no card"
    );
    assert_eq!(turn.spawned_task, None, "and nothing is linked to one");
}

/// One message, one card — including the road #463 could not see (issue #1035).
///
/// The REST chat handler opens a card on **two** signals: the triage naming
/// a title, and the operator's composer asking for a workflow, which it
/// takes as an override and supplies a title for when the triage declined
/// to. The runtime re-derived "did the handler card this?" from the triage
/// alone, which is true for the first road and false for the second — so a
/// workflow request whose wording no lexical rule recognises arrived here
/// looking uncarded and got a second card beside the one it already had.
///
/// The fixture is the same residue `a_non_chatter_verdict_still_opens_the_direct_card`
/// uses, and that is the point: with no deliverable it cards, so a run that
/// opens nothing here is the flag doing the work rather than the message
/// being unremarkable.
#[tokio::test]
async fn a_workflow_the_handler_already_carded_opens_no_second_card() {
    let residue = "the pricing page copy, before Friday if you can";
    assert!(
        crate::company::task_intent::triage_message_detailed(residue)
            .triage
            .title()
            .is_none(),
        "fixture must be a message the triage does NOT name — that is the \
         road the handler took its override on"
    );

    let fx = Fixture::new();
    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("on it")]);
    let turn = fx
        .runner(&turns)
        .requested(Some(crate::ports::types::MessageIntent::Workflow))
        .handle_operator_message("engineer", residue, Some("eng_desk"))
        .await
        .expect("operator message handled");

    assert!(
        fx.cards().await.is_empty(),
        "the handler carded this message on the operator's request; the \
         runtime must not open a second one"
    );
    assert_eq!(turn.spawned_task, None, "and nothing is linked to one");
}

/// **Issue #465, the reported card**, on the path that still opens one: a
/// hand-off whose delegate ran clean lands in In Review, and one whose
/// delegate's first tool call parked for approval does not — it produced
/// nothing to review, so its card parks where the operator can see it is
/// blocked and the console offers the Resume.
///
/// Paired so a fix that simply stopped writing In Review would fail here.
#[tokio::test]
async fn a_hand_off_that_finished_cleanly_lands_in_review() {
    crate::harness::built_in::policy::policy_test_helpers_tests::in_cycle(async {
        let fx = Fixture::new();
        let turns = ScriptedTurns::new(
            &fx,
            vec![
                Turn::queueing("on it", vec![handoff("read the pricing repo")]),
                Turn::reply("modules.md is written"),
                Turn::reply("relayed"),
            ],
        );
        fx.runner(&turns)
            .handle_operator_message("chief", "map out the pricing repo", Some("general"))
            .await
            .expect("operator message handled");

        let cards = fx.cards().await;
        assert_eq!(cards.len(), 1, "{cards:?}");
        assert_eq!(cards[0].column, COLUMN_IN_REVIEW, "{cards:?}");
        assert_eq!(fx.approvals.queued(), 0, "nothing was parked");
    })
    .await;
}

/// An approval left over from an *earlier* turn must not park this card.
/// The count is differenced across the turn precisely so a queue the cycle
/// was already holding cannot be misread as something this turn did.
#[tokio::test]
async fn an_approval_parked_before_this_turn_does_not_park_its_card() {
    crate::harness::built_in::policy::policy_test_helpers_tests::in_cycle(async {
        let fx = Fixture::new();
        // Something a previous turn parked and nobody has resolved yet.
        fx.approvals.push(crate::harness::policy::ApprovalRequest {
            tool: "send_email".to_string(),
            reason: "supervised".to_string(),
            effect: crate::ports::types::Effect {
                kind: "send_email".to_string(),
                group: crate::ports::types::EffectGroup::Other,
                amount_usd: None,
                established_thread: false,
                first_time_counterparty: false,
                payload: serde_json::json!({}),
                agent: Some("someone_else".to_string()),
                run_id: None,
            },
        });

        let turns = ScriptedTurns::new(
            &fx,
            vec![
                Turn::queueing("on it", vec![handoff("read the pricing repo")]),
                Turn::reply("modules.md is written"),
                Turn::reply("relayed"),
            ],
        );
        fx.runner(&turns)
            .handle_operator_message("chief", "map out the pricing repo", Some("general"))
            .await
            .expect("operator message handled");

        let cards = fx.cards().await;
        assert_eq!(
            cards[0].column, COLUMN_IN_REVIEW,
            "this turn parked nothing of its own: {cards:?}"
        );
    })
    .await;
}

/// A desk **hand-off** whose turn parks: the delegate stopped at an
/// unauthorised call, so its card is blocked rather than reviewable.
#[tokio::test]
async fn a_hand_off_whose_turn_parks_also_leaves_its_card_blocked() {
    crate::harness::built_in::policy::policy_test_helpers_tests::in_cycle(async {
        let fx = Fixture::new();
        let turns = ScriptedTurns::new(
            &fx,
            vec![
                Turn::queueing("on it", vec![handoff("read the pricing repo")]),
                Turn::parked("I need approval before I can read the repo", "fs_read"),
                Turn::reply("relayed"),
            ],
        );
        fx.runner(&turns)
            .handle_operator_message("chief", "map out the pricing repo", Some("general"))
            .await
            .expect("operator message handled");

        let cards = fx.cards().await;
        assert_eq!(cards.len(), 1, "{cards:?}");
        assert_eq!(
            cards[0].column, COLUMN_PAUSED,
            "the delegate parked its first call: {cards:?}"
        );
    })
    .await;
}

/// One message, one card. When the REST chat handler has opened a card for
/// this message (it does so for the composer's explicit workflow request),
/// this path adopts it rather than opening another — and, since a direct
/// ask opens nothing here anyway, the turn simply links to the handler's.
#[tokio::test]
async fn a_message_the_chat_handler_already_carded_opens_no_second_card() {
    let fx = Fixture::new();
    let mut handler = handler_card_in("Draft the launch plan".to_string(), COLUMN_TODO);
    handler.id = "handler-card".to_string();
    TaskStore::upsert(&*fx.tasks, &fx.record.id, &handler)
        .await
        .expect("seed the handler card");
    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("planned")]);
    let turn = fx
        .runner(&turns)
        .answering(Some(handler_seq()))
        .requested(Some(crate::ports::types::MessageIntent::Workflow))
        .handle_operator_message(
            "engineer",
            "draft the launch plan for next quarter",
            Some("eng_desk"),
        )
        .await
        .expect("operator message handled");
    assert_eq!(
        fx.cards().await.len(),
        1,
        "the chat handler's card is the card; this path opens none"
    );
    assert_eq!(turn.spawned_task.as_deref(), Some("handler-card"));
}
