//! What decides whether a settled episode gets a closing turn, and what the
//! seat that takes it is told.

use super::{PERSONA_NOTE, eligible, routing_text};
use crate::ports::types::EpisodeReason;

/// A desk that settled cleanly with several seats gets the turn; the three
/// cases that would make it wrong do not.
///
/// Each `false` here is a way the turn misleads rather than merely wastes a
/// call: a DM owner has already assembled (`host::dm_persona_note` requires
/// it, and three live runs did it 3/3), a lone seat would be summarising
/// itself, and an episode that hit a cap or a failure has unfinished lanes --
/// asking for what it "adds up to" invites a partial run to be presented as a
/// complete answer.
#[test]
fn only_a_multi_seat_desk_that_settled_cleanly_concludes() {
    assert!(eligible("all_hands", EpisodeReason::CompleteEpisode, 5));

    assert!(
        !eligible("dm:creative_director", EpisodeReason::CompleteEpisode, 7),
        "an operator's line already ends with its owner assembling"
    );
    assert!(
        !eligible("all_hands", EpisodeReason::CompleteEpisode, 1),
        "one seat is already its own conclusion"
    );
    for reason in [
        EpisodeReason::RoundCap,
        EpisodeReason::Timeout,
        EpisodeReason::Failed,
        EpisodeReason::MembershipChanged,
    ] {
        assert!(
            !eligible("all_hands", reason, 5),
            "{reason:?} leaves lanes unfinished, so there is nothing whole to assemble"
        );
    }
}

/// The closing seat is told to answer the request, not to report on the desk.
///
/// A live desk run asked for "the plan" produced a plan written *first*, by the
/// strategist, ahead of every lane it was supposed to draw on -- so the note
/// has to say that assembly comes last and that pointing at files is not an
/// answer.
#[test]
fn the_closing_seat_is_told_to_answer_rather_than_report() {
    assert!(
        PERSONA_NOTE.contains("what it all adds up to"),
        "{PERSONA_NOTE}"
    );
    assert!(
        PERSONA_NOTE.contains("not a status"),
        "a list of who did what is the failure mode: {PERSONA_NOTE}"
    );
    assert!(
        PERSONA_NOTE.contains("cannot hand this on"),
        "the seat has no `broadcast`, so the note must say so: {PERSONA_NOTE}"
    );
    assert!(
        PERSONA_NOTE.contains("complete_episode"),
        "and must name the verb that does finish: {PERSONA_NOTE}"
    );
}

/// Jev is asked with the operator's own request inside the message, because
/// `route_desk` scores which seat should take a message -- and the message is
/// the assembly job for *this* request, not assembly in the abstract.
#[test]
fn the_routing_question_carries_the_original_request() {
    let text = routing_text("relaunch our pricing on 14 Oct");
    assert!(text.contains("relaunch our pricing on 14 Oct"), "{text}");
    assert!(text.contains("assemble"), "{text}");
}

/// The router is given one line per seat that spoke on the desk, so the choice
/// is made against what the episode produced rather than against role
/// descriptions alone.
///
/// `desk_request`'s `thread_context` is the only channel for that, and
/// `conducted::opening` correctly passes it empty -- there is no transcript
/// when an episode opens. At the settle point there is one, and a live run
/// where the strategist wrote five lanes and the copywriter one is
/// indistinguishable, to a router given no context, from a run that spread the
/// work evenly.
#[test]
fn the_router_is_given_each_seat_s_finding() {
    use crate::ports::types::{CompanyEvent, CompanyId, EventSeq, StoredEvent};

    let row = |seq: u64, chat: &str, agent: &str, text: &str| StoredEvent {
        seq: EventSeq::new(seq),
        company: CompanyId::new("c"),
        event: CompanyEvent::AgentReply {
            chat_id: chat.to_owned(),
            agent_id: agent.to_owned(),
            text: text.to_owned(),
            steps: Vec::new(),
            outputs: Vec::new(),
            task_id: None,
            parent: None,
            mentions: Vec::new(),
            mention_depth: 0,
            audience: Vec::new(),
            episode: None,
        },
        at_millis: seq,
    };
    let rows = vec![
        row(
            1,
            "all_hands",
            "brand_strategist",
            "positioning is zero overage",
        ),
        row(
            2,
            "dm:a+b",
            "copywriter",
            "a side conversation, not the desk",
        ),
        row(3, "all_hands", "copywriter", "headline written"),
    ];

    let lines = super::findings(&rows, "all_hands");
    assert_eq!(
        lines,
        vec![
            "brand_strategist: positioning is zero overage".to_owned(),
            "copywriter: headline written".to_owned(),
        ],
        "one line per desk reply, in order, and nothing from a pair channel"
    );
}

