use super::*;
use crate::ports::types::{CompanyId, ReplyEpisode, UtteranceKind};

use super::referral_origin_test_support::*;

/// The conclusion the conductor mints when an exchange ends, as the host
/// journals it: a `Dm` to the asker, threaded under the ask.
fn conclusion_row(chat: &str, who: &str, asker: &str, text: &str, root: EventSeq) -> CompanyEvent {
    let mut event = pair_row(chat, who, text, Some(root));
    if let CompanyEvent::AgentReply {
        audience, episode, ..
    } = &mut event
    {
        *audience = vec![asker.to_string()];
        *episode = Some(ReplyEpisode {
            id: "ep-1".into(),
            revision: 1,
            kind: UtteranceKind::Dm,
            to: vec![asker.to_string()],
            routed_by: None,
        });
    }
    event
}

/// A seat that steps aside to ask another seat writes the exchange to the pair
/// channel, so the desk never sees it. The reference rows are all this desk
/// gets, and what they have to be turned back into is the exchange, folded
/// onto the row that reported it.
///
/// The shape below is taken from a live run rather than invented: two seats on
/// one desk asked each other inside a single episode, which is the ordinary
/// case and not a corner one.
fn pair_row(chat: &str, who: &str, text: &str, parent: Option<EventSeq>) -> CompanyEvent {
    CompanyEvent::AgentReply {
        chat_id: chat.to_string(),
        agent_id: who.to_string(),
        text: text.to_string(),
        steps: Vec::new(),
        task_id: None,
        outputs: Vec::new(),
        parent,
        mentions: Vec::new(),
        mention_depth: 0,
        audience: Vec::new(),
        episode: None,
    }
}

