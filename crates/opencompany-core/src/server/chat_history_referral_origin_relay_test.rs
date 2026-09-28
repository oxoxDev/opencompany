use super::*;
use crate::ports::types::CompanyId;

use super::referral_origin_test_support::*;

/// **The desk that was ASKED can read the question it answered.**
///
/// A room crossing journals the question on the asking desk — the asker's
/// own committed line — and nothing at all on the desk it goes to; the far
/// seat simply takes a turn there. So the answering side rendered an answer
/// to a question nobody on that desk could see: `product_designer`
/// explaining what they would change, over a chip reading "Asked by
/// @software_engineer", and the question itself one desk away.
///
/// Folded with the roles swapped (`inbound`), because every other field on
/// a crossing is named from the asking side. One line, not two: the row it
/// hangs on is this desk's answer already.
#[tokio::test]
async fn the_answering_desk_carries_the_question_it_was_asked() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    // The ask, as it was said — a committed MOVE on the asking desk, which
    // is the only place a room crossing's question exists. Its sequence is
    // read back from the append rather than assumed: the marker points at
    // this row, and a runtime that has journalled anything at boot makes
    // any guess wrong.
    let asked = runtime
        .events()
        .append(
            &id,
            CompanyEvent::AgentReply {
                chat_id: "engineering".to_string(),
                agent_id: "software_engineer".to_string(),
                text: "@#design can the error messages be redone?".to_string(),
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
    for event in [
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
        // The far seat's turn, on its own desk and under its own id — the
        // only row this crossing leaves here.
        CompanyEvent::AgentReply {
            chat_id: "design".to_string(),
            agent_id: "product_designer".to_string(),
            text: "they read like a copy task and are not one".to_string(),
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
        .expect("the answering turn carries the provenance");
    assert_eq!(
        answered.text, "they read like a copy task and are not one",
        "the desk's own answer is still the row"
    );

    let crossing = answered
        .referral_conversation
        .as_ref()
        .expect("and the question it answered rides it");
    assert!(
        crossing.inbound,
        "this desk was asked; the label reads \"asked by\" rather than \"asked\""
    );
    assert_eq!(
        crossing.asker_id, "product_designer",
        "the local side first"
    );
    assert_eq!(crossing.other_id, "software_engineer");
    assert_eq!(crossing.other_desk_id, "engineering");
    assert_eq!(crossing.other_desk_name, "Engineering");
    assert_eq!(
        crossing.lines.len(),
        1,
        "the answer is the row, so folding it too would print it twice: {:?}",
        crossing.lines
    );
    let question = &crossing.lines[0];
    assert!(!question.outbound, "the question came IN to this desk");
    assert_eq!(question.author_id, "software_engineer");
    assert!(
        question.text.contains("can the error messages be redone?"),
        "{:?}",
        question.text
    );
    assert!(
        !question.text.starts_with('!'),
        "a seat speaks in prose; nothing is rewritten for a reader: {:?}",
        question.text
    );
}

/// **The marker names its own forward, so the ask is found however far back
/// it is.**
///
/// The scan this replaces looked back a fixed number of events from the
/// oldest visible row, so a crossing whose ask fell outside that window
/// rendered with the answer alone and said "1 message" — quietly wrong, and
/// wrong in the direction that looks plausible. The host already located
/// that marker to authorize the return and was keeping only a bool;
/// `answers` records it instead.
///
/// Here the two legs are separated by far more than the scan's `LOOKBACK`,
/// so the fallback cannot reach the ask and only the pointer can.
#[tokio::test]
async fn a_marker_that_names_its_forward_pairs_beyond_the_scan_window() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    // The marker's OWN sequence, not a guess at it: the runtime journals
    // its own setup rows first, so the first referral event is not
    // sequence zero. Pointing `answers` at a sequence that happens to hold
    // something else is the confusion the pointer exists to remove.
    let mut forward_seq = 0u64;
    for (i, event) in referral_leg(
        "engineering",
        "Engineering",
        "software_engineer",
        "design",
        "product_designer",
        false,
        "what would you change about the error messages?",
    )
    .into_iter()
    .enumerate()
    {
        let seq = runtime.events().append(&id, event).await.expect("journal");
        if i == 0 {
            forward_seq = seq.value();
        }
    }
    // The forward marker is sequence 0, its question 1. Bury them under
    // enough unrelated traffic that the scan's window cannot reach back.
    for i in 0..200 {
        runtime
            .events()
            .append(
                &id,
                CompanyEvent::AgentReply {
                    chat_id: "engineering".to_string(),
                    agent_id: "software_engineer".to_string(),
                    text: format!("unrelated line {i}"),
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
    }
    for event in referral_leg_answering(
        "design",
        "Design",
        "product_designer",
        "engineering",
        "software_engineer",
        true,
        "error messages look like a copy task and are not one",
        Some(forward_seq),
    ) {
        runtime.events().append(&id, event).await.expect("journal");
    }
    runtime
        .events()
        .append(
            &id,
            CompanyEvent::AgentReply {
                chat_id: "engineering".to_string(),
                agent_id: "software_engineer".to_string(),
                text: "design came back: it is a design-system problem".to_string(),
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

    // Only the tail is on screen, so the ask is far outside the scan.
    let history = history_for_desk(
        &runtime,
        "engineering",
        "engineering",
        &Viewer::Operator,
        None,
        5,
        true,
    )
    .await
    .expect("history");
    let crossing = history
        .iter()
        .find_map(|m| m.referral_conversation.as_ref())
        .expect("the crossing rides the report");
    assert_eq!(
        crossing.lines.len(),
        2,
        "the pointer reaches an ask the scan cannot: {:?}",
        crossing.lines
    );
    assert!(crossing.lines[0].outbound);
    assert_eq!(
        crossing.lines[0].text,
        "what would you change about the error messages?"
    );
}

/// **A rendered relay shows the answer and none of the host's note.**
///
/// Seen in the console, not reasoned about: the asker's turn died on an
/// empty model response, the fallback rendered the relay, and #engineering
/// was told "you are the only one who has seen it" by the design desk's
/// agent. The note is written FOR the asker and is private to it; the
/// fallback exists to preserve the ANSWER, so that is all it may publish.
#[tokio::test]
async fn a_rendered_relay_keeps_the_answer_and_drops_the_note() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    let answer = "use a skeleton, not a spinner";
    let note = format!(
        "{}product_designer on the Design desk answered what you asked them. \
         This did not appear in your channel — you are the only one who has seen it.",
        crate::ports::types::RELAY_NOTE_MARKER
    );
    // No reply follows, so the fallback renders this relay.
    for event in referral_leg(
        "design",
        "Design",
        "product_designer",
        "engineering",
        "software_engineer",
        true,
        &format!("{answer}{note}"),
    ) {
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
    let relayed = history
        .iter()
        .find(|m| m.referred_from.is_some())
        .expect("the relay renders, because nothing else carries the answer");

    assert_eq!(
        relayed.text, answer,
        "the other desk's own words, and only those"
    );
    assert!(
        !relayed.text.contains("only one who has seen it"),
        "a note addressed to the asker is not published to the channel"
    );
}

/// **The fail-safe half: a relay renders while the report is still missing.**
///
/// The test below drops the relay once the asker has reported. Until then
/// there is nothing else carrying design's answer, and dropping it would
/// lose the answer outright — so it renders, in the wrong voice, saying
/// truthfully that it is an answer rather than an ask.
///
/// **Which leg this is, is the host's to say (the "Answered by" chip).**
///
/// Both legs are agent-authored lines on a desk, so every signal the
/// console holds reads identically on each — it guessed from `from` and
/// called every returning answer an ask. `tinyhivemind` decided it already
/// (`ReferralKind`), and the marker carries that decision.
#[tokio::test]
async fn a_relay_with_no_report_yet_still_renders_and_says_it_is_an_answer() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    for event in referral_leg(
        "engineering",
        "Engineering",
        "software_engineer",
        "design",
        "product_designer",
        false,
        "what would you change about the error messages?",
    )
    .into_iter()
    .chain(referral_leg(
        "design",
        "Design",
        "product_designer",
        "engineering",
        "software_engineer",
        true,
        "error messages look like a copy task and are not one",
    )) {
        runtime.events().append(&id, event).await.expect("journal");
    }

    for (desk, desk_name, returning) in [
        ("design", "Engineering", false),
        ("engineering", "Design", true),
    ] {
        let history = history_for_desk(&runtime, desk, desk, &Viewer::Operator, None, 50, true)
            .await
            .expect("history");
        let origin = history
            .iter()
            .find_map(|m| m.referred_from.as_ref())
            .unwrap_or_else(|| panic!("#{desk} carries a referral origin"));
        assert_eq!(origin.desk_name, desk_name, "on #{desk}");
        assert_eq!(
            origin.returning,
            returning,
            "#{desk} draws the {} chip",
            if returning {
                "\"Answered by\""
            } else {
                "\"Asked by\""
            }
        );
    }
}
