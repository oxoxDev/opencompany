use super::tests_core::*;

fn desk(id: &str, tools: &[&str]) -> GroupChat {
    GroupChat {
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        members: Vec::new(),
        tools: tools.iter().map(|t| t.to_string()).collect(),
        hive: crate::hive::routing::RoutingConfig::default(),
    }
}

fn held(entries: &[(&str, &[&str])]) -> std::collections::BTreeMap<String, Vec<String>> {
    entries
        .iter()
        .map(|(id, tools)| {
            (
                id.to_string(),
                tools.iter().map(|t| t.to_string()).collect(),
            )
        })
        .collect()
}

/// A routine redeploy that changed nothing keeps the operator's console
/// ceilings — clearing on every rebuild would silently revert a console
/// action with nothing to show it had moved.
#[test]
fn an_unchanged_seed_carries_the_override() {
    let seed = [desk("finance", &["docs.*"])];
    let carried = carry_desk_tool_overrides(&seed, &seed, &held(&[("finance", &["web"])]));
    assert_eq!(
        carried.get("finance").map(Vec::as_slice),
        Some(&["web".to_string()][..])
    );
}

/// The security property: version control narrowing a desk must not be
/// silently overridden by a wider console value set before the edit.
#[test]
fn a_changed_seed_clears_that_desks_override() {
    let carried = carry_desk_tool_overrides(
        &[desk("finance", &["*"])],
        &[desk("finance", &["docs.*"])],
        &held(&[("finance", &["web"])]),
    );
    assert!(carried.is_empty(), "{carried:?}");
}

/// Per desk, not whole-block: editing one department says nothing about
/// another, and clearing both would revert an action nobody's edit was
/// about.
#[test]
fn editing_one_desk_leaves_another_desks_override_alone() {
    let carried = carry_desk_tool_overrides(
        &[desk("finance", &["*"]), desk("creative", &["docs.*"])],
        &[desk("finance", &["docs.*"]), desk("creative", &["docs.*"])],
        &held(&[("finance", &["web"]), ("creative", &["media"])]),
    );
    assert!(!carried.contains_key("finance"), "{carried:?}");
    assert_eq!(
        carried.get("creative").map(Vec::as_slice),
        Some(&["media".to_string()][..])
    );
}

/// An operator-created desk has no seed value that could have changed,
/// so its ceiling always survives a rebuild.
#[test]
fn an_override_for_a_desk_the_seed_does_not_declare_is_carried() {
    let carried = carry_desk_tool_overrides(
        &[desk("finance", &["*"])],
        &[desk("finance", &["*"])],
        &held(&[("adhoc", &["docs.*"])]),
    );
    assert!(carried.contains_key("adhoc"), "{carried:?}");
}

/// A desk deleted from the seed *has* changed — from declaring a ceiling
/// to declaring nothing — so its stale override is dropped rather than
/// outliving the desk in version control.
#[test]
fn deleting_a_desk_from_the_seed_clears_its_override() {
    let carried = carry_desk_tool_overrides(
        &[desk("finance", &["docs.*"])],
        &[],
        &held(&[("finance", &["web"])]),
    );
    assert!(carried.is_empty(), "{carried:?}");
}
