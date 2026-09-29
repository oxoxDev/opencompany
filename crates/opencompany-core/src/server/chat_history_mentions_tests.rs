use super::tests_reactions::{at, labels};
use super::*;

/// A desk-visible reply by the CEO on `chat_id`.
fn agent_reply(chat_id: &str) -> CompanyEvent {
    CompanyEvent::AgentReply {
        audience: Vec::new(),
        mentions: Vec::new(),
        mention_depth: 0,
        parent: None,
        task_id: None,
        outputs: Vec::new(),
        chat_id: chat_id.to_string(),
        agent_id: "ceo".to_string(),
        text: "hi".to_string(),
        steps: Vec::new(),
        episode: None,
    }
}

fn mention(target: MentionTarget, text: &str, offset: usize) -> Mention {
    Mention {
        target,
        text: text.to_string(),
        offset,
        quiet: false,
    }
}

fn message_mentioning(mentions: Vec<Mention>) -> CompanyEvent {
    CompanyEvent::OperatorMessage {
        mentions,
        parent: None,
        text: "ping".to_string(),
        by: None,
        chat: Some("studio".to_string()),
        deliverable: None,
        attachments: Vec::new(),
    }
}

/// Who *typed* a line is a fact only the host still holds (issue #1734).
///
/// Every downstream shortcut for it is wrong, and the two obvious ones are
/// wrong in ways that look right:
///
/// * `mine` is per-viewer, so a colleague's own message is `mine: false`
///   and lands on the company side of their reader's transcript, beside the
///   agent replies.
/// * `channel == "operator"` collides head-on. The offline echo brain names
///   its own outbound channel `operator` (`brain::echo`), exactly as this
///   arm does, so a journaled echo reply and a human's message carry the
///   same label. A console that split on it marked neither, which suppressed
///   the marker on precisely the replies it exists for — caught in a browser
///   against a live host, not by a unit test.
///
/// So the projection says it, and this test pins both directions with the
/// echo brain's own channel label in play, because that is the collision.
#[test]
fn only_a_persons_message_is_projected_as_by_person() {
    let typed = MessageView::project(
        at(
            1,
            CompanyEvent::OperatorMessage {
                mentions: Vec::new(),
                parent: None,
                text: "on it".to_string(),
                by: Some(Actor {
                    kind: ActorKind::User,
                    id: "u1".to_string(),
                }),
                chat: Some("studio".to_string()),
                deliverable: None,
                attachments: Vec::new(),
            },
        ),
        // Projected for *another* reader, which is the case that matters:
        // for them this is `mine: false` and nothing else distinguishes it.
        &Viewer::User("u2".to_string()),
        &labels(),
    );
    assert!(typed.by_person, "a person typed this");
    assert!(!typed.mine, "and it is not this reader's own line");

    // The echo brain's reply as the runtime journals it: an `AgentReply`
    // whose agent id is the outbound channel the brain named — `operator`,
    // the very label the arm above hardcodes.
    let echoed = MessageView::project(
        at(
            2,
            CompanyEvent::AgentReply {
                audience: Vec::new(),
                mentions: Vec::new(),
                mention_depth: 0,
                parent: None,
                task_id: None,
                outputs: Vec::new(),
                chat_id: "studio".to_string(),
                agent_id: "operator".to_string(),
                text: "You said: on it".to_string(),
                steps: Vec::new(),
                episode: None,
            },
        ),
        &Viewer::User("u2".to_string()),
        &labels(),
    );
    assert!(!echoed.by_person, "no person typed the echo brain's reply");
    assert_eq!(
        echoed.channel, typed.channel,
        "the collision is real: the channel label cannot tell these apart",
    );
}

/// A person's mention reaches a reader as a **label**, never as the user id
/// it is stored under — the same rule `by_label` follows for reactions.
#[test]
fn project_resolves_a_person_to_a_label_and_never_to_an_id() {
    let view = MessageView::project(
        at(
            7,
            message_mentioning(vec![mention(
                MentionTarget::User {
                    id: "u1".to_string(),
                },
                "@Ada",
                0,
            )]),
        ),
        &Viewer::Operator,
        &labels(),
    );
    assert_eq!(view.mentions.len(), 1);
    assert_eq!(view.mentions[0].label, "Ada");
    assert_eq!(view.mentions[0].text, "@Ada");
    assert_eq!(view.mentions[0].offset, 0);
    assert!(
        !view.mentions[0].label.contains("u1"),
        "the stored id must not reach a reader"
    );
}

/// `mine` is per viewer: the same stored row is the reader's own mention
/// for one person and somebody else's for everyone else.
#[test]
fn project_decides_mine_per_viewer() {
    let event = at(
        8,
        message_mentioning(vec![mention(
            MentionTarget::User {
                id: "u1".to_string(),
            },
            "@Ada",
            0,
        )]),
    );
    let ada = MessageView::project(event.clone(), &Viewer::User("u1".to_string()), &labels());
    assert!(ada.mentions[0].mine);

    let grace = MessageView::project(event, &Viewer::User("u2".to_string()), &labels());
    assert!(!grace.mentions[0].mine);
}

