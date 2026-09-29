use super::tests_core::*;
use super::tests_core2::*;
use super::tests_core3::*;

/// A card still running is never called finished — the "concluded the work
/// had finished when it had in fact parked" misreading #377 exists to
/// remove, in briefing form.
#[tokio::test]
async fn work_still_running_is_not_briefed_as_finished() {
    let home_dir = tmp_home();
    let rt = Arc::new(
        RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest("supervised"))
            .build()
            .await
            .unwrap(),
    );
    let id = rt.id().clone();
    let record = rt
        .store
        .load(&id)
        .await
        .unwrap()
        .expect("the company record");

    let mut running = settled_card("t-running", "Rebuild the pricing page");
    running.column = crate::ports::tasks::COLUMN_IN_PROGRESS.to_string();
    running.origin = TaskOrigin::new(Some("growth".to_string()), None);
    rt.tasks().upsert(&id, &running).await.unwrap();

    let mut events = vec![operator_in_thread("growth", None, "did that ship?")];
    CycleRunner::new(&rt)
        .inject_handed_task_awareness(
            &record,
            &mut events,
            &rt.tasks().list(&id).await.expect("list"),
        )
        .await;
    let text = message_text(&events[0]);
    assert!(
        !text.contains(SETTLED_WORK_ANNOTATION),
        "nothing has settled, so there is no settled briefing at all: {text}"
    );
}

/// Past the cap the briefing **says so**. A model handed 5 of 9 with no
/// marker answers "that is everything" confidently and wrongly, which is
/// worse than the silence it replaced.
#[tokio::test]
async fn a_truncated_settled_briefing_declares_what_it_left_out() {
    let home_dir = tmp_home();
    let rt = Arc::new(
        RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest("supervised"))
            .build()
            .await
            .unwrap(),
    );
    let id = rt.id().clone();
    let record = rt
        .store
        .load(&id)
        .await
        .unwrap()
        .expect("the company record");

    let total = SETTLED_WORK_BRIEFING_MAX + 4;
    for n in 0..total {
        let mut card = settled_card(&format!("t-{n}"), &format!("Card number {n}"));
        card.origin = TaskOrigin::new(Some("growth".to_string()), None);
        // Ascending, so the newest is the highest-numbered — the order the
        // briefing keeps and the cap cuts against.
        card.updated_at_millis = n as u64;
        rt.tasks().upsert(&id, &card).await.unwrap();
    }

    let mut events = vec![operator_in_thread("growth", None, "where are we?")];
    CycleRunner::new(&rt)
        .inject_handed_task_awareness(
            &record,
            &mut events,
            &rt.tasks().list(&id).await.expect("list"),
        )
        .await;
    let text = message_text(&events[0]);

    assert!(
        text.contains(&format!(
            "(and {} more, not listed)",
            total - SETTLED_WORK_BRIEFING_MAX
        )),
        "the truncation is declared, never silent: {text}"
    );
    // Most recent first, so the newest card is in and the oldest is out.
    assert!(
        text.contains(&format!("Card number {}", total - 1)),
        "the newest settle is what 'did that ship?' is about: {text}"
    );
    assert!(
        !text.contains("Card number 0 "),
        "the oldest is what the cap cuts: {text}"
    );
}

/// A settled card in one channel says nothing in another. The briefing is
/// scoped by the conversation that raised the work, not by the company.
#[tokio::test]
async fn a_settled_card_says_nothing_in_another_channel() {
    let home_dir = tmp_home();
    let rt = Arc::new(
        RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest("supervised"))
            .build()
            .await
            .unwrap(),
    );
    let id = rt.id().clone();
    let record = rt
        .store
        .load(&id)
        .await
        .unwrap()
        .expect("the company record");

    let mut card = settled_card("t-growth", "Draft the launch email");
    card.origin = TaskOrigin::new(Some("growth".to_string()), None);
    rt.tasks().upsert(&id, &card).await.unwrap();

    let mut events = vec![operator_in_thread("engineering", None, "what's up?")];
    CycleRunner::new(&rt)
        .inject_handed_task_awareness(
            &record,
            &mut events,
            &rt.tasks().list(&id).await.expect("list"),
        )
        .await;
    let text = message_text(&events[0]);
    assert!(!text.contains(SETTLED_WORK_ANNOTATION), "{text}");
    assert!(!text.contains("Draft the launch email"), "{text}");
}

