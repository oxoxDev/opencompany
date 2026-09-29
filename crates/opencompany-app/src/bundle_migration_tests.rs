use super::*;

use std::collections::HashMap;

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let map: HashMap<String, OsString> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
        .collect();
    move |name| map.get(name).cloned()
}

#[test]
fn the_new_identifier_is_the_one_tauri_conf_ships() {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
    assert_eq!(config["identifier"].as_str().unwrap(), IDENTIFIER);
    assert_ne!(LEGACY_IDENTIFIER, IDENTIFIER);
}

#[test]
fn the_keychain_keeps_the_legacy_identifier() {
    // The keychain is deliberately not migrated; see keychain::SERVICE.
    let source = include_str!("keychain.rs");
    assert!(source.contains(&format!("const SERVICE: &str = \"{LEGACY_IDENTIFIER}\";")));
}

#[test]
fn macos_roots_cover_app_support_and_the_webview_stores() {
    let roots = identifier_roots("macos", &env_of(&[("HOME", "/Users/op")]));
    assert_eq!(
        roots,
        vec![
            PathBuf::from("/Users/op/Library/Application Support"),
            PathBuf::from("/Users/op/Library/WebKit"),
            PathBuf::from("/Users/op/Library/HTTPStorages"),
        ]
    );
    assert!(identifier_roots("macos", &env_of(&[])).is_empty());
}

#[test]
fn linux_roots_prefer_xdg_and_fall_back_to_home() {
    let roots = identifier_roots("linux", &env_of(&[("HOME", "/home/op")]));
    assert_eq!(
        roots,
        vec![
            PathBuf::from("/home/op/.local/share"),
            PathBuf::from("/home/op/.config")
        ]
    );
    let roots = identifier_roots(
        "linux",
        &env_of(&[
            ("HOME", "/home/op"),
            ("XDG_DATA_HOME", "/xdg/data"),
            ("XDG_CONFIG_HOME", ""),
        ]),
    );
    assert_eq!(
        roots,
        vec![
            PathBuf::from("/xdg/data"),
            PathBuf::from("/home/op/.config")
        ]
    );
}

#[test]
fn windows_roots_are_roaming_and_local_app_data() {
    let roots = identifier_roots(
        "windows",
        &env_of(&[
            ("APPDATA", r"C:\a\Roaming"),
            ("LOCALAPPDATA", r"C:\a\Local"),
        ]),
    );
    assert_eq!(
        roots,
        vec![PathBuf::from(r"C:\a\Roaming"), PathBuf::from(r"C:\a\Local")]
    );
}

#[test]
fn copies_the_legacy_tree_when_the_new_one_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join(LEGACY_IDENTIFIER);
    let new = root.path().join(IDENTIFIER);
    std::fs::create_dir_all(old.join("WebsiteData/LocalStorage")).unwrap();
    std::fs::write(
        old.join("WebsiteData/LocalStorage/localstorage.sqlite3"),
        b"profiles",
    )
    .unwrap();
    std::fs::write(old.join("salt"), b"s").unwrap();

    assert_eq!(migrate_dir(&old, &new).unwrap(), Outcome::Copied);

    assert_eq!(
        std::fs::read(new.join("WebsiteData/LocalStorage/localstorage.sqlite3")).unwrap(),
        b"profiles"
    );
    assert_eq!(std::fs::read(new.join("salt")).unwrap(), b"s");
    // Copied, not moved: an older build still installed keeps its state.
    assert!(old.join("salt").is_file());
    assert!(!root.path().join(format!("{IDENTIFIER}.migrating")).exists());

    // One-shot: the second launch sees the new directory and does nothing.
    assert_eq!(migrate_dir(&old, &new).unwrap(), Outcome::AlreadyPresent);
}

#[test]
fn never_overwrites_state_the_new_identifier_already_has() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join(LEGACY_IDENTIFIER);
    let new = root.path().join(IDENTIFIER);
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("f"), b"old").unwrap();
    std::fs::create_dir_all(&new).unwrap();
    std::fs::write(new.join("f"), b"new").unwrap();

    assert_eq!(migrate_dir(&old, &new).unwrap(), Outcome::AlreadyPresent);
    assert_eq!(std::fs::read(new.join("f")).unwrap(), b"new");
}

#[test]
fn nothing_to_do_without_legacy_state() {
    let root = tempfile::tempdir().unwrap();
    let new = root.path().join(IDENTIFIER);
    assert_eq!(
        migrate_dir(&root.path().join(LEGACY_IDENTIFIER), &new).unwrap(),
        Outcome::NothingToMigrate
    );
    assert!(!new.exists());
}

#[test]
fn an_interrupted_copy_is_redone_not_trusted() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join(LEGACY_IDENTIFIER);
    let new = root.path().join(IDENTIFIER);
    let staging = root.path().join(format!("{IDENTIFIER}.migrating"));
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("complete"), b"yes").unwrap();
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("partial"), b"junk").unwrap();

    assert_eq!(migrate_dir(&old, &new).unwrap(), Outcome::Copied);
    assert!(new.join("complete").is_file());
    assert!(!new.join("partial").exists());
    assert!(!staging.exists());
}
