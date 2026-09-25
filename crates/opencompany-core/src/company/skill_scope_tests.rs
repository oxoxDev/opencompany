//! The inversion's state table, one case per row, plus the collapse it exists
//! to prevent.
//!
//! The whole value of this module is that `state` is carried from what is
//! stored rather than derived from what resolves. A projection that reported
//! only the effective set would pass every `holds` assertion below and get
//! every `state` one wrong for the two rows that resolve identically — which is
//! why the two fields are asserted separately throughout.

use super::*;

/// One agent with the given stored scope.
fn agent(id: &str, requested: Option<&[&str]>) -> AgentSkillScope {
    AgentSkillScope {
        id: id.to_string(),
        requested: requested.map(|slugs| slugs.iter().map(|s| s.to_string()).collect()),
    }
}

/// Row 1 — never edited, so the skill reaches the agent while the company has
/// it on.
#[test]
fn a_null_scope_inherits_and_holds_an_enabled_skill() {
    let rows = agents_for_skill("brand-voice", true, &[agent("jamie", None)]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "jamie");
    assert_eq!(rows[0].state, SkillScopeState::Inherited);
    assert!(rows[0].holds);
}

/// Row 2 — a deliberate empty list excludes, and holds nothing.
#[test]
fn an_empty_scope_excludes_and_holds_nothing() {
    let rows = agents_for_skill("brand-voice", true, &[agent("jamie", Some(&[]))]);
    assert_eq!(rows[0].state, SkillScopeState::Excluded);
    assert!(!rows[0].holds);
}

/// Row 3 — a list naming the slug includes, and holds.
#[test]
fn a_list_naming_the_slug_includes_and_holds() {
    let rows = agents_for_skill(
        "brand-voice",
        true,
        &[agent("jamie", Some(&["invoicing", "brand-voice"]))],
    );
    assert_eq!(rows[0].state, SkillScopeState::Included);
    assert!(rows[0].holds);
}

/// Row 4 — a list that omits the slug excludes, and holds nothing.
#[test]
fn a_list_omitting_the_slug_excludes_and_holds_nothing() {
    let rows = agents_for_skill("brand-voice", true, &[agent("jamie", Some(&["invoicing"]))]);
    assert_eq!(rows[0].state, SkillScopeState::Excluded);
    assert!(!rows[0].holds);
}

/// The collapse the issue forbids, asserted as a distinction rather than as two
/// separate passes.
///
/// On a company where the skill is disabled, an agent that never edited its
/// scope and an agent given an explicit empty one resolve to the same effective
/// set — nothing. They are not the same state: switch the skill back on and the
/// first holds it while the second still does not. A projection keyed on the
/// effective set cannot tell them apart, so the two rows are compared here in
/// one test.
#[test]
fn a_disabled_skill_keeps_inherited_apart_from_an_explicitly_empty_scope() {
    let rows = agents_for_skill(
        "brand-voice",
        false,
        &[agent("inheriting", None), agent("denied", Some(&[]))],
    );

    assert!(
        !rows[0].holds,
        "nothing reaches anybody while the switch is off"
    );
    assert!(!rows[1].holds);
    assert_eq!(
        rows[0].state,
        SkillScopeState::Inherited,
        "an agent that never edited its scope still inherits"
    );
    assert_eq!(rows[1].state, SkillScopeState::Excluded);
    assert_ne!(
        rows[0].state, rows[1].state,
        "both hold nothing, and they are not the same state"
    );
}

/// A scope naming a slug the company has disabled: asked for, and not granted.
///
/// `state` reports what is stored, so this is `included`; `holds` reports what
/// the harness will materialize, so it is false. Reporting either one for both
/// would make a disabled skill look scoped to nobody or make a narrowed agent
/// look like it holds a skill it does not.
#[test]
fn a_disabled_skill_a_list_names_is_included_and_held_by_nobody() {
    let rows = agents_for_skill(
        "brand-voice",
        false,
        &[agent("jamie", Some(&["brand-voice"]))],
    );
    assert_eq!(rows[0].state, SkillScopeState::Included);
    assert!(!rows[0].holds);
}

/// Every roster agent gets a row, in the order they were given, whatever state
/// each is in.
///
/// A sparse answer would leave a client deciding what an absent agent means,
/// and the only reading available — "inherits" — is the one state that must
/// never be inferred.
#[test]
fn every_agent_gets_a_row_in_order() {
    let rows = agents_for_skill(
        "brand-voice",
        true,
        &[
            agent("ceo", None),
            agent("designer", Some(&[])),
            agent("writer", Some(&["brand-voice"])),
            agent("analyst", Some(&["invoicing"])),
        ],
    );
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        vec!["ceo", "designer", "writer", "analyst"]
    );
    assert_eq!(
        rows.iter().map(|row| row.state).collect::<Vec<_>>(),
        vec![
            SkillScopeState::Inherited,
            SkillScopeState::Excluded,
            SkillScopeState::Included,
            SkillScopeState::Excluded,
        ]
    );
    assert_eq!(
        rows.iter().map(|row| row.holds).collect::<Vec<_>>(),
        vec![true, false, true, false]
    );
}

/// An empty roster answers with no rows rather than with anything invented.
#[test]
fn an_empty_roster_answers_with_no_rows() {
    assert!(agents_for_skill("brand-voice", true, &[]).is_empty());
}

/// Matching is exact, the same rule `agent_effective_skills` applies.
///
/// A prefix or wildcard match would reach a skill installed after the scope was
/// written, which is the widening the flat-slug rule exists to rule out.
#[test]
fn a_scope_matches_a_slug_exactly_and_never_by_prefix() {
    for near in ["brand", "brand-voice-2", "brand-*", "*", "Brand-Voice"] {
        let rows = agents_for_skill("brand-voice", true, &[agent("jamie", Some(&[near]))]);
        assert_eq!(
            rows[0].state,
            SkillScopeState::Excluded,
            "`{near}` is not `brand-voice`"
        );
        assert!(!rows[0].holds, "`{near}` confers nothing: {rows:?}");
    }
}

/// The wire names the three states in the vocabulary the console reads.
///
/// The console keys its three renderings on these strings, so a rename here is
/// a silent break there — the serialization is part of the contract rather than
/// a detail of the enum.
#[test]
fn the_three_states_serialize_as_the_console_names_them() {
    let rows = agents_for_skill(
        "brand-voice",
        true,
        &[
            agent("ceo", None),
            agent("writer", Some(&["brand-voice"])),
            agent("designer", Some(&[])),
        ],
    );
    let wire = serde_json::to_value(&rows).expect("the projection serializes");
    assert_eq!(wire[0]["state"], "inherited");
    assert_eq!(wire[1]["state"], "included");
    assert_eq!(wire[2]["state"], "excluded");
    assert_eq!(wire[0]["holds"], true);
    assert_eq!(wire[0]["id"], "ceo");
}
