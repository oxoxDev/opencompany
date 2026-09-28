//! The half of the MCP tests that needs a [`SecretStore`]: credential
//! resolution, and the per-tool policy the same read resolves alongside it.

use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use super::tests::server;

// ---- secret resolution (write-only auth) ------------------------------

#[derive(Default)]
struct MemSecrets {
    map: Mutex<HashMap<String, String>>,
}

#[async_trait]
impl SecretStore for MemSecrets {
    async fn get(&self, _c: &CompanyId, key: &str) -> Result<Option<SecretValue>> {
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

#[tokio::test]
async fn resolve_effective_fills_bearer_and_index_roundtrips() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();

    // Runtime-add a server + write its token (write-only).
    save_runtime_index(
        &company,
        &secrets,
        &[server("notion", "https://notion.example/mcp")],
    )
    .await
    .unwrap();
    store_bearer(&company, "notion", "sk-secret-123", &secrets)
        .await
        .unwrap();

    let decls = resolve_effective(&company, &[], &[], &secrets)
        .await
        .unwrap();
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0].auth, AuthMaterial::Bearer("sk-secret-123".into()));
    assert_eq!(decls[0].source, McpSource::Runtime);

    // The token is never exposed by the status helper — only a bool.
    assert!(
        auth_configured(
            &company,
            &server("notion", "https://notion.example/mcp"),
            &secrets
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn cleared_auth_reads_back_as_unconfigured() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    store_bearer(&company, "notion", "tok", &secrets)
        .await
        .unwrap();
    clear_auth(&company, "notion", &secrets).await.unwrap();
    let material = load_auth(&company, "notion", &secrets, None).await.unwrap();
    assert_eq!(material, AuthMaterial::None);
}

// ---- query-param auth (BrowserBase style) -----------------------------

#[tokio::test]
async fn store_and_resolve_query_param_auth_round_trips() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    save_runtime_index(
        &company,
        &secrets,
        &[server(
            "browserbase",
            "https://api.browserbase.com/mcp?projectId=pid",
        )],
    )
    .await
    .unwrap();
    store_auth(
        &company,
        "browserbase",
        &AuthMaterial::QueryParam {
            name: "apiKey".into(),
            value: "qp-secret".into(),
        },
        &secrets,
    )
    .await
    .unwrap();

    let decls = resolve_effective(&company, &[], &[], &secrets)
        .await
        .unwrap();
    assert_eq!(
        decls[0].auth,
        AuthMaterial::QueryParam {
            name: "apiKey".into(),
            value: "qp-secret".into(),
        }
    );
    // The non-secret project id stays in the endpoint URL, unchanged.
    assert!(decls[0].endpoint.contains("projectId=pid"));
}

#[test]
fn secret_values_lists_the_credential_for_scrubbing() {
    assert_eq!(
        AuthMaterial::Bearer("tok".into()).secret_values(),
        vec!["tok".to_string()]
    );
    assert_eq!(
        AuthMaterial::QueryParam {
            name: "apiKey".into(),
            value: "qp".into(),
        }
        .secret_values(),
        vec!["qp".to_string()]
    );
    assert!(AuthMaterial::None.secret_values().is_empty());
}

// ---- health persistence -----------------------------------------------

#[tokio::test]
async fn health_round_trips_and_clears() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    assert_eq!(
        load_health(&company, "notion", &secrets).await.unwrap(),
        None
    );

    let health = McpHealth {
        status: McpStatus::Ok,
        message: "8 tools available".into(),
        tool_count: 8,
        checked_at_millis: 123,
        auth_hint: None,
    };
    save_health(&company, "notion", &health, &secrets)
        .await
        .unwrap();
    assert_eq!(
        load_health(&company, "notion", &secrets).await.unwrap(),
        Some(health)
    );

    clear_health(&company, "notion", &secrets).await.unwrap();
    assert_eq!(
        load_health(&company, "notion", &secrets).await.unwrap(),
        None
    );
}

// ---- endpoint validation ----------------------------------------------

#[test]
fn userinfo_endpoint_is_rejected() {
    let problems = validate_servers(&[server("creds", "https://user:pass@host/mcp")]);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("must not embed credentials")),
        "{problems:?}"
    );
}