/// The index is the channel's other threads — the turn's own is excluded,
/// because a thread does not need pointing at itself and the line would
/// spend budget saying nothing.
#[test]
fn the_index_lists_the_other_threads_and_not_this_one() {
    let page = vec![
        op(41, "growth", None, "draft the launch email"),
        agent_reply(42, "growth", 41),
        op(43, "growth", None, "what's our Q3 CAC?"),
        agent_reply(44, "growth", 43),
    ];
    let (lines, omitted) =
        thread_index(&page, "growth", "growth", Some(EventSeq::new(41)), "", &[]);
    assert_eq!(omitted, 0);
    let rendered: Vec<String> = lines.iter().map(ThreadLine::render).collect();
    assert_eq!(
        rendered,
        vec![format!(r#"- [{}] "what's our Q3 CAC?" — 1 reply"#, 43)]
    );
}

/// A channel-level turn is in no thread, so it sees them all. That is the
/// epic's "both directions" falling out of one rule rather than needing two.
#[test]
fn a_channel_level_turn_sees_every_thread() {
    let page = vec![
        op(41, "growth", None, "draft the launch email"),
        op(43, "growth", None, "what's our Q3 CAC?"),
    ];
    let (lines, _) = thread_index(&page, "growth", "growth", None, "", &[]);
    assert_eq!(lines.len(), 2);
}

/// Newest first, so "the other one" resolves to the thread most likely
/// meant — and so the cap below cuts the stale tail rather than the live
/// head.
/// **Fed newest-first, the way `read_before` delivers it.**
///
/// The original version of this test built the page in ascending order,
/// which production never produces — and that hid the bug it was meant to
/// pin: a reply is met *before* its root, so updating the root's line in
/// place found nothing and every thread kept its opening sequence as its
/// recency (codex + coderabbit on #1972).
#[test]
fn the_index_is_ordered_by_recency() {
    let mut page = vec![
        op(10, "growth", None, "the old one"),
        op(11, "growth", None, "the middle one"),
        agent_reply(30, "growth", 10), // revives the oldest root
        op(12, "growth", None, "the newest root"),
    ];
    page.sort_by_key(|e| std::cmp::Reverse(e.seq));
    let (lines, _) = thread_index(&page, "growth", "growth", None, "", &[]);
    assert_eq!(
        lines.iter().map(|l| l.opening.clone()).collect::<Vec<_>>(),
        vec!["the old one", "the newest root", "the middle one"],
        "recency is the thread's LAST activity, not when it opened"
    );
}

/// Past the cap the index **says so**. A selection presented as an
/// enumeration is answered from confidently and wrongly — the same rule
/// #1890 C's briefing follows.
#[test]
fn a_truncated_index_declares_what_it_left_out() {
    let total = THREAD_INDEX_MAX + 3;
    let page: Vec<crate::ports::types::StoredEvent> = (0..total)
        .map(|n| op(100 + n as u64, "growth", None, &format!("topic {n}")))
        .collect();
    let (lines, omitted) = thread_index(&page, "growth", "growth", None, "", &[]);
    assert_eq!(lines.len(), THREAD_INDEX_MAX);
    assert_eq!(omitted, 3);
    // The newest survive; the oldest are what the cap cut.
    assert!(
        lines
            .iter()
            .any(|l| l.opening == format!("topic {}", total - 1))
    );
    assert!(!lines.iter().any(|l| l.opening == "topic 0"));
}

/// The card was raised addressing the desk by **name**; the follow-up
/// addresses it by id. Same desk, same thread, so the briefing is owed.
///
/// The filter compared the two selectors verbatim, on the argument that
/// both sides are the raw chat id stamped from the same field. That holds
/// only while every caller spells the desk the same way — the console does,
/// a REST or ACP client need not — and when it broke, the briefing went
/// missing exactly when the operator asked "did that ship?" (codex on
/// #1972).
///
/// **This direction, and not its mirror.** Resolution canonicalises the
/// addressed selector to the desk *id*, so a name-addressed message already
/// finds an id-stamped card with one term. Only a card stamped under the
/// name needs the second, which makes the reverse pairing the one that can
/// tell the fix from its absence — the first draft of this test used it and
/// passed with the fix reverted.
#[tokio::test]
async fn a_settled_card_is_briefed_through_the_desks_other_spelling() {
    let home_dir = tmp_home();
    let rt = Arc::new(
        RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest_with_named_desk())
            .build()
            .await
            .unwrap(),
    );
    let id = rt.id().clone();
    let record = rt.store.load(&id).await.unwrap().expect("the record");

    let mut card = settled_card("t-id", "Draft the launch email");
    card.origin = TaskOrigin::new(Some("Growth".to_string()), Some(EventSeq::new(41)));
    rt.tasks().upsert(&id, &card).await.unwrap();

    let mut events = vec![operator_in_thread(
        "growth_desk",
        Some(41),
        "did that ship?",
    )];
    CycleRunner::new(&rt)
        .inject_handed_task_awareness(&record, &mut events, &rt.tasks().list(&id).await.unwrap())
        .await;
    let text = message_text(&events[0]);

    assert!(
        text.contains("Draft the launch email"),
        "the name-stamped card is the id-addressed desk's own work: {text}"
    );
}

/// A card **no conversation raised** is briefed into none of them.
#[tokio::test]
async fn a_card_no_conversation_raised_is_briefed_into_none_of_them() {
    let home_dir = tmp_home();
    let rt = Arc::new(
        RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest("supervised"))
            .build()
            .await
            .unwrap(),
    );
    let id = rt.id().clone();
    let record = rt.store.load(&id).await.unwrap().expect("the record");

    // Raised on the board, or through `spawn_task` from a turn with no
    // conversation: no desk, and therefore no thread inside one.
    let mut card = settled_card("board-only", "Rotate the signing key");
    card.origin = None;
    rt.tasks().upsert(&id, &card).await.unwrap();

    let mut events = vec![CompanyEvent::OperatorMessage {
        mentions: Vec::new(),
        text: "did that ship?".to_string(),
        by: None,
        chat: None,
        parent: None,
        deliverable: None,
        attachments: Vec::new(),
    }];
    CycleRunner::new(&rt)
        .inject_handed_task_awareness(&record, &mut events, &rt.tasks().list(&id).await.unwrap())
        .await;

    assert!(
        !message_text(&events[0]).contains("Rotate the signing key"),
        "work no conversation raised is not this conversation's: {}",
        message_text(&events[0])
    );
}

