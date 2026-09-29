//! The coordination fold, rule by rule, over rows built by hand.

use super::*;
use crate::hive::routing::{Router, RoutingPlanDto};
use crate::ports::types::{EpisodeReason, ReplyEpisode, TurnOutcome, UtteranceKind};

fn company() -> CompanyId {
    CompanyId::new("acme")
}

/// Rows at one-millisecond intervals, sequenced from 1.
fn rows(events: Vec<CompanyEvent>) -> Vec<StoredEvent> {
    events
        .into_iter()
        .enumerate()
        .map(|(index, event)| StoredEvent {
            seq: EventSeq::new(index as u64 + 1),
            company: company(),
            event,
            at_millis: 1_000 + index as u64,
        })
        .collect()
}

fn started(turn: &str, agent: &str, episode: &str) -> CompanyEvent {
    CompanyEvent::TurnStarted {
        turn_id: turn.into(),
        chat_id: "engineering".into(),
        parent: None,
        by: None,
        agent_id: Some(agent.into()),
        episode_id: Some(episode.into()),
        round_revision: Some(0),
    }
}

fn settled(turn: &str, agent: &str) -> CompanyEvent {
    CompanyEvent::TurnSettled {
        turn_id: turn.into(),
        agent_id: Some(agent.into()),
        chat_id: Some("engineering".into()),
        episode_id: None,
        round_revision: None,
        outcome: TurnOutcome::Committed,
    }
}

fn opened(episode: &str, chat: &str) -> CompanyEvent {
    CompanyEvent::EpisodeOpened {
        chat_id: chat.into(),
        episode_id: episode.into(),
        opened_by_seq: 1,
        parent: None,
        participants: vec!["engineer".into(), "ceo".into()],
        plan: RoutingPlanDto::Fallback {
            primary_id: "engineer".into(),
            reason: "provider_unavailable".into(),
        },
        hop: 0,
    }
}

fn round(episode: &str, chat: &str) -> CompanyEvent {
    CompanyEvent::RoundStarted {
        chat_id: chat.into(),
        episode_id: episode.into(),
        revision: 0,
        agent_ids: vec!["engineer".into(), "ceo".into()],
    }
}

fn completed(episode: &str, chat: &str, rounds: u32, reason: EpisodeReason) -> CompanyEvent {
    CompanyEvent::EpisodeCompleted {
        chat_id: chat.into(),
        episode_id: episode.into(),
        revision: 2,
        completed_by: Some("engineer".into()),
        rounds,
        reason,
        summary_seq: None,
    }
}

fn reply(agent: &str, episode: &str, kind: UtteranceKind, to: Vec<String>) -> CompanyEvent {
    CompanyEvent::AgentReply {
        chat_id: "engineering".into(),
        agent_id: agent.into(),
        text: "…".into(),
        steps: Vec::new(),
        outputs: Vec::new(),
        task_id: None,
        parent: None,
        mentions: Vec::new(),
        mention_depth: 0,
        audience: to.clone(),
        episode: Some(ReplyEpisode {
            id: episode.into(),
            revision: 0,
            kind,
            to,
            routed_by: None,
        }),
    }
}

/// Two seats bracketed at once peak at two and overlap once; the same agent
/// starting again while its own turn is open is the one overlap that counts
/// against the run.
#[test]
fn turn_brackets_give_the_peak_and_flag_a_same_agent_overlap() {
    let report = measure_rows(
        &company(),
        EventSeq::new(0),
        &rows(vec![
            started("t1", "engineer", "ep"),
            started("t2", "ceo", "ep"),
            settled("t1", "engineer"),
            settled("t2", "ceo"),
            started("t3", "writer", "ep2"),
            settled("t3", "writer"),
        ]),
    );
    assert_eq!(report.max_concurrent_turns, 2);
    assert_eq!(report.overlaps, 1);
    assert_eq!(report.same_agent_overlaps, 0);
    assert_eq!(report.open_turns, 0);

    let clash = measure_rows(
        &company(),
        EventSeq::new(0),
        &rows(vec![
            started("t1", "ceo", "ep"),
            started("t2", "ceo", "ep2"),
            CompanyEvent::TurnFailed {
                turn_id: "t1".into(),
                error: "boom".into(),
                agent_id: Some("ceo".into()),
                chat_id: None,
                episode_id: None,
                round_revision: None,
                outcome: Some(TurnOutcome::TimedOut),
            },
        ]),
    );
    assert_eq!(clash.same_agent_overlaps, 1);
    assert_eq!(clash.open_turns, 1, "t2 never settled");
    assert!(
        clash
            .failures(&Thresholds::default())
            .iter()
            .any(|failure| failure.contains("same-agent overlaps 1")),
        "{:?}",
        clash.failures(&Thresholds::default())
    );
}