#[test]
fn email_in_query_is_not_mistaken_for_userinfo() {
    // The '@' lives in the query, not the authority — must stay valid.
    assert!(validate_servers(&[server("ok", "https://host/mcp?to=a@b.com")]).is_empty());
}

#[test]
fn secret_in_query_is_a_non_blocking_advisory() {
    // A key-ish query param yields an advisory but NOT a validation error.
    assert!(endpoint_secret_advisory("https://host/mcp?apiKey=sk-123").is_some());
    assert!(
        validate_servers(&[server("browserbase", "https://host/mcp?apiKey=sk-123")]).is_empty()
    );
    // A non-secret id (BrowserBase's projectId) is fine — no advisory.
    assert!(endpoint_secret_advisory("https://host/mcp?projectId=pid").is_none());
    // No query string at all — no advisory.
    assert!(endpoint_secret_advisory("https://host/mcp").is_none());
}

// ---- per-tool policy resolution ---------------------------------------

use crate::company::mcp_policy::{
    ApprovalMode, McpToolPolicies, ToolPolicy, ToolTier, resolve_policy, save_tool_policies,
    tool_policies_key,
};

fn read_only_server(name: &str, endpoint: &str, read_only: &[&str]) -> McpServer {
    let mut s = server(name, endpoint);
    s.read_only_tools = read_only.iter().map(|t| t.to_string()).collect();
    s
}

/// A server with no stored document still resolves its declared read-only
/// tools, so nothing about a company's approval behaviour moves on upgrade.
#[tokio::test]
async fn resolve_effective_layers_the_declared_read_only_list() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let manifest = vec![read_only_server(
        "notion",
        "https://notion.example/mcp",
        &["search_pages"],
    )];

    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();
    let policies = &decls[0].tool_policies;
    assert_eq!(
        resolve_policy(policies, "search_pages", None).mode,
        ApprovalMode::AlwaysAllow
    );
    assert_eq!(
        resolve_policy(policies, "move_page", None).mode,
        ApprovalMode::NeedsApproval
    );
}

#[tokio::test]
async fn resolve_effective_layers_a_stored_document_over_the_declaration() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let manifest = vec![read_only_server(
        "notion",
        "https://notion.example/mcp",
        &["search_pages"],
    )];
    let mut stored = McpToolPolicies::default();
    stored.overrides.insert(
        "move_page".into(),
        ToolPolicy {
            tier: Some(ToolTier::WriteDelete),
            mode: Some(ApprovalMode::Blocked),
        },
    );
    save_tool_policies(&company, &secrets, &tool_policies_key("notion"), &stored)
        .await
        .unwrap();

    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();
    let policies = &decls[0].tool_policies;
    assert_eq!(
        resolve_policy(policies, "move_page", None).mode,
        ApprovalMode::Blocked
    );
    assert_eq!(
        resolve_policy(policies, "search_pages", None).mode,
        ApprovalMode::AlwaysAllow
    );
}

/// One unreadable policy key degrades its own server and leaves every other
/// server standing. Surfacing it would travel up as "this company gets no MCP
/// servers at all".
#[tokio::test]
async fn an_unreadable_policy_key_does_not_strip_the_other_servers() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let manifest = vec![
        read_only_server("notion", "https://notion.example/mcp", &["search_pages"]),
        read_only_server("linear", "https://linear.example/mcp", &["list_issues"]),
    ];
    secrets
        .set(
            &company,
            &tool_policies_key("notion"),
            SecretValue("{not json".into()),
        )
        .await
        .unwrap();

    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();
    assert_eq!(decls.len(), 2);
    let notion = decls.iter().find(|d| d.name == "notion").unwrap();
    let linear = decls.iter().find(|d| d.name == "linear").unwrap();
    // The damaged server falls to all-park, losing even its declared read-only.
    assert_eq!(
        resolve_policy(&notion.tool_policies, "search_pages", None).mode,
        ApprovalMode::NeedsApproval
    );
    // Its neighbour is untouched.
    assert_eq!(
        resolve_policy(&linear.tool_policies, "list_issues", None).mode,
        ApprovalMode::AlwaysAllow
    );
}

// ---- the migration is a restatement, not a behaviour change -----------

