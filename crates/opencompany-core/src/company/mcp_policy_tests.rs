use super::*;

#[test]
fn an_empty_policy_serializes_to_an_empty_object() {
    let policy = ToolPolicy::default();
    assert!(policy.is_empty());
    assert_eq!(serde_json::to_string(&policy).unwrap(), "{}");
}

/// Both fields must survive a round trip independently: a row the operator only
/// set the mode on keeps inheriting its tier, and storing `null` for the other
/// field would turn that inheritance into a pinned `None`.
#[test]
fn a_half_set_policy_round_trips_without_pinning_the_other_field() {
    let policy = ToolPolicy {
        tier: None,
        mode: Some(ApprovalMode::AlwaysAllow),
    };
    let raw = serde_json::to_string(&policy).unwrap();
    assert_eq!(raw, r#"{"mode":"always_allow"}"#);
    assert_eq!(serde_json::from_str::<ToolPolicy>(&raw).unwrap(), policy);
}

#[test]
fn tier_and_mode_use_snake_case_on_the_wire() {
    assert_eq!(
        serde_json::to_string(&ToolTier::WriteDelete).unwrap(),
        r#""write_delete""#
    );
    assert_eq!(
        serde_json::to_string(&ApprovalMode::NeedsApproval).unwrap(),
        r#""needs_approval""#
    );
}

/// The tier is a map key in `tier_defaults`, which only works while it
/// serializes as a plain string.
#[test]
fn tier_defaults_round_trip_with_the_tier_as_a_map_key() {
    let mut policies = McpToolPolicies::default();
    policies
        .tier_defaults
        .insert(ToolTier::ReadOnly, ApprovalMode::AlwaysAllow);
    let raw = serde_json::to_string(&policies).unwrap();
    assert!(raw.contains(r#""read_only":"always_allow""#), "{raw}");
    assert_eq!(
        serde_json::from_str::<McpToolPolicies>(&raw).unwrap(),
        policies
    );
}

#[test]
fn an_absent_document_parses_as_the_default() {
    let parsed: McpToolPolicies = serde_json::from_str("{}").unwrap();
    assert!(parsed.tier_defaults.is_empty());
    assert!(parsed.overrides.is_empty());
}

#[test]
fn pruning_drops_entries_that_decide_nothing() {
    let mut policies = McpToolPolicies::default();
    policies
        .overrides
        .insert("search_pages".into(), ToolPolicy::default());
    policies.overrides.insert(
        "move_page".into(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::Blocked),
        },
    );
    policies.prune();
    assert_eq!(policies.overrides.len(), 1);
    assert!(policies.overrides.contains_key("move_page"));
}

/// Read-only is the one tier that runs without a human; the other two park.
#[test]
fn hardcoded_tier_defaults_park_everything_but_read_only() {
    assert_eq!(
        default_mode_for(ToolTier::ReadOnly),
        ApprovalMode::AlwaysAllow
    );
    assert_eq!(
        default_mode_for(ToolTier::Interactive),
        ApprovalMode::NeedsApproval
    );
    assert_eq!(
        default_mode_for(ToolTier::WriteDelete),
        ApprovalMode::NeedsApproval
    );
}

#[test]
fn policy_keys_are_namespaced_per_server_kind() {
    assert_eq!(tool_policies_key("notion"), "mcp/notion/tool_policies");
    assert_eq!(
        registry_tool_policies_key("0b8f4b0e-3c2a-4a1d-9e77-6d5a2f1c8e40"),
        "mcp_registry/0b8f4b0e-3c2a-4a1d-9e77-6d5a2f1c8e40/tool_policies"
    );
}

// ---- suggested tiers --------------------------------------------------

#[test]
fn read_verbs_suggest_read_only() {
    for name in [
        "get_page",
        "list_databases",
        "read_file",
        "search_pages",
        "searchPages",
        "search-pages",
        "search",
    ] {
        assert_eq!(suggest_tool_tier(name, None), ToolTier::ReadOnly, "{name}");
    }
}

#[test]
fn destructive_verbs_suggest_write_delete() {
    for name in ["delete_page", "remove_user", "drop_table", "purgeCache"] {
        assert_eq!(
            suggest_tool_tier(name, None),
            ToolTier::WriteDelete,
            "{name}"
        );
    }
}

#[test]
fn anything_else_falls_to_the_conservative_middle() {
    for name in ["move_page", "create_issue", "append_block", "run", ""] {
        assert_eq!(
            suggest_tool_tier(name, None),
            ToolTier::Interactive,
            "{name}"
        );
    }
}

/// The verb must be the whole leading segment. `getaway_book` is not a getter,
/// and a prefix match would make it one.
#[test]
fn a_verb_must_be_the_whole_leading_segment() {
    assert_eq!(
        suggest_tool_tier("getaway_book", None),
        ToolTier::Interactive
    );
    assert_eq!(
        suggest_tool_tier("listen_events", None),
        ToolTier::Interactive
    );
    assert_eq!(
        suggest_tool_tier("dropbox_upload", None),
        ToolTier::Interactive
    );
}

/// A description may escalate an unclassified tool, because `WriteDelete` and
/// `Interactive` carry the same nominal default — escalating loosens nothing.
#[test]
fn a_description_can_escalate_an_unclassified_tool() {
    assert_eq!(
        suggest_tool_tier("archive_page", Some("Delete the page permanently.")),
        ToolTier::WriteDelete
    );
}

/// The direction that would loosen a boundary is closed: prose written by
/// whoever runs the remote server can never pull a tool down to read-only.
#[test]
fn a_description_can_never_suggest_read_only() {
    assert_eq!(
        suggest_tool_tier(
            "move_page",
            Some("Read-only. Just lists things. get list search")
        ),
        ToolTier::Interactive
    );
    assert_eq!(
        suggest_tool_tier("delete_page", Some("A harmless read-only lookup.")),
        ToolTier::WriteDelete
    );
}

/// A qualified name classifies on its namespace, not the verb behind it — and
/// a namespace is not a verb, so it parks. The bridge names a remote tool
/// unqualified (the server travels as its own argument), so this is the shape
/// that would arrive only from somewhere passing the wrong string.
#[test]
fn a_qualified_name_does_not_classify_on_its_trailing_verb() {
    assert_eq!(
        suggest_tool_tier("notion.search", None),
        ToolTier::Interactive
    );
    assert_eq!(
        suggest_tool_tier("notion.delete", None),
        ToolTier::Interactive
    );
}

/// `query` and `fetch` read like getters but are deliberately not in the read
/// set — a suggested read-only is the only tier whose nominal default skips a
/// human.
#[test]
fn ambiguous_read_shaped_verbs_stay_out_of_the_read_tier() {
    assert_eq!(
        suggest_tool_tier("query_database", None),
        ToolTier::Interactive
    );
    assert_eq!(suggest_tool_tier("fetch_url", None), ToolTier::Interactive);
}

// ---- the resolution ladder --------------------------------------------

fn with_override(tool: &str, policy: ToolPolicy) -> McpToolPolicies {
    let mut policies = McpToolPolicies::default();
    policies.overrides.insert(tool.to_string(), policy);
    policies
}

/// Nothing stored, nothing suggested: the conservative middle, which parks.
#[test]
fn an_unknown_tool_parks_under_the_middle_tier() {
    let resolved = resolve_policy(&McpToolPolicies::default(), "move_page", None);
    assert_eq!(resolved.tier, ToolTier::Interactive);
    assert_eq!(resolved.mode, ApprovalMode::NeedsApproval);
    assert!(!resolved.is_override);
}

/// The deviation that matters: a merely *suggested* read-only still parks. A
/// name heuristic may group a row, never skip the gate for it.
#[test]
fn a_suggested_read_only_tool_still_parks() {
    let resolved = resolve_policy(
        &McpToolPolicies::default(),
        "search_pages",
        Some(ToolTier::ReadOnly),
    );
    assert_eq!(resolved.tier, ToolTier::ReadOnly);
    assert_eq!(resolved.mode, ApprovalMode::NeedsApproval);
    assert!(!resolved.is_override);
}

/// …whereas an operator who reclassified the row to read-only did make a risk
/// statement, so the tier's nominal default is theirs to inherit.
#[test]
fn an_operator_confirmed_read_only_tool_inherits_allow() {
    let policies = with_override(
        "search_pages",
        ToolPolicy {
            tier: Some(ToolTier::ReadOnly),
            mode: None,
        },
    );
    let resolved = resolve_policy(&policies, "search_pages", None);
    assert_eq!(resolved.mode, ApprovalMode::AlwaysAllow);
    assert!(resolved.is_override);
}

/// Bulk allow stays one deliberate action away: a stored tier default reaches
/// every tool the suggestion groups there.
#[test]
fn a_stored_tier_default_reaches_suggested_rows() {
    let mut policies = McpToolPolicies::default();
    policies
        .tier_defaults
        .insert(ToolTier::ReadOnly, ApprovalMode::AlwaysAllow);
    let resolved = resolve_policy(&policies, "search_pages", Some(ToolTier::ReadOnly));
    assert_eq!(resolved.mode, ApprovalMode::AlwaysAllow);
    // Inherited from the tier, not decided on this row.
    assert!(!resolved.is_override);
}

#[test]
fn a_tool_override_wins_over_the_tier_default() {
    let mut policies = with_override(
        "search_pages",
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::Blocked),
        },
    );
    policies
        .tier_defaults
        .insert(ToolTier::ReadOnly, ApprovalMode::AlwaysAllow);
    let resolved = resolve_policy(&policies, "search_pages", Some(ToolTier::ReadOnly));
    assert_eq!(resolved.mode, ApprovalMode::Blocked);
    assert_eq!(resolved.tier, ToolTier::ReadOnly);
    assert!(resolved.is_override);
}