/// Episodes count their rounds, their reason and their time to complete;
/// contacts fold to pairs; referrals cross only when the desks differ and
/// only on the forward leg; the kinds come off the reply rows.
#[test]
fn the_fold_mirrors_the_node_twin_frame_for_frame() {
    let report = measure_rows(
        &company(),
        EventSeq::new(0),
        &rows(vec![
            opened("ep1", "engineering"),
            round("ep1", "engineering"),
            reply("engineer", "ep1", UtteranceKind::Post, Vec::new()),
            reply("ceo", "ep1", UtteranceKind::Dm, vec!["engineer".into()]),
            CompanyEvent::DmDelivered {
                chat_id: "engineering".into(),
                episode_id: "ep1".into(),
                from: "ceo".into(),
                to: vec!["engineer".into()],
                message_seq: 4,
            },
            round("ep1", "engineering"),
            reply("engineer", "ep1", UtteranceKind::Broadcast, Vec::new()),
            CompanyEvent::BroadcastRouted {
                chat_id: "engineering".into(),
                episode_id: "ep1".into(),
                revision: 2,
                agent_id: "engineer".into(),
                message_seq: 7,
                plan: RoutingPlanDto::Hive {
                    primary_id: "ceo".into(),
                    invited_ids: vec!["engineer".into()],
                },
                probabilities: None,
                router: Router::Fallback,
            },
            CompanyEvent::ReferralEnqueued {
                from_desk: "engineering".into(),
                trigger_sequence: 7,
                from_desk_name: "Engineering".into(),
                returning: false,
                answers: None,
                conversation: None,
                rows: None,
                asker: "engineer".into(),
                asker_label: "Engineer".into(),
                to_desk: "content".into(),
                target: "writer".into(),
                episode_id: Some("ep1".into()),
                to_episode_id: None,
                hop: 1,
            },
            CompanyEvent::ReferralEnqueued {
                from_desk: "content".into(),
                trigger_sequence: 12,
                from_desk_name: "Content".into(),
                returning: true,
                answers: Some(9),
                conversation: None,
                rows: None,
                asker: "writer".into(),
                asker_label: "Writer".into(),
                to_desk: "engineering".into(),
                target: "engineer".into(),
                episode_id: Some("ep2".into()),
                to_episode_id: Some("ep1".into()),
                hop: 1,
            },
            opened("ep2", "content"),
            completed("ep2", "content", 1, EpisodeReason::CompleteEpisode),
            reply("ceo", "ep1", UtteranceKind::CompleteEpisode, Vec::new()),
            completed("ep1", "engineering", 3, EpisodeReason::RoundCap),
        ]),
    );
    assert_eq!(report.episodes_opened, 2);
    assert_eq!(report.episodes_completed, 2);
    let ep1 = &report.episodes["ep1"];
    assert_eq!(ep1.chat_id, "engineering");
    assert_eq!(
        ep1.rounds, 3,
        "the completion's count outranks two proposals"
    );
    assert_eq!(ep1.reason.as_deref(), Some("round_cap"));
    assert_eq!(ep1.time_to_complete_millis, Some(13));
    assert_eq!(report.episodes["ep2"].time_to_complete_millis, Some(1));
    assert_eq!(report.broadcasts, 1);
    assert_eq!(report.dms, 1);
    assert_eq!(
        report.cross_desk_referrals, 1,
        "the return leg does not count"
    );
    assert_eq!(report.referral_pairs, vec!["engineering→content"]);
    assert_eq!(
        report.distinct_pairs.iter().cloned().collect::<Vec<_>>(),
        vec!["ceo→engineer", "engineer→ceo", "engineer→writer"],
        "a broadcast to oneself is not a pair; the referral's forward pair is"
    );
    assert_eq!(report.utterance_kinds["post"], 1);
    assert_eq!(report.utterance_kinds["dm"], 1);
    assert_eq!(report.utterance_kinds["broadcast"], 1);
    assert_eq!(report.utterance_kinds["complete_episode"], 1);
    assert_eq!(report.plan_kinds["fallback"], 2);
    assert_eq!(report.plan_kinds["hive"], 1);
    assert_eq!(report.routers["fallback"], 1);
    // No turn ever opened, so the one threshold this journal misses is the
    // peak; everything else is met.
    assert_eq!(
        report.failures(&Thresholds::default()),
        vec!["max concurrent turns 0 < 2".to_string()]
    );
}

