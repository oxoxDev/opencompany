//! What an agent may learn about a directory install, and what it may not.

use super::*;

use openhuman_core::mcp::registry::types::CommandKind;

const INSTALL_A: &str = "0b8f4b0e-3c2a-4a1d-9e77-6d5a2f1c8e40";
const INSTALL_B: &str = "7f1c9d22-55ae-4f3b-8c10-2b9e4d6a3f51";

/// The credential shapes an install row can carry. Each is a real one: a
/// query-parameter token in an HTTP-remote URL, a `--token` flag in a stdio
/// install's arguments, and a key inside the opaque config blob.
const URL_SECRET: &str = "sk-url-9c1f2b7a";
const ARG_SECRET: &str = "sk-arg-4d8e0a63";
const CONFIG_SECRET: &str = "sk-config-11b5c7fe";

fn grants(g: &[&str]) -> Vec<String> {
    g.iter().map(|s| s.to_string()).collect()
}

fn remote(server_id: &str) -> InstalledServer {
    InstalledServer {
        server_id: server_id.to_string(),
        qualified_name: "@acme/notes".to_string(),
        display_name: "Acme Notes".to_string(),
        description: Some("Notes for the team".to_string()),
        icon_url: None,
        command_kind: CommandKind::Node,
        command: String::new(),
        args: Vec::new(),
        env_keys: vec!["ACME_TOKEN".to_string()],
        config: Some(serde_json::json!({ "apiKey": CONFIG_SECRET })),
        installed_at: 1,
        last_connected_at: Some(2),
        transport: Transport::HttpRemote {
            url: format!("https://mcp.acme.example/mcp?token={URL_SECRET}"),
        },
        enabled: true,
    }
}

fn stdio(server_id: &str) -> InstalledServer {
    InstalledServer {
        command: "npx".to_string(),
        args: vec![
            "@acme/notes-server".to_string(),
            format!("--token={ARG_SECRET}"),
        ],
        transport: Transport::Stdio,
        ..remote(server_id)
    }
}

fn rendered(installs: &[InstalledServer], grant_list: &[&str]) -> String {
    let rows = installed_rows(installs, &grants(grant_list));
    serde_json::to_string(&serde_json::json!({ "installed": rows })).unwrap()
}

#[test]
fn no_dial_string_reaches_the_agent() {
    let listing = rendered(&[remote(INSTALL_A), stdio(INSTALL_B)], &["mcp_registry"]);

    // The whole point: the record serialised whole carries all three.
    assert!(!listing.contains(URL_SECRET), "{listing}");
    assert!(!listing.contains(ARG_SECRET), "{listing}");
    assert!(!listing.contains(CONFIG_SECRET), "{listing}");
    assert!(!listing.contains("mcp.acme.example"), "{listing}");
    assert!(!listing.contains("npx"), "{listing}");
}

#[test]
fn the_transport_is_named_by_kind_alone() {
    let rows = installed_rows(
        &[remote(INSTALL_A), stdio(INSTALL_B)],
        &grants(&["mcp_registry"]),
    );

    assert_eq!(rows[0]["transport"], "http_remote");
    assert_eq!(rows[1]["transport"], "stdio");
}

#[test]
fn what_the_agent_needs_is_still_there() {
    let rows = installed_rows(&[remote(INSTALL_A)], &grants(&["mcp_registry"]));

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["server_id"], INSTALL_A);
    assert_eq!(rows[0]["qualified_name"], "@acme/notes");
    assert_eq!(rows[0]["display_name"], "Acme Notes");
    assert_eq!(rows[0]["enabled"], true);
}

#[test]
fn a_scoped_grant_enumerates_only_its_own_install() {
    let rows = installed_rows(
        &[remote(INSTALL_A), remote(INSTALL_B)],
        &grants(&[&format!("mcp_registry.{INSTALL_A}")]),
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["server_id"], INSTALL_A);
}

#[test]
fn an_ungranted_agent_enumerates_nothing() {
    // The catch-all does not confer `mcp_registry`, so it does not confer the
    // enumeration either — listing must not be the way around the grant.
    assert!(installed_rows(&[remote(INSTALL_A)], &grants(&["*"])).is_empty());
    assert!(installed_rows(&[remote(INSTALL_A)], &grants(&[])).is_empty());
}

#[test]
fn a_bare_grant_enumerates_every_install() {
    let rows = installed_rows(
        &[remote(INSTALL_A), remote(INSTALL_B)],
        &grants(&["mcp_registry"]),
    );

    assert_eq!(rows.len(), 2);
}
