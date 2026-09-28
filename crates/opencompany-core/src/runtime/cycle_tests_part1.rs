use super::tests_core::*;
use super::tests_core2::*;

/// `single_agent` picks an agent slot only when the batch is one addressed
/// operator message, and falls back to the whole-company lock otherwise —
/// the invariant the per-agent lock leans on (issue: parallel agent turns).
#[test]
fn single_agent_picks_one_addressee_and_falls_back_otherwise() {
    fn op(chat: Option<&str>) -> (Option<EventSeq>, CompanyEvent) {
        (
            None,
            CompanyEvent::OperatorMessage {
                text: "hi".to_string(),
                by: None,
                chat: chat.map(str::to_string),
                parent: None,
                deliverable: None,
                mentions: Vec::new(),
                attachments: Vec::new(),
            },
        )
    }

    // One message addressed to one agent -> that agent's slot.
    assert_eq!(
        single_agent(&[op(Some("frits"))]),
        Some("frits".to_string())
    );
    // Several messages, all the same agent -> still that agent's slot.
    assert_eq!(
        single_agent(&[op(Some("frits")), op(Some("frits"))]),
        Some("frits".to_string())
    );
    // Two different agents in one batch -> whole company.
    assert_eq!(single_agent(&[op(Some("frits")), op(Some("sjaan"))]), None);
    // Unaddressed message (routed to the orchestrator) -> whole company.
    assert_eq!(single_agent(&[op(None)]), None);
    // A non-operator event in the batch -> whole company.
    assert_eq!(
        single_agent(&[(
            None,
            CompanyEvent::TurnStarted {
                turn_id: "t1".to_string(),
                chat_id: "frits".to_string(),
                parent: None,
                by: None,
                agent_id: None,
                episode_id: None,
                round_revision: None,
            },
        )]),
        None
    );
    // Empty batch -> whole company.
    assert_eq!(single_agent(&[]), None);
}

/// Issue #845: a `workflow` message reaches the brain carrying the builder
/// briefing, and nothing else does.
///
/// This is the fix for the mode actually observed on staging: the builder
/// pass had already produced a proposal for `weekly-aeo-audit` while the
/// desk agent answering the same message was telling the operator that it
/// "cannot make it exist". The turn was right about its own toolset and
/// wrong about the company, because nothing told it.
///
/// The `chat` case is issue #1152's, and "nothing else does" is why it is
/// pinned here rather than assumed: a "Just chatting" message opens no card,
/// so there is no builder pass owning anything, so briefing the turn that a
/// build is under way would be telling it something untrue. The injection
/// matches `Workflow` exactly and this is what keeps it exact.
#[test]
fn only_a_workflow_message_gets_the_builder_briefing() {
    let msg = |deliverable| CompanyEvent::OperatorMessage {
        mentions: Vec::new(),
        text: "set up a weekly AEO audit".to_string(),
        by: None,
        chat: None,
        parent: None,
        deliverable,
        attachments: Vec::new(),
    };
    let text_of = |event: &CompanyEvent| match event {
        CompanyEvent::OperatorMessage { text, .. } => text.clone(),
        _ => unreachable!("fixture is an operator message"),
    };

    let mut events = vec![
        msg(Some(MessageIntent::Workflow)),
        msg(Some(MessageIntent::Once)),
        msg(None),
        msg(Some(MessageIntent::Chat)),
        // A non-operator event must be left entirely alone.
        CompanyEvent::ScheduleFired {
            cron: "0 6 * * 5".to_string(),
            prompt: "run the audit".to_string(),
        },
    ];
    CycleRunner::inject_workflow_builder_awareness(&mut events);

    let briefed = text_of(&events[0]);
    assert!(briefed.contains(BUILDER_ANNOTATION), "{briefed}");
    assert!(
        briefed.starts_with("set up a weekly AEO audit"),
        "the operator's own words come first, untouched: {briefed}"
    );
    // The whole point: the turn is told not to deny the capability.
    assert!(
        briefed.contains("do not report that you cannot"),
        "{briefed}"
    );

    for (i, label) in [(1, "once"), (2, "no choice"), (3, "chat")] {
        let text = text_of(&events[i]);
        assert_eq!(
            text, "set up a weekly AEO audit",
            "a `{label}` message must reach the brain exactly as typed"
        );
        assert!(
            !text.contains(BUILDER_ANNOTATION),
            "a `{label}` message must carry no builder briefing: {text}"
        );
    }
    assert!(matches!(events[4], CompanyEvent::ScheduleFired { .. }));
}

