//! A seat turn's claims, its park, and the decision that releases it.

use std::sync::{Arc, Mutex};

use tinyhivemind::aside::Viewer;
use tinyhivemind::{Conversation, SESSION_WINDOW, SessionQuery, project_session};
use tinyhivemind_openhuman::{Disposition, EpisodeHost, Journal};

use super::*;
use crate::hive::test_support::MemoryLog;
use crate::ports::events::EventLog;
use crate::ports::types::{Effect, EffectGroup};
use crate::runtime::episode_resume::{SeatAsk, SeatVerdict};

fn host(events: Arc<dyn EventLog>) -> DeskHost {
    DeskHost::new(
        CompanyId::new("acme"),
        "engineering".to_owned(),
        "Engineering".to_owned(),
        events,
        vec!["one".to_owned(), "two".to_owned()],
    )
    .episode("ep1")
    .in_thread(Some(EventSeq::new(7)))
}

fn request(tool: &str) -> ApprovalRequest {
    ApprovalRequest {
        tool: tool.to_owned(),
        reason: "needs sign-off".to_owned(),
        effect: Effect {
            kind: tool.to_owned(),
            group: EffectGroup::Other,
            amount_usd: None,
            established_thread: false,
            first_time_counterparty: false,
            payload: serde_json::json!({ "to": "a@b.c" }),
            agent: Some("one".to_owned()),
            run_id: None,
        },
    }
}

/// Parks every request and records what it was handed.
#[derive(Default)]
struct Recording(Mutex<Vec<(String, Vec<String>)>>);

#[async_trait::async_trait]
impl SeatParking for Recording {
    async fn park(&self, seat: &str, requests: Vec<ApprovalRequest>) -> SeatParked {
        let tools: Vec<String> = requests.iter().map(|r| r.tool.clone()).collect();
        let parked = (0..tools.len())
            .map(|n| ApprovalId::new(format!("{seat}-{n}")))
            .collect();
        self.0.lock().unwrap().push((seat.to_owned(), tools));
        SeatParked {
            parked,
            refused: Vec::new(),
        }
    }
}

fn desk() -> Conversation {
    Conversation {
        desk_id: "engineering".to_owned(),
        desk_name: "Engineering".to_owned(),
        thread_root: None,
    }
}

