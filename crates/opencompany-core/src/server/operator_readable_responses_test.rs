use super::readable_responses;
use crate::ports::types::{
    CompanyEvent, CompanyId, CompanyRecord, EventSeq, Mention, MentionTarget, OutboundMessage,
    StoredEvent,
};
use crate::server::chat_history::{MessageView, Viewer};
use crate::server::readable::{DisplayNames, project_history};

fn names() -> DisplayNames {
    let mut record = CompanyRecord::from_manifest(
        CompanyId::new("acme"),
        toml::from_str(
            r#"
[company]
name = "Acme"

[[agent]]
id = "software_engineer"
role = "Software Engineer"

[[agent]]
id = "qa_engineer"
role = "QA Engineer"
"#,
        )
        .expect("valid manifest"),
    );
    record
        .overlay_agent_edits
        .push(crate::ports::types::AgentOverride {
            agent_id: "software_engineer".to_string(),
            name: Some("Sam".to_string()),
            ..Default::default()
        });
    record
        .overlay_agent_edits
        .push(crate::ports::types::AgentOverride {
            agent_id: "qa_engineer".to_string(),
            name: Some("Quinn".to_string()),
            ..Default::default()
        });
    DisplayNames::from_record(&record)
}

const BODY: &str = "[conversation: engineering]\n`qa_engineer` found it, @qa_engineer please look";

fn chip() -> Mention {
    let offset = BODY.find("@qa_engineer").unwrap();
    Mention {
        target: MentionTarget::Agent {
            id: "qa_engineer".to_string(),
        },
        text: "@qa_engineer".to_string(),
        offset,
        quiet: false,
    }
}

fn reply(text: &str, mentions: Vec<Mention>) -> OutboundMessage {
    OutboundMessage {
        channel: "engineering".to_string(),
        agent: Some("software_engineer".to_string()),
        text: text.to_string(),
        steps: Vec::new(),
        reply_to: None,
        task_id: None,
        outputs: Vec::new(),
        message_id: None,
        mentions,
    }
}

fn stored() -> StoredEvent {
    StoredEvent {
        seq: EventSeq::new(7),
        company: CompanyId::new("acme"),
        event: CompanyEvent::AgentReply {
            audience: Vec::new(),
            episode: None,
            chat_id: "engineering".to_string(),
            agent_id: "software_engineer".to_string(),
            text: BODY.to_string(),
            steps: Vec::new(),
            task_id: None,
            outputs: Vec::new(),
            parent: None,
            mentions: vec![chip()],
            mention_depth: 0,
        },
        at_millis: 1,
    }
}

/// **A row reads the same live, on the stream, and after a reload.**
#[test]
fn a_live_reply_reads_as_the_reloaded_one_will() {
    let names = names();
    let posted = readable_responses(vec![reply(BODY, vec![chip()])], &names).remove(0);
    let streamed = super::project_event_for_viewer(
        &stored(),
        &std::collections::HashMap::new(),
        &names,
        &Viewer::Operator,
        true,
    )
    .expect("a reply is projected");
    let mut history = vec![MessageView::project(
        stored(),
        &Viewer::Operator,
        &std::collections::HashMap::new(),
    )];
    project_history(&mut history, &names);

    let expected = "Quinn found it, @qa_engineer please look";
    assert_eq!(posted.text, expected);
    assert_eq!(streamed["text"], expected);
    assert_eq!(history[0].text, expected);
    assert_eq!(
        streamed["cueText"], BODY,
        "the body as written rides beside it"
    );
    assert_eq!(history[0].cue_text, BODY);

    let chip_at = expected.find("@qa_engineer").unwrap();
    assert_eq!(posted.mentions[0].offset, chip_at);
    assert_eq!(streamed["mentions"][0]["offset"], chip_at);
    assert_eq!(history[0].mentions[0].offset, chip_at);
}

#[test]
fn an_ordinary_reply_is_untouched() {
    let cleaned = readable_responses(
        vec![reply("here is the summary you asked for", Vec::new())],
        &names(),
    );
    assert_eq!(cleaned[0].text, "here is the summary you asked for");
}
