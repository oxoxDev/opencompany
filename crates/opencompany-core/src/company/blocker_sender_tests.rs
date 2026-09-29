use super::*;
use crate::ports::types::{CompanyId, CompanyRecord};

/// A record from a bare manifest, with every overlay empty. Agents and desks
/// come from the TOML; nothing else matters to sender resolution.
fn record(manifest_toml: &str) -> CompanyRecord {
    CompanyRecord {
        general_channel: Default::default(),
        overlay_desk_hive: Vec::new(),
        id: CompanyId::new("acme"),
        manifest: toml::from_str(manifest_toml).expect("parse manifest"),
        ledger: Vec::new(),
        lifecycle: "running".to_string(),
        overlay_agents: Vec::new(),
        overlay_desk_members: Vec::new(),
        overlay_desk_order: Vec::new(),
        overlay_desks: Vec::new(),
        overlay_workflows: Vec::new(),
        overlay_agent_edits: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_policy: None,
        overlay_tool_grants: None,
        overlay_desk_tools: Default::default(),
        overlay_budgets: Vec::new(),
        disabled_workflows: Vec::new(),
        template_provenance: None,
        setup: None,
        name_confirmed: false,
        activation_completed_at: None,
        created_at_millis: None,
    }
}

const MANIFEST: &str = "[company]\nname = \"Acme\"\n\
     [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n\
     [[agent]]\nid = \"eng\"\nrole = \"Engineer\"\n\
     [[agent]]\nid = \"des\"\nrole = \"Designer\"\n\
     [[group_chat]]\nid = \"studio\"\nname = \"Studio\"\nmembers = [\"des\", \"eng\"]\n";

#[test]
fn the_triggering_agent_wins() {
    let record = record(MANIFEST);
    let sender = resolve_sender(
        &record,
        &BlockerSenderSignals {
            started_by: Some(StartedBy::Agent("eng".to_string())),
            owner_desk: Some("studio".to_string()),
            assignee: Some("des".to_string()),
        },
    );
    assert_eq!(
        sender, "eng",
        "the agent that started the run owns its stop"
    );
}

#[test]
fn an_unknown_triggering_agent_falls_through() {
    let record = record(MANIFEST);
    let sender = resolve_sender(
        &record,
        &BlockerSenderSignals {
            started_by: Some(StartedBy::Agent("ghost".to_string())),
            owner_desk: Some("studio".to_string()),
            assignee: None,
        },
    );
    assert_eq!(
        sender, "des",
        "a started_by naming nobody on the roster does not win; the desk lead does"
    );
}

#[test]
fn an_operator_started_run_does_not_attribute_an_agent() {
    let record = record(MANIFEST);
    let sender = resolve_sender(
        &record,
        &BlockerSenderSignals {
            started_by: Some(StartedBy::Operator),
            owner_desk: None,
            assignee: Some("eng".to_string()),
        },
    );
    assert_eq!(
        sender, "eng",
        "an operator-started run names no agent, so the assignee answers"
    );
}

#[test]
fn the_owner_desk_lead_answers_when_no_agent_triggered() {
    let record = record(MANIFEST);
    let sender = resolve_sender(
        &record,
        &BlockerSenderSignals {
            started_by: None,
            owner_desk: Some("studio".to_string()),
            assignee: Some("ceo".to_string()),
        },
    );
    assert_eq!(
        sender, "des",
        "the desk's first member leads and outranks the assignee"
    );
}

#[test]
fn the_assignee_answers_when_no_run_and_no_desk() {
    let record = record(MANIFEST);
    let sender = resolve_sender(
        &record,
        &BlockerSenderSignals {
            started_by: None,
            owner_desk: None,
            assignee: Some("eng".to_string()),
        },
    );
    assert_eq!(sender, "eng");
}

#[test]
fn the_orchestrator_answers_an_unattributed_stop() {
    let record = record(MANIFEST);
    let sender = resolve_sender(&record, &BlockerSenderSignals::default());
    assert_eq!(
        sender, "ceo",
        "with nothing named, the first (orchestrator) agent answers"
    );
}

#[test]
fn the_host_answers_when_the_company_has_no_roster() {
    let record = record("[company]\nname = \"Empty\"\n");
    let sender = resolve_sender(
        &record,
        &BlockerSenderSignals {
            started_by: Some(StartedBy::Agent("nobody".to_string())),
            owner_desk: Some("nowhere".to_string()),
            assignee: Some("nobody".to_string()),
        },
    );
    assert_eq!(
        sender, HOST_SENDER,
        "no roster names anyone; the host takes it"
    );
}

#[test]
fn the_dm_thread_is_the_console_channel_form() {
    assert_eq!(dm_thread("eng"), "dm:eng");
    assert_eq!(dm_thread(HOST_SENDER), "dm:workflow");
}