async fn readable_by(host: &DeskHost, seat: &str) -> Vec<String> {
    project_session(
        host.log(),
        &SessionQuery {
            conversation: desk(),
            viewer: Viewer::Agent { id: seat.into() },
            before: None,
            window: SESSION_WINDOW,
        },
    )
    .await
    .expect("reads")
    .iter()
    .filter_map(|row| row.readable().map(str::to_owned))
    .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_seat_turn_claims_its_approvals_without_a_pool() {
    let queue = ApprovalRequestQueue::default();
    let events: Arc<dyn EventLog> = Arc::new(MemoryLog::default());
    let host = host(events).claiming(queue.clone());
    let pushed = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&pushed);
    let pushing = queue.clone();
    host.wrap_turn(
        "one",
        Box::pin(async move {
            *seen.lock().unwrap() = Some(pushing.push(request("send_email")));
            Ok("asked".to_owned())
        }),
    )
    .await
    .expect("the turn runs");
    assert_eq!(
        *pushed.lock().unwrap(),
        Some(crate::harness::built_in::policy::ApprovalPush::Queued),
        "a seat turn's request lands in a claim rather than being refused"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_seat_approval_is_not_drained_by_an_unrelated_cycle() {
    let queue = ApprovalRequestQueue::default();
    let events: Arc<dyn EventLog> = Arc::new(MemoryLog::default());
    let parking = Arc::new(Recording::default());
    let host = host(events)
        .claiming(queue.clone())
        .parking(parking.clone());
    let pushing = queue.clone();
    host.wrap_turn(
        "one",
        Box::pin(async move {
            pushing.push(request("send_email"));
            Ok("asked".to_owned())
        }),
    )
    .await
    .expect("the turn runs");
    let cycle = queue.claim(ApprovalScope::Cycle);
    assert!(
        cycle
            .drain(MAX_APPROVAL_REQUESTS_PER_TURN)
            .requests
            .is_empty(),
        "a chat cycle must not park a seat's request under its own thread"
    );
    drop(cycle);
    assert_eq!(
        host.after_turn("one", None).expect("the hook runs"),
        Disposition::Parked
    );
    assert_eq!(
        *parking.0.lock().unwrap(),
        vec![("one".to_owned(), vec!["send_email".to_owned()])]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parked_seat_is_journaled_with_what_it_waits_on() {
    let log = Arc::new(MemoryLog::default());
    let events: Arc<dyn EventLog> = log.clone();
    let queue = ApprovalRequestQueue::default();
    let host = host(events)
        .claiming(queue.clone())
        .parking(Arc::new(Recording::default()));
    let pushing = queue.clone();
    host.wrap_turn(
        "one",
        Box::pin(async move {
            pushing.push(request("send_email"));
            Ok(String::new())
        }),
    )
    .await
    .unwrap();
    assert_eq!(host.after_turn("one", None).unwrap(), Disposition::Parked);
    assert_eq!(host.after_turn("two", None).unwrap(), Disposition::Done);
    host.event(&tinyhivemind_driver::Event::Parked {
        seat: "one".to_owned(),
        thread: None,
    });
    let parked = log
        .rows()
        .into_iter()
        .find_map(|stored| match stored.event {
            CompanyEvent::EpisodeSeatParked {
                chat_id,
                episode_id,
                seat,
                approval_ids,
                ..
            } => Some((chat_id, episode_id, seat, approval_ids)),
            _ => None,
        })
        .expect("the park is journaled");
    assert_eq!(
        parked,
        (
            "engineering".to_owned(),
            "ep1".to_owned(),
            "one".to_owned(),
            vec![ApprovalId::new("one-0")]
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_nothing_can_park_is_told_to_the_seat() {
    let log = Arc::new(MemoryLog::default());
    let events: Arc<dyn EventLog> = log.clone();
    let queue = ApprovalRequestQueue::default();
    let host = host(events).claiming(queue.clone());
    let pushing = queue.clone();
    host.wrap_turn(
        "one",
        Box::pin(async move {
            pushing.push(request("send_email"));
            Ok(String::new())
        }),
    )
    .await
    .unwrap();
    assert_eq!(host.after_turn("one", None).unwrap(), Disposition::Done);
    let told = readable_by(&host, "one").await;
    assert!(
        told.iter().any(|row| row.contains("nobody was asked")),
        "{told:?}"
    );
    assert!(readable_by(&host, "two").await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_production_parking_uses_the_seat_key_and_the_desk_thread() {
    let dir = tempfile::tempdir().unwrap();
    let policy = toml::from_str("mode = \"supervised\"\n").unwrap();
    let gate: Arc<dyn crate::ports::ApprovalGate> =
        Arc::new(crate::policy::ManifestApprovalGate::new(policy));
    let journal = Arc::new(crate::runtime::journal::RuntimeJournal::new(
        dir.path().join("journal.jsonl"),
    ));
    let events: Arc<dyn EventLog> = Arc::new(MemoryLog::default());
    let parker = ApprovalParker::new(
        gate,
        journal.clone(),
        crate::runtime::grants::GrantSet::default(),
        crate::runtime::continuation::ContinuationQueue::default(),
        events,
    );
    let parking = EpisodeSeatParking::new(
        parker,
        CompanyId::new("acme"),
        "engineering".to_owned(),
        Some(EventSeq::new(7)),
        "ep1".to_owned(),
    );
    let outcome = parking.park("one", vec![request("send_email")]).await;
    assert!(outcome.refused.is_empty());
    let id = &outcome.parked[0];
    assert_eq!(
        journal.approval_cycle(id).flatten().as_deref(),
        Some("episode-seat:ep1:one")
    );
    let conversation = journal.approval_conversation(id).expect("recorded");
    assert_eq!(conversation.thread.as_deref(), Some("engineering"));
    assert_eq!(conversation.parent, Some(EventSeq::new(7)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_decision_releases_the_seat_and_is_what_it_reads_next() {
    let events: Arc<dyn EventLog> = Arc::new(MemoryLog::default());
    let releases = EpisodeReleases::default();
    let host = host(events).releasing(releases.clone());
    releases.start("ep1");
    host.event(&tinyhivemind_driver::Event::Parked {
        seat: "one".to_owned(),
        thread: None,
    });
    releases.deliver(
        "ep1",
        "one",
        vec![SeatDecision {
            approval_id: ApprovalId::new("a1"),
            ask: SeatAsk::Request {
                title: "email the client".to_owned(),
            },
            verdict: SeatVerdict::Approved,
            answer: "keep it short".to_owned(),
        }],
    );
    let released = host.released(&["one".to_owned()]).await.expect("released");
    assert_eq!(released, vec!["one".to_owned()]);
    let read = readable_by(&host, "one").await;
    assert!(
        read.iter().any(
            |row| row.contains("approved your request: email the client")
                && row.contains("keep it short")
        ),
        "the released seat reads the decision: {read:?}"
    );
    assert!(
        readable_by(&host, "two").await.is_empty(),
        "the decision is the asking seat's alone"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_with_no_registry_releases_nobody() {
    let events: Arc<dyn EventLog> = Arc::new(MemoryLog::default());
    let host = host(events);
    assert!(
        host.released(&["one".to_owned()])
            .await
            .expect("answers")
            .is_empty()
    );
}

/// A seat's publish is accepted, and lands in the episode's own bucket.
///
/// This is the whole of #2464 at the claim layer. The bucket used to be
/// claimed as `Unclaimed`, which made `push` return `false` and the tool
/// refuse in-turn — correct while nothing filed what a seat published, and
/// the reason a seat that spent a turn producing a report could not hand it
/// over. Naming the episode is what makes the file recordable; `settle`
/// hands it to `park_seat`, which files it.
#[tokio::test(flavor = "multi_thread")]
async fn a_seat_turn_publishes_into_its_own_episode() {
    let publishes = crate::harness::built_in::publish::PendingPublishQueue::default();
    let events: Arc<dyn EventLog> = Arc::new(MemoryLog::default());
    let host = host(events)
        .claiming(ApprovalRequestQueue::default())
        .publishing(publishes.clone());

    let accepted = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&accepted);
    let pushing = publishes.clone();
    host.wrap_turn(
        "one",
        Box::pin(async move {
            *seen.lock().unwrap() = Some(pushing.push(
                crate::harness::built_in::publish::PendingPublish {
                    agent: "one".to_owned(),
                    source: "report.md".to_owned(),
                    title: "Report".to_owned(),
                    kind: crate::ports::artifacts::ArtifactKind::Markdown,
                    note: None,
                    payload: crate::harness::built_in::publish::PublishPayload::Text(
                        "body".to_owned(),
                    ),
                },
            ));
            Ok("published".to_owned())
        }),
    )
    .await
    .expect("the turn runs");

    assert_eq!(
        *accepted.lock().unwrap(),
        Some(true),
        "a seat's publish is staged rather than refused"
    );

    // **Through the drain, not just the push.**
    //
    // Stopping at `push` would pass even if `settle` threw the files away.
    // `after_turn` takes the claims and settles them, and `park_seat` is
    // where they are filed -- so the assertion has to reach it.
    let settled = host
        .take_seat_claims("one")
        .expect("the turn's claims were kept for `after_turn`")
        .settle();
    assert_eq!(
        settled.publishes.len(),
        1,
        "settle hands the staged file on to be filed, rather than counting it"
    );
    assert_eq!(settled.publishes[0].source, "report.md");

    // This host has no roster, so filing cannot succeed -- which is the path
    // worth pinning: the seat must be told *which* file did not land, by
    // name, because nothing else can recover it. A bare count would leave it
    // reporting a number the operator cannot act on.
    let held = host.park_seat("one", settled).await;
    assert!(!held, "a failed filing does not hold the seat");
    let told = readable_by(&host, "one").await;
    assert!(
        told.iter().any(|row| row.contains("report.md")),
        "the seat is told which file could not be filed: {told:?}"
    );
}
