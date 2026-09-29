use super::*;
use crate::ports::types::CompanyId;

use super::referral_origin_test_support::*;

/// **Repeated crossings to the same person do not swallow each other.**
///
/// `pair_conversation` is deterministic, so two questions to the same
/// teammate share one `dm:<a>+<b>` thread. Collecting to the end of the
/// page gave the FIRST crossing every row the pair went on to exchange: one
/// live episode rendered the same conversation five times in a single
/// thread, labelled 20, 16, 12, 8 and 4 messages, and only the last was
/// true.
#[tokio::test]
async fn two_crossings_to_one_person_each_fold_their_own_exchange() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");
    let pair = crate::hive::referral::pair_conversation("software_engineer", "researcher");

    let ask = |text: &str| CompanyEvent::AgentReply {
        chat_id: "engineering".to_string(),
        agent_id: "software_engineer".to_string(),
        text: text.to_string(),
        steps: Vec::new(),
        task_id: None,
        outputs: Vec::new(),
        parent: None,
        mentions: Vec::new(),
        mention_depth: 0,
        audience: Vec::new(),
        episode: None,
    };
    let pair_row = |who: &str, text: &str| CompanyEvent::AgentReply {
        chat_id: pair.clone(),
        agent_id: who.to_string(),
        text: text.to_string(),
        steps: Vec::new(),
        task_id: None,
        outputs: Vec::new(),
        parent: None,
        mentions: Vec::new(),
        mention_depth: 0,
        audience: Vec::new(),
        episode: None,
    };
    // The marker names the row its crossing folds onto, so each points at
    // its own ask rather than a constant.
    let marker = |trigger: EventSeq| CompanyEvent::ReferralEnqueued {
        conversation: Some(pair.clone()),
        answers: None,
        from_desk: "engineering".to_string(),
        from_desk_name: "Engineering".to_string(),
        asker: "software_engineer".to_string(),
        asker_label: "software_engineer".to_string(),
        trigger_sequence: trigger.value(),
        to_desk: "engineering".to_string(),
        target: "researcher".to_string(),
        returning: false,
        rows: None,
        episode_id: None,
        to_episode_id: None,
        hop: 0,
    };

    // Two crossings to the same person, each with its own two-row exchange.
    let first_ask = runtime
        .events()
        .append(&id, ask("!question do we have latency numbers?"))
        .await
        .expect("journal");
    for event in [
        marker(first_ask),
        pair_row("software_engineer", "do we have latency numbers?"),
        pair_row("researcher", "p99 is 400ms"),
    ] {
        runtime.events().append(&id, event).await.expect("journal");
    }
    let second_ask = runtime
        .events()
        .append(&id, ask("!question and the write path?"))
        .await
        .expect("journal");
    for event in [
        marker(second_ask),
        pair_row("software_engineer", "and the write path?"),
        pair_row("researcher", "unmeasured so far"),
    ] {
        runtime.events().append(&id, event).await.expect("journal");
    }

    let history = history_for_desk(
        &runtime,
        "engineering",
        "engineering",
        &Viewer::Operator,
        None,
        50,
        true,
    )
    .await
    .expect("history");
    let fold = |seq: EventSeq| {
        history
            .iter()
            .find(|m| m.id == seq.value().to_string())
            .and_then(|m| m.referral_conversation.as_ref())
            .unwrap_or_else(|| panic!("a crossing folds onto {seq:?}"))
    };

    let first = fold(first_ask);
    assert_eq!(
        first.lines.len(),
        2,
        "the first crossing holds only its own exchange: {:?}",
        first.lines
    );
    assert!(
        first
            .lines
            .iter()
            .all(|line| !line.text.contains("write path")),
        "a later question is not part of an earlier crossing: {:?}",
        first.lines
    );
    let second = fold(second_ask);
    assert_eq!(second.lines.len(), 2, "{:?}", second.lines);
    assert!(
        second
            .lines
            .iter()
            .any(|line| line.text.contains("write path")),
        "{:?}",
        second.lines
    );
}

