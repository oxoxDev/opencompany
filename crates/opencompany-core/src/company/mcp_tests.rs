use super::*;
use std::sync::Mutex;

use async_trait::async_trait;
use std::collections::HashMap;

fn server(name: &str, endpoint: &str) -> McpServer {
    McpServer {
        name: name.to_string(),
        endpoint: endpoint.to_string(),
        description: None,
        command: None,
        allowed_tools: Vec::new(),
        disallowed_tools: Vec::new(),
        read_only_tools: Vec::new(),
        timeout_secs: 30,
        enabled: true,
        auth_secret: None,
    }
}

// ---- merge precedence -------------------------------------------------

#[test]
fn effective_unions_manifest_and_runtime() {
    let manifest = vec![server("notion", "https://notion.example/mcp")];
    let runtime = vec![server("linear", "https://linear.example/mcp")];
    let eff = effective_mcp_servers(&[], &manifest, &runtime);
    let names: Vec<&str> = eff.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["notion", "linear"]);
    assert_eq!(eff[0].source, McpSource::Manifest);
    assert_eq!(eff[1].source, McpSource::Runtime);
}

#[test]
fn runtime_overrides_manifest_but_keeps_manifest_source() {
    let manifest = vec![server("notion", "https://notion.example/mcp")];
    let mut override_entry = server("notion", "https://notion.example/mcp");
    override_entry.enabled = false;
    override_entry.allowed_tools = vec!["search".into()];
    let eff = effective_mcp_servers(&[], &manifest, &[override_entry]);
    assert_eq!(eff.len(), 1, "override does not duplicate the server");
    assert_eq!(eff[0].source, McpSource::Manifest, "still manifest-badged");
    assert!(!eff[0].enabled, "override wins the enabled flag");
    assert_eq!(eff[0].allowed_tools, vec!["search".to_string()]);
}

// ---- install defaults, the third merge layer (issue #527) -------------

#[test]
fn a_default_ships_enabled_with_its_own_badge() {
    // The acceptance criterion: a fresh install has the server active with
    // no user action and no company.toml edit.
    let defaults = vec![server("deepwiki", "https://deepwiki.example/mcp")];
    let eff = effective_mcp_servers(&defaults, &[], &[]);
    assert_eq!(eff.len(), 1);
    assert_eq!(eff[0].name, "deepwiki");
    assert!(eff[0].enabled, "a default is active without user action");
    assert_eq!(
        eff[0].source,
        McpSource::Default,
        "not Manifest — nobody wrote it into this company"
    );
}

#[test]
fn no_defaults_leaves_the_two_layer_result_untouched() {
    // The compatibility property every existing install depends on: an
    // install that configures no defaults resolves exactly as before.
    let manifest = vec![server("notion", "https://notion.example/mcp")];
    let runtime = vec![server("linear", "https://linear.example/mcp")];
    let eff = effective_mcp_servers(&[], &manifest, &runtime);
    let names: Vec<&str> = eff.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["notion", "linear"]);
    assert_eq!(eff[0].source, McpSource::Manifest);
    assert_eq!(eff[1].source, McpSource::Runtime);
}

#[test]
fn the_manifest_shadows_a_default_of_the_same_name() {
    // A company that declares the server has said something specific about
    // it; the install-wide default is the fallback it overrides.
    let defaults = vec![server("shared", "https://default.example/mcp")];
    let manifest = vec![server("shared", "https://manifest.example/mcp")];
    let eff = effective_mcp_servers(&defaults, &manifest, &[]);
    assert_eq!(eff.len(), 1, "one row, not two claiming the same slug");
    assert_eq!(eff[0].endpoint, "https://manifest.example/mcp");
    assert_eq!(eff[0].source, McpSource::Manifest);
}

#[test]
fn a_runtime_override_disables_a_default_but_keeps_its_badge() {
    // This is how an operator turns a shipped default off. It must persist
    // as an override rather than a deletion, because the declaration lives
    // in the install config where the console cannot reach it.
    let defaults = vec![server("deepwiki", "https://deepwiki.example/mcp")];
    let mut off = server("deepwiki", "https://deepwiki.example/mcp");
    off.enabled = false;
    let eff = effective_mcp_servers(&defaults, &[], &[off]);
    assert_eq!(eff.len(), 1, "the override does not duplicate the server");
    assert!(!eff[0].enabled, "the operator's disable wins");
    assert_eq!(
        eff[0].source,
        McpSource::Default,
        "still default-badged, so the console still refuses to delete it"
    );
}

#[test]
fn ordering_puts_manifest_first_then_defaults_then_runtime_only() {
    let defaults = vec![server("d", "https://d.example/mcp")];
    let manifest = vec![server("m", "https://m.example/mcp")];
    let runtime = vec![server("r", "https://r.example/mcp")];
    let eff = effective_mcp_servers(&defaults, &manifest, &runtime);
    let names: Vec<&str> = eff.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["m", "d", "r"]);
    assert_eq!(eff[2].source, McpSource::Runtime);
}

// ---- normalizing the configured defaults (issue #527) -----------------

#[test]
fn an_empty_default_list_is_authoritative() {
    // "Ship no defaults" — never "fall back to a built-in set". There is no
    // compiled-in list to fall back to, and adding one later must not
    // change this.
    let (kept, problems) = normalize_default_servers(&[]);
    assert!(kept.is_empty());
    assert!(problems.is_empty());
}