/// One journaled desk reply, for the read-back rules below.
fn reply(seq: u64, chat: &str, agent: &str, text: &str) -> crate::ports::types::StoredEvent {
    use crate::ports::types::{CompanyEvent, CompanyId, EventSeq, StoredEvent};
    StoredEvent {
        seq: EventSeq::new(seq),
        company: CompanyId::new("c"),
        event: CompanyEvent::AgentReply {
            chat_id: chat.to_owned(),
            agent_id: agent.to_owned(),
            text: text.to_owned(),
            steps: Vec::new(),
            outputs: Vec::new(),
            task_id: None,
            parent: None,
            mentions: Vec::new(),
            mention_depth: 0,
            audience: Vec::new(),
            episode: None,
        },
        at_millis: seq,
    }
}

/// A whole finding survives: the budget is on the state, not on each line.
///
/// Thirteen live runs put the median finding at 1,024 characters against a
/// documented 32k-token window that a whole worst-case episode fills to about an
/// eighth. An earlier per-finding cap of 600 truncated 78% of findings and 48.5%
/// of every character -- including the last one, which is the message the
/// `needed` question is judged on.
#[test]
fn a_long_finding_is_kept_whole() {
    use crate::ports::types::{CompanyEvent, CompanyId, EventSeq, StoredEvent};

    let long = "x".repeat(2_600);
    let rows = vec![StoredEvent {
        seq: EventSeq::new(1),
        company: CompanyId::new("c"),
        event: CompanyEvent::AgentReply {
            chat_id: "all_hands".to_owned(),
            agent_id: "analytics_analyst".to_owned(),
            text: long,
            steps: Vec::new(),
            outputs: Vec::new(),
            task_id: None,
            parent: None,
            mentions: Vec::new(),
            mention_depth: 0,
            audience: Vec::new(),
            episode: None,
        },
        at_millis: 1,
    }];
    let line = super::findings(&rows, "all_hands").remove(0);
    // Exact, not "long enough": an assertion that only demanded more than a
    // thousand characters would pass against a cut at any length above it.
    assert_eq!(
        line,
        format!("analytics_analyst: {}", "x".repeat(2_600)),
        "the whole finding survives, prefix and all"
    );
}

/// The closing seat must distinguish a lane its owner produced from one a
/// stand-in drafted.
///
/// A live run concluded an episode where Jev seated two of eight teammates: the
/// strategist drafted five lanes to get the work moving, the copywriter wrote
/// one, and the other six seats never ran. The closing message was accurate
/// about its sources and still read exactly like a fully-staffed plan -- the
/// operator could not tell that the email and paid lanes were a guess. A good
/// synthesis of a thin round is the one way this turn can mislead, so the note
/// has to require the provenance line.
#[test]
fn the_closing_seat_must_say_which_lanes_had_their_owner() {
    assert!(
        PERSONA_NOTE.contains("stand-in draft"),
        "a proxy-written lane needs a name: {PERSONA_NOTE}"
    );
    assert!(
        PERSONA_NOTE.contains("cannot tell from a well-written answer"),
        "and the note must say why it matters: {PERSONA_NOTE}"
    );
}