/// Issue #1859: the handed-task briefing distinguishes real board state
/// instead of rendering every open card as a bare title. Two cards, two
/// different shapes: a paused card with two attempts (the latest failed)
/// renders `[Paused · attempt 2 failed]` — the LATEST attempt, not the
/// first, which succeeded; a to-do card nobody has attempted yet renders
/// `[To-do]` with the attempt clause omitted entirely rather than
/// claiming an attempt that never happened.
#[tokio::test]
async fn handed_task_briefing_carries_column_and_attempt_status() {
    let home_dir = tmp_home();
    let rt = Arc::new(
        RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest("full"))
            .build()
            .await
            .unwrap(),
    );

    let card = |id: &str, title: &str, column: &str| TaskRecord {
        id: id.to_string(),
        title: TaskTitle::authored(title),
        note: None,
        column: column.to_string(),
        priority: "medium".to_string(),
        assignee: "ceo".to_string(),
        updated_at_millis: 1,
        origin: TaskOrigin::new(None, None),
        parent_task_id: None,
        output: None,
        plan: None,
        planning_attempts: Vec::new(),
        deliverable: crate::ports::tasks::TaskDeliverable::Once,
        workflow_proposal: None,
        origin_run_id: None,
        origin_workflow_id: None,
        origin_message_seq: None,
        bounced: None,
    };
    rt.tasks()
        .upsert(
            rt.id(),
            &card(
                "t-paused",
                "Investigate the flaky nightly job",
                crate::ports::tasks::COLUMN_PAUSED,
            ),
        )
        .await
        .unwrap();
    rt.tasks()
        .upsert(
            rt.id(),
            &card("t-todo", "Draft the launch memo", COLUMN_TODO),
        )
        .await
        .unwrap();

    // Two attempts at the paused card: the first succeeded, the second
    // (newest) failed — the briefing must report the LATEST.
    let mut r1 = rt
        .runs()
        .create_run(
            rt.id(),
            crate::ports::runs::NewRun::for_task("r1", "t-paused", "ceo"),
        )
        .await
        .unwrap();
    r1.status = RunStatus::Succeeded;
    rt.runs().put_run(rt.id(), &r1).await.unwrap();
    let mut r2 = rt
        .runs()
        .create_run(
            rt.id(),
            crate::ports::runs::NewRun::for_task("r2", "t-paused", "ceo"),
        )
        .await
        .unwrap();
    r2.status = RunStatus::Failed;
    rt.runs().put_run(rt.id(), &r2).await.unwrap();
    // t-todo gets no run at all.

    let record = rt.store.load(rt.id()).await.unwrap().unwrap();
    let mut events = vec![CompanyEvent::OperatorMessage {
        text: "what are you working on?".into(),
        by: Some(operator()),
        chat: Some("ceo".into()),
        parent: None,
        deliverable: None,
        mentions: Vec::new(),
        attachments: Vec::new(),
    }];

    CycleRunner::new(&rt)
        .inject_handed_task_awareness(
            &record,
            &mut events,
            &rt.tasks().list(rt.id()).await.expect("list"),
        )
        .await;

    let CompanyEvent::OperatorMessage { text, .. } = &events[0] else {
        unreachable!("fixture is an operator message");
    };
    assert!(
        text.contains("- Investigate the flaky nightly job [Paused · attempt 2 failed]"),
        "the paused card must show its column and its LATEST attempt's status: {text}"
    );
    assert!(
        text.contains("- Draft the launch memo [To-do]"),
        "a never-attempted card must show its column with no attempt clause: {text}"
    );
    assert!(
        !text.contains("Draft the launch memo [To-do · attempt"),
        "a card with zero runs must never claim an attempt: {text}"
    );
}

