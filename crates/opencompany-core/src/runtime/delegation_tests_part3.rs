use super::tests_core::*;
use super::tests_core2::*;
use super::*;

/// The whole point: the model is asked only about the residue. A message the
/// lexical layer classified costs nothing and waits for nothing.
#[tokio::test]
async fn a_message_the_cheap_layer_named_is_never_escalated() {
    let fx = Fixture::new();
    let escalation = ScriptedTriage::new(crate::harness::triage::TriageVerdict::Answer);
    for named in [
        "what is on the board?",
        "draft the launch plan for next quarter",
        "hi",
    ] {
        assert!(
            !crate::company::task_intent::triage_message_detailed(named).abstained(),
            "fixture must be a message a rule decides: {named:?}"
        );
        let turns = ScriptedTurns::new(&fx, vec![Turn::reply("ok")]);
        fx.runner(&turns)
            .with_triage(&escalation)
            .handle_operator_message("chief", named, Some("general"))
            .await
            .expect("operator message handled");
    }
    assert!(
        escalation.asked().is_empty(),
        "escalating a message the cheap layer already named is the cost this \
         design exists to avoid: {:?}",
        escalation.asked()
    );
}

/// An abstention IS escalated, and a verdict of `answer` narrows the claim —
/// the same narrowing a lexical `Answer` produces, reached by a second
/// opinion instead of a rule.
#[tokio::test]
async fn an_abstention_the_model_reads_as_a_question_narrows_the_claim() {
    let residue = "the deck looks good to me";
    assert!(
        crate::company::task_intent::triage_message_detailed(residue).abstained(),
        "fixture must be a message no rule decides"
    );
    let fx = Fixture::new();
    let escalation = ScriptedTriage::new(crate::harness::triage::TriageVerdict::Answer);
    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("noted")]);
    fx.runner(&turns)
        .with_triage(&escalation)
        .handle_operator_message("chief", residue, Some("general"))
        .await
        .expect("operator message handled");

    assert_eq!(
        escalation.asked(),
        vec![residue.to_string()],
        "the residue is exactly what the model should have been asked"
    );
    assert_eq!(
        turns.claim_at_turn(0),
        orchestrator::DrainClaim::Answering,
        "an `answer` verdict narrows the claim, so the model's pure board \
         writes are refused in its own turn"
    );
}

/// `Work` and `Chatter` leave the gate exactly where the abstention left it.
/// A verdict may narrow the claim; it may never widen what a turn can do,
/// and it never mints a card — the #463 title contract forbids a
/// model-authored one.
#[tokio::test]
async fn a_non_answer_verdict_changes_nothing() {
    let residue = "the deck looks good to me";
    for verdict in [
        crate::harness::triage::TriageVerdict::Work,
        crate::harness::triage::TriageVerdict::Chatter,
        crate::harness::triage::TriageVerdict::Unavailable,
    ] {
        let fx = Fixture::new();
        let escalation = ScriptedTriage::new(verdict);
        let turns = ScriptedTurns::new(&fx, vec![Turn::reply("noted")]);
        fx.runner(&turns)
            .with_triage(&escalation)
            .handle_operator_message("chief", residue, Some("general"))
            .await
            .expect("operator message handled");
        assert_eq!(
            turns.claim_at_turn(0),
            orchestrator::DrainClaim::Full,
            "{verdict:?} must leave the ungated claim the abstention had"
        );
        assert!(
            fx.cards().await.is_empty(),
            "{verdict:?} must not mint a card"
        );
    }
}

/// No evaluator wired is the pre-#678 world, and it has to stay reachable:
/// a build without one must behave exactly as it did.
#[tokio::test]
async fn without_an_evaluator_an_abstention_keeps_the_deterministic_answer() {
    let residue = "the deck looks good to me";
    let fx = Fixture::new();
    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("noted")]);
    fx.runner(&turns)
        .handle_operator_message("chief", residue, Some("general"))
        .await
        .expect("operator message handled");
    assert_eq!(
        turns.claim_at_turn(0),
        orchestrator::DrainClaim::Full,
        "an abstention with nobody to ask stays ungated"
    );
}