/// An unaddressed message is the General desk, not "addressed to nothing".
///
/// `chat_and_emit` routes a request that omits `chat` to General and every
/// reader of the journal folds `None` there, but the briefings required
/// `Some` — so a bare REST or ACP caller asking "did that ship?" was
/// answered blind, in the one conversation the console itself defaults to
/// (codex on #1972).
#[tokio::test]
async fn an_unaddressed_message_is_briefed_as_the_general_desk() {
    let home_dir = tmp_home();
    let rt = Arc::new(
        RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest("supervised"))
            .build()
            .await
            .unwrap(),
    );
    let id = rt.id().clone();
    let record = rt.store.load(&id).await.unwrap().expect("the record");

    let mut card = settled_card("t-general", "Renew the domain");
    // Journaled by a client that named the desk; the message below names
    // nothing. Both are General, so they are one conversation.
    card.origin = TaskOrigin::new(Some("General".to_string()), None);
    rt.tasks().upsert(&id, &card).await.unwrap();

    let mut events = vec![CompanyEvent::OperatorMessage {
        text: "did that ship?".to_string(),
        by: None,
        chat: None,
        parent: None,
        deliverable: None,
        mentions: Vec::new(),
        attachments: Vec::new(),
    }];
    CycleRunner::new(&rt)
        .inject_handed_task_awareness(&record, &mut events, &rt.tasks().list(&id).await.unwrap())
        .await;
    let text = message_text(&events[0]);

    assert!(
        text.contains("Renew the domain"),
        "an unaddressed turn is owed the General desk's briefing: {text}"
    );
}