/// Every fixture the switch-over has to survive, in one place: a server with a
/// declared read-only tool, a server that declares none, a disabled server, and
/// a tool nobody declared.
fn differential_fixture() -> Vec<McpServer> {
    vec![
        read_only_server(
            "notion",
            "https://notion.example/mcp",
            &["search_pages", "get_page"],
        ),
        read_only_server("linear", "https://linear.example/mcp", &[]),
        {
            let mut disabled =
                read_only_server("archive", "https://archive.example/mcp", &["read_doc"]);
            disabled.enabled = false;
            disabled
        },
    ]
}

/// The hard merge gate. `mcp_call_reach` must answer identically whether it is
/// handed the flat declaration or the resolved policy, across every
/// (server, tool) pair in the fixture and both bridge tool names — including
/// pairs naming a server or tool that does not exist.
///
/// Both flatteners stay in the tree while this runs, which is the only way the
/// comparison exists at all.
#[tokio::test]
async fn the_policy_flattener_reproduces_the_declaration_flattener() {
    use crate::policy::consequence::{MCP_CALL_TOOL, MCP_REGISTRY_TOOL_CALL, mcp_call_reach};

    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let manifest = differential_fixture();
    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();

    let before = mcp_read_set(&decls);
    let after = crate::company::mcp_policy::mcp_allow_set(&decls);

    let servers = ["notion", "linear", "archive", "ghost"];
    let tools = ["search_pages", "get_page", "read_doc", "move_page", "ghost"];
    let bridges = [
        (MCP_CALL_TOOL, "server", "tool"),
        (MCP_REGISTRY_TOOL_CALL, "server_id", "tool_name"),
    ];

    let mut compared = 0usize;
    for (bridge, server_key, tool_key) in bridges {
        for server in servers {
            for tool in tools {
                let args = serde_json::json!({ server_key: server, tool_key: tool });
                assert_eq!(
                    mcp_call_reach(bridge, &args, &before),
                    mcp_call_reach(bridge, &args, &after),
                    "{bridge} disagrees for ({server}, {tool})"
                );
                compared += 1;
            }
        }
    }
    assert_eq!(compared, bridges.len() * servers.len() * tools.len());

    // The comparison would pass vacuously if neither set downgraded anything.
    assert!(!before.is_empty());
    assert!(!after.is_empty());
}

/// A disabled server hands out no tool, so its declaration reaches nothing —
/// the property the cross-product above would still satisfy if both flatteners
/// were wrong the same way.
#[tokio::test]
async fn a_disabled_server_contributes_no_allow() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let decls = resolve_effective(&company, &[], &differential_fixture(), &secrets)
        .await
        .unwrap();
    let allow = crate::company::mcp_policy::mcp_allow_set(&decls);
    assert!(allow.contains("notion", "search_pages"));
    assert!(!allow.contains("archive", "read_doc"));
}

/// The flattener must not read the name heuristic. A tool the heuristic calls
/// read-only, with nothing stored about it, stays out of the allow set.
#[tokio::test]
async fn the_flattener_never_promotes_a_merely_suggested_read() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let manifest = vec![read_only_server(
        "notion",
        "https://notion.example/mcp",
        &[],
    )];
    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();
    assert_eq!(
        crate::company::mcp_policy::suggest_tool_tier("search_pages", None),
        ToolTier::ReadOnly
    );
    assert!(crate::company::mcp_policy::mcp_allow_set(&decls).is_empty());
}

/// Only `AlwaysAllow` reaches the set. The cross-product above cannot pin this:
/// with no stored document every entry comes from the migration, so it is
/// already the only mode present there, and a flattener admitting everything
/// that is merely not blocked would pass it.
#[tokio::test]
async fn only_always_allow_reaches_the_set() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let manifest = vec![read_only_server(
        "notion",
        "https://notion.example/mcp",
        &["search_pages"],
    )];
    let mut stored = McpToolPolicies::default();
    stored.overrides.insert(
        "move_page".into(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::NeedsApproval),
        },
    );
    stored.overrides.insert(
        "delete_page".into(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::Blocked),
        },
    );
    save_tool_policies(&company, &secrets, &tool_policies_key("notion"), &stored)
        .await
        .unwrap();

    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();
    let allow = crate::company::mcp_policy::mcp_allow_set(&decls);
    assert!(allow.contains("notion", "search_pages"));
    assert!(!allow.contains("notion", "move_page"));
    assert!(!allow.contains("notion", "delete_page"));
}