/// The defect, at the seam that still opens cards by construction: the card
/// a hand-off opens is named after the **work**, not after the instruction.
///
/// The assertion that matters is the negative one. A card titled
/// `hey can you take a look at the pricing page, I think the tiers are…` is
/// a prefix of the request wearing an ellipsis, and that is what a board of
/// them read as — a chat log. Asserting only the expected string would still
/// pass if the title were an excerpt that happened to match.
#[tokio::test]
async fn a_hand_off_card_is_named_after_the_work_not_the_instruction() {
    let rambling = "hey can you take a look at the pricing page, I think the tiers are \
                    confusing and we should probably reword the middle one";
    let fx = Fixture::new();
    let titler = ScriptedTitler::new("Reword the middle pricing tier");
    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("on it")]);

    fx.runner(&turns)
        .with_titler(&titler)
        .run_delegation(handoff(rambling), None, MessageContext::default())
        .await
        .expect("delegation runs");

    let cards = fx.cards().await;
    assert_eq!(cards.len(), 1, "one hand-off, one card: {cards:?}");
    assert_eq!(cards[0].title, "Reword the middle pricing tier");
    assert!(
        !rambling.starts_with(cards[0].title.trim_end_matches('…')),
        "the headline is still an excerpt of the request: {}",
        cards[0].title
    );
    // The full instruction is not lost — it moved to where the detail belongs.
    assert!(
        cards[0]
            .note
            .as_deref()
            .is_some_and(|note| note.contains("the tiers are confusing")),
        "the instruction must survive on the card: {:?}",
        cards[0].note
    );
    assert_eq!(titler.asked(), vec![rambling.to_string()]);
}

/// No titler wired — an offline company, a default build — still opens the
/// hand-off card, named the way every card was named before.
#[tokio::test]
async fn without_a_titler_a_hand_off_card_is_still_opened_and_still_named() {
    let request = "hey can you take a look at the pricing page, I think the tiers are \
                   confusing and we should probably reword the middle one";
    let fx = Fixture::new();
    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("on it")]);

    fx.runner(&turns)
        .run_delegation(handoff(request), None, MessageContext::default())
        .await
        .expect("delegation runs");

    let cards = fx.cards().await;
    assert_eq!(cards.len(), 1, "one hand-off, one card: {cards:?}");
    assert_eq!(
        cards[0].title,
        crate::ports::tasks::TaskTitle::truncated(request)
    );
    assert!(!cards[0].title.is_empty());
}

/// The coupling this change exists to break: the handler's card is adopted
/// even when its headline bears **no relation** to the message.
///
/// Adoption used to re-derive the title lexically and match it byte-for-byte,
/// so this card — named the way a titling pass names one — was invisible to
/// it. That is the whole reason a model-authored title could not ship: the
/// failure is silent, and it costs the "Card opened" chip and the workflow
/// settle rather than an error.
#[tokio::test]
async fn a_handler_card_is_adopted_by_its_message_not_by_its_title() {
    let imperative = "draft the launch plan for next quarter";
    let fx = Fixture::new();
    let mut handler = handler_card_in("Reword the middle pricing tier".to_string(), COLUMN_TODO);
    handler.id = "handler-card".to_string();
    TaskStore::upsert(&*fx.tasks, &fx.record.id, &handler)
        .await
        .expect("seed the handler card");

    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("on it")]);
    let turn = fx
        .runner(&turns)
        .answering(Some(handler_seq()))
        .handle_operator_message("chief", imperative, None)
        .await
        .expect("operator message handled");

    let cards = fx.cards().await;
    assert_eq!(cards.len(), 1, "one message, one card: {cards:?}");
    assert_eq!(
        turn.spawned_task.as_deref(),
        Some("handler-card"),
        "a card whose title is a NAME is still this message's card"
    );
}

/// Two alike-reading messages in one thread are two cards, and a turn
/// adopts its **own** — even when that is not the newest one on the board.
///
/// Title equality could not tell them apart: both cards carry the headline
/// the old key derived from the message, so the matcher had two equally good
/// candidates and took the first the store returned. `list` is newest-first,
/// so it took the *later* card — and a person who asks twice in one thread
/// then watches their first ask settle the second ask's card.
///
/// The fixture puts the right answer in the older card deliberately. With
/// both cards equally titled and the newer one wrong, only an identity that
/// names the message can pick correctly.
#[tokio::test]
async fn a_turn_adopts_its_own_card_not_the_newest_alike_one() {
    let imperative = "draft the launch plan for next quarter";
    let derived = crate::company::task_intent::detect_task_intent(imperative)
        .expect("fixture must be a message the chat handler cards");
    let fx = Fixture::new();
    // The card this turn's message opened — and the OLDER of the two.
    for (id, seq, updated) in [
        ("card-mine", 41u64, 1_000u64),
        ("card-later", 77u64, 2_000u64),
    ] {
        let mut card = handler_card_in(derived.clone(), COLUMN_TODO);
        card.id = id.to_string();
        card.origin_message_seq = Some(EventSeq::new(seq));
        card.updated_at_millis = updated;
        TaskStore::upsert(&*fx.tasks, &fx.record.id, &card)
            .await
            .expect("seed a handler card");
    }

    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("on it")]);
    let turn = fx
        .runner(&turns)
        .answering(Some(EventSeq::new(41)))
        .handle_operator_message("chief", imperative, None)
        .await
        .expect("operator message handled");

    assert_eq!(
        turn.spawned_task.as_deref(),
        Some("card-mine"),
        "the turn must adopt the card opened for ITS message, not the newest alike one"
    );
}

