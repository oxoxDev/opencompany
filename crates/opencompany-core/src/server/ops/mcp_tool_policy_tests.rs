use super::*;

use crate::company::mcp_policy::{
    ApprovalMode, McpToolInventory, McpToolPolicies, ToolPolicy, ToolTier, inventory_from_discovery,
};

fn entry(tool: &str, tier: Option<ToolTier>, mode: Option<ApprovalMode>) -> PutToolPolicyEntry {
    PutToolPolicyEntry {
        tool: tool.to_string(),
        tier,
        mode,
    }
}

fn patch(tools: Vec<PutToolPolicyEntry>) -> PutToolPolicy {
    PutToolPolicy {
        tier_defaults: None,
        tools: Some(tools),
    }
}

/// A body naming neither field changes nothing, and saying so is more useful
/// than silently storing the document unchanged.
#[test]
fn a_body_naming_neither_field_is_refused() {
    let err = apply_tool_policy_patch(McpToolPolicies::default(), PutToolPolicy::default())
        .expect_err("refused");
    assert!(err.contains("tierDefaults"), "{err}");
    assert!(err.contains("tools"), "{err}");
}

/// The distinction the contract turns on: an entry naming no field is the
/// reset, and it is valid rather than empty.
#[test]
fn an_entry_naming_no_field_resets_that_row() {
    let mut stored = McpToolPolicies::default();
    stored.overrides.insert(
        "search_pages".into(),
        ToolPolicy {
            tier: Some(ToolTier::WriteDelete),
            mode: Some(ApprovalMode::Blocked),
        },
    );
    let merged =
        apply_tool_policy_patch(stored, patch(vec![entry("search_pages", None, None)])).unwrap();
    assert!(merged.overrides.is_empty());
}

/// The second level of the merge, and the one that matters: setting a mode
/// must leave a tier reclassification intact. Replace-semantics here would make
/// every press of a three-way control silently revert the operator's tier
/// decision.
#[test]
fn setting_a_mode_leaves_the_tier_override_standing() {
    let mut stored = McpToolPolicies::default();
    stored.overrides.insert(
        "move_page".into(),
        ToolPolicy {
            tier: Some(ToolTier::ReadOnly),
            mode: Some(ApprovalMode::Blocked),
        },
    );
    let merged = apply_tool_policy_patch(
        stored,
        patch(vec![entry(
            "move_page",
            None,
            Some(ApprovalMode::AlwaysAllow),
        )]),
    )
    .unwrap();
    let row = merged.overrides.get("move_page").unwrap();
    assert_eq!(row.mode, Some(ApprovalMode::AlwaysAllow));
    assert_eq!(row.tier, Some(ToolTier::ReadOnly), "tier must survive");
}

/// …and the mirror: reclassifying leaves the mode alone.
#[test]
fn setting_a_tier_leaves_the_mode_override_standing() {
    let mut stored = McpToolPolicies::default();
    stored.overrides.insert(
        "move_page".into(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::Blocked),
        },
    );
    let merged = apply_tool_policy_patch(
        stored,
        patch(vec![entry("move_page", Some(ToolTier::WriteDelete), None)]),
    )
    .unwrap();
    let row = merged.overrides.get("move_page").unwrap();
    assert_eq!(row.tier, Some(ToolTier::WriteDelete));
    assert_eq!(row.mode, Some(ApprovalMode::Blocked), "mode must survive");
}

/// A row a patch does not mention is untouched — the merge is per-entry, not a
/// whole-document replace.
#[test]
fn an_unmentioned_row_is_untouched() {
    let mut stored = McpToolPolicies::default();
    stored.overrides.insert(
        "search_pages".into(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::AlwaysAllow),
        },
    );
    let merged = apply_tool_policy_patch(
        stored,
        patch(vec![entry("move_page", None, Some(ApprovalMode::Blocked))]),
    )
    .unwrap();
    assert_eq!(
        merged.overrides.get("search_pages").unwrap().mode,
        Some(ApprovalMode::AlwaysAllow)
    );
}