/// **A crossing that convened the far desk folds what that desk SAID.**
///
/// The collapsed crossing exists so an operator can read the exchange
/// rather than the asker's paraphrase of it. A deliberated crossing has a
/// whole conversation to show — every turn journaled on the far desk — and
/// folding only the conclusion showed none of it: "asked #design ·
/// 2 messages" over a question and one summary, for a room that ran three
/// turns across both seats.
///
/// The question is unwrapped too. A room has to READ the question to
/// deliberate on it, so the whole referral prompt is journaled on that
/// desk, and the matcher finds that row — rendering "…has asked you a
/// question. Answer it from what you and this desk know." where the ask
/// belongs.
#[tokio::test]
async fn a_convened_desks_own_turns_are_the_folded_crossing() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    let asked = runtime
        .events()
        .append(
            &id,
            CompanyEvent::AgentReply {
                chat_id: "engineering".to_string(),
                agent_id: "software_engineer".to_string(),
                text: "!question @#design ^2 can the error messages be redone?".to_string(),
                steps: Vec::new(),
                task_id: None,
                outputs: Vec::new(),
                parent: None,
                mentions: Vec::new(),
                mention_depth: 0,
                audience: Vec::new(),
                episode: None,
            },
        )
        .await
        .expect("journal");
    let forward = runtime
        .events()
        .append(
            &id,
            CompanyEvent::ReferralEnqueued {
                conversation: None,
                answers: None,
                from_desk: "engineering".to_string(),
                from_desk_name: "Engineering".to_string(),
                asker: "software_engineer".to_string(),
                asker_label: "software_engineer".to_string(),
                trigger_sequence: asked.value(),
                to_desk: "design".to_string(),
                target: "product_designer".to_string(),
                returning: false,
                rows: None,
                episode_id: None,
                to_episode_id: None,
                hop: 0,
            },
        )
        .await
        .expect("journal");
    // The prompt the room was handed, on the desk being asked — the row a
    // deliberated crossing journals, the one the matcher finds, and the
    // thread every turn of that room is rooted on.
    let root = runtime
        .events()
        .append(
            &id,
            CompanyEvent::OperatorMessage {
                text: "@software_engineer on #Engineering asks: can the error messages be redone?"
                    .to_string(),
                by: Some(Actor {
                    kind: ActorKind::Agent,
                    id: "software_engineer".to_string(),
                }),
                chat: Some("design".to_string()),
                parent: None,
                deliverable: None,
                mentions: Vec::new(),
                attachments: Vec::new(),
            },
        )
        .await
        .expect("journal");
    // The room: both seats, plus its closing row, which is the desk's own
    // bookkeeping and never a line somebody said. Every one parented on the
    // question, as a referred episode journals them.
    let mut events: Vec<CompanyEvent> = [
        (
            "product_designer",
            "!propose #copy they read like a copy task",
        ),
        ("researcher", "!support #copy ^1 and the tests agree"),
        // A legacy closing row (the trace-grammar hive's), never a line
        // somebody said: dropped from the fold as it is from the transcript.
        ("hive-report", "The desk settled."),
    ]
    .into_iter()
    .map(|(agent, text)| CompanyEvent::AgentReply {
        chat_id: "design".to_string(),
        agent_id: agent.to_string(),
        text: text.to_string(),
        steps: Vec::new(),
        task_id: None,
        outputs: Vec::new(),
        parent: Some(root),
        mentions: Vec::new(),
        mention_depth: 0,
        audience: Vec::new(),
        episode: None,
    })
    .collect();
    // **Concurrent traffic on the same desk, inside the same window.** A
    // desk that was asked keeps working while the referred room runs, and a
    // fold scoped by sequence interval alone renders this as part of the
    // crossing.
    events.push(CompanyEvent::AgentReply {
        chat_id: "design".to_string(),
        agent_id: "product_designer".to_string(),
        text: "unrelated: the icon set ships Thursday".to_string(),
        steps: Vec::new(),
        task_id: None,
        outputs: Vec::new(),
        parent: None,
        mentions: Vec::new(),
        mention_depth: 0,
        audience: Vec::new(),
        episode: None,
    });
    for event in events {
        runtime.events().append(&id, event).await.expect("journal");
    }
    // The return, naming the forward it answers, carrying the room's note.
    for event in [
        CompanyEvent::ReferralEnqueued {
            conversation: None,
            answers: Some(forward.value()),
            from_desk: "design".to_string(),
            from_desk_name: "Design".to_string(),
            asker: "product_designer".to_string(),
            asker_label: "product_designer".to_string(),
            trigger_sequence: asked.value(),
            to_desk: "engineering".to_string(),
            target: "software_engineer".to_string(),
            returning: true,
            rows: None,
            episode_id: None,
            to_episode_id: None,
            hop: 0,
        },
        CompanyEvent::AgentReply {
            chat_id: "engineering".to_string(),
            agent_id: crate::hive::referral::HIVE_REFERRAL_AUTHOR.to_string(),
            text: crate::hive::referral::returned_note(
                "product_designer",
                "Design",
                "The desk settled.",
            ),
            steps: Vec::new(),
            task_id: None,
            outputs: Vec::new(),
            parent: None,
            mentions: Vec::new(),
            mention_depth: 0,
            audience: Vec::new(),
            episode: None,
        },
        // The asker's own report, which the crossing folds onto.
        CompanyEvent::AgentReply {
            chat_id: "engineering".to_string(),
            agent_id: "software_engineer".to_string(),
            text: "design says it is a copy problem".to_string(),
            steps: Vec::new(),
            task_id: None,
            outputs: Vec::new(),
            parent: None,
            mentions: Vec::new(),
            mention_depth: 0,
            audience: Vec::new(),
            episode: None,
        },
    ] {
        runtime.events().append(&id, event).await.expect("journal");
    }

    let history = history_for_desk(
        &runtime,
        "engineering",
        "engineering",
        &Viewer::Operator,
        None,
        50,
        true,
    )
    .await
    .expect("history");
    let crossing = history
        .iter()
        .find_map(|m| m.referral_conversation.as_ref())
        .expect("the crossing rides the asker's report");

    assert_eq!(
        crossing.lines.len(),
        3,
        "the question and both seats that answered it: {:?}",
        crossing.lines
    );
    let question = &crossing.lines[0];
    assert!(question.outbound);
    assert_eq!(
        question.text, "can the error messages be redone?",
        "the ask, not the instructions wrapped around it"
    );
    assert_eq!(crossing.lines[1].author_id, "product_designer");
    assert_eq!(crossing.lines[2].author_id, "researcher");
    assert!(
        crossing.lines[1..].iter().all(|line| !line.outbound),
        "both came back from the other desk"
    );
    assert!(
        crossing
            .lines
            .iter()
            .all(|line| !line.text.contains("The desk settled")),
        "the closing row is the desk's bookkeeping, not a line anybody said: {:?}",
        crossing.lines
    );
    assert!(
        crossing
            .lines
            .iter()
            .all(|line| !line.text.contains("answered the question")),
        "and the relayed note is dropped — it summarises exactly these lines: {:?}",
        crossing.lines
    );
    assert!(
        crossing
            .lines
            .iter()
            .all(|line| !line.text.contains("icon set")),
        "and a reply this desk made outside the crossing is not part of it: {:?}",
        crossing.lines
    );
}