/// An open episode fails the run by name, an empty journal fails it as
/// "no episode opened", and the table names the verdict.
#[test]
fn the_verdict_names_what_is_missing() {
    let empty = measure_rows(&company(), EventSeq::new(0), &[]);
    let failures = empty.failures(&Thresholds::default());
    assert!(
        failures.contains(&"no episode opened".to_string()),
        "{failures:?}"
    );
    assert!(empty.to_table(&Thresholds::default()).contains("FAIL ("));

    let dangling = measure_rows(
        &company(),
        EventSeq::new(0),
        &rows(vec![
            opened("ep1", "engineering"),
            round("ep1", "engineering"),
        ]),
    );
    let failures = dangling.failures(&Thresholds::default());
    assert!(
        failures.contains(&"1 episode(s) never completed: engineering/ep1".to_string()),
        "{failures:?}"
    );
    let table = dangling.to_table(&Thresholds::default());
    assert!(
        table.contains("episodes                  0/1 completed"),
        "{table}"
    );
    assert!(table.contains("rounds per episode        ep1=1"), "{table}");
}

/// `since` drops the rows before it, and the JSON shape is camelCase.
#[test]
fn since_narrows_the_window_and_the_report_serializes_camel_case() {
    let all = rows(vec![
        opened("old", "engineering"),
        completed("old", "engineering", 1, EpisodeReason::CompleteEpisode),
        opened("new", "content"),
    ]);
    let report = measure_rows(&company(), EventSeq::new(3), &all);
    assert_eq!(report.rows, 1);
    assert_eq!(report.since_seq, 3);
    assert_eq!(report.episodes_opened, 1);
    assert!(report.episodes.contains_key("new"));
    let wire = serde_json::to_value(&report).unwrap();
    assert_eq!(wire["maxConcurrentTurns"], 0);
    assert_eq!(wire["sameAgentOverlaps"], 0);
    assert_eq!(wire["episodesOpened"], 1);
    assert_eq!(wire["episodes"]["new"]["chatId"], "content");
    assert_eq!(wire["crossDeskReferrals"], 0);
}

/// The same fold over the port, paged.
#[tokio::test]
async fn measure_reads_the_journal_through_the_port() {
    let log = crate::hive::test_support::MemoryLog::default();
    let company = crate::hive::test_support::MemoryLog::company();
    for event in [
        opened("ep1", "engineering"),
        started("t1", "engineer", "ep1"),
        started("t2", "ceo", "ep1"),
        settled("t2", "ceo"),
        settled("t1", "engineer"),
        completed("ep1", "engineering", 1, EpisodeReason::CompleteEpisode),
    ] {
        log.append(&company, event).await.unwrap();
    }
    let report = measure(&log, &company, EventSeq::new(0)).await.unwrap();
    assert_eq!(report.rows, 6);
    assert_eq!(report.max_concurrent_turns, 2);
    assert_eq!(report.episodes_completed, 1);
}
