use super::*;

/// Issue #374: a resolution that minted only a STANDING grant must still
/// re-dispatch the agent.
///
/// This is the feature's happy path, and it was the one real gap in the
/// plan. `redispatch_granted_call` peeked only the single-use set and
/// no-ops silently on a miss — correct for every legitimate miss (a deny, a
/// native effect, a legacy park) and catastrophic here: the operator picks
/// the broader scope, the permission is armed, and the call they were
/// looking at never runs. It would have looked exactly like #243's original
/// bug, one scope over.
#[tokio::test]
async fn a_standing_grant_also_redispatches_its_agent() {
    let dir = tempfile::tempdir().unwrap();
    let log: Arc<dyn crate::ports::EventLog> =
        Arc::new(crate::store::FsEventLog::new(dir.path().to_path_buf()));
    let requests = crate::harness::policy::ApprovalRequestQueue::default();
    requests
        .grants()
        .grant_standing(crate::runtime::grants::StandingGrant {
            id: crate::runtime::grants::GrantId::new("g1"),
            agent: "ceo".into(),
            workflow: None,
            tool: "workspace_write".into(),
            verdict: Verdict::Approve,
            granted_by: crate::ports::types::Actor {
                kind: crate::ports::types::ActorKind::User,
                id: "user-1".into(),
            },
            approval_id: ApprovalId::new("appr-1"),
            at_millis: now_millis(),
            expires_at_millis: now_millis() + 60 * 60 * 1000,
            origin_thread: None,
            origin_parent: None,
            origin_task: None,
            scope: None,
        });
    let brain = brain_with_queue_and_events(dir.path(), requests, log.clone());

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
    assert_ne!(
        bubble.text, "Acknowledged.",
        "a standing grant must re-dispatch, not fall through to the no-op"
    );
    assert!(bubble.text.contains("workspace_write"), "{}", bubble.text);
    // No exact-arguments pin: a standing grant admits any arguments, which
    // is what the operator consented to by choosing this scope. Telling the
    // model to reproduce a specific argument object would make the broad
    // scope behave like the narrow one.
    assert!(
        !bubble.text.contains("Do not modify them"),
        "a standing grant must not pin arguments: {}",
        bubble.text
    );

    // Journaling the reply belongs to the runtime now (issue #469), so the
    // brain must not write a second copy. See
    // `server::operator::test::a_continuation_answers_in_the_thread_the_sign_off_was_raised_in`
    // for the round trip.
    assert!(no_replies_journaled(&log).await);
}

/// A DENIED approval runs no turn. "No" must never re-dispatch anything.
#[tokio::test]
async fn a_denied_approval_redispatches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let log: Arc<dyn crate::ports::EventLog> =
        Arc::new(crate::store::FsEventLog::new(dir.path().to_path_buf()));
    let requests = crate::harness::policy::ApprovalRequestQueue::default();
    // A grant for a DIFFERENT approval is live, to prove the arm keys on the
    // resolved id rather than reaching for whatever is lying around.
    requests
        .grants()
        .grant(crate::runtime::grants::GrantedCall {
            approval_id: ApprovalId::new("appr-other"),
            agent: "ceo".into(),
            tool: "composio_execute".into(),
            args: serde_json::json!({}),
            at_millis: now_millis(),
            origin_thread: None,
            origin_parent: None,
            origin_task: None,
        });
    requests
        .grants()
        .grant_standing(crate::runtime::grants::StandingGrant {
            id: crate::runtime::grants::GrantId::new("deny-1"),
            agent: "ceo".into(),
            workflow: None,
            tool: "workspace_write".into(),
            verdict: Verdict::Deny,
            granted_by: crate::ports::types::Actor {
                kind: crate::ports::types::ActorKind::User,
                id: "user-1".into(),
            },
            approval_id: ApprovalId::new("appr-1"),
            at_millis: now_millis(),
            expires_at_millis: now_millis() + 60_000,
            origin_thread: None,
            origin_parent: None,
            origin_task: None,
            scope: None,
        });
    let brain = brain_with_queue_and_events(dir.path(), requests, log.clone());

    let result = brain
        .run_cycle(
            cycle_over(vec![approval_resolved("appr-1", Verdict::Deny)]),
            &NoopHost,
        )
        .await
        .unwrap();

    assert_eq!(result.channel_responses.len(), 1);
    assert_eq!(
        result.channel_responses[0].text, "Acknowledged.",
        "a deny falls through to the fallback, exactly as before #243"
    );
    assert!(
        log.read_from(&CompanyId::new("acme"), crate::ports::EventSeq::new(0), 100)
            .await
            .unwrap()
            .is_empty(),
        "nothing is journaled for a deny"
    );
}

