use super::*;
use crate::ports::types::CompanyId;

use super::referral_origin_test_support::*;

/// **An episode's turns are the conversation; its closing row is not.**
///
/// The room journals every turn as an ordinary reply by the teammate that
/// took it, then one summary under `hive-report`. Rendered, that summary
/// appeared as a *teammate* — a participant in a channel where no such
/// teammate exists and none can, since the id is hyphenated exactly so no
/// roster id can equal it. The fold already reads it as `System`; this
/// makes the console agree.
///
/// The turns must survive: dropping the room and keeping only its summary
/// would hide the reasoning, the losing options and every objection — the
/// one thing a room produces that a single answer cannot.
#[tokio::test]
async fn an_episodes_turns_render_but_its_closing_row_does_not() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    for (agent, text) in [
        (
            "software_engineer",
            "!propose #lazy-load defer each section",
        ),
        (
            "junior_engineer",
            "!object >1 ^1 users bounce between sections",
        ),
        (
            "hive-report",
            "The desk settled after 2 turns (#lazy-load, backed by software_engineer): defer each section",
        ),
    ] {
        runtime
            .events()
            .append(
                &id,
                CompanyEvent::AgentReply {
                    chat_id: "engineering".to_string(),
                    agent_id: agent.to_string(),
                    text: text.to_string(),
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

    let voices: Vec<&str> = history.iter().map(|m| m.channel.as_str()).collect();
    assert!(
        voices.contains(&"software_engineer") && voices.contains(&"junior_engineer"),
        "every teammate's turn is on screen, the objection included: {voices:?}"
    );
    assert!(
        !voices.contains(&"hive-report"),
        "and the room's own bookkeeping is not a participant in it: {voices:?}"
    );
}

/// **A suppressed row must not shorten the page.**
///
/// Filtered after the page was assembled, an episode's closing row silently
/// cost the reader a message: a page asked for `n` came back with `n - 1`,
/// and the row that should have taken its place stayed unfetched. The
/// admission point already excludes an admin-only row for exactly this
/// reason, and says so.
#[tokio::test]
async fn a_suppressed_report_does_not_shorten_the_page() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    // Four teammate turns with the room's closing row in the middle.
    for (agent, text) in [
        ("software_engineer", "first"),
        ("junior_engineer", "second"),
        ("hive-report", "The desk settled."),
        ("qa_engineer", "third"),
        ("software_engineer", "fourth"),
    ] {
        runtime
            .events()
            .append(
                &id,
                CompanyEvent::AgentReply {
                    chat_id: "engineering".to_string(),
                    agent_id: agent.to_string(),
                    text: text.to_string(),
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

    let page = history_for_desk(
        &runtime,
        "engineering",
        "engineering",
        &Viewer::Operator,
        None,
        4,
        true,
    )
    .await
    .expect("history");

    assert_eq!(
        page.len(),
        4,
        "a page of four is four teammate turns, not three and a hole: {page:?}"
    );
    assert!(
        page.iter().all(|m| m.channel != "hive-report"),
        "and none of them is the room's bookkeeping: {page:?}"
    );
}

/// **Both legacy bookkeeping rows stay out of the room.** The trace-grammar
/// hive wrote a closing report and a failure notice under reserved authors;
/// nothing writes them now (a failed seat turn is a `turn_settled` frame and
/// a run row), and a journal that still carries them must not start showing
/// a teammate that never existed.
#[tokio::test]
async fn legacy_report_and_failure_rows_stay_out_of_the_room() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    for (agent, text) in [
        (
            "hive-failure",
            "qa_engineer was asked and could not answer.",
        ),
        ("hive-report", "The desk settled after 2 turns."),
    ] {
        runtime
            .events()
            .append(
                &id,
                CompanyEvent::AgentReply {
                    chat_id: "engineering".to_string(),
                    agent_id: agent.to_string(),
                    text: text.to_string(),
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
    let voices: Vec<&str> = history.iter().map(|m| m.channel.as_str()).collect();

    assert!(
        !voices.contains(&"hive-failure"),
        "a legacy failure notice is not a teammate: {voices:?}"
    );
    assert!(
        !voices.contains(&"hive-report"),
        "and neither is the closing summary: {voices:?}"
    );
}

/// **A referred line is the ASKING AGENT speaking, not the desk.**
///
/// `senderOf` in the console draws the byline off `channel`, and treats
/// "operator" as "no distinct speaker — use the room's own name". That is
/// right for a message a person sent and wrong for a referral, which
/// arrives authored by a teammate: hardcoding "operator" made design's own
/// name the speaker, so an engineer asking design read as design talking to
/// itself. An `AgentReply` already names its agent here; this makes the two
/// paths agree rather than teaching the console a second rule.
#[tokio::test]
async fn a_referred_message_is_voiced_by_the_agent_that_asked() {
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
    ) {
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
    let referred = history
        .iter()
        .find(|m| m.referred_from.is_some())
        .expect("the referred line");

    assert_eq!(
        referred.channel, "software_engineer",
        "the byline names the agent, not the desk it landed on: {referred:?}"
    );
    assert!(
        !referred.by_person,
        "an agent is not a person, whatever the event it rides on"
    );
}

/// **One agent speaks in both rooms, and it is the asker.**
///
/// The asker asks on the other desk under its own name; that desk answers
/// on its own desk; the asker comes home and reports. The relay that
/// carried the answer back is an input to the asker, not a line anyone
/// reads — rendering it put the other desk's agent in a room it is not part
/// of, saying the same thing the asker was about to say.
#[tokio::test]
async fn the_asker_brings_the_answer_home_and_the_other_desk_stays_out_of_the_room() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    let mut events: Vec<CompanyEvent> = referral_leg(
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
    ))
    .collect();
    // The asker's report — the only thing #engineering should show.
    events.push(CompanyEvent::AgentReply {
        chat_id: "engineering".to_string(),
        agent_id: "software_engineer".to_string(),
        text: "design came back: error messages are a design-system problem".to_string(),
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

    assert!(
        history.iter().all(|m| m.channel != "product_designer"),
        "the answering desk never speaks in the room it was asked from: {history:?}"
    );
    let referred = history
        .iter()
        .find(|m| m.referred_from.is_some())
        .expect("something carries the provenance");
    assert_eq!(
        referred.channel, "software_engineer",
        "the chip rides the asker's own report: {referred:?}"
    );
    let origin = referred.referred_from.as_ref().expect("origin");
    assert!(origin.returning, "and it reads as an answer, not an ask");
    assert_eq!(origin.desk_name, "Design");

    // **The crossing itself rides the report, both legs of it.**
    //
    // The relayed rows still go — that is the assertion above, and the
    // reason for it — but the exchange they carried is kept here so an
    // operator can read what was actually asked and answered instead of
    // only the asker's paraphrase of it. `lines.len()` is the count the
    // collapsed label shows, which is why the QUESTION has to be captured
    // too: an answer on its own would always read "1 message".
    let crossing = referred
        .referral_conversation
        .as_ref()
        .expect("the crossing rides the report that brought it home");
    assert_eq!(crossing.asker_id, "software_engineer");
    assert_eq!(crossing.other_id, "product_designer");
    assert_eq!(crossing.other_desk_name, "Design");
    assert_eq!(crossing.lines.len(), 2, "{:?}", crossing.lines);
    assert!(crossing.lines[0].outbound, "the question goes out first");
    assert_eq!(
        crossing.lines[0].text,
        "what would you change about the error messages?"
    );
    assert!(!crossing.lines[1].outbound, "then the answer comes back");
    assert_eq!(
        crossing.lines[1].text,
        "error messages look like a copy task and are not one"
    );
}
