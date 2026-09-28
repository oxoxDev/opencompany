//! Manifest validation of the built-in #general channel.

use super::*;

const ROSTER: &str = "[company]\nname = \"Acme\"\n\n[[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n";

fn parse(text: &str) -> CompanyManifest {
    toml::from_str(text).expect("valid toml")
}

#[test]
fn a_manifest_setting_general_desk_is_refused_with_a_clear_message() {
    let manifest = parse(&ROSTER.replace(
        "name = \"Acme\"\n",
        "name = \"Acme\"\ngeneral_desk = \"front\"\n",
    ));
    let problems = manifest.validate();
    assert!(
        problems
            .iter()
            .any(|p| p.contains("`[company].general_desk` is no longer supported")),
        "{problems:?}"
    );
    assert!(
        manifest
            .reserved_problems()
            .iter()
            .any(|p| p.contains("general_desk")),
        "a stored manifest that already set it still reloads"
    );
}

#[test]
fn a_desk_named_for_general_is_refused() {
    for (id, name) in [
        ("general", "Front"),
        ("main", "Front"),
        ("front", "General"),
    ] {
        let manifest = parse(&format!(
            "{ROSTER}\n[[group_chat]]\nid = \"{id}\"\nname = \"{name}\"\nmembers = [\"ceo\"]\n"
        ));
        let problems = manifest.validate();
        assert!(
            problems.iter().any(|p| p.contains("#general")),
            "{id}/{name}: {problems:?}"
        );
    }
    let ordinary = parse(&format!(
        "{ROSTER}\n[[group_chat]]\nid = \"front\"\nname = \"Front office\"\nmembers = [\"ceo\"]\n"
    ));
    assert!(ordinary.validate().is_empty(), "{:?}", ordinary.validate());
}

#[test]
fn general_desk_is_not_written_back() {
    let manifest = parse(&ROSTER.replace(
        "name = \"Acme\"\n",
        "name = \"Acme\"\ngeneral_desk = \"front\"\n",
    ));
    let json = serde_json::to_value(&manifest).expect("serialize");
    assert!(json["company"].get("general_desk").is_none(), "{json}");
}
