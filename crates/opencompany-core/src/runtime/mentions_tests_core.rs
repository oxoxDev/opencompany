pub(super) use super::*;
pub(super) use crate::ports::types::{AgentOverride, CompanyId, CompanyRecord, OverlayAgent};
pub(super) use crate::ports::users::{UserRole, UserStatus};

pub(super) const MANIFEST: &str = r#"
[company]
name = "Acme"

[[agent]]
id = "engineer"
role = "Backend Engineer"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[group_chat]]
id = "engineering"
name = "Engineering"
members = ["engineer", "ceo"]
"#;

pub(super) fn record(toml_src: &str) -> CompanyRecord {
    CompanyRecord {
        general_channel: Default::default(),
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: CompanyId::new("acme"),
        manifest: toml::from_str(toml_src).expect("parse manifest"),
        ledger: Vec::new(),
        lifecycle: "running".to_string(),
        overlay_agents: Vec::new(),
        overlay_desk_members: Vec::new(),
        overlay_desk_order: Vec::new(),
        overlay_desks: Vec::new(),
        overlay_workflows: Vec::new(),
        overlay_budgets: Vec::new(),
        overlay_policy: None,
        overlay_tool_grants: None,
        overlay_desk_tools: Default::default(),
        disabled_workflows: Vec::new(),
        template_provenance: None,
        setup: None,
        name_confirmed: false,
        activation_completed_at: None,
        created_at_millis: None,
    }
}

pub(super) fn acme() -> CompanyRecord {
    record(MANIFEST)
}

pub(super) fn user(id: &str, email: &str, display: Option<&str>) -> UserRecord {
    UserRecord {
        id: id.to_string(),
        email: email.to_string(),
        display_name: display.map(str::to_string),
        avatar: None,
        role: UserRole::Member,
        status: UserStatus::Active,
        password_hash: None,
        must_change_password: false,
        created_at_millis: 0,
        last_seen_at_millis: None,
        updated_at_millis: 0,
    }
}

pub(super) fn people() -> Vec<UserRecord> {
    vec![
        user("u1", "jane@acme.test", Some("Jane Doe")),
        user("u2", "sam@acme.test", None),
    ]
}

pub(super) fn resolve_text(text: &str) -> Vec<Mention> {
    let record = acme();
    let users = people();
    resolve(text, None, None, &record, &users)
}

pub(super) fn targets(mentions: &[Mention]) -> Vec<&MentionTarget> {
    mentions.iter().map(|m| &m.target).collect()
}

pub(super) fn agent(id: &str) -> MentionTarget {
    MentionTarget::Agent { id: id.to_string() }
}

// -----------------------------------------------------------------------
// Routing
// -----------------------------------------------------------------------

/// Two desks with no overlap, so "not on this desk" is expressible.
pub(super) const TWO_DESKS: &str = r#"
[company]
name = "Acme"

[[agent]]
id = "engineer"
role = "Backend Engineer"

[[agent]]
id = "designer"
role = "Product Designer"

[[group_chat]]
id = "engineering"
name = "Engineering"
members = ["engineer"]

[[group_chat]]
id = "design"
name = "Design"
members = ["designer"]
"#;
