//! What a committed utterance's `@names` do: they are journaled on the row,
//! they badge the people they name, and they move nothing (#2441).
//!
//! Driven straight through [`DeskHost::commit`], which is the seam that turns
//! an utterance into a row. The coverage this replaces ran the same
//! assertions through the hand-rolled round loop over a scripted seat runner;
//! that loop is gone, and every assertion it made reads `log.rows()`, so the
//! commit seam is where they belong now. Nothing here needs an episode to run.

use std::sync::Arc;

use tinyhivemind_driver::Commit;
use tinyhivemind_openhuman::Journal;

use super::DeskHost;
use crate::hive::test_support::MemoryLog;
use crate::ports::events::EventLog;
use crate::ports::types::{CompanyEvent, Mention, MentionTarget, StoredEvent};
use crate::ports::users::{UserRecord, UserRole, UserStatus};

/// One desk of two, so `@writer` has a teammate to resolve to.
const ONE_DESK: &str = r#"
[company]
name = "Acme"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[agent]]
id = "writer"
role = "Writer"

[[group_chat]]
id = "engineering"
name = "Engineering desk"
description = "How things are built."
members = ["ceo", "writer"]
"#;

/// A desk row from `ceo`, said on the open desk.
///
/// Built through serde rather than a struct literal: `Commit::kind` is
/// `pub(super)` to the driver, because a host only ever receives commits --
/// the conductor is the one that makes them. `Deserialize` is the seam the
/// library leaves open for a caller on this side of the wire.
fn post(text: &str) -> Commit {
    serde_json::from_value(serde_json::json!({
        "author": "ceo",
        "utterance": { "kind": "post", "message": text },
        "thread": null,
        "only_for": null,
        "conversation": null,
        "purpose": { "kind": "desk" },
    }))
    .expect("a desk post is a commit the driver would make")
}

/// A live runtime over a temp home, one human collaborator seeded, and a desk
/// host whose mention seam is that runtime's.
///
/// The host's company record is the same manifest the runtime was built with,
/// so the directory the seam resolves against is the roster the desk is
/// seated from.
async fn host() -> (
    DeskHost,
    Arc<MemoryLog>,
    Arc<crate::company::runtime::CompanyRuntime>,
    String,
    tempfile::TempDir,
) {
    let home = tempfile::Builder::new()
        .prefix("opencompany-hive-mentions-")
        .tempdir()
        .expect("tempdir");
    let manifest: crate::company::CompanyManifest =
        toml::from_str(ONE_DESK).expect("test manifest parses");
    let runtime = Arc::new(
        crate::runtime::RuntimeBuilder::new(home.path().to_path_buf(), manifest)
            .with_id(MemoryLog::company())
            .build()
            .await
            .expect("runtime"),
    );

    let person = crate::ports::generate_id();
    let now = crate::ports::now_millis();
    runtime
        .users()
        .upsert_user(
            runtime.id(),
            &UserRecord {
                id: person.clone(),
                email: "dana@example.test".to_string(),
                display_name: Some("Dana".to_string()),
                avatar: None,
                role: UserRole::Member,
                status: UserStatus::Active,
                password_hash: None,
                must_change_password: false,
                created_at_millis: now,
                last_seen_at_millis: None,
                updated_at_millis: now,
            },
        )
        .await
        .expect("seed the person");

    let log = Arc::new(MemoryLog::default());
    let events: Arc<dyn EventLog> = log.clone();
    let host = DeskHost::new(
        MemoryLog::company(),
        "engineering".to_owned(),
        "Engineering desk".to_owned(),
        events,
        vec!["ceo".to_owned(), "writer".to_owned()],
    )
    .resolving_mentions(runtime.mention_seam());

    (host, log, runtime, person, home)
}

/// Every `AgentReply` journaled, as `(author, text, mentions)`.
fn replies(rows: &[StoredEvent]) -> Vec<(String, String, Vec<Mention>)> {
    rows.iter()
        .filter_map(|stored| match &stored.event {
            CompanyEvent::AgentReply {
                agent_id,
                text,
                mentions,
                ..
            } => Some((agent_id.clone(), text.clone(), mentions.clone())),
            _ => None,
        })
        .collect()
}

/// The bug: a committed utterance naming a teammate journaled
/// `mentions: []`, so 63 of 105 replies on a live journal named somebody and
/// recorded nobody.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_committed_utterance_journals_the_mentions_it_names() {
    let (host, log, _runtime, _person, _home) = host().await;
    host.commit(&post("@writer take the draft."))
        .expect("the row commits");

    let named = replies(&log.rows())
        .into_iter()
        .find(|(_, text, _)| text.contains("@writer"))
        .expect("the utterance naming the writer was journaled");
    assert_eq!(
        named.2.len(),
        1,
        "the row has to carry the teammate it names, not an empty list: {:?}",
        named.2
    );
    assert_eq!(
        named.2[0].target,
        MentionTarget::Agent {
            id: "writer".to_string()
        }
    );
    assert_eq!(named.2[0].text, "@writer");
}

