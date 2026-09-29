use crate::company::runtime::CompanyRuntime;
use crate::ports::types::{CompanyEvent, CompanyId, EventSeq};
use std::sync::Arc;
use tempfile::TempDir;

/// A company where one spelling reaches two different things: a roster
/// teammate `writer` and a desk `writer`. The reported case was a
/// teammate and a *person* sharing a name, which `mentions.rs` covers
/// directly; the collision is the same one and this needs no user store
/// to set up.
async fn runtime() -> (Arc<CompanyRuntime>, TempDir) {
    let home = tempfile::Builder::new()
        .prefix("opencompany-ambiguous-mentions-")
        .tempdir()
        .expect("tempdir");
    let manifest: crate::company::CompanyManifest = toml::from_str(
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n\
         [[agent]]\nid = \"writer\"\nrole = \"Writer\"\n\
         [[group_chat]]\nid = \"writer\"\nname = \"Writer desk\"\nmembers = [\"writer\"]\n",
    )
    .expect("manifest");
    let runtime = Arc::new(
        crate::runtime::RuntimeBuilder::new(home.path().to_path_buf(), manifest)
            .with_id(CompanyId::new("acme"))
            .build()
            .await
            .expect("runtime"),
    );
    (runtime, home)
}

/// Every `AgentReply` journaled so far, as `(agent, chat, text)`.
async fn replies(runtime: &Arc<CompanyRuntime>) -> Vec<(String, String, String)> {
    runtime
        .events
        .read_from(runtime.id(), EventSeq::new(0), 500)
        .await
        .expect("events")
        .into_iter()
        .filter_map(|stored| match stored.event {
            CompanyEvent::AgentReply {
                agent_id,
                chat_id,
                text,
                ..
            } => Some((agent_id, chat_id, text)),
            _ => None,
        })
        .collect()
}

/// The signal the founder never got: a positive line in the channel
/// saying the ping matched two things and reached neither. It is
/// attributed to the runtime itself, not to a teammate — the console
/// renders `SYSTEM_AUTHOR` as a centred system pill, and putting a
/// roster face on the runtime's own refusal would misstate who decided.
#[tokio::test]
async fn an_ambiguous_name_is_reported_in_the_channel_it_was_sent_to() {
    let (runtime, _home) = runtime().await;
    let resolved = runtime
        .resolve_mentions_reporting("@writer can you draft the autumn brief?", None, None)
        .await;
    assert!(
        resolved.mentions.is_empty(),
        "the ping is still refused: {:?}",
        resolved.mentions
    );
    assert_eq!(resolved.ambiguous.len(), 1, "and reported once");

    runtime
        .post_mention_ambiguity_note("general", None, &resolved.ambiguous)
        .await;

    let posted = replies(&runtime).await;
    assert_eq!(posted.len(), 1, "exactly one line: {posted:?}");
    let (agent, chat, text) = &posted[0];
    assert_eq!(agent, crate::ports::SYSTEM_AUTHOR);
    assert_eq!(chat, "general", "into the conversation it was sent to");
    assert!(text.contains("@writer"), "names the literal typed: {text}");
    assert!(
        text.contains("pinged nobody"),
        "states what happened: {text}"
    );
}

/// The threaded case: an ambiguous `@name` sent as a reply inside a
/// thread must get its explanatory note posted into that same thread,
/// not top-level in the channel — otherwise the note contradicts its own
/// doc comment's promise to speak "in the conversation itself" the
/// moment the operator is looking at a thread rather than the main
/// timeline.
#[tokio::test]
async fn an_ambiguous_name_in_a_thread_is_reported_into_that_thread() {
    let (runtime, _home) = runtime().await;

    // Seed a root message to thread off of, the same way a real
    // threaded reply would name an existing event as its parent.
    let root = runtime
        .events
        .append(
            runtime.id(),
            CompanyEvent::OperatorMessage {
                text: "kicking off a thread".to_string(),
                by: None,
                chat: Some("general".to_string()),
                parent: None,
                deliverable: None,
                mentions: Vec::new(),
                attachments: Vec::new(),
            },
        )
        .await
        .expect("root event");

    let resolved = runtime
        .resolve_mentions_reporting("@writer can you draft the autumn brief?", None, None)
        .await;
    assert_eq!(resolved.ambiguous.len(), 1, "reported once");

    runtime
        .post_mention_ambiguity_note("general", Some(root), &resolved.ambiguous)
        .await;

    let threaded = runtime
        .events
        .read_from(runtime.id(), EventSeq::new(0), 500)
        .await
        .expect("events")
        .into_iter()
        .filter_map(|stored| match stored.event {
            CompanyEvent::AgentReply {
                parent, chat_id, ..
            } => Some((parent, chat_id)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(threaded.len(), 1, "exactly one reply: {threaded:?}");
    let (parent, chat) = &threaded[0];
    assert_eq!(chat, "general");
    assert_eq!(
        *parent,
        Some(root),
        "the note lands in the thread the ambiguous ping was sent in, \
         not top-level in the channel"
    );
}

/// The negative half, and the one that keeps the notice worth reading: a
/// message whose names all resolve says nothing at all.
#[tokio::test]
async fn an_unambiguous_message_posts_nothing() {
    let (runtime, _home) = runtime().await;
    let resolved = runtime
        .resolve_mentions_reporting("@ceo can you take a look?", None, None)
        .await;
    assert_eq!(resolved.mentions.len(), 1, "the ping resolves");
    runtime
        .post_mention_ambiguity_note("general", None, &resolved.ambiguous)
        .await;
    assert!(
        replies(&runtime).await.is_empty(),
        "nothing is posted for a message that named somebody"
    );
}