/// The re-run of a card carrying review feedback reads that feedback: the
/// operator's `[reviewer]` note block is part of the turn instruction the
/// fresh dispatch is built from, which is why `apply_review_feedback`
/// appends to the note *before* re-dispatch.
#[test]
fn task_instruction_carries_a_reviewer_note_block() {
    let mut card = card_in_review("card-1");
    card.note = Some("[reviewer] tighten the intro".to_string());
    let instruction = task_instruction(&card);
    assert!(
        instruction.contains("[reviewer] tighten the intro"),
        "the fresh run must see the reviewer's feedback: {instruction}"
    );
    assert!(instruction.starts_with(&format!("Task: {}", card.title)));
}

#[test]
fn public_research_task_instruction_keeps_prior_agent_results_out_of_the_current_assignment() {
    let mut card = card_in_review("card-1");
    card.note = Some(
        "[operator] Research competitors and cite sources.\n\n\
         [researcher] I stopped because my calls looped.\n\n\
         The old run produced no findings.\n\n\
         [reviewer] Include pricing where it is verifiable."
            .to_string(),
    );

    let instruction = task_instruction(&card);
    let history = instruction
        .split("## Prior attempt history")
        .nth(1)
        .and_then(|rest| rest.split("## Current assignment").next())
        .expect("history section");
    let assignment = instruction
        .split("## Current assignment")
        .nth(1)
        .expect("assignment section");

    assert!(history.contains("omitted from this turn"));
    assert!(!history.contains("I stopped because my calls looped"));
    assert!(!history.contains("The old run produced no findings"));
    assert!(!assignment.contains("old run produced no findings"));
    assert!(assignment.contains("Research competitors and cite sources"));
    assert!(assignment.contains("Include pricing where it is verifiable"));
    assert!(assignment.contains("Perform the current assignment now"));
    assert!(assignment.contains("do not read the tasks ledger to rediscover it"));
    assert!(assignment.contains("`web_search`"));
    assert!(assignment.contains("begin with `web_search` now"));
    assert!(assignment.contains("Do not inspect the company workspace"));
    assert!(assignment.contains("stop after that one call"));
    assert!(assignment.contains("Do not retry it with another query"));
}

#[test]
fn ordinary_task_instructions_do_not_claim_they_are_public_research() {
    let mut card = card_in_review("card-1");
    card.note = Some("[operator] Fix the checkout button.".to_string());
    let instruction = task_instruction(&card);
    assert!(!instruction.contains("begin with `web_search` now"));
}

/// **The reachability assertion.** A test that the drain works when called
/// is not coverage that the drain is reached — and on this path it was not.
///
/// `redispatch_granted_call` runs a full toolbelt turn and claimed publishes
/// only, so a `review_task` the re-issued call made was staged, answered
/// with "the card has moved to done", and destroyed by the next turn's
/// `clear()`. It **drains** rather than refusing, deliberately: `review_task`
/// is a gateable Write effect, so refusing here would make an operator's own
/// approval unspendable — approve, refuse, re-park.
#[tokio::test]
async fn a_granted_redispatch_drains_the_board_work_its_turn_queued() {
    let dir = tempfile::tempdir().unwrap();
    let requests = crate::harness::policy::ApprovalRequestQueue::default();
    requests.grants().grant(granted("appr-1", "review_task"));
    let base_url = spawn_model_script(vec![
        ScriptTurn::Call {
            tool: "review_task",
            args: serde_json::json!({ "task_id": "card-1", "decision": "approve" }),
        },
        ScriptTurn::Say("Approved."),
    ])
    .await;
    let brain = brain_over_script(dir.path(), requests, base_url);
    let tasks = brain.deps.tasks.clone().expect("task store");
    tasks
        .upsert(&CompanyId::new("acme"), &card_in_review("card-1"))
        .await
        .expect("seed the card");

    let result = brain
        .run_cycle(
            cycle_over(vec![approval_resolved("appr-1", Verdict::Approve)]),
            &NoopHost,
        )
        .await
        .expect("cycle runs");
    assert_eq!(result.channel_responses.len(), 1);

    let cards = tasks.list(&CompanyId::new("acme")).await.expect("list");
    assert_eq!(
        cards[0].column,
        crate::ports::tasks::COLUMN_DONE,
        "the approved card must actually move — staging it and returning was the defect"
    );
    assert_eq!(
        brain.deps.delegations.queued(),
        0,
        "and nothing may be left for a later turn's clear() to destroy"
    );
    assert!(
        !brain.deps.delegations.drain_committed(),
        "the claim releases with the re-dispatch turn"
    );
}