/// A mode-only override must not disturb the tier, or every press of a console's
/// three-way control would silently revert a reclassification.
#[test]
fn a_mode_only_override_leaves_the_suggested_tier_in_place() {
    let policies = with_override(
        "move_page",
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::AlwaysAllow),
        },
    );
    let resolved = resolve_policy(&policies, "move_page", Some(ToolTier::WriteDelete));
    assert_eq!(resolved.tier, ToolTier::WriteDelete);
    assert_eq!(resolved.mode, ApprovalMode::AlwaysAllow);
}

/// An operator's reclassification beats the heuristic, including when the
/// heuristic was the more permissive of the two.
#[test]
fn a_reclassification_beats_the_suggestion() {
    let policies = with_override(
        "search_pages",
        ToolPolicy {
            tier: Some(ToolTier::WriteDelete),
            mode: None,
        },
    );
    let resolved = resolve_policy(&policies, "search_pages", Some(ToolTier::ReadOnly));
    assert_eq!(resolved.tier, ToolTier::WriteDelete);
    assert_eq!(resolved.mode, ApprovalMode::NeedsApproval);
}

/// An entry that decides nothing resolves exactly as no entry does, so a reset
/// is indistinguishable from never having touched the row.
#[test]
fn an_empty_entry_resolves_as_no_entry() {
    let policies = with_override("search_pages", ToolPolicy::default());
    let resolved = resolve_policy(&policies, "search_pages", Some(ToolTier::ReadOnly));
    let untouched = resolve_policy(
        &McpToolPolicies::default(),
        "search_pages",
        Some(ToolTier::ReadOnly),
    );
    assert_eq!(resolved, untouched);
    assert!(!resolved.is_override);
}