/// The bug this slice fixes. The orchestrator answers "create a workflow
/// named X" by authoring the graph in its own turn; the handler's card was
/// adopted (the bubble links to it) and then left in To-do forever, because
/// the only `WorkflowRefQueue` drain lived on the dispatched-card path.
#[tokio::test]
async fn a_workflow_authored_in_a_chat_turn_settles_the_card_it_adopted() {
    let imperative = "create a workflow named nightly digest";
    let title = crate::company::task_intent::detect_task_intent(imperative)
        .expect("fixture must be a message the chat handler cards");
    let fx = Fixture::new();
    // The card the REST handler already opened for this message (#463).
    let handler = handler_card_in(title, COLUMN_TODO);
    TaskStore::upsert(&*fx.tasks, &fx.record.id, &handler)
        .await
        .expect("seed the handler card");
    let turns = ScriptedTurns::new(
        &fx,
        vec![Turn::authoring(
            "authored it",
            vec![authored("nightly-digest")],
        )],
    );
    fx.runner(&turns)
        .answering(Some(handler_seq()))
        .handle_operator_message("chief", imperative, Some("general"))
        .await
        .expect("operator message handled");

    let cards = fx.cards().await;
    assert_eq!(cards.len(), 1, "one message, one card");
    // The exact landing, not merely "moved". Issue #576 gave the handler a
    // second landing column, so `!= todo` would pass without this fix on
    // every card that started in Planning — the commonest of the two.
    assert_eq!(
        cards[0].column,
        lifecycle::settled_landing_column(TaskRunEnd::Completed, 0),
        "a completed turn's card lands in the success terminal"
    );
    let note = cards[0].note.clone().unwrap_or_default();
    assert!(
        note.contains("nightly-digest"),
        "the note is the prose record of what was authored: {note}"
    );
    assert_eq!(
        fx.workflow_refs.queued(),
        0,
        "the drain empties the queue, or the next turn inherits this turn's workflows"
    );
}

/// The other side of the stamp: a turn with **no** chat thread to address
/// gets the note and no output link. There is no conversation to point at,
/// and a stamp pointing nowhere is worse than none — the same reason
/// `primaryLink` falls back to the card rather than synthesising a target.
#[tokio::test]
async fn a_turn_with_no_chat_thread_settles_without_an_output_link() {
    let imperative = "create a workflow named nightly digest";
    let title = crate::company::task_intent::detect_task_intent(imperative)
        .expect("fixture must be a message the chat handler cards");
    let fx = Fixture::new();
    let handler = handler_card_in(title, COLUMN_TODO);
    TaskStore::upsert(&*fx.tasks, &fx.record.id, &handler)
        .await
        .expect("seed the handler card");
    let turns = ScriptedTurns::new(
        &fx,
        vec![Turn::authoring(
            "authored it",
            vec![authored("nightly-digest")],
        )],
    );
    fx.runner(&turns)
        .answering(Some(handler_seq()))
        .handle_operator_message("chief", imperative, None)
        .await
        .expect("operator message handled");

    let cards = fx.cards().await;
    assert_eq!(
        cards[0].column,
        lifecycle::settled_landing_column(TaskRunEnd::Completed, 0),
        "the settle itself does not depend on having a thread"
    );
    assert!(
        cards[0]
            .note
            .clone()
            .unwrap_or_default()
            .contains("nightly-digest"),
        "the note still records what was authored"
    );
    assert!(
        cards[0].output.is_none(),
        "no thread to address, so no link is written"
    );
}