/// **Another pair's crossing does not end this one's window.**
///
/// The child search is bounded so a crossing that FAILED cannot latch onto
/// its target's next unrelated reply. `page` is the company's journal
/// though, not this pair's, so bounding at the next marker of ANY kind let
/// a third desk's crossing end the window — and a child journaled after it
/// was skipped, dropping an answered crossing from the projection
/// altogether. Matching `to_desk` alone is not enough either: two desks can
/// ask the same one (Codex and CodeRabbit both, #2332).
#[tokio::test]
async fn an_unrelated_pairs_marker_does_not_end_this_crossings_window() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    let marker = |from: &str, to: &str, asker: &str, target: &str| CompanyEvent::ReferralEnqueued {
        conversation: None,
        answers: None,
        from_desk: from.to_string(),
        from_desk_name: from.to_string(),
        asker: asker.to_string(),
        asker_label: asker.to_string(),
        trigger_sequence: 1,
        to_desk: to.to_string(),
        target: target.to_string(),
        returning: false,
        rows: None,
        episode_id: None,
        to_episode_id: None,
        hop: 0,
    };
    // This crossing: engineering asks design.
    runtime
        .events()
        .append(
            &id,
            marker(
                "engineering",
                "design",
                "software_engineer",
                "product_designer",
            ),
        )
        .await
        .expect("journal");
    // A crossing between two entirely unrelated desks, interleaved BEFORE
    // our child lands. It ends no window of ours — but the unscoped bound
    // stopped here, so the child below was never reached.
    runtime
        .events()
        .append(&id, marker("sales", "triage", "ae", "triager"))
        .await
        .expect("journal");
    // Our child: the target's own turn on the desk that was asked.
    runtime
        .events()
        .append(
            &id,
            CompanyEvent::AgentReply {
                chat_id: "design".to_string(),
                agent_id: "product_designer".to_string(),
                text: "they read like a copy task".to_string(),
                steps: Vec::new(),
                task_id: None,
                outputs: Vec::new(),
                parent: None,
                mentions: Vec::new(),
                mention_depth: 0,
                audience: Vec::new(),
                episode: None,
            },
        )
        .await
        .expect("journal");

    let history = history_for_desk(
        &runtime,
        "design",
        "design",
        &Viewer::Operator,
        None,
        50,
        true,
    )
    .await
    .expect("history");
    let answered = history
        .iter()
        .find(|m| m.referred_from.is_some())
        .expect("the crossing still finds its child past another pair's marker");
    assert_eq!(
        answered.referred_from.as_ref().expect("origin").desk_id,
        "engineering",
        "and it is attributed to the desk that actually asked"
    );
    assert_eq!(
        answered.text, "they read like a copy task",
        "the child is the target's own turn, found past the unrelated marker"
    );
}