/// End to end through the injector: a turn answering in one thread is told
/// what else its channel is about, and told **not to read it**.
///
/// The gate is half the mechanism. Without it an agent pulls every thread
/// it is shown "to be safe", which rebuilds the flat channel window #1890 A
/// removed — in the prompt, and paid for twice.
#[tokio::test]
async fn a_threaded_turn_is_oriented_without_being_invited_to_read() {
    let home_dir = tmp_home();
    let rt = Arc::new(
        RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest("supervised"))
            .build()
            .await
            .unwrap(),
    );
    let id = rt.id().clone();
    for stored in [
        op(41, "growth", None, "draft the launch email"),
        agent_reply(42, "growth", 41),
        op(43, "growth", None, "what's our Q3 CAC?"),
    ] {
        rt.events().append(&id, stored.event).await.unwrap();
    }

    // Answering inside thread 41 — the seqs the fixture appended start at
    // 1, so the roots are whatever the log assigned; read them back.
    let page = rt.events().read_before(&id, None, 64).await.unwrap();
    let first_root = page
        .iter()
        .rev()
        .find_map(|e| match &e.event {
            CompanyEvent::OperatorMessage { parent: None, .. } => Some(e.seq),
            _ => None,
        })
        .expect("a root");

    let mut events = vec![operator_in_thread(
        "growth",
        Some(first_root.value()),
        "make it shorter",
    )];
    let record = rt.store.load(&id).await.unwrap().unwrap();
    CycleRunner::new(&rt)
        .inject_thread_index(&record, &mut events, &[])
        .await;
    let text = message_text(&events[0]);

    assert!(text.starts_with("make it shorter"), "{text}");
    assert!(text.contains(THREAD_INDEX_ANNOTATION), "{text}");
    assert!(
        text.contains("what's our Q3 CAC?"),
        "the other thread is named: {text}"
    );
    assert!(
        !text.contains("draft the launch email"),
        "but not the one being answered in: {text}"
    );
    assert!(
        text.contains("do NOT read or answer from them"),
        "the gate rides with the index or the index undoes A: {text}"
    );
}

/// A channel with no other thread gets no index at all — an empty briefing
/// is prompt budget spent to say nothing.
#[tokio::test]
async fn a_channel_with_nothing_else_open_gets_no_index() {
    let home_dir = tmp_home();
    let rt = Arc::new(
        RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest("supervised"))
            .build()
            .await
            .unwrap(),
    );
    let mut events = vec![operator_in_thread("growth", None, "anything happening?")];
    let record = rt.store.load(rt.id()).await.unwrap().unwrap();
    CycleRunner::new(&rt)
        .inject_thread_index(&record, &mut events, &[])
        .await;
    assert!(!message_text(&events[0]).contains(THREAD_INDEX_ANNOTATION));
}

/// A message never appears in its own index.
///
/// At channel level there is no thread to exclude, but the operator's
/// message is journaled before the cycle runs — so it is an unparented root
/// on the page, and without this the index shows a reader their own message
/// back as somebody else's conversation. Found by
/// `redeem_replays_the_markers_attachments`, which printed the index into
/// its failure message.
#[test]
fn a_message_is_not_listed_in_its_own_index() {
    let page = vec![
        op(41, "growth", None, "review the attached report"),
        op(43, "growth", None, "what's our Q3 CAC?"),
    ];
    let (lines, _) = thread_index(
        &page,
        "growth",
        "growth",
        None,
        "review the attached report",
        &[],
    );
    assert_eq!(
        lines.iter().map(|l| l.opening.clone()).collect::<Vec<_>>(),
        vec!["what's our Q3 CAC?"],
        "the message being answered is not one of its own other conversations"
    );
}

/// A thread whose work settled says where it landed — the question a reader
/// is actually asking, and answerable only because #1890 B records which
/// thread raised a card.
#[test]
fn a_thread_whose_work_settled_says_where_it_landed() {
    let page = vec![op(41, "growth", None, "draft the launch email")];
    let mut card = settled_card("t-1", "Draft the launch email");
    card.origin = TaskOrigin::new(Some("growth".to_string()), Some(EventSeq::new(41)));
    let settled = vec![&card];
    let (lines, _) = thread_index(&page, "growth", "growth", None, "", &settled);
    assert_eq!(
        lines[0].render(),
        format!(
            r#"- [{}] "draft the launch email" — finished → In review"#,
            41
        ),
        "state beats a reply count: it is what a reader is asking"
    );
}

/// Another channel's threads are another channel's business. An index that
/// crossed channels would be a wider leak than the one this epic closed.
#[test]
fn the_index_never_crosses_channels() {
    let page = vec![
        op(41, "growth", None, "draft the launch email"),
        op(42, "engineering", None, "the migration plan"),
    ];
    let (lines, _) = thread_index(&page, "growth", "growth", None, "", &[]);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].opening, "draft the launch email");
}

/// An agent reply never opens a thread — it is always parented to the
/// question it answers, so treating one as a root would invent a
/// conversation the operator never started.
#[test]
fn an_agent_reply_is_never_a_root() {
    let page = vec![
        op(41, "growth", None, "draft the launch email"),
        agent_reply(42, "growth", 41),
    ];
    let (lines, _) = thread_index(&page, "growth", "growth", None, "", &[]);
    assert_eq!(
        lines.len(),
        1,
        "one root, not two: {lines:?}",
        lines = lines.len()
    );
}
