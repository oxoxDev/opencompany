use super::*;
use crate::ports::types::{CompanyId, OverlayAgent, OverlayBlob};

const ROSTER: &str = "[company]\nname = \"Acme\"\n\
     [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n\
     [[agent]]\nid = \"writer\"\nrole = \"Writer\"\n";

fn record() -> CompanyRecord {
    CompanyRecord::from_manifest(
        CompanyId::new("acme"),
        toml::from_str(ROSTER).expect("manifest"),
    )
}

fn overlay(id: &str) -> OverlayAgent {
    OverlayAgent {
        provider: None,
        id: id.into(),
        name: id.into(),
        role: "Worker".into(),
        description: None,
        tools: None,
        skills: None,
        model: None,
        harness: None,
    }
}

#[test]
fn a_new_record_has_general_with_the_whole_roster() {
    let record = record();
    assert_eq!(record.general_channel.id, GENERAL_CHANNEL_ID);
    assert_eq!(record.general_channel.name, GENERAL_CHANNEL_NAME);
    assert_eq!(record.general_channel.members, ["ceo", "writer"]);
}

#[test]
fn sync_orders_manifest_before_overlay_and_drops_the_retired() {
    let mut record = record();
    record.overlay_agents.push(overlay("analyst"));
    let delta = record.retire_agent("ceo");
    assert_eq!(record.general_channel.members, ["writer", "analyst"]);
    assert_eq!(delta.added, ["analyst"]);
    assert_eq!(delta.removed, ["ceo"]);
}

#[test]
fn sync_restores_a_tampered_identity() {
    let mut record = record();
    record.general_channel.id = "main".into();
    record.general_channel.name = "Lobby".into();
    let delta = record.sync_general_members();
    assert!(delta.is_empty());
    assert_eq!(
        record.general_channel,
        GeneralChannel {
            id: GENERAL_CHANNEL_ID.into(),
            name: GENERAL_CHANNEL_NAME.into(),
            members: vec!["ceo".into(), "writer".into()],
        }
    );
}

#[test]
fn general_survives_the_overlay_blob() {
    let record = record();
    let json = serde_json::to_string(&OverlayBlob::from_record(&record)).unwrap();
    let blob = OverlayBlob::parse(&json).unwrap();
    assert_eq!(blob.general_channel, Some(record.general_channel));
}

#[test]
fn a_blob_written_before_general_was_stored_loads_without_it() {
    let blob = OverlayBlob::parse(r#"{"agents":[]}"#).unwrap();
    assert_eq!(blob.general_channel, None);
    let legacy = OverlayBlob::parse("[]").unwrap();
    assert_eq!(legacy.general_channel, None);
}

#[test]
fn a_record_written_before_general_was_stored_defaults_it() {
    let mut value = serde_json::to_value(record()).unwrap();
    value.as_object_mut().unwrap().remove("general_channel");
    let loaded: CompanyRecord = serde_json::from_value(value).unwrap();
    assert_eq!(loaded.general_channel, GeneralChannel::default());
}
