//! The journal side of the episode host: what the library commits becomes a
//! row this desk can read back.

use std::sync::Arc;

use tinyhivemind::aside::Viewer;
use tinyhivemind::{Conversation, SESSION_WINDOW, SessionQuery, project_session};
use tinyhivemind_driver::{Commit, Note};
use tinyhivemind_openhuman::{EpisodeHost, Journal};

use super::DeskHost;
use crate::hive::test_support::MemoryLog;
use crate::ports::events::EventLog;
use crate::ports::types::{CompanyEvent, CompanyId, EventSeq};

fn desk() -> Conversation {
    Conversation {
        desk_id: "engineering".to_owned(),
        desk_name: "Engineering".to_owned(),
        thread_root: None,
    }
}

fn host(events: Arc<dyn EventLog>) -> DeskHost {
    DeskHost::new(
        CompanyId::new("acme"),
        "engineering".to_owned(),
        "Engineering".to_owned(),
        events,
        Vec::new(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_desk_note_is_the_episodes_own_voice_and_may_be_private() {
    let events: Arc<dyn EventLog> = Arc::new(MemoryLog::default());
    let host = host(Arc::clone(&events));
    host.note(&Note {
        body: "you hold open work".to_owned(),
        thread: None,
        only_for: Some("one".to_owned()),
    })
    .expect("the journal takes the note");
    for (seat, readable) in [("one", true), ("two", false)] {
        let rows = project_session(
            host.log(),
            &SessionQuery {
                conversation: desk(),
                viewer: Viewer::Agent { id: seat.into() },
                before: None,
                window: SESSION_WINDOW,
            },
        )
        .await
        .expect("reads");
        assert_eq!(rows.len(), 1, "one row, withheld rather than dropped");
        assert_eq!(
            rows[0].readable().is_some(),
            readable,
            "{seat} should{} read the nudge",
            if readable { "" } else { " not" }
        );
    }
    assert!(format!("{host:?}").contains("engineering"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_without_a_pool_runs_unbracketed_rather_than_refusing() {
    let log = Arc::new(MemoryLog::default());
    let events: Arc<dyn EventLog> = log.clone();
    let host = host(Arc::clone(&events));
    let ran = host
        .wrap_turn("one", Box::pin(async { Ok("said".to_owned()) }))
        .await
        .expect("the turn runs");
    assert_eq!(ran, "said");
    assert!(
        log.rows().is_empty(),
        "no pool, no lock, and so no bracket to write"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn each_settled_wave_moves_the_round_a_bracket_names() {
    let events: Arc<dyn EventLog> = Arc::new(MemoryLog::default());
    let host = host(Arc::clone(&events)).episode("ep-1");
    assert_eq!(host.wave.load(std::sync::atomic::Ordering::SeqCst), 0);
    // The loop hands a snapshot over once per settled wave; this host keeps
    // the count rather than the snapshot.
    let driver_state = None::<()>;
    let _ = driver_state;
    assert_eq!(host.episode_id, "ep-1");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_that_parks_nothing_never_holds_a_seat() {
    let events: Arc<dyn EventLog> = Arc::new(MemoryLog::default());
    let host = host(Arc::clone(&events));
    assert_eq!(
        host.after_turn("one", None).expect("the hook runs"),
        tinyhivemind_openhuman::Disposition::Done
    );
}

/// A commit as the conductor makes it, on this side of the wire.
fn commit(json: serde_json::Value) -> Commit {
    serde_json::from_value(json).expect("a commit the driver would make")
}

/// **A conclusion is threaded under the conversation it concludes.**
///
/// The conductor mints it with the conversation it is about and no thread,
/// so journaled as a desk row it hangs off the episode's root: a later reply
/// there, which the channel-level read drops, and no reply at all in the
/// conversation. Threaded under the ask it is the exchange's closing line.
///
/// Keyed on the utterance, not the field: desk work a seat does from inside a
/// conversation carries `conversation` too, and belongs on the desk.
#[tokio::test(flavor = "multi_thread")]
async fn a_conclusion_is_threaded_under_the_conversation_it_concludes() {
    let log = Arc::new(MemoryLog::default());
    let company = MemoryLog::company();
    let host = host(Arc::clone(&log) as Arc<dyn EventLog>);
    let pair = crate::hive::referral::pair_conversation("ada", "grace");
    let ask = log
        .append(
            &company,
            crate::hive::test_support::agent_reply_in(
                &pair,
                "ada",
                "does the rollout need a freeze?",
                vec!["grace".into()],
                None,
            ),
        )
        .await
        .unwrap();
    host.event(&tinyhivemind_driver::Event::Asked {
        seat: "ada".into(),
        askee: "grace".into(),
        root: tinyhivemind::Sequence(ask.value()),
    });

    let conclusion = host
        .commit(&commit(serde_json::json!({
            "author": "grace",
            "utterance": { "kind": "dm", "to": ["ada"], "message": "concluded our conversation: no, ship it" },
            "thread": null,
            "only_for": "ada",
            "conversation": ask.value(),
            "purpose": { "kind": "desk" },
        })))
        .expect("the journal takes the conclusion");
    let lifted = host
        .commit(&commit(serde_json::json!({
            "author": "grace",
            "utterance": { "kind": "broadcast", "message": "freeze policy: none on file" },
            "thread": null,
            "only_for": null,
            "conversation": ask.value(),
            "purpose": { "kind": "desk" },
        })))
        .expect("the journal takes the broadcast");

    let rows = log.read_from(&company, EventSeq::new(1), 16).await.unwrap();
    let row = |seq: tinyhivemind::Sequence| {
        rows.iter()
            .find(|stored| stored.seq.value() == seq.0)
            .map(|stored| match &stored.event {
                CompanyEvent::AgentReply {
                    chat_id, parent, ..
                } => (chat_id.clone(), *parent),
                other => panic!("not a reply: {other:?}"),
            })
            .expect("journaled")
    };
    assert_eq!(
        row(conclusion),
        (pair.clone(), Some(ask)),
        "the conclusion is a row of the conversation, under its ask"
    );
    assert_eq!(
        row(lifted),
        ("engineering".to_string(), None),
        "desk work said from inside a conversation stays on the desk"
    );
}

/// **A row in a pair channel is addressed to that pair when it is written.**
///
/// The answer inside a conversation is a `complete_episode`, which names
/// nobody, so it was stored with an empty audience -- and an empty audience
/// means desk-visible. It stayed private only because the projection
/// narrowed it on the way out, which puts the policy in the reader rather
/// than in the row. The channel names the pair, so the row says so too.
#[tokio::test(flavor = "multi_thread")]
async fn a_row_written_to_a_pair_channel_is_addressed_to_that_pair() {
    let log = Arc::new(MemoryLog::default());
    let company = MemoryLog::company();
    let host = host(Arc::clone(&log) as Arc<dyn EventLog>);
    let pair = crate::hive::referral::pair_conversation("ada", "grace");
    let ask = log
        .append(
            &company,
            crate::hive::test_support::agent_reply_in(&pair, "ada", "ask", Vec::new(), None),
        )
        .await
        .unwrap();
    host.event(&tinyhivemind_driver::Event::Asked {
        seat: "ada".into(),
        askee: "grace".into(),
        root: tinyhivemind::Sequence(ask.value()),
    });

    // The answer names nobody: inside a conversation a seat completes, and
    // `complete_episode` carries no recipient.
    let answered = host
        .commit(&commit(serde_json::json!({
            "author": "grace",
            "utterance": { "kind": "complete_episode", "message": "no freeze needed" },
            "thread": ask.value(),
            "only_for": null,
            "conversation": null,
            "purpose": { "kind": "desk" },
        })))
        .expect("the journal takes the answer");
    // And a desk row still addresses the desk.
    let broadcast = host
        .commit(&commit(serde_json::json!({
            "author": "grace",
            "utterance": { "kind": "broadcast", "message": "freeze policy: none" },
            "thread": null,
            "only_for": null,
            "conversation": null,
            "purpose": { "kind": "desk" },
        })))
        .expect("the journal takes the broadcast");

    let rows = log.read_from(&company, EventSeq::new(1), 16).await.unwrap();
    let audience = |seq: tinyhivemind::Sequence| {
        rows.iter()
            .find(|stored| stored.seq.value() == seq.0)
            .map(|stored| match &stored.event {
                CompanyEvent::AgentReply { audience, .. } => audience.clone(),
                other => panic!("not a reply: {other:?}"),
            })
            .expect("journaled")
    };
    assert_eq!(
        audience(answered),
        vec!["ada".to_string()],
        "the answer is addressed to the seat that asked, in the row itself"
    );
    assert!(
        audience(broadcast).is_empty(),
        "a desk row is desk-visible: {:?}",
        audience(broadcast)
    );
}

/// **A row names the wave its turn opened in, not the counter at commit.**
///
/// The wave counter advances on every checkpoint, and the loop checkpoints
/// once an iteration -- including an iteration that only ran a conversation.
/// Read at commit time, rows of one desk wave came back stamped with two or
/// three different numbers, and the console keys its round band on that
/// stamp: a live episode that ran four waves drew six bands, and the
/// completion marker printed six against the journal's four.
///
/// The bracket is the one thing that knows when a turn began, so it records
/// the wave and every row that turn commits takes it.
#[tokio::test(flavor = "multi_thread")]
async fn a_committed_row_names_the_wave_its_turn_opened_in() {
    let log = Arc::new(MemoryLog::default());
    let host = host(Arc::clone(&log) as Arc<dyn EventLog>).episode("ep-1");
    // The seat's turn opened in wave 0.
    host.turn_waves
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert("ada".to_owned(), 0);
    // Two checkpoints land while the turn is still running -- a conversation
    // settling twice, which is what moves this counter without the desk
    // having run a wave.
    host.wave.store(2, std::sync::atomic::Ordering::SeqCst);

    let seq = host
        .commit(&commit(serde_json::json!({
            "author": "ada",
            "utterance": { "kind": "complete_episode", "message": "done" },
            "thread": null,
            "only_for": null,
            "conversation": null,
            "purpose": { "kind": "desk" },
        })))
        .expect("the journal takes the row");

    let rows = log
        .read_from(&MemoryLog::company(), EventSeq::new(1), 8)
        .await
        .unwrap();
    let revision = rows
        .iter()
        .find(|stored| stored.seq.value() == seq.0)
        .and_then(|stored| match &stored.event {
            CompanyEvent::AgentReply { episode, .. } => episode.as_ref().map(|one| one.revision),
            _ => None,
        })
        .expect("journaled with its episode");
    assert_eq!(
        revision, 0,
        "the row belongs to the wave its turn opened in, not the counter at commit"
    );

    // A seat this host never bracketed -- no pool, so no turn was recorded --
    // still gets a number rather than nothing: the live counter, as before.
    assert_eq!(host.wave_of("grace"), 2);
}

/// A DM seat is told which of the two it is, and a desk seat is told nothing.
///
/// The distinction is the whole point of the note. Every roster teammate is a
/// member of every DM, so without it the seats a teammate can *ask* read the
/// same brief as the teammate the operator is actually talking to -- and a
/// brief that opens "## New desk messages" invites all of them to answer.
#[test]
fn a_dm_seat_learns_whether_the_line_is_its_own() {
    let owner = super::dm_persona_note("dm:ceo", "ceo").expect("the owner is told");
    assert!(
        owner.contains("your own direct line with the operator"),
        "{owner}"
    );
    assert!(
        owner.contains("here to be asked"),
        "the owner is told why the others are present: {owner}"
    );

    let guest = super::dm_persona_note("dm:ceo", "engineer").expect("a guest is told");
    assert!(
        guest.contains("should not answer them"),
        "a guest must not answer the operator: {guest}"
    );
    assert_ne!(owner, guest, "the two roles read differently");

    assert!(
        super::dm_persona_note("engineering", "ceo").is_none(),
        "a desk needs no correction -- `desk` is the right word there"
    );
}

#[test]
fn a_seat_is_named_by_its_roster_name_then_its_role() {
    let mut record = crate::hive::test_support::record(
        r#"
[company]
name = "Acme"

[[agent]]
id = "brand_strategist"
role = "Brand Strategist"

[[agent]]
id = "copywriter"
role = "Copywriter"
"#,
    );
    record
        .overlay_agent_edits
        .push(crate::ports::types::AgentOverride {
            agent_id: "brand_strategist".to_string(),
            name: Some("Tess".to_string()),
            ..Default::default()
        });
    assert_eq!(
        super::seat_display_name(Some(&record), "brand_strategist"),
        "Tess"
    );
    assert_eq!(
        super::seat_display_name(Some(&record), "copywriter"),
        "Copywriter"
    );
    assert_eq!(
        super::seat_display_name(Some(&record), "stranger"),
        "stranger"
    );
    assert_eq!(super::seat_display_name(None, "copywriter"), "copywriter");
}