/// **Two conversations in one channel do not swallow each other.**
///
/// `pair_conversation` is deterministic, so both directions of an exchange
/// between the same two seats share one `dm:<a>+<b>` channel — and inside one
/// episode they interleave there. Collecting the channel's rows by channel
/// alone gives each conversation the other's rows too: both render four
/// messages where each had two, both look plausible, and nothing on screen
/// says otherwise. The exchange is the ask row and the rows rooted at it.
#[tokio::test]
async fn each_exchange_folds_its_own_rows_onto_the_row_that_reported_it() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");
    let pair = crate::hive::referral::pair_conversation("planner", "strategist");
    let append = |event: CompanyEvent| {
        let runtime = Arc::clone(&runtime);
        let id = id.clone();
        async move { runtime.events().append(&id, event).await.expect("journal") }
    };

    // The desk row that sent them aside.
    let prompt = append(pair_row(
        "engineering",
        "strategist",
        "Plan the rollout.",
        None,
    ))
    .await;

    // Strategist asks the planner; the ask itself is written to the pair
    // channel, which is why `root` names a row this desk does not have.
    let ask_a = append(pair_row(
        &pair,
        "strategist",
        "sequencing constraints?",
        Some(prompt),
    ))
    .await;
    append(CompanyEvent::ConversationOpened {
        chat_id: "engineering".into(),
        episode_id: "ep-1".into(),
        conversation_id: pair.clone(),
        root: ask_a.value(),
        asker: "strategist".into(),
        askee: "planner".into(),
    })
    .await;
    // And the planner asks back, into the same channel.
    let ask_b = append(pair_row(
        &pair,
        "planner",
        "what stage is Acme at?",
        Some(prompt),
    ))
    .await;
    append(CompanyEvent::ConversationOpened {
        chat_id: "engineering".into(),
        episode_id: "ep-1".into(),
        conversation_id: pair.clone(),
        root: ask_b.value(),
        asker: "planner".into(),
        askee: "strategist".into(),
    })
    .await;

    // The answers interleave, and only `parent` says which exchange each is in.
    append(pair_row(
        &pair,
        "strategist",
        "nothing on record.",
        Some(ask_b),
    ))
    .await;
    append(pair_row(&pair, "planner", "clean slate then.", Some(ask_a))).await;
    // The conclusion is threaded under the ask too, so a seat's thread read
    // reaches it. It restates the planner's last line, and the widget must
    // not say it twice.
    append(conclusion_row(
        &pair,
        "planner",
        "strategist",
        "concluded our conversation: clean slate then.",
        ask_a,
    ))
    .await;

    // The conclusion is journaled, and the asker reports back after it --
    // the order a live run produces, and the order the fold anchors on.
    append(CompanyEvent::ConversationConcluded {
        chat_id: "engineering".into(),
        episode_id: "ep-1".into(),
        conversation_id: pair.clone(),
        root: ask_a.value(),
        asker: "strategist".into(),
        askee: "planner".into(),
        forced: false,
    })
    .await;
    let report_a = append(pair_row(
        "engineering",
        "strategist",
        "asked the planner: clean slate.",
        Some(prompt),
    ))
    .await;
    append(CompanyEvent::ConversationConcluded {
        chat_id: "engineering".into(),
        episode_id: "ep-1".into(),
        conversation_id: pair,
        root: ask_b.value(),
        asker: "planner".into(),
        askee: "strategist".into(),
        forced: false,
    })
    .await;
    let report_b = append(pair_row(
        "engineering",
        "planner",
        "asked the strategist: nothing on record.",
        Some(prompt),
    ))
    .await;

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
            .and_then(|m| m.agent_conversations.first())
            .unwrap_or_else(|| panic!("an exchange folds onto {seq:?}"))
    };

    // The pair channel holds four rows; each exchange owns exactly two of them.
    let a = fold(report_a);
    assert_eq!(a.asker_id, "strategist");
    assert_eq!(a.askee_id, "planner");
    assert!(a.concluded && !a.forced);
    assert_eq!(
        a.lines
            .iter()
            .map(|l| (l.author_id.as_str(), l.outbound))
            .collect::<Vec<_>>(),
        vec![("strategist", true), ("planner", false)],
        "the ask and the row rooted at it, and nothing from the other exchange",
    );

    let b = fold(report_b);
    assert_eq!(b.asker_id, "planner");
    assert_eq!(b.askee_id, "strategist");
    assert_eq!(
        b.lines
            .iter()
            .map(|l| (l.author_id.as_str(), l.outbound))
            .collect::<Vec<_>>(),
        vec![("planner", true), ("strategist", false)],
    );

    // The ask rows themselves are not on this desk, so nothing folds onto them.
    assert!(history.iter().all(|m| m.id != ask_a.value().to_string()));
}

/// An exchange still running has no report to fold onto yet, so it folds onto
/// the row that sent the seats aside — and says so in the present tense by
/// carrying `concluded: false`.
#[tokio::test]
async fn a_running_exchange_folds_onto_the_row_that_sent_them_aside() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");
    let pair = crate::hive::referral::pair_conversation("planner", "strategist");
    let append = |event: CompanyEvent| {
        let runtime = Arc::clone(&runtime);
        let id = id.clone();
        async move { runtime.events().append(&id, event).await.expect("journal") }
    };

    let prompt = append(pair_row(
        "engineering",
        "strategist",
        "Plan the rollout.",
        None,
    ))
    .await;
    let ask = append(pair_row(&pair, "strategist", "sequencing?", Some(prompt))).await;
    append(CompanyEvent::ConversationOpened {
        chat_id: "engineering".into(),
        episode_id: "ep-1".into(),
        conversation_id: pair.clone(),
        root: ask.value(),
        asker: "strategist".into(),
        askee: "planner".into(),
    })
    .await;
    append(pair_row(&pair, "planner", "still looking.", Some(ask))).await;

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
    let held = history
        .iter()
        .find(|m| m.id == prompt.value().to_string())
        .and_then(|m| m.agent_conversations.first())
        .expect("a running exchange folds onto the row that provoked it");
    assert!(!held.concluded, "it has not ended");
    assert_eq!(held.lines.len(), 2);
}
