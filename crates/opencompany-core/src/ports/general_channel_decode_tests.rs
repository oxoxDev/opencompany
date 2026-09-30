use super::*;
use crate::ports::types::CompanyEvent;
use crate::server::chat_history::owns;

fn decode(line: &str) -> CompanyEvent {
    serde_json::from_str(line).expect("a journaled line loads")
}

#[test]
fn every_legacy_spelling_is_general() {
    assert_eq!(GENERAL_CHANNEL_ID, "general-channel");
    for spelling in [
        "",
        "general",
        "General",
        "GENERAL",
        "main",
        "Main",
        "general-channel",
        "General-Channel",
    ] {
        assert!(is_general_spelling(spelling), "{spelling:?}");
        assert_eq!(decode_general_chat_id(spelling.into()), GENERAL_CHANNEL_ID);
    }
    for other in ["engineering", "dm:ceo", "general_store", "mainframe"] {
        assert!(!is_general_spelling(other), "{other:?}");
        assert_eq!(decode_general_chat_id(other.into()), other);
    }
    assert_eq!(decode_general_chat_opt(None), None);
}

#[test]
fn a_mixed_legacy_journal_reads_as_one_general_transcript() {
    let lines = [
        r#"{"kind":"OperatorMessage","text":"unaddressed"}"#,
        r#"{"kind":"OperatorMessage","text":"from the console","chat":"main"}"#,
        r#"{"kind":"OperatorMessage","text":"now","chat":"general"}"#,
        r#"{"kind":"AgentReply","chat_id":"General","agent_id":"ceo","text":"a"}"#,
        r#"{"kind":"AgentReply","chat_id":"","agent_id":"ceo","text":"b"}"#,
        r#"{"kind":"AgentReply","chat_id":"main","agent_id":"ceo","text":"c"}"#,
    ];
    for line in lines {
        let event = decode(line);
        assert!(
            owns(GENERAL_CHANNEL_ID, GENERAL_CHANNEL_NAME, &event),
            "#general owns {line}"
        );
        assert!(!owns("engineering", "Engineering", &event), "{line}");
    }
    let desk =
        decode(r#"{"kind":"AgentReply","chat_id":"engineering","agent_id":"ceo","text":"d"}"#);
    assert!(!owns(GENERAL_CHANNEL_ID, GENERAL_CHANNEL_NAME, &desk));
}

#[test]
fn a_legacy_chat_id_decodes_to_general_on_every_stamped_field() {
    match decode(r#"{"kind":"AgentReply","chat_id":"main","agent_id":"ceo","text":"hi"}"#) {
        CompanyEvent::AgentReply { chat_id, .. } => assert_eq!(chat_id, GENERAL_CHANNEL_ID),
        other => panic!("{other:?}"),
    }
    match decode(r#"{"kind":"OperatorMessage","text":"hi","chat":"General"}"#) {
        CompanyEvent::OperatorMessage { chat, .. } => {
            assert_eq!(chat.as_deref(), Some(GENERAL_CHANNEL_ID))
        }
        other => panic!("{other:?}"),
    }
    match decode(r#"{"kind":"OperatorMessage","text":"hi"}"#) {
        CompanyEvent::OperatorMessage { chat, .. } => assert_eq!(chat, None),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_absent_optional_chat_stays_absent() {
    assert_eq!(
        deserialize_general_chat_opt(serde_json::Value::Null).unwrap(),
        None
    );
    assert_eq!(
        deserialize_general_chat_opt(serde_json::json!("main")).unwrap(),
        Some(GENERAL_CHANNEL_ID.to_string())
    );
    assert_eq!(
        deserialize_general_chat(serde_json::json!("")).unwrap(),
        GENERAL_CHANNEL_ID
    );
}

#[test]
fn a_legacy_teammate_called_main_keeps_its_dm_apart_from_general() {
    let record = crate::ports::types::CompanyRecord::from_manifest(
        crate::ports::types::CompanyId::new("acme"),
        toml::from_str(
            "[company]\nname = \"Acme\"\n\n[[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n\n\
             [[agent]]\nid = \"main\"\nrole = \"Legacy\"\n",
        )
        .expect("manifest"),
    );
    let (id, name) = crate::server::chat_history::desk_aliases(&record, Some("dm:main"));
    assert_eq!(id, "dm:main");

    let dm = decode(r#"{"kind":"AgentReply","chat_id":"dm:main","agent_id":"main","text":"hi"}"#);
    assert!(owns(&id, &name, &dm));
    assert!(!owns(GENERAL_CHANNEL_ID, GENERAL_CHANNEL_NAME, &dm));

    let general =
        decode(r#"{"kind":"AgentReply","chat_id":"general","agent_id":"ceo","text":"all"}"#);
    assert!(
        !owns(&id, &name, &general),
        "#general is not the teammate's DM"
    );

    assert_eq!(
        crate::runtime::delegation_tools::chat_responder(&record, "dm:main").as_deref(),
        Some("main")
    );
    assert_eq!(
        crate::runtime::delegation_tools::chat_responder(&record, GENERAL_CHANNEL_ID),
        None
    );
}

#[test]
fn a_record_stored_under_the_legacy_id_loads_as_general_channel() {
    let stored: GeneralChannel =
        serde_json::from_str(r#"{"id":"general","name":"General","members":["ceo"]}"#)
            .expect("a stored #general loads");
    assert_eq!(stored.id, GENERAL_CHANNEL_ID);
    assert_eq!(stored.name, GENERAL_CHANNEL_NAME);
    assert_eq!(stored.members, vec!["ceo".to_string()]);

    match decode(r#"{"kind":"AgentReply","chat_id":"general","agent_id":"ceo","text":"hi"}"#) {
        CompanyEvent::AgentReply { chat_id, .. } => assert_eq!(chat_id, GENERAL_CHANNEL_ID),
        other => panic!("{other:?}"),
    }
    match decode(r#"{"kind":"OperatorMessage","text":"hi","chat":"general"}"#) {
        CompanyEvent::OperatorMessage { chat, .. } => {
            assert_eq!(chat.as_deref(), Some(GENERAL_CHANNEL_ID))
        }
        other => panic!("{other:?}"),
    }
}