#[test]
fn a_blank_tool_name_is_refused() {
    let err = apply_tool_policy_patch(
        McpToolPolicies::default(),
        patch(vec![entry("   ", None, Some(ApprovalMode::Blocked))]),
    )
    .expect_err("refused");
    assert!(err.contains("`tool` name"), "{err}");
}

#[test]
fn an_unknown_tier_name_is_refused() {
    let mut defaults = std::collections::HashMap::new();
    defaults.insert("read-only".to_string(), ApprovalMode::AlwaysAllow);
    let err = apply_tool_policy_patch(
        McpToolPolicies::default(),
        PutToolPolicy {
            tier_defaults: Some(defaults),
            tools: None,
        },
    )
    .expect_err("refused");
    assert!(err.contains("read-only"), "{err}");
}

#[test]
fn tier_defaults_are_stored_under_the_wire_spelling() {
    let mut defaults = std::collections::HashMap::new();
    defaults.insert("read_only".to_string(), ApprovalMode::AlwaysAllow);
    let merged = apply_tool_policy_patch(
        McpToolPolicies::default(),
        PutToolPolicy {
            tier_defaults: Some(defaults),
            tools: None,
        },
    )
    .unwrap();
    assert_eq!(
        merged.tier_defaults.get(&ToolTier::ReadOnly),
        Some(&ApprovalMode::AlwaysAllow)
    );
}

// ---- the rendered document -------------------------------------------

/// Every tier appears in `tierDefaults`, resolved. The console therefore ships
/// no copy of the fallbacks and cannot drift from them.
#[test]
fn tier_defaults_are_total_on_the_wire() {
    let dto = tool_policy_dto(
        "notion",
        &McpToolPolicies::default(),
        &McpToolInventory::default(),
    );
    assert_eq!(dto.tier_defaults.len(), ToolTier::ALL.len());
    assert_eq!(
        dto.tier_defaults.get("read_only"),
        Some(&ApprovalMode::AlwaysAllow)
    );
    assert_eq!(
        dto.tier_defaults.get("write_delete"),
        Some(&ApprovalMode::NeedsApproval)
    );
}

/// A reclassified row's suggestion and its effective tier may legitimately
/// disagree, and both are sent so the page can say so instead of looking
/// broken.
#[test]
fn a_reclassified_row_reports_both_tiers() {
    let mut policies = McpToolPolicies::default();
    policies.overrides.insert(
        "search_pages".into(),
        ToolPolicy {
            tier: Some(ToolTier::WriteDelete),
            mode: None,
        },
    );
    let inventory = inventory_from_discovery([("search_pages", None)], 9);
    let dto = tool_policy_dto("notion", &policies, &inventory);
    let row = dto.tools.iter().find(|r| r.tool == "search_pages").unwrap();
    assert_eq!(row.effective_tier, ToolTier::WriteDelete);
    assert_eq!(row.suggested_tier, Some(ToolTier::ReadOnly));
    assert!(row.is_override);
}

/// The row set is the union: a tool only discovery knows about, and a tool only
/// the operator decided about, both appear.
#[test]
fn the_row_set_unions_discovery_and_overrides() {
    let mut policies = McpToolPolicies::default();
    policies.overrides.insert(
        "retired_tool".into(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::Blocked),
        },
    );
    let inventory = inventory_from_discovery([("search_pages", None)], 1);
    let dto = tool_policy_dto("notion", &policies, &inventory);
    let names: Vec<&str> = dto.tools.iter().map(|r| r.tool.as_str()).collect();
    assert!(names.contains(&"search_pages"));
    assert!(names.contains(&"retired_tool"));
}

/// An inherited row is not an override, which is what a console's reset
/// affordance keys off.
#[test]
fn an_inherited_row_is_not_marked_an_override() {
    let inventory = inventory_from_discovery([("search_pages", None)], 1);
    let dto = tool_policy_dto("notion", &McpToolPolicies::default(), &inventory);
    let row = dto.tools.iter().find(|r| r.tool == "search_pages").unwrap();
    assert!(!row.is_override);
    // Suggested read-only still parks: the suggestion groups, never allows.
    assert_eq!(row.mode, ApprovalMode::NeedsApproval);
}