/// The #476 nuance: one continuation cycle can run **several** re-dispatch
/// turns, one per batched resolution. The claim is therefore per turn, not
/// per cycle — each re-dispatch owns its own drain window.
///
/// A per-cycle claim would pass a single-approval test and fail here in the
/// worst way: the second turn's staged verdict would ride on the first
/// turn's already-spent window, or the second acquire would clear work the
/// first had not drained yet.
#[tokio::test]
async fn batched_resolutions_each_get_their_own_drain_window() {
    let dir = tempfile::tempdir().unwrap();
    let requests = crate::harness::policy::ApprovalRequestQueue::default();
    requests.grants().grant(granted("appr-1", "review_task"));
    requests.grants().grant(granted("appr-2", "review_task"));
    let base_url = spawn_model_script(vec![
        ScriptTurn::Call {
            tool: "review_task",
            args: serde_json::json!({ "task_id": "card-1", "decision": "approve" }),
        },
        ScriptTurn::Say("Approved card-1."),
        ScriptTurn::Call {
            tool: "review_task",
            args: serde_json::json!({ "task_id": "card-2", "decision": "revise" }),
        },
        ScriptTurn::Say("Sent card-2 back."),
    ])
    .await;
    let brain = brain_over_script(dir.path(), requests, base_url);
    let tasks = brain.deps.tasks.clone().expect("task store");
    let company = CompanyId::new("acme");
    for id in ["card-1", "card-2"] {
        tasks
            .upsert(&company, &card_in_review(id))
            .await
            .expect("seed the card");
    }

    let result = brain
        .run_cycle(
            cycle_over(vec![
                approval_resolved("appr-1", Verdict::Approve),
                approval_resolved("appr-2", Verdict::Approve),
            ]),
            &NoopHost,
        )
        .await
        .expect("cycle runs");
    assert_eq!(
        result.channel_responses.len(),
        2,
        "both resolutions re-dispatch"
    );

    let cards = tasks.list(&company).await.expect("list");
    let column = |id: &str| {
        cards
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.column.clone())
            .unwrap_or_else(|| panic!("{id} is on the board"))
    };
    assert_eq!(
        column("card-1"),
        crate::ports::tasks::COLUMN_DONE,
        "the first re-dispatch's verdict must survive the second re-dispatch's claim"
    );
    assert_eq!(
        column("card-2"),
        crate::ports::tasks::COLUMN_TODO,
        "and the second's own verdict lands too"
    );
    assert_eq!(brain.deps.delegations.queued(), 0);
    assert!(!brain.deps.delegations.drain_committed());
}

/// An approved resolution with NO grant behind it is a silent no-op.
///
/// This is the common case, not an edge: a native effect the runtime already
/// executed, a legacy parked effect from before `Effect::agent` existed (it
/// replays as `None` and mints nothing), a grant already consumed, and a
/// grant already swept all land here. Every one of them must keep the exact
/// pre-#243 behaviour rather than manufacturing a turn.
#[tokio::test]
async fn an_approval_with_no_grant_is_a_silent_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let log: Arc<dyn crate::ports::EventLog> =
        Arc::new(crate::store::FsEventLog::new(dir.path().to_path_buf()));
    let requests = crate::harness::policy::ApprovalRequestQueue::default();
    let brain = brain_with_queue_and_events(dir.path(), requests, log.clone());

    let result = brain
        .run_cycle(
            cycle_over(vec![approval_resolved("appr-native", Verdict::Approve)]),
            &NoopHost,
        )
        .await
        .unwrap();

    assert_eq!(result.channel_responses.len(), 1);
    assert_eq!(result.channel_responses[0].text, "Acknowledged.");
    assert!(
        log.read_from(&CompanyId::new("acme"), crate::ports::EventSeq::new(0), 100)
            .await
            .unwrap()
            .is_empty()
    );
}