/// A run-history read failure, not "no attempts": the briefing must mark
/// the card's attempt status unavailable rather than rendering it
/// identically to a card nobody has ever attempted.
#[tokio::test]
async fn handed_task_briefing_marks_attempt_status_unavailable_on_a_run_history_read_failure() {
    let home_dir = tmp_home();
    let runs_backing: Arc<dyn crate::ports::RunStore> =
        Arc::new(crate::store::FsOps::new(home_dir.path().to_path_buf()));
    let rt = Arc::new(
        RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest("full"))
            .with_runs(Arc::new(FailingRunHistory(runs_backing)))
            .build()
            .await
            .unwrap(),
    );

    rt.tasks()
        .upsert(
            rt.id(),
            &TaskRecord {
                id: "t-paused".to_string(),
                title: TaskTitle::authored("Investigate the flaky nightly job"),
                note: None,
                column: crate::ports::tasks::COLUMN_PAUSED.to_string(),
                priority: "medium".to_string(),
                assignee: "ceo".to_string(),
                updated_at_millis: 1,
                origin: TaskOrigin::new(None, None),
                parent_task_id: None,
                output: None,
                plan: None,
                planning_attempts: Vec::new(),
                deliverable: crate::ports::tasks::TaskDeliverable::Once,
                workflow_proposal: None,
                origin_run_id: None,
                origin_workflow_id: None,
                origin_message_seq: None,
                bounced: None,
            },
        )
        .await
        .unwrap();

    let record = rt.store.load(rt.id()).await.unwrap().unwrap();
    let mut events = vec![CompanyEvent::OperatorMessage {
        text: "what are you working on?".into(),
        by: Some(operator()),
        chat: Some("ceo".into()),
        parent: None,
        deliverable: None,
        mentions: Vec::new(),
        attachments: Vec::new(),
    }];

    CycleRunner::new(&rt)
        .inject_handed_task_awareness(
            &record,
            &mut events,
            &rt.tasks().list(rt.id()).await.expect("list"),
        )
        .await;

    let CompanyEvent::OperatorMessage { text, .. } = &events[0] else {
        unreachable!("fixture is an operator message");
    };
    assert!(
        text.contains("attempt status unavailable"),
        "a run-history read failure must be marked unavailable: {text}"
    );
    assert!(
        !text.contains("[Paused]"),
        "must not render identically to a card with no attempt clause at all: {text}"
    );
}