/// A broadcast is addressed to whoever is reading, so it is everybody's own
/// mention — that is what makes it badge every recipient.
#[test]
fn everyone_is_mine_for_every_reader() {
    let event = at(
        9,
        message_mentioning(vec![mention(MentionTarget::Everyone, "@everyone", 0)]),
    );
    for viewer in [
        Viewer::Operator,
        Viewer::User("u1".to_string()),
        Viewer::User("u2".to_string()),
    ] {
        let view = MessageView::project(event.clone(), &viewer, &labels());
        assert!(view.mentions[0].mine, "viewer: {viewer:?}");
        assert_eq!(view.mentions[0].label, "everyone");
    }
}

/// A person who has since been removed has no label to resolve to. The
/// literal text the author typed is the honest fallback — it is what a
/// reader would have seen anyway — and it must not be the raw id.
#[test]
fn a_mention_of_a_departed_person_falls_back_to_the_typed_text() {
    let view = MessageView::project(
        at(
            10,
            message_mentioning(vec![mention(
                MentionTarget::User {
                    id: "gone".to_string(),
                },
                "@Bob",
                0,
            )]),
        ),
        &Viewer::Operator,
        &labels(),
    );
    assert_eq!(view.mentions[0].label, "Bob");
}

#[test]
fn a_teammate_and_a_desk_project_their_ids_as_labels() {
    let view = MessageView::project(
        at(
            11,
            message_mentioning(vec![
                mention(
                    MentionTarget::Agent {
                        id: "engineer".to_string(),
                    },
                    "@engineer",
                    0,
                ),
                mention(
                    MentionTarget::Desk {
                        id: "engineering".to_string(),
                    },
                    "@engineering",
                    10,
                ),
            ]),
        ),
        &Viewer::Operator,
        &labels(),
    );
    let labels: Vec<&str> = view.mentions.iter().map(|m| m.label.as_str()).collect();
    assert_eq!(labels, vec!["engineer", "engineering"]);
    assert!(
        view.mentions.iter().all(|m| !m.mine),
        "a teammate or a desk is never the human reader"
    );
}

#[test]
fn a_quiet_mention_projects_as_quiet() {
    let view = MessageView::project(
        at(
            12,
            message_mentioning(vec![Mention {
                quiet: true,
                ..mention(
                    MentionTarget::User {
                        id: "u1".to_string(),
                    },
                    "@Ada",
                    0,
                )
            }]),
        ),
        &Viewer::User("u1".to_string()),
        &labels(),
    );
    assert!(view.mentions[0].quiet);
}

#[test]
fn a_message_that_mentions_nobody_projects_an_empty_list() {
    let view = MessageView::project(
        at(13, message_mentioning(Vec::new())),
        &Viewer::Operator,
        &labels(),
    );
    assert!(view.mentions.is_empty());
}

/// A thread parent survives projection on both halves of an exchange, as
/// the message id a reader can resolve rather than a raw sequence number.
#[test]
fn project_carries_the_thread_parent() {
    let operator = MessageView::project(
        at(
            12,
            CompanyEvent::OperatorMessage {
                mentions: Vec::new(),
                parent: Some(EventSeq::new(4)),
                text: "a follow-up".to_string(),
                by: None,
                chat: Some("studio".to_string()),
                deliverable: None,
                attachments: Vec::new(),
            },
        ),
        &Viewer::Operator,
        &labels(),
    );
    assert_eq!(operator.parent_id.as_deref(), Some("4"));

    let reply = MessageView::project(
        at(
            13,
            CompanyEvent::AgentReply {
                audience: Vec::new(),
                mentions: Vec::new(),
                mention_depth: 0,
                parent: Some(EventSeq::new(4)),
                task_id: None,
                outputs: Vec::new(),
                chat_id: "studio".to_string(),
                agent_id: "ceo".to_string(),
                text: "on it".to_string(),
                steps: Vec::new(),
                episode: None,
            },
        ),
        &Viewer::Operator,
        &labels(),
    );
    assert_eq!(reply.parent_id.as_deref(), Some("4"));

    // A message with no parent is in the channel, not in a thread — which
    // is every message journaled before threads were persisted.
    let plain = MessageView::project(at(14, agent_reply("studio")), &Viewer::Operator, &labels());
    assert!(plain.parent_id.is_none());
}

#[test]
fn legacy_operator_message_without_chat_stays_on_general() {
    let event = CompanyEvent::OperatorMessage {
        mentions: Vec::new(),
        parent: None,
        text: "hi".to_string(),
        by: None,
        chat: None,
        deliverable: None,
        attachments: Vec::new(),
    };
    assert!(owns(GENERAL_CHANNEL_ID, GENERAL_CHANNEL_ID, &event));
    assert!(!owns("strategy", "Strategy desk", &event));
}