// ---- migration and storage --------------------------------------------

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

#[derive(Default)]
struct MemSecrets {
    map: Mutex<HashMap<String, String>>,
    fail_reads: bool,
}

#[async_trait]
impl SecretStore for MemSecrets {
    async fn get(&self, _c: &CompanyId, key: &str) -> Result<Option<SecretValue>> {
        if self.fail_reads {
            return Err(OpenCompanyError::Store("store is down".into()));
        }
        Ok(self
            .map
            .lock()
            .unwrap()
            .get(key)
            .map(|v| SecretValue(v.clone())))
    }
    async fn set(&self, _c: &CompanyId, key: &str, value: SecretValue) -> Result<()> {
        self.map.lock().unwrap().insert(key.to_string(), value.0);
        Ok(())
    }
}

fn company() -> CompanyId {
    CompanyId::new("acme")
}

/// The restatement writes both fields concretely. A future tidy that drops
/// either one would change what the gate enforces, so it fails here first.
#[test]
fn migration_states_both_the_tier_and_the_mode() {
    let policies = migrate_read_only_tools(&["search_pages".into(), "  ".into()]);
    assert_eq!(policies.overrides.len(), 1);
    let entry = policies.overrides.get("search_pages").unwrap();
    assert_eq!(entry.tier, Some(ToolTier::ReadOnly));
    assert_eq!(entry.mode, Some(ApprovalMode::AlwaysAllow));
    assert!(policies.tier_defaults.is_empty());
}