/// One call carries both questions, because `SystemOneRequest.questions` is a
/// map of independently evaluated questions rather than a single one.
#[test]
fn the_closing_decision_is_one_call_with_two_questions() {
    use tinyhivemind_typesafe::Question;

    let seats = vec!["creative_director".to_owned(), "copywriter".to_owned()];
    let ask =
        super::closing_questions("relaunch pricing", &["copywriter: done".to_owned()], &seats);

    assert_eq!(ask.questions.len(), 2, "one round trip, not two");
    assert!(matches!(
        ask.questions.get("needed"),
        Some(Question::Noul { .. })
    ));
    let Some(Question::Choice { criteria, .. }) = ask.questions.get("who") else {
        panic!("`who` must be a Choice over the seats");
    };
    assert_eq!(
        criteria.keys().cloned().collect::<Vec<_>>(),
        vec!["copywriter".to_owned(), "creative_director".to_owned()],
        "every seat is an alternative"
    );
    assert_eq!(ask.state["request"], "relaunch pricing");
}

/// A confident "already assembled" is the only thing that skips the turn.
///
/// Everything else concludes, including a missing answer and a seat that is not
/// on this desk. The asymmetry is deliberate: an unnecessary closing turn cost
/// roughly twenty-five seconds on the thinnest live run, while a wrongly skipped
/// one cost a whole episode its answer — one run's last desk row, after fourteen
/// rows of work, was the system's own approval echo.
#[test]
fn only_a_confident_yes_skips_and_everything_else_concludes() {
    use std::collections::BTreeMap;
    use tinyhivemind_typesafe::{
        ChoiceAnswer, NoulAnswer, SystemOneAnswer, SystemOneResponse, TokenUsage,
    };

    let seats = vec!["creative_director".to_owned(), "copywriter".to_owned()];
    let response = |answers: Vec<(&str, SystemOneAnswer)>| SystemOneResponse {
        model: "jev-1.13.0".to_owned(),
        answers: answers
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect::<BTreeMap<_, _>>(),
        usage: TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
        },
    };
    let choice = |seat: &str| {
        SystemOneAnswer::Choice(ChoiceAnswer {
            choice: seat.to_owned(),
            probabilities: BTreeMap::new(),
            confidence: 1.0,
        })
    };
    let noul = |p: f64| SystemOneAnswer::Noul(NoulAnswer { noul: p });

    assert_eq!(
        super::read_decision(
            &response(vec![("needed", noul(0.95)), ("who", choice("copywriter"))]),
            &seats,
            "creative_director"
        ),
        super::Decision::NotNeeded,
        "a confident yes skips even though a seat was named"
    );
    assert_eq!(
        super::read_decision(
            &response(vec![("needed", noul(0.5)), ("who", choice("copywriter"))]),
            &seats,
            "creative_director"
        ),
        super::Decision::Conclude("copywriter".to_owned()),
        "an unsure answer concludes with the seat System One picked"
    );
    assert_eq!(
        super::read_decision(&response(Vec::new()), &seats, "creative_director"),
        super::Decision::Conclude("creative_director".to_owned()),
        "no answers at all still concludes, with the lead"
    );
    assert_eq!(
        super::read_decision(
            &response(vec![("who", choice("an_outsider"))]),
            &seats,
            "creative_director"
        ),
        super::Decision::Conclude("creative_director".to_owned()),
        "a seat that is not on this desk resolves to the lead, never to a skip"
    );
    assert_eq!(
        super::read_decision(
            &response(vec![("needed", noul(super::ALREADY_ASSEMBLED))]),
            &seats,
            "creative_director"
        ),
        super::Decision::NotNeeded,
        "the threshold itself skips"
    );
}