/// A stored refusal takes an allow away from a tool the declaration still lists
/// — the first place the two flatteners are *supposed* to disagree, so that the
/// merge gate above is measuring equality rather than a feature that does
/// nothing.
#[tokio::test]
async fn a_stored_refusal_overrides_the_declaration() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let manifest = vec![read_only_server(
        "notion",
        "https://notion.example/mcp",
        &["search_pages"],
    )];
    let mut stored = McpToolPolicies::default();
    stored.overrides.insert(
        "search_pages".into(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::Blocked),
        },
    );
    save_tool_policies(&company, &secrets, &tool_policies_key("notion"), &stored)
        .await
        .unwrap();

    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();
    assert!(mcp_read_set(&decls).contains("notion", "search_pages"));
    assert!(
        !crate::company::mcp_policy::mcp_allow_set(&decls).contains("notion", "search_pages"),
        "a stored refusal must win over the declaration"
    );
}

// ---- the inventory is what a tier default resolves against ------------

use crate::company::mcp_policy::{
    McpToolInventory, inventory_from_discovery, registry_tool_inventory_key, save_tool_inventory,
    tool_inventory_key,
};

async fn seed_inventory(company: &CompanyId, secrets: &MemSecrets, server: &str, tools: &[&str]) {
    let inventory = inventory_from_discovery(tools.iter().map(|t| (*t, None)), 1);
    save_tool_inventory(company, secrets, &tool_inventory_key(server), &inventory)
        .await
        .unwrap();
}

/// The step-3 trap, pinned. A per-tier bulk default names no tools; the
/// inventory is the only thing that says which tools it is *for*. Without it
/// the operator's decision is silently not enforced — no error, no row, just a
/// tool that keeps parking after they allowed it.
#[tokio::test]
async fn a_tier_default_reaches_the_tools_discovery_found() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let manifest = vec![read_only_server(
        "notion",
        "https://notion.example/mcp",
        &[],
    )];
    seed_inventory(&company, &secrets, "notion", &["search_pages", "move_page"]).await;

    let mut stored = McpToolPolicies::default();
    stored
        .tier_defaults
        .insert(ToolTier::ReadOnly, ApprovalMode::AlwaysAllow);
    save_tool_policies(&company, &secrets, &tool_policies_key("notion"), &stored)
        .await
        .unwrap();

    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();
    let allow = crate::company::mcp_policy::mcp_allow_set(&decls);
    // `search_pages` is suggested read-only, so the bulk default reaches it.
    assert!(allow.contains("notion", "search_pages"));
    // `move_page` is not, so it keeps parking.
    assert!(!allow.contains("notion", "move_page"));
}

/// The same company with no inventory: the tier default reaches nothing, which
/// is the silent wrongness the threading exists to prevent.
#[tokio::test]
async fn without_an_inventory_a_tier_default_reaches_nothing() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let manifest = vec![read_only_server(
        "notion",
        "https://notion.example/mcp",
        &[],
    )];
    let mut stored = McpToolPolicies::default();
    stored
        .tier_defaults
        .insert(ToolTier::ReadOnly, ApprovalMode::AlwaysAllow);
    save_tool_policies(&company, &secrets, &tool_policies_key("notion"), &stored)
        .await
        .unwrap();

    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();
    assert!(decls[0].tool_inventory.tools.is_empty());
    assert!(crate::company::mcp_policy::mcp_allow_set(&decls).is_empty());
}

/// A suggested tier still cannot allow on its own — the inventory supplies the
/// grouping, never the permission. Same rule as step 3, now with the suggestion
/// arriving from storage rather than a literal.
#[tokio::test]
async fn an_inventory_alone_grants_nothing() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let manifest = vec![read_only_server(
        "notion",
        "https://notion.example/mcp",
        &[],
    )];
    seed_inventory(&company, &secrets, "notion", &["search_pages"]).await;

    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();
    assert_eq!(
        decls[0].tool_inventory.suggested("search_pages"),
        Some(ToolTier::ReadOnly)
    );
    assert!(crate::company::mcp_policy::mcp_allow_set(&decls).is_empty());
}