/// The hive twin of `a_mention_in_an_agent_reply_notifies_the_person_it_names`
/// (`server/operator_test_group_14.rs`): the orchestrator path files this row
/// and the desk path did not, for the same `@` in the same company.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_named_in_a_desk_reply_is_notified() {
    let (host, log, runtime, person, _home) = host().await;
    host.commit(&post("@Dana can you confirm?"))
        .expect("the row commits");

    let named = replies(&log.rows())
        .into_iter()
        .find(|(_, text, _)| text.contains("@Dana"))
        .expect("the utterance naming Dana was journaled");
    assert_eq!(
        named.2.first().map(|m| &m.target),
        Some(&MentionTarget::User { id: person.clone() }),
        "the person has to resolve through the same directory the operator \
         path uses: {:?}",
        named.2
    );

    let notes = runtime
        .notifications()
        .list(runtime.id(), &person)
        .await
        .expect("notifications read");
    assert_eq!(notes.len(), 1, "one row, not one per recipient: {notes:?}");
    assert_eq!(notes[0].notification.kind, "mention");
    assert_eq!(
        notes[0].notification.audience.as_deref(),
        Some(std::slice::from_ref(&person)),
        "the audience has to be the person the reply named"
    );
}

/// A teammate this desk already seats is named. That teammate is already
/// holding the whole conversation; minting anything here would move the
/// episode twice for one utterance.
///
/// This is the fuse the `AgentReply::mentions` doc claims, made enforceable:
/// the mention is recorded and it moves nothing. The commit writes one row
/// and only one row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_same_desk_mention_records_but_mints_no_turn_and_no_child_cycle() {
    let (host, log, _runtime, _person, _home) = host().await;
    host.commit(&post("@writer take the draft."))
        .expect("the row commits");

    let named = replies(&log.rows())
        .into_iter()
        .find(|(_, text, _)| text.contains("@writer"))
        .expect("the utterance naming the writer was journaled");
    assert_eq!(named.2.len(), 1, "recorded");

    let kinds: Vec<&'static str> = log.rows().iter().map(|row| row.event.kind()).collect();
    assert!(
        !kinds.contains(&"ReferralEnqueued"),
        "a same-desk mention must enqueue no referral: {kinds:?}"
    );
    assert!(
        !kinds.contains(&"TaskDispatched"),
        "a same-desk mention must mint no child cycle: {kinds:?}"
    );
    assert_eq!(
        kinds,
        vec!["AgentReply"],
        "one utterance is one row: {kinds:?}"
    );
}

/// The two halves of a mention must agree: the set stored on the row and the
/// set a referral decision would act on are one resolution, not two.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_stored_mentions_are_the_ones_the_referral_decision_reads() {
    let (host, log, _runtime, person, _home) = host().await;
    host.commit(&post("@writer and @Dana, please."))
        .expect("the row commits");

    let named = replies(&log.rows())
        .into_iter()
        .find(|(_, text, _)| text.contains("@writer"))
        .expect("the utterance was journaled");
    let stored: Vec<_> = named.2.iter().map(|m| m.target.clone()).collect();
    assert_eq!(
        stored,
        vec![
            MentionTarget::Agent {
                id: "writer".to_string()
            },
            MentionTarget::User { id: person },
        ],
        "both targets, in authored order"
    );

    // What `refer` reads is these same rows put through the library's shape,
    // so the targets it judges are the targets that were recorded.
    let carried: Vec<_> = named
        .2
        .iter()
        .map(crate::hive::dispatch::tinyhivemind_mention)
        .map(|m| m.target)
        .collect();
    assert_eq!(
        carried,
        vec![
            tinyhivemind_core::mention::MentionTarget::Agent {
                id: "writer".to_string()
            },
            tinyhivemind_core::mention::MentionTarget::Person {
                id: stored
                    .iter()
                    .find_map(|t| match t {
                        MentionTarget::User { id } => Some(id.clone()),
                        _ => None,
                    })
                    .expect("the person target"),
            },
        ],
    );
}

/// The seam resolves nothing when it is absent, and the row still commits —
/// the state every pre-seam host is in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_with_no_seam_journals_a_reply_with_no_mentions() {
    let (mut host, log, _runtime, _person, _home) = host().await;
    host.mentions = None;
    host.commit(&post("@writer take the draft."))
        .expect("the row commits");

    let named = replies(&log.rows())
        .into_iter()
        .find(|(_, text, _)| text.contains("@writer"))
        .expect("the utterance was still journaled");
    assert!(named.2.is_empty(), "no seam, no mentions: {:?}", named.2);
}
