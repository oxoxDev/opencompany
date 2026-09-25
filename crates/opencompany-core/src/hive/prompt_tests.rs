//! Tests for the seat prompt: sentinel, delta, catalogue, fence, and the
//! sharing walk.

use std::sync::Arc;

use super::*;
use crate::hive::session_log::EventLogSessionLog;
use crate::hive::test_support::{MemoryLog, agent_reply, agent_reply_in, operator_message};
use crate::ports::events::EventLog;
use crate::ports::types::CompanyEvent;
use tinyhivemind::aside::Audience;

fn message(seq: u64, author: SessionAuthor, text: &str) -> SessionMessage {
    SessionMessage {
        sequence: Sequence(seq),
        author,
        content: text.into(),
        audience: Audience::Desk,
        elided: None,
    }
}

#[test]
fn the_sentinel_is_line_one_and_names_desk_episode_and_round() {
    let delta = [message(4, SessionAuthor::Operator, "Ship the login page.")];
    let prompt = SeatPrompt {
        desk_id: "engineering",
        desk_name: "Engineering desk",
        episode_id: "ep-1",
        revision: 2,
        agent_id: "engineer",
        assignment: "Answer the operator.",
        delta: &delta,
        allowed: DESK_KINDS,
        retry_note: None,
    }
    .render();
    let first = prompt.lines().next().unwrap();
    assert_eq!(first, "Hive turn: desk engineering, episode ep-1, round 2.");
    assert_eq!(first, sentinel("engineering", "ep-1", 2));
    assert!(prompt.contains("operator (^4): Ship the login page."));
    assert!(prompt.contains("## This assignment\nAnswer the operator."));
    assert!(prompt.contains("`mcp_call_tool` on server `opencompany`"));
    assert!(prompt.contains("`post` | `broadcast` | `dm` | `complete_episode`"));
    assert!(prompt.contains("`message` argument"));
    assert!(prompt.contains("- `dm`:"));
    assert!(!prompt.contains("- `read`:"));
}

#[test]
fn a_solo_seat_is_offered_post_and_complete_only_and_a_retry_keeps_the_sentinel() {
    let note = retry_note(2);
    let prompt = SeatPrompt {
        desk_id: "d",
        desk_name: "D",
        episode_id: "e",
        revision: 0,
        agent_id: "a",
        assignment: "Go.",
        delta: &[],
        allowed: SOLO_KINDS,
        retry_note: Some(&note),
    }
    .render();
    assert!(prompt.starts_with("Hive turn: desk d, episode e, round 0.\n"));
    assert!(prompt.contains("## Reminder\nYour previous answer (attempt 2)"));
    assert!(prompt.contains("(none)"));
    assert!(prompt.contains("tool `post` | `complete_episode`,"));
    // Not offered. The word still appears inside `post`'s own description,
    // which contrasts the two, so this checks the catalogue line rather than
    // the whole prompt.
    assert!(!prompt.contains("- `broadcast`:"));
}

#[test]
fn the_delta_renders_attributed_lines_and_keeps_elisions_visible() {
    let messages = [
        message(1, SessionAuthor::Operator, "Q"),
        message(
            2,
            SessionAuthor::Agent {
                id: "ceo".into(),
                label: "Cee".into(),
            },
            "A",
        ),
        message(
            3,
            SessionAuthor::System {
                kind: "hive-referral".into(),
                label: "hive-referral".into(),
            },
            "answer",
        ),
        SessionMessage {
            sequence: Sequence(4),
            author: SessionAuthor::Agent {
                id: "writer".into(),
                label: "writer".into(),
            },
            content: String::new(),
            audience: Audience::Aside {
                members: vec!["ceo".into()],
            },
            elided: Some(tinyhivemind::Elision {
                through: Sequence(5),
                messages: 2,
                settled_at: None,
            }),
        },
    ];
    let rendered = render_delta(&messages);
    assert_eq!(
        rendered,
        "operator (^1): Q\n@ceo (^2): A\n[hive-referral] (^3): answer\n@writer (^4–^5): [2 private line(s) you may not read]"
    );
    assert_eq!(non_desk_prefix(&[]), None);
    assert!(
        non_desk_prefix(&messages[..1])
            .unwrap()
            .starts_with("## Since your last turn here\noperator (^1): Q")
    );
}

#[tokio::test]
async fn the_sharing_walk_hands_a_window_first_and_only_the_delta_afterwards() {
    let log = Arc::new(MemoryLog::default());
    let company = MemoryLog::company();
    log.append(&company, operator_message("engineering", "Plan it.", None))
        .await
        .unwrap();
    log.append(&company, agent_reply("engineering", "ceo", "On it."))
        .await
        .unwrap();
    // A private line the engineer may not read, and one for it.
    log.append(
        &company,
        agent_reply_in(
            "engineering",
            "ceo",
            "psst writer",
            vec!["writer".into()],
            None,
        ),
    )
    .await
    .unwrap();
    log.append(
        &company,
        agent_reply_in(
            "engineering",
            "ceo",
            "psst engineer",
            vec!["engineer".into()],
            None,
        ),
    )
    .await
    .unwrap();
    let round_started = log
        .append(
            &company,
            CompanyEvent::RoundStarted {
                chat_id: "engineering".into(),
                episode_id: "ep".into(),
                revision: 0,
                agent_ids: vec!["engineer".into()],
            },
        )
        .await
        .unwrap();
    let adapter = EventLogSessionLog::new(
        Arc::clone(&log) as Arc<dyn EventLog>,
        company.clone(),
        "engineering".into(),
        "Engineering desk".into(),
        Vec::new(),
    );
    let conversation = adapter.conversation(None);
    let viewer = Viewer::Agent {
        id: "engineer".into(),
    };
    let before = Sequence(round_started.value());
    let (window, state) = delta_for(&adapter, &conversation, &viewer, None, before)
        .await
        .unwrap();
    let texts: Vec<Option<&str>> = window.iter().map(SessionMessage::readable).collect();
    assert_eq!(
        texts,
        vec![
            Some("Plan it."),
            Some("On it."),
            None,
            Some("psst engineer")
        ]
    );
    assert_eq!(state.watermark, before);

    // Nothing new: an empty delta, same watermark.
    let (delta, next) = delta_for(&adapter, &conversation, &viewer, Some(&state), before)
        .await
        .unwrap();
    assert!(delta.is_empty());
    assert_eq!(next.watermark, before);

    // One more row, then a later round: exactly that row.
    log.append(&company, agent_reply("engineering", "ceo", "Done."))
        .await
        .unwrap();
    let later = log
        .append(
            &company,
            CompanyEvent::RoundStarted {
                chat_id: "engineering".into(),
                episode_id: "ep".into(),
                revision: 1,
                agent_ids: vec!["engineer".into()],
            },
        )
        .await
        .unwrap();
    let (delta, next) = delta_for(
        &adapter,
        &conversation,
        &viewer,
        Some(&state),
        Sequence(later.value()),
    )
    .await
    .unwrap();
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].readable(), Some("Done."));
    assert_eq!(next.watermark, Sequence(later.value()));
}