/// An unreadable inventory degrades that server only, and the declaration it
/// carries keeps working.
#[tokio::test]
async fn an_unreadable_inventory_does_not_disturb_the_declaration() {
    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let manifest = vec![read_only_server(
        "notion",
        "https://notion.example/mcp",
        &["search_pages"],
    )];
    secrets
        .set(
            &company,
            &tool_inventory_key("notion"),
            SecretValue("{not json".into()),
        )
        .await
        .unwrap();

    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();
    assert_eq!(decls[0].tool_inventory, McpToolInventory::default());
    assert!(
        crate::company::mcp_policy::mcp_allow_set(&decls).contains("notion", "search_pages"),
        "the declared read-only tool must survive an unreadable inventory"
    );
}

#[test]
fn the_registry_inventory_key_is_addressed_by_install_id() {
    assert_eq!(
        registry_tool_inventory_key("0b8f4b0e"),
        "mcp_registry/0b8f4b0e/tool_inventory"
    );
}

/// Clearing a bulk default has to reach the gate, not just the document.
///
/// The console had no way to express this at all before — the wire took a mode
/// per tier and nothing else, so a tier an operator set could be changed but
/// never unset. A clear that stored cleanly but left the allow set alone would
/// be the same bug wearing a fix.
#[tokio::test]
async fn clearing_a_tier_default_returns_its_tools_to_the_gate() {
    use crate::server::ops::mcp_tool_policy::{PutToolPolicy, apply_tool_policy_patch};

    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let manifest = vec![read_only_server(
        "notion",
        "https://notion.example/mcp",
        &[],
    )];
    seed_inventory(&company, &secrets, "notion", &["search_pages"]).await;

    let mut stored = McpToolPolicies::default();
    stored
        .tier_defaults
        .insert(ToolTier::ReadOnly, ApprovalMode::AlwaysAllow);
    save_tool_policies(&company, &secrets, &tool_policies_key("notion"), &stored)
        .await
        .unwrap();

    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();
    assert!(
        crate::company::mcp_policy::mcp_allow_set(&decls).contains("notion", "search_pages"),
        "the allow has to be in force before clearing it can mean anything"
    );

    let mut defaults = std::collections::HashMap::new();
    defaults.insert("read_only".to_string(), None);
    let cleared = apply_tool_policy_patch(
        stored,
        PutToolPolicy {
            tier_defaults: Some(defaults),
            tools: None,
        },
    )
    .unwrap();
    save_tool_policies(&company, &secrets, &tool_policies_key("notion"), &cleared)
        .await
        .unwrap();

    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();
    assert!(
        !crate::company::mcp_policy::mcp_allow_set(&decls).contains("notion", "search_pages"),
        "with nothing stored the suggestion is back on its own, and a suggestion asks"
    );
}

/// A per-tool decision is not a bulk one: clearing the tier leaves it standing.
#[tokio::test]
async fn clearing_a_tier_default_leaves_a_per_tool_decision_alone() {
    use crate::server::ops::mcp_tool_policy::{PutToolPolicy, apply_tool_policy_patch};

    let company = CompanyId::new("acme");
    let secrets = MemSecrets::default();
    let manifest = vec![read_only_server(
        "notion",
        "https://notion.example/mcp",
        &[],
    )];
    seed_inventory(&company, &secrets, "notion", &["search_pages", "move_page"]).await;

    let mut stored = McpToolPolicies::default();
    stored
        .tier_defaults
        .insert(ToolTier::ReadOnly, ApprovalMode::AlwaysAllow);
    stored.overrides.insert(
        "move_page".into(),
        ToolPolicy {
            tier: None,
            mode: Some(ApprovalMode::AlwaysAllow),
        },
    );

    let mut defaults = std::collections::HashMap::new();
    defaults.insert("read_only".to_string(), None);
    let cleared = apply_tool_policy_patch(
        stored,
        PutToolPolicy {
            tier_defaults: Some(defaults),
            tools: None,
        },
    )
    .unwrap();
    save_tool_policies(&company, &secrets, &tool_policies_key("notion"), &cleared)
        .await
        .unwrap();

    let decls = resolve_effective(&company, &[], &manifest, &secrets)
        .await
        .unwrap();
    let allow = crate::company::mcp_policy::mcp_allow_set(&decls);
    assert!(
        allow.contains("notion", "move_page"),
        "the operator decided this row itself, and the tier is not what carried it"
    );
    assert!(!allow.contains("notion", "search_pages"));
}
