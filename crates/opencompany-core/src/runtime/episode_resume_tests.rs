use super::*;

fn decision(id: &str, verdict: SeatVerdict) -> SeatDecision {
    SeatDecision {
        approval_id: ApprovalId::new(id),
        ask: SeatAsk::Request {
            title: "ship it".to_owned(),
        },
        verdict,
        answer: String::new(),
    }
}

#[test]
fn a_turn_key_round_trips_through_parse() {
    let key = turn_key("ep1", "writer");
    assert_eq!(key, "episode-seat:ep1:writer");
    assert_eq!(
        parse(&key),
        Some(EpisodeSeat {
            episode_id: "ep1".to_owned(),
            seat: "writer".to_owned(),
        })
    );
}

#[test]
fn other_turn_keys_are_not_episode_seats() {
    assert_eq!(parse("cycle-123"), None);
    assert_eq!(parse("workflow-run:abc"), None);
    assert_eq!(parse("episode-seat:ep1"), None);
    assert_eq!(parse("episode-seat::writer"), None);
    assert_eq!(parse("episode-seat:ep1:"), None);
}

#[test]
fn delivery_to_an_episode_not_running_is_banked_and_reported() {
    let releases = EpisodeReleases::default();
    assert!(!releases.deliver("ep1", "writer", vec![decision("a1", SeatVerdict::Approved)]));
    assert!(releases.start("ep1"));
    assert!(!releases.start("ep1"));
    let taken = releases.take("ep1", &["writer".to_owned()]);
    assert_eq!(taken["writer"].len(), 1);
}

#[test]
fn delivery_to_a_running_episode_is_taken_by_it() {
    let releases = EpisodeReleases::default();
    releases.start("ep1");
    assert!(releases.deliver("ep1", "writer", vec![decision("a1", SeatVerdict::Denied)]));
    assert!(releases.take("ep1", &["reviewer".to_owned()]).is_empty());
    assert_eq!(releases.take("ep1", &["writer".to_owned()]).len(), 1);
    releases.finish("ep1");
    assert!(!releases.is_running("ep1"));
}

#[tokio::test]
async fn a_waiting_episode_wakes_on_delivery() {
    let releases = EpisodeReleases::default();
    releases.start("ep1");
    let waiter = {
        let releases = releases.clone();
        tokio::spawn(async move { releases.released("ep1", &["writer".to_owned()]).await })
    };
    tokio::task::yield_now().await;
    releases.deliver("ep1", "writer", vec![decision("a1", SeatVerdict::Approved)]);
    let released = tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
        .await
        .expect("woken")
        .expect("joined");
    assert_eq!(released["writer"][0].approval_id.as_ref(), "a1");
}

#[test]
fn notes_say_what_was_decided_in_plain_words() {
    let call = SeatDecision {
        approval_id: ApprovalId::new("a1"),
        ask: SeatAsk::Call {
            tool: "send_email".to_owned(),
            args: serde_json::json!({"to": "a@b.c"}),
        },
        verdict: SeatVerdict::Approved,
        answer: String::new(),
    };
    let note = call.note();
    assert!(note.contains("approved your `send_email` call"), "{note}");
    assert!(note.contains(r#"{"to":"a@b.c"}"#), "{note}");

    let question = SeatDecision {
        approval_id: ApprovalId::new("a2"),
        ask: SeatAsk::Question {
            needed: "which region".to_owned(),
        },
        verdict: SeatVerdict::Approved,
        answer: "eu-west".to_owned(),
    };
    assert!(question.note().contains("\"eu-west\""));
    assert!(
        decision("a3", SeatVerdict::Denied)
            .note()
            .contains("denied your request: ship it")
    );
}