/// The whole point of the migration: a legacy read-only tool keeps running
/// without a human, and everything else keeps parking.
#[test]
fn the_legacy_baseline_reproduces_todays_behaviour() {
    let policies = effective_policies(&["search_pages".into()], StoredPolicies::Absent);
    assert_eq!(
        resolve_policy(&policies, "search_pages", None).mode,
        ApprovalMode::AlwaysAllow
    );
    assert_eq!(
        resolve_policy(&policies, "move_page", None).mode,
        ApprovalMode::NeedsApproval
    );
}

/// A stored entry naming only a mode keeps the baseline's tier, so editing one
/// row cannot quietly regroup it.
#[test]
fn a_stored_entry_layers_field_by_field_over_the_baseline() {
    let mut stored = McpToolPolicies::default();
    stored.overrides.insert(
        "search_pages".into(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::NeedsApproval),
        },
    );
    let policies = effective_policies(&["search_pages".into()], StoredPolicies::Stored(stored));
    let resolved = resolve_policy(&policies, "search_pages", None);
    assert_eq!(resolved.mode, ApprovalMode::NeedsApproval);
    assert_eq!(resolved.tier, ToolTier::ReadOnly);
}

/// Editing one row must not retire the declaration's remaining rows.
#[test]
fn editing_one_row_leaves_the_other_declared_rows_alone() {
    let mut stored = McpToolPolicies::default();
    stored.overrides.insert(
        "move_page".into(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::Blocked),
        },
    );
    let policies = effective_policies(
        &["search_pages".into(), "get_page".into()],
        StoredPolicies::Stored(stored),
    );
    assert_eq!(
        resolve_policy(&policies, "search_pages", None).mode,
        ApprovalMode::AlwaysAllow
    );
    assert_eq!(
        resolve_policy(&policies, "get_page", None).mode,
        ApprovalMode::AlwaysAllow
    );
    assert_eq!(
        resolve_policy(&policies, "move_page", None).mode,
        ApprovalMode::Blocked
    );
}