/// An assignee with more open cards than [`HANDED_TASK_ATTEMPT_LOOKUP_CAP`]
/// must not pay one `list_runs` round trip per card while the cycle guard
/// is held — the lookup is bounded, and cards past the cap still render
/// (with no attempt clause) rather than being dropped from the briefing.
#[tokio::test]
async fn handed_task_briefing_bounds_attempt_lookups_regardless_of_open_card_count() {
    let home_dir = tmp_home();
    let runs_backing: Arc<dyn crate::ports::RunStore> =
        Arc::new(crate::store::FsOps::new(home_dir.path().to_path_buf()));
    let counting = Arc::new(CountingRunHistory {
        inner: runs_backing,
        list_runs_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let rt = Arc::new(
        RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest("full"))
            .with_runs(counting.clone())
            .build()
            .await
            .unwrap(),
    );

    let card_count = HANDED_TASK_ATTEMPT_LOOKUP_CAP + 4;
    for n in 0..card_count {
        rt.tasks()
            .upsert(
                rt.id(),
                &TaskRecord {
                    id: format!("t-{n}"),
                    title: TaskTitle::authored(&format!("Card {n}")),
                    note: None,
                    column: COLUMN_TODO.to_string(),
                    priority: "medium".to_string(),
                    assignee: "ceo".to_string(),
                    updated_at_millis: 1,
                    origin: TaskOrigin::new(None, None),
                    parent_task_id: None,
                    output: None,
                    plan: None,
                    planning_attempts: Vec::new(),
                    deliverable: crate::ports::tasks::TaskDeliverable::Once,
                    workflow_proposal: None,
                    origin_run_id: None,
                    origin_workflow_id: None,
                    origin_message_seq: None,
                    bounced: None,
                },
            )
            .await
            .unwrap();
    }

    let record = rt.store.load(rt.id()).await.unwrap().unwrap();
    let mut events = vec![CompanyEvent::OperatorMessage {
        text: "what are you working on?".into(),
        by: Some(operator()),
        chat: Some("ceo".into()),
        parent: None,
        deliverable: None,
        mentions: Vec::new(),
        attachments: Vec::new(),
    }];

    let baseline = counting
        .list_runs_calls
        .load(std::sync::atomic::Ordering::Relaxed);
    CycleRunner::new(&rt)
        .inject_handed_task_awareness(
            &record,
            &mut events,
            &rt.tasks().list(rt.id()).await.expect("list"),
        )
        .await;

    assert_eq!(
        counting
            .list_runs_calls
            .load(std::sync::atomic::Ordering::Relaxed)
            - baseline,
        HANDED_TASK_ATTEMPT_LOOKUP_CAP,
        "the attempt lookup must not run once per open card — it must stop at the cap"
    );
    let CompanyEvent::OperatorMessage { text, .. } = &events[0] else {
        unreachable!("fixture is an operator message");
    };
    for n in 0..card_count {
        assert!(
            text.contains(&format!("Card {n}")),
            "every open card must still render, even past the lookup cap: {text}"
        );
    }
}

// ── Issue #1725: a bare greeting must not run the agentic loop ──

/// The reported bug, end to end and at the level that costs money: "hi"
/// reaches the cycle, an answer comes back, and **the brain is never
/// called**.
///
/// A turn count rather than a reply assertion, deliberately. The observed
/// failure was not "the wording is wrong" — it was a greeting spending a
/// full agentic turn (memory retrieval, a tool step, a long answer carried
/// over from a task nobody had asked about). `CountingBrain` bills 4,500
/// tokens and writes a memory trace per call, so a regression shows up as a
/// call count, a metered spend and a trace that should not exist.
#[tokio::test]
async fn a_bare_greeting_answers_without_calling_the_brain() {
    let home_dir = tmp_home();
    let brain = Arc::new(CountingBrain::default());
    let rt = Arc::new(
        RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest("full"))
            .with_brain(brain.clone())
            .build()
            .await
            .unwrap(),
    );

    let report = rt
        .run_cycle(vec![CompanyEvent::OperatorMessage {
            text: "hi".into(),
            by: Some(operator()),
            chat: None,
            parent: None,
            deliverable: None,
            mentions: Vec::new(),
            attachments: Vec::new(),
        }])
        .await
        .unwrap();

    assert_eq!(
        brain.calls(),
        0,
        "a greeting must not spend a turn: the brain was called"
    );
    assert_eq!(
        report.responses.len(),
        1,
        "the operator still gets an answer"
    );
    let reply = &report.responses[0];
    assert_eq!(
        reply.text,
        crate::company::task_intent::SmallTalk::Hello.reply()
    );
    assert!(
        reply.steps.is_empty(),
        "no tool ran, so the timeline is empty: {:?}",
        reply.steps
    );
    // Issue #885: the reply is the company's, not the operator's.
    assert_eq!(
        reply.agent.as_deref(),
        Some("ceo"),
        "the greeting comes back in the voice the turn would have used"
    );
    // Nothing was written back for a later turn to retrieve.
    assert!(
        rt.memory
            .recent_traces(rt.id(), 8)
            .await
            .unwrap()
            .is_empty(),
        "a pleasantry must leave no memory behind it"
    );
}