/// Issue #806: the settled card carries a real **output link**, not just a
/// note. `TaskOutput` used to require a `run_id` and an operator chat turn
/// has no run row, so this card could carry no output at all — the board's
/// contract (#339, *"Done carries a link to what it produced"*) is written
/// in terms of links, and prose is not one.
///
/// The source is the conversation. Asserting `run_id()` is `None` is half
/// the point: minting a run for a turn that attempted no work would make the
/// Attempts tab lie, which #183 §4 settled deliberately.
#[tokio::test]
async fn a_workflow_authored_in_a_chat_turn_gives_its_card_an_output_link() {
    let imperative = "create a workflow named nightly digest";
    let title = crate::company::task_intent::detect_task_intent(imperative)
        .expect("fixture must be a message the chat handler cards");
    let fx = Fixture::new();
    // NOTE: no `origin_chat_id` on the seeded card. Since issue #982 one
    // naming THIS turn's thread would be adopted too, but a card carrying a
    // different thread is still unadoptable and nothing would settle at all.
    // The conversation the stamp addresses is the TURN's, passed to
    // `handle_operator_message` below.
    let handler = handler_card_in(title, COLUMN_TODO);
    TaskStore::upsert(&*fx.tasks, &fx.record.id, &handler)
        .await
        .expect("seed the handler card");
    let turns = ScriptedTurns::new(
        &fx,
        vec![Turn::authoring(
            "authored it",
            vec![authored("nightly-digest")],
        )],
    );
    fx.runner(&turns)
        .answering(Some(handler_seq()))
        .handle_operator_message("chief", imperative, Some("general"))
        .await
        .expect("operator message handled");

    let cards = fx.cards().await;
    let output = cards[0]
        .output
        .clone()
        .expect("a settled chat turn stamps an output link");
    assert_eq!(
        output.source,
        TaskOutputSource::ChatTurn {
            chat_id: "general".to_string()
        },
        "the producer is the conversation this turn happened in"
    );
    assert_eq!(
        output.source.run_id(),
        None,
        "an operator chat turn attempted no work, so it mints no run"
    );
    assert_eq!(
        output
            .workflows
            .iter()
            .map(|w| w.workflow_id.as_str())
            .collect::<Vec<_>>(),
        vec!["nightly-digest"],
        "the link points at what the turn actually produced"
    );
    assert!(
        output.artifacts.is_empty(),
        "this turn published no file — the workflow is the deliverable"
    );
}

/// The same settle when the handler's card landed in **Planning** rather
/// than To-do (issue #576).
///
/// This is the commonest of the two: a signed-in person's prompt-box card is
/// created directly in Planning, and only a machine's lands in To-do. A test
/// that asserted merely "no longer in To-do" would pass here without the fix
/// at all, which is why the assertion names the settled landing column.
#[tokio::test]
async fn a_workflow_authored_for_a_card_that_landed_in_planning_settles_it_too() {
    let imperative = "create a workflow named nightly digest";
    let title = crate::company::task_intent::detect_task_intent(imperative)
        .expect("fixture must be a message the chat handler cards");
    let fx = Fixture::new();
    let handler = handler_card_in(title, COLUMN_PLANNING);
    TaskStore::upsert(&*fx.tasks, &fx.record.id, &handler)
        .await
        .expect("seed the handler card");

    let turns = ScriptedTurns::new(
        &fx,
        vec![Turn::authoring(
            "authored it",
            vec![authored("nightly-digest")],
        )],
    );
    fx.runner(&turns)
        .answering(Some(handler_seq()))
        .handle_operator_message("chief", imperative, Some("general"))
        .await
        .expect("operator message handled");

    let cards = fx.cards().await;
    assert_eq!(cards.len(), 1, "one message, one card");
    assert_eq!(
        cards[0].column,
        lifecycle::settled_landing_column(TaskRunEnd::Completed, 0),
        "a Planning card settles exactly like a To-do one"
    );
    assert!(
        cards[0]
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("nightly-digest"),
        "the note names what was authored"
    );
}

/// A workflow authored with no card in scope is not a reason to mint one.
/// #267's rule stands: the card doors are the handler's and the model's, and
/// a settle is neither.
#[tokio::test]
async fn a_workflow_authored_with_no_handler_card_opens_none() {
    let chatter = "thanks, that looks great";
    assert!(
        crate::company::task_intent::detect_task_intent(chatter).is_none(),
        "fixture must be a message the chat handler does NOT card"
    );
    let fx = Fixture::new();
    let turns = ScriptedTurns::new(
        &fx,
        vec![Turn::authoring("done", vec![authored("nightly-digest")])],
    );
    fx.runner(&turns)
        .handle_operator_message("chief", chatter, Some("general"))
        .await
        .expect("operator message handled");

    assert!(
        fx.cards().await.is_empty(),
        "a settle is not a card door; nothing to settle means nothing to open"
    );
    assert_eq!(fx.workflow_refs.queued(), 0, "drained either way");
}