/// `summary_seq` names a row the closing turn wrote, never one the seat wrote
/// earlier in the episode.
///
/// The concluding seat is usually one that already spoke — the lead, most often
/// — so "its last reply on the desk" is an ordinary deliberation row until the
/// closing turn adds one. A closing turn that records nothing is the case
/// `Conclusion::summary_seq` documents as `None`; the first version of this read
/// scanned the whole episode and would instead have handed `EpisodeCompleted` a
/// mid-episode message for the console to label the summary.
///
/// This calls `closing_summary_seq` itself. An earlier version of this test
/// re-implemented the watermark rule inline and would have passed against a
/// regression that ignored the watermark entirely.
#[test]
fn a_summary_never_points_at_a_row_from_before_the_closing_turn() {
    let desk = "all_hands";
    let seat = "creative_director";
    // The seat spoke at 24 during the episode proper, and another seat at 31.
    let settled = vec![
        reply(10, desk, "brand_strategist", "positioning"),
        reply(24, desk, seat, "a deliberation line"),
        reply(31, desk, "copywriter", "copy"),
    ];
    let before = settled
        .iter()
        .map(|stored| stored.seq.value())
        .max()
        .unwrap_or(0);

    assert_eq!(
        super::closing_summary_seq(&settled, desk, seat, before),
        None,
        "a closing turn that recorded nothing names no row, even though the seat spoke at 24"
    );

    let mut spoke = settled.clone();
    spoke.push(reply(37, desk, seat, "the closing synthesis"));
    assert_eq!(
        super::closing_summary_seq(&spoke, desk, seat, before),
        Some(37),
        "the closing turn's own row, not the seat's earlier one"
    );

    // A row above the watermark by another seat is not this seat's summary.
    let mut other = settled.clone();
    other.push(reply(37, desk, "copywriter", "something else"));
    assert_eq!(
        super::closing_summary_seq(&other, desk, seat, before),
        None,
        "only the concluding seat's own row counts"
    );

    // A row in a pair channel is not a desk row.
    let mut aside = settled.clone();
    aside.push(reply(
        37,
        "dm:copywriter+creative_director",
        seat,
        "in a thread",
    ));
    assert_eq!(
        super::closing_summary_seq(&aside, desk, seat, before),
        None,
        "the summary is a desk row, not a thread row"
    );
}

/// The state is bounded whatever arrives: a request nobody would read, and a
/// single finding larger than the whole budget.
///
/// `findings` stops trimming at one line, so before this a lone oversized reply
/// went to the wire untouched — a seat that pastes a file into its finding is
/// not a strange thing to happen. And the request sat in the same state without
/// ever being counted.
#[test]
fn neither_a_vast_request_nor_a_vast_finding_can_overrun_the_state() {
    let huge_request = "q".repeat(super::STATE_CHAR_BUDGET * 2);
    let seats = vec!["creative_director".to_owned()];

    let ask = super::closing_questions(&huge_request, &["ceo: fine".to_owned()], &seats);
    let request = ask.state["request"].as_str().expect("a request");
    assert!(
        request.chars().count() <= super::REQUEST_CHAR_BUDGET,
        "the request is cut to its share of the budget, marker included: {} chars",
        request.chars().count()
    );
    assert!(
        request.ends_with("(cut to fit the routing call)"),
        "and says it was cut"
    );

    // One finding, bigger than everything: it is the only thing left to cut.
    let huge_finding = format!("ceo: {}", "x".repeat(super::STATE_CHAR_BUDGET * 2));
    let ask = super::closing_questions("relaunch pricing", &[huge_finding], &seats);
    let findings = ask.state["findings"].as_array().expect("findings");
    let total: usize = findings
        .iter()
        .map(|line| line.as_str().unwrap_or_default().chars().count())
        .sum();
    assert!(
        total <= super::STATE_CHAR_BUDGET,
        "the lone finding is cut rather than sent whole: {total} chars"
    );
    assert_eq!(
        findings.len(),
        1,
        "and it is still the finding, not dropped"
    );
}

/// With no watermark to trust, nothing qualifies as the summary.
///
/// A failed episode read leaves the closing turn unable to tell its own row from
/// a deliberation row, so the watermark is `u64::MAX` and `closing_summary_seq`
/// must select nothing — rather than defaulting to zero, which accepts every row
/// and reintroduces exactly the bug the watermark exists to prevent.
#[test]
fn an_unreachable_watermark_selects_no_summary() {
    let desk = "all_hands";
    let seat = "creative_director";
    let rows = vec![
        reply(24, desk, seat, "a deliberation line"),
        reply(37, desk, seat, "and another"),
    ];
    assert_eq!(
        super::closing_summary_seq(&rows, desk, seat, u64::MAX),
        None,
        "an unknown watermark names no row at all"
    );
    assert_eq!(
        super::closing_summary_seq(&rows, desk, seat, 0),
        Some(37),
        "and zero would have accepted one, which is why it is not the default"
    );
}