/// The other half, and the one that matters more: everything that is not a
/// bare pleasantry still runs the full turn.
///
/// A fast path that swallowed a real request would answer "Hey! What can I
/// help you with?" to "build the landing page" and drop the work on the
/// floor — worse than the bug it fixes. Each case here is one of the
/// conditions in `small_talk_result`, driven through the real cycle.
#[tokio::test]
async fn everything_that_is_not_a_pleasantry_still_runs_the_turn() {
    let home_dir = tmp_home();
    let brain = Arc::new(CountingBrain::default());
    let rt = Arc::new(
        RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest("full"))
            .with_brain(brain.clone())
            .build()
            .await
            .unwrap(),
    );

    let message = |text: &str, deliverable, attachments: Vec<crate::ports::types::Attachment>| {
        CompanyEvent::OperatorMessage {
            text: text.into(),
            by: Some(operator()),
            chat: None,
            parent: None,
            deliverable,
            mentions: Vec::new(),
            attachments,
        }
    };
    let attached = vec![crate::ports::types::Attachment {
        node_id: "node-1".into(),
        name: "brief.pdf".into(),
        mime: "application/pdf".into(),
        size: 12,
        extracted_text: None,
    }];

    let cases = vec![
        (
            "a real request",
            message("build the landing page", None, Vec::new()),
        ),
        // A greeting with an ask under it is an ask.
        (
            "a greeting with an ask",
            message("hi, build the landing page", None, Vec::new()),
        ),
        // "yes" answering a teammate's question is an instruction.
        ("an acknowledgement", message("yes", None, Vec::new())),
        // The operator said, positively, that this message asks for work.
        (
            "an explicit work choice",
            message("hi", Some(MessageIntent::Once), Vec::new()),
        ),
        (
            "an explicit workflow choice",
            message("hi", Some(MessageIntent::Workflow), Vec::new()),
        ),
        // A file with "hi" over it is a request to look at the file.
        ("an attachment", message("hi", None, attached)),
    ];

    for (i, (label, event)) in cases.into_iter().enumerate() {
        rt.run_cycle(vec![event]).await.unwrap();
        assert_eq!(brain.calls(), i + 1, "{label} must still run a full turn");
    }
}

/// The conditions `small_talk_result` decides on its own, driven directly
/// so the ones a live cycle cannot easily reach are still pinned: a
/// confined workflow-copilot thread, a batch of more than one event, and
/// who the reply is attributed to on an addressed thread.
#[test]
fn the_fast_path_declines_a_copilot_thread_and_a_batch() {
    let manifest: crate::company::CompanyManifest = toml::from_str(
        r#"
[company]
name = "Acme"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[agent]]
id = "writer"
role = "Writer"

[[group_chat]]
id = "content"
name = "Content desk"
members = ["writer"]
"#,
    )
    .expect("valid manifest");
    let record = CompanyRecord {
        general_channel: Default::default(),
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: CompanyId::new("acme"),
        manifest,
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
    };
    let hi = |chat: Option<&str>| CompanyEvent::OperatorMessage {
        text: "hi".into(),
        by: None,
        chat: chat.map(str::to_string),
        parent: None,
        deliverable: None,
        mentions: Vec::new(),
        attachments: Vec::new(),
    };

    // An addressed desk answers in that desk's lead's voice, not the
    // orchestrator's — the same routing `responder_for` does.
    let desk = small_talk_result(&record, &[hi(Some("content"))]).expect("a pleasantry");
    assert_eq!(desk.channel_responses[0].agent.as_deref(), Some("writer"));
    assert!(desk.token_usage.is_zero(), "no model was called");

    // Unaddressed falls to the orchestrator.
    let main = small_talk_result(&record, &[hi(None)]).expect("a pleasantry");
    assert_eq!(main.channel_responses[0].agent.as_deref(), Some("ceo"));

    // A workflow copilot thread is confined and answered by an ephemeral
    // agent this cannot speak as, so it declines (issue #416).
    assert!(
        small_talk_result(&record, &[hi(Some("workflow-copilot:weekly-aeo-audit"))]).is_none(),
        "a copilot thread keeps its confined turn"
    );

    // A batch is a scheduler tick or several messages at once; neither is
    // small talk, even when one of its members looks like it.
    assert!(small_talk_result(&record, &[hi(None), hi(None)]).is_none());
    assert!(
        small_talk_result(
            &record,
            &[CompanyEvent::ScheduleFired {
                cron: "0 6 * * 5".into(),
                prompt: "run the audit".into(),
            }]
        )
        .is_none()
    );
    assert!(small_talk_result(&record, &[]).is_none());

    // A company with nobody on the roster has no voice to answer in, so it
    // declines rather than journaling an unattributed bubble (issue #885).
    let mut empty = record.clone();
    empty.manifest.agents.clear();
    empty.manifest.group_chats.clear();
    assert!(small_talk_result(&empty, &[hi(None)]).is_none());
}