/// The unchanged half, pinned so the drain cannot start settling cards for
/// turns that authored nothing. A "create a workflow" ask whose turn never
/// called the tool has produced nothing, and its card is still to do.
#[tokio::test]
async fn a_carded_turn_that_authored_no_workflow_leaves_its_card_alone() {
    let imperative = "create a workflow named nightly digest";
    let title = crate::company::task_intent::detect_task_intent(imperative)
        .expect("fixture must be a message the chat handler cards");
    let fx = Fixture::new();
    let handler = handler_card_in(title, COLUMN_TODO);
    TaskStore::upsert(&*fx.tasks, &fx.record.id, &handler)
        .await
        .expect("seed the handler card");

    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("I could not build that")]);
    fx.runner(&turns)
        .handle_operator_message("chief", imperative, Some("general"))
        .await
        .expect("operator message handled");

    let cards = fx.cards().await;
    assert_eq!(cards.len(), 1);
    assert_eq!(
        cards[0].column,
        crate::ports::tasks::COLUMN_TODO,
        "nothing was authored, so nothing settled"
    );
}

/// Only what THIS turn staged may be attributed to this turn's card — the
/// same discipline `run_task` keeps. Without the pre-turn clear, a workflow
/// left staged by an earlier turn would settle the next unrelated card and
/// name a workflow that turn never touched.
#[tokio::test]
async fn a_workflow_left_staged_by_an_earlier_turn_settles_nothing() {
    let imperative = "create a workflow named nightly digest";
    let title = crate::company::task_intent::detect_task_intent(imperative)
        .expect("fixture must be a message the chat handler cards");
    let fx = Fixture::new();
    let handler = handler_card_in(title, COLUMN_TODO);
    TaskStore::upsert(&*fx.tasks, &fx.record.id, &handler)
        .await
        .expect("seed the handler card");
    // Staged before this turn begins, by whatever ran last.
    fx.workflow_refs.push(authored("someone-elses-workflow"));

    let turns = ScriptedTurns::new(&fx, vec![Turn::reply("I could not build that")]);
    fx.runner(&turns)
        .handle_operator_message("chief", imperative, Some("general"))
        .await
        .expect("operator message handled");

    let cards = fx.cards().await;
    assert_eq!(
        cards[0].column,
        crate::ports::tasks::COLUMN_TODO,
        "this turn authored nothing; the stale staging belongs to no card here"
    );
    let note = cards[0].note.clone().unwrap_or_default();
    assert!(
        !note.contains("someone-elses-workflow"),
        "a card must never name a workflow its own turn did not author: {note}"
    );
}

/// The same stand-down on the **hand-off** path (issue #463). #442 guarded
/// only the direct path, so a recognised imperative the orchestrator handed
/// off produced the handler's card AND the delegation's — measured on a live
/// host as two cards for one message.
///
/// The guard cannot live in `run_delegation`: what reaches there is the
/// instruction the model wrote, not the operator's words, and the handler
/// classified the latter.
#[tokio::test]
async fn a_hand_off_of_a_message_the_chat_handler_carded_opens_no_second_card() {
    let imperative = "draft the launch plan for next quarter";
    let title = crate::company::task_intent::detect_task_intent(imperative)
        .expect("fixture must be a message the chat handler cards");
    let fx = Fixture::new();
    // The card the REST handler wrote moments before the cycle started.
    fx.tasks
        .upsert(
            &fx.record.id,
            &TaskRecord {
                id: "handler-card".to_string(),
                title: TaskTitle::authored(&title),
                note: None,
                column: COLUMN_TODO.to_string(),
                priority: "medium".to_string(),
                assignee: String::new(),
                updated_at_millis: now_millis(),
                origin: None,
                parent_task_id: None,
                output: None,
                plan: None,
                planning_attempts: Vec::new(),
                deliverable: crate::ports::tasks::TaskDeliverable::Once,
                workflow_proposal: None,
                origin_run_id: None,
                origin_workflow_id: None,
                origin_message_seq: Some(handler_seq()),
                bounced: None,
            },
        )
        .await
        .expect("seed the handler's card");

    let turns = ScriptedTurns::new(
        &fx,
        vec![
            Turn::queueing("on it", vec![handoff("Draft the launch plan.")]),
            Turn::reply("drafted"),
            Turn::reply("the desk drafted it"),
        ],
    );
    let turn = fx
        .runner(&turns)
        .answering(Some(handler_seq()))
        .handle_operator_message("chief", imperative, None)
        .await
        .expect("operator message handled");

    let cards = fx.cards().await;
    assert_eq!(cards.len(), 1, "one message, one card: {cards:?}");
    assert_eq!(cards[0].id, "handler-card");
    // …and the turn ADOPTS it, which is what lets a publish later in the
    // same message file onto it instead of minting a rival beside it.
    assert_eq!(turn.spawned_task.as_deref(), Some("handler-card"));
}