#[test]
fn one_bad_entry_does_not_cost_the_good_ones() {
    let raw = vec![
        server("good", "https://good.example/mcp"),
        server("", "https://nameless.example/mcp"),
        server("alsogood", "https://also.example/mcp"),
    ];
    let (kept, problems) = normalize_default_servers(&raw);
    let names: Vec<&str> = kept.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["good", "alsogood"]);
    assert_eq!(problems.len(), 1, "and the drop is explained, not silent");
}

#[test]
fn a_credential_in_the_endpoint_query_string_is_refused_not_scrubbed() {
    // Refused, because scrubbing would ship a server whose auth silently no
    // longer works — failing at an agent's first tool call instead of here.
    let raw = vec![
        server("qs", "https://api.example.com/mcp?apiKey=leaked"),
        server(
            "qs2",
            "https://api.example.com/mcp?projectId=p&token=leaked",
        ),
        server("qs3", "https://api.example.com/mcp?access_token=leaked"),
        server("fine", "https://api.example.com/mcp?projectId=p"),
    ];
    let (kept, problems) = normalize_default_servers(&raw);
    let names: Vec<&str> = kept.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["fine"], "a benign query parameter is kept");
    assert_eq!(problems.len(), 3);
}

#[test]
fn an_encoded_credential_key_in_the_query_string_is_refused() {
    // Percent-decode each key before matching, or `api%4Bey` (apiKey) sails
    // past the raw-name scan and ships a secret-bearing default.
    let raw = vec![
        server("enc-apikey", "https://api.example.com/mcp?api%4Bey=secret"),
        server("enc-token", "https://api.example.com/mcp?tok%65n=secret"),
        server(
            "enc-access-token",
            "https://api.example.com/mcp?access%2Dtoken=secret",
        ),
        server("fine", "https://api.example.com/mcp?project%5Fid=p"),
    ];
    let (kept, problems) = normalize_default_servers(&raw);
    let names: Vec<&str> = kept.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["fine"], "a decoded-benign key is kept");
    assert_eq!(problems.len(), 3);
}

#[test]
fn a_default_may_not_depend_on_a_credential_key() {
    // A default is handed to every agent on the install unprompted, so it
    // has to work unattended. One that needs auth belongs per company.
    let mut needs_auth = server("private", "https://private.example/mcp");
    needs_auth.auth_secret = Some("mcp/private/auth".to_string());
    let (kept, problems) = normalize_default_servers(&[needs_auth]);
    assert!(kept.is_empty());
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("auth_secret"));
}

#[test]
fn a_non_http_or_stdio_default_is_refused_by_the_shared_validator() {
    // Delegated to `validate_one`, so defaults and every other declaration
    // path enforce the hosted-v1 transport boundary identically.
    let mut stdio = server("stdio", "");
    stdio.command = Some("npx some-mcp-server".to_string());
    let raw = vec![
        stdio,
        server("ftp", "ftp://files.example/mcp"),
        server("ok", "http://localhost:9000/mcp"),
    ];
    let (kept, problems) = normalize_default_servers(&raw);
    let names: Vec<&str> = kept.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["ok"]);
    assert!(!problems.is_empty());
}

#[test]
fn a_userinfo_credential_in_a_default_endpoint_is_refused() {
    let raw = vec![server("ui", "https://user:pass@host.example/mcp")];
    let (kept, problems) = normalize_default_servers(&raw);
    assert!(kept.is_empty(), "the user:pass@host form never ships");
    assert_eq!(problems.len(), 1);
}

#[test]
fn a_duplicated_default_name_keeps_the_first() {
    // Two rows claiming one slug would let merge order decide which won.
    let raw = vec![
        server("dup", "https://first.example/mcp"),
        server("dup", "https://second.example/mcp"),
    ];
    let (kept, problems) = normalize_default_servers(&raw);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].endpoint, "https://first.example/mcp");
    assert_eq!(problems.len(), 1);
}

#[test]
fn a_clean_default_survives_with_its_fields_intact() {
    let mut full = server("full", "https://full.example/mcp");
    full.description = Some("a documentation server".to_string());
    full.allowed_tools = vec!["read".to_string()];
    full.timeout_secs = 45;
    let (kept, problems) = normalize_default_servers(&[full]);
    assert!(problems.is_empty());
    assert_eq!(kept.len(), 1);
    assert_eq!(
        kept[0].description.as_deref(),
        Some("a documentation server")
    );
    assert_eq!(kept[0].allowed_tools, vec!["read".to_string()]);
    assert_eq!(kept[0].timeout_secs, 45);
}

// ---- validation -------------------------------------------------------

#[test]
fn valid_http_server_passes() {
    assert!(validate_servers(&[server("notion", "https://notion.example/mcp")]).is_empty());
}

#[test]
fn duplicate_names_are_rejected() {
    let problems = validate_servers(&[
        server("dup", "https://a.example/mcp"),
        server("dup", "https://b.example/mcp"),
    ]);
    assert!(
        problems.iter().any(|p| p.contains("more than once")),
        "{problems:?}"
    );
}

#[test]
fn non_http_endpoint_is_rejected() {
    let problems = validate_servers(&[server("bad", "ftp://x.example/mcp")]);
    assert!(problems.iter().any(|p| p.contains("http")), "{problems:?}");
}

#[test]
fn missing_endpoint_is_rejected() {
    let problems = validate_servers(&[server("bare", "")]);
    assert!(
        problems.iter().any(|p| p.contains("endpoint")),
        "{problems:?}"
    );
}

#[test]
fn stdio_command_is_rejected_in_hosted_v1() {
    let mut s = server("local", "https://x.example/mcp");
    s.command = Some("npx some-mcp".into());
    let problems = validate_servers(&[s]);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("stdio") && p.contains("hosted v1")),
        "{problems:?}"
    );
}

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