#[tokio::test]
async fn an_absent_document_reads_as_the_empty_one() {
    let secrets = MemSecrets::default();
    let key = tool_policies_key("notion");
    assert_eq!(
        load_tool_policies(&company(), &secrets, &key).await,
        StoredPolicies::Absent
    );
    assert_eq!(
        load_tool_policies_strict(&company(), &secrets, &key)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_document_round_trips_through_the_store() {
    let secrets = MemSecrets::default();
    let key = tool_policies_key("notion");
    let mut policies = McpToolPolicies::default();
    policies
        .tier_defaults
        .insert(ToolTier::ReadOnly, ApprovalMode::AlwaysAllow);
    policies.overrides.insert(
        "move_page".into(),
        ToolPolicy {
            tier: Some(ToolTier::WriteDelete),
            mode: Some(ApprovalMode::Blocked),
        },
    );
    save_tool_policies(&company(), &secrets, &key, &policies)
        .await
        .unwrap();
    assert_eq!(
        load_tool_policies(&company(), &secrets, &key).await,
        StoredPolicies::Stored(policies)
    );
}

/// An entry deciding nothing is never persisted, so a reset leaves no residue
/// for a later reader to treat as an override.
#[tokio::test]
async fn saving_prunes_entries_that_decide_nothing() {
    let secrets = MemSecrets::default();
    let key = tool_policies_key("notion");
    let mut policies = McpToolPolicies::default();
    policies
        .overrides
        .insert("search_pages".into(), ToolPolicy::default());
    save_tool_policies(&company(), &secrets, &key, &policies)
        .await
        .unwrap();
    let StoredPolicies::Stored(read_back) = load_tool_policies(&company(), &secrets, &key).await
    else {
        panic!("a saved document reads back as stored");
    };
    assert!(read_back.overrides.is_empty());
    assert!(!resolve_policy(&read_back, "search_pages", None).is_override);
}

/// The two faces disagree on purpose: the gate degrades to all-park, the
/// operator-facing read refuses.
#[tokio::test]
async fn an_unreadable_document_degrades_for_the_gate_and_surfaces_for_an_operator() {
    let secrets = MemSecrets::default();
    let key = tool_policies_key("notion");
    secrets
        .set(&company(), &key, SecretValue("{not json".into()))
        .await
        .unwrap();

    assert_eq!(
        load_tool_policies(&company(), &secrets, &key).await,
        StoredPolicies::Unreadable
    );
    // An unreadable document drops the declaration too: it may have carried a
    // refusal, and falling back would restore an allow the operator removed.
    let degraded = effective_policies(&["search_pages".into()], StoredPolicies::Unreadable);
    assert_eq!(
        resolve_policy(&degraded, "search_pages", None).mode,
        ApprovalMode::NeedsApproval
    );

    assert!(
        load_tool_policies_strict(&company(), &secrets, &key)
            .await
            .is_err()
    );
}

/// A store that cannot be read at all degrades the same way. It must never
/// travel up as an error: the caller above treats one as "this company gets no
/// MCP servers", which would strip every server over one unreadable key.
#[tokio::test]
async fn a_failing_store_degrades_rather_than_propagating() {
    let secrets = MemSecrets {
        fail_reads: true,
        ..Default::default()
    };
    let key = tool_policies_key("notion");
    assert_eq!(
        load_tool_policies(&company(), &secrets, &key).await,
        StoredPolicies::Unreadable
    );
    assert!(
        load_tool_policies_strict(&company(), &secrets, &key)
            .await
            .is_err()
    );
}

/// Clearing writes the empty document rather than leaving the unparseable one,
/// which is the only repair available where the store has no delete.
#[tokio::test]
async fn clearing_replaces_an_unparseable_document() {
    let secrets = MemSecrets::default();
    let key = tool_policies_key("notion");
    secrets
        .set(&company(), &key, SecretValue("{not json".into()))
        .await
        .unwrap();
    clear_tool_policies(&company(), &secrets, &key)
        .await
        .unwrap();
    assert_eq!(
        load_tool_policies_strict(&company(), &secrets, &key)
            .await
            .unwrap(),
        Some(McpToolPolicies::default())
    );
}

// ---- the discovered tool inventory ------------------------------------

#[test]
fn discovery_suggests_a_tier_per_tool_and_drops_blanks() {
    let inventory = inventory_from_discovery(
        [
            ("search_pages", None),
            ("delete_page", None),
            ("move_page", Some("Move a page.")),
            ("  ", None),
        ],
        1_700_000_000_000,
    );
    assert_eq!(
        inventory.suggested("search_pages"),
        Some(ToolTier::ReadOnly)
    );
    assert_eq!(
        inventory.suggested("delete_page"),
        Some(ToolTier::WriteDelete)
    );
    assert_eq!(
        inventory.suggested("move_page"),
        Some(ToolTier::Interactive)
    );
    assert_eq!(inventory.tools.len(), 3);
    assert_eq!(inventory.discovered_at_millis, 1_700_000_000_000);
}

/// A tool nobody discovered has no suggestion, which resolves under the
/// conservative middle tier.
#[test]
fn an_undiscovered_tool_has_no_suggestion() {
    let inventory = inventory_from_discovery([("search_pages", None)], 0);
    assert_eq!(inventory.suggested("ghost"), None);
    assert_eq!(
        resolve_policy(
            &McpToolPolicies::default(),
            "ghost",
            inventory.suggested("ghost")
        )
        .tier,
        ToolTier::Interactive
    );
}

/// The map is ordered, so re-writing an unchanged inventory produces identical
/// bytes rather than a spurious diff.
#[test]
fn an_unchanged_inventory_serializes_identically() {
    let first = inventory_from_discovery([("b_tool", None), ("a_tool", None)], 7);
    let second = inventory_from_discovery([("a_tool", None), ("b_tool", None)], 7);
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );
}

#[tokio::test]
async fn an_inventory_round_trips_through_the_store() {
    let secrets = MemSecrets::default();
    let key = tool_inventory_key("notion");
    let inventory = inventory_from_discovery([("search_pages", None)], 42);
    save_tool_inventory(&company(), &secrets, &key, &inventory)
        .await
        .unwrap();
    assert_eq!(
        load_tool_inventory(&company(), &secrets, &key).await,
        inventory
    );
}

/// An unreadable inventory suggests nothing rather than failing the read, so a
/// damaged document parks tools instead of hiding them.
#[tokio::test]
async fn an_unreadable_inventory_suggests_nothing() {
    let secrets = MemSecrets::default();
    let key = tool_inventory_key("notion");
    secrets
        .set(&company(), &key, SecretValue("{not json".into()))
        .await
        .unwrap();
    assert_eq!(
        load_tool_inventory(&company(), &secrets, &key).await,
        McpToolInventory::default()
    );

    let failing = MemSecrets {
        fail_reads: true,
        ..Default::default()
    };
    assert_eq!(
        load_tool_inventory(&company(), &failing, &key).await,
        McpToolInventory::default()
    );
}

#[test]
fn inventory_keys_are_namespaced_per_server_kind() {
    assert_eq!(tool_inventory_key("notion"), "mcp/notion/tool_inventory");
    assert_eq!(
        registry_tool_inventory_key("0b8f4b0e-3c2a-4a1d-9e77-6d5a2f1c8e40"),
        "mcp_registry/0b8f4b0e-3c2a-4a1d-9e77-6d5a2f1c8e40/tool_inventory"
    );
}

/// The wire string and the serde representation must not drift: the console
/// keys its tier sections off one and reads rows through the other.
#[test]
fn the_tier_wire_string_matches_its_serde_form() {
    for tier in ToolTier::ALL {
        let serialized = serde_json::to_string(&tier).unwrap();
        assert_eq!(serialized, format!("\"{}\"", tier.as_str()), "{tier:?}");
    }
}

#[test]
fn a_stored_bulk_default_reaches_a_merely_suggested_tier() {
    let mut policies = McpToolPolicies::default();
    policies
        .tier_defaults
        .insert(ToolTier::ReadOnly, ApprovalMode::AlwaysAllow);

    let resolved = resolve_policy(&policies, "read_wiki_contents", Some(ToolTier::ReadOnly));
    assert!(policies.overrides.is_empty());
    assert_eq!(
        resolved.mode,
        ApprovalMode::AlwaysAllow,
        "a stored bulk default is the operator's deliberate allow, and it applies \
         to the tools the suggestion grouped under that tier"
    );
    assert!(
        !resolved.is_override,
        "nothing was decided about this tool itself"
    );
}

#[test]
fn without_a_bulk_default_a_suggested_tier_still_asks() {
    let policies = McpToolPolicies::default();
    assert_eq!(
        resolve_policy(&policies, "read_wiki_contents", Some(ToolTier::ReadOnly)).mode,
        ApprovalMode::NeedsApproval,
        "a name heuristic alone must never skip the approval gate"
    );
}

#[test]
fn a_tool_that_only_reads_is_still_suggested_read_only() {
    for name in [
        "get_page",
        "list_items",
        "read_wiki_contents",
        "search_pages",
    ] {
        assert_eq!(suggest_tool_tier(name, None), ToolTier::ReadOnly, "{name}");
    }
}

#[test]
fn an_unrecognised_verb_stays_in_the_conservative_middle() {
    for name in ["ask_wiki_question", "fleet_status", "catalogue"] {
        assert_eq!(
            suggest_tool_tier(name, None),
            ToolTier::Interactive,
            "{name}"
        );
    }
}
