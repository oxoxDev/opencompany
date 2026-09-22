use super::*;

fn company() -> CompanyId {
    CompanyId::new("acme")
}

fn call(tool: &str) -> ToolCall {
    ToolCall {
        tool: tool.into(),
        args: serde_json::Value::Null,
    }
}

#[tokio::test]
async fn ungranted_tool_is_rejected() {
    let provider = StubToolProvider::new(vec!["email.send".into()]);
    let err = provider
        .invoke(&company(), call("payment.send"))
        .await
        .unwrap_err();
    assert!(matches!(err, OpenCompanyError::ToolNotGranted(t) if t == "payment.send"));
}

#[tokio::test]
async fn granted_tool_passes_the_gate() {
    let provider = StubToolProvider::new(vec!["email.*".into()]);
    let result = provider
        .invoke(&company(), call("email.send"))
        .await
        .unwrap();
    assert!(!result.ok);
}

#[tokio::test]
async fn wildcard_grants_everything() {
    let provider = StubToolProvider::new(vec!["*".into()]);
    assert!(provider.invoke(&company(), call("anything")).await.is_ok());
}

#[tokio::test]
async fn empty_catalog() {
    let provider = StubToolProvider::default();
    assert!(provider.catalog(&company()).await.unwrap().is_empty());
}

// --- Prefix grants stop on a namespace boundary (issue #461) -------------

/// The defect verbatim: a trailing-`*` grant used to be a bare
/// `starts_with`, so `file*` reached every tool whose *name* merely began
/// with those letters. `filesystem_wipe` is the shape that makes it a
/// permission bug rather than a cosmetic one.
#[test]
fn prefix_grant_stops_at_the_snake_case_boundary() {
    assert!(grant_matches("file*", "file_read"));
    assert!(grant_matches("file*", "file_write"));
    // The grant itself, with nothing under it, is still covered.
    assert!(grant_matches("file*", "file"));
    // …but a longer *word* is a different namespace, not a sub-tool.
    assert!(!grant_matches("file*", "filesystem_wipe"));
    assert!(!grant_matches("file*", "filed"));
}

/// The issue's second example: `composio_list*` may cover the list family
/// and nothing else.
#[test]
fn prefix_grant_covers_only_its_own_family() {
    assert!(grant_matches("composio_list*", "composio_list"));
    assert!(grant_matches("composio_list*", "composio_list_apps"));
    assert!(grant_matches("composio_list*", "composio_list_toolkits"));
    assert!(!grant_matches("composio_list*", "composio_listen"));
    assert!(!grant_matches("composio_list*", "composio_execute"));
}

#[test]
fn wildcard_does_not_cover_mcp_servers() {
    assert!(!grants_cover_server(&["*".into()], "notion"));
    assert!(grants_cover_server(&["mcp:*".into()], "notion"));
    assert!(grants_cover_server(&["mcp:notion".into()], "notion"));
    assert!(!grants_cover_server(&["mcp:notion".into()], "linear"));
}

/// Registry-install grant scoping. Gated with the harness, where
/// `grants_cover_registry_server`'s callers are compiled.
#[cfg(feature = "openhuman")]
mod registry_installs {
    use super::*;

    /// A bare `mcp_registry` grant keeps the reach it has always had, so upgrading
    /// changes nothing for a company that never writes a scoped grant.
    #[test]
    fn bare_registry_grant_covers_every_install() {
        let grants = vec!["mcp_registry".to_string()];
        assert!(grants_cover_registry_server(
            &grants,
            "0b8f4b0e-3c2a-4a1d-9e77-6d5a2f1c8e40"
        ));
        assert!(grants_cover_registry_server(&grants, "any-other-install"));
    }

    /// The point of the change: a scoped grant reaches its own install and no
    /// other.
    #[test]
    fn scoped_registry_grant_narrows_to_one_install() {
        let a = "0b8f4b0e-3c2a-4a1d-9e77-6d5a2f1c8e40";
        let b = "7f1c9d22-55ae-4f3b-8c10-2b9e4d6a3f51";
        let grants = vec![format!("mcp_registry.{a}")];
        assert!(grants_cover_registry_server(&grants, a));
        assert!(!grants_cover_registry_server(&grants, b));
    }

    #[test]
    fn registry_wildcard_grant_covers_every_install() {
        let grants = vec!["mcp_registry.*".to_string()];
        assert!(grants_cover_registry_server(&grants, "notion-install"));
        assert!(grants_cover_registry_server(&grants, "linear-install"));
    }

    /// The catch-all confers nothing here, matching `grants_cover_server` and
    /// `grants_mcp_registry_explicit`.
    #[test]
    fn wildcard_does_not_cover_registry_installs() {
        assert!(!grants_cover_registry_server(
            &["*".into()],
            "notion-install"
        ));
        assert!(!grants_cover_registry_server(
            &["composio".into()],
            "notion-install"
        ));
        assert!(!grants_cover_registry_server(
            &["mcp:notion".into()],
            "notion-install"
        ));
    }

    /// A uuid install id survives the boundary matcher unchanged: `-` is not a
    /// separator, so a prefix grant cannot straddle one id into another.
    #[test]
    fn uuid_install_ids_match_exactly() {
        let a = "0b8f4b0e-3c2a-4a1d-9e77-6d5a2f1c8e40";
        let grants = vec![format!("mcp_registry.{a}")];
        assert!(grants_cover_registry_server(&grants, a));
        assert!(!grants_cover_registry_server(&grants, "0b8f4b0e"));
        assert!(!grants_cover_registry_server(
            &grants,
            "0b8f4b0e-3c2a-4a1d-9e77-6d5a2f1c8e40-extra"
        ));
    }

    /// `mcp*` is a grant written for the `mcp:<server>` bridge, and it does NOT
    /// reach a directory install — even though `_` is a namespace boundary for
    /// the shared matcher, which would otherwise let it span into
    /// `mcp_registry.<id>`.
    ///
    /// `grants_mcp_registry_explicit` decides whether these tools are wired at
    /// all and accepts only a grant rooted at `mcp_registry`, so honouring a
    /// spanning wildcard here would leave the two gates disagreeing about what
    /// confers the namespace — which is how a permission boundary widens
    /// without anyone seeing it.
    #[test]
    fn a_bridge_prefix_grant_does_not_reach_an_install() {
        assert!(!grants_cover_registry_server(
            &["mcp*".into()],
            "notion-install"
        ));
        // Unchanged for the bridge namespace it was written for.
        assert!(grants_cover_server(&["mcp*".into()], "notion"));
    }
}

/// Every grant shape the shipped `companies/*/company.toml` manifests use
/// keeps working exactly as before — the fix must be invisible to them.
#[test]
fn shipped_grant_shapes_are_unchanged() {
    // Dotted namespace globs.
    assert!(grant_matches("email.*", "email.send"));
    assert!(grant_matches("web.*", "web.fetch"));
    assert!(grant_matches("workspace.*", "workspace.read"));
    assert!(!grant_matches("email.*", "payment.send"));
    // Colon-namespaced MCP servers.
    assert!(grant_matches("mcp:*", "mcp:notion"));
    assert!(grant_matches("mcp:*", "mcp:any-server"));
    assert!(grant_matches("mcp:notion", "mcp:notion"));
    assert!(!grant_matches("mcp:notion", "mcp:slack"));
    assert!(!grant_matches("mcp:*", "mcpx:notion"));
    // Exact tokens and the catch-all.
    assert!(grant_matches("composio", "composio"));
    assert!(!grant_matches("composio", "composio.execute"));
    assert!(grant_matches("*", "anything_at_all"));
}

/// A bare `*` is a broad grant, not an unlimited one: the metered
/// `web_search` surface (issue #238) must still be opted into by name.
/// Pinned here too because the boundary fix touches the same grant lists.
#[test]
fn catch_all_still_confers_no_web_search() {
    assert!(!crate::company::grants_search_explicit(&["*".into()]));
    assert!(crate::company::grants_search_explicit(&["search".into()]));
}

/// The whole point of issue #461: one rule, two matchers. Over a shared
/// corpus, `grant_matches` (tool names) and `grants_cover` (namespaces)
/// must never disagree about whether a dotted grant reaches a namespace.
///
/// Gated with the harness, which is where `grants_cover` is compiled.
#[cfg(feature = "openhuman")]
#[test]
fn both_matchers_agree_over_a_shared_corpus() {
    use crate::harness::build::grants_cover;

    // (grant, namespace) pairs written the way a manifest writes them.
    let corpus = [
        ("docs", "docs"),
        ("docs.*", "docs"),
        ("docs.read", "docs"),
        ("*", "docs"),
        ("web.*", "docs"),
        ("documentation.*", "docs"),
        ("doc.*", "docs"),
        ("workspace", "workspace"),
        ("workspace.write", "workspace"),
        ("workspaces.write", "workspace"),
        ("search", "search"),
        ("searching", "search"),
    ];

    for (grant, namespace) in corpus {
        let by_namespace = grants_cover(&[grant.to_string()], namespace);
        // The per-tool matcher asked the same question: does this grant
        // reach *something* in the namespace? A namespace grant is probed
        // as the namespace's own glob.
        let by_tool = grant_matches(&format!("{namespace}.*"), grant)
            || grant_matches(grant, namespace)
            || grant == "*";
        assert_eq!(
            by_namespace, by_tool,
            "matchers disagree on grant {grant:?} vs namespace {namespace:?}"
        );
    }
}

#[test]
fn extends_on_boundary_is_exact_prefix_or_separator() {
    // Identity.
    assert!(extends_on_boundary("docs", "docs", &['.']));
    // The prefix already carries the separator.
    assert!(extends_on_boundary("email.send", "email.", &['.']));
    assert!(extends_on_boundary("mcp:notion", "mcp:", &[':']));
    // The name breaks on one.
    assert!(extends_on_boundary("docs.read", "docs", &['.']));
    assert!(extends_on_boundary("file_read", "file", &['_']));
    // Mid-word extension is not a boundary.
    assert!(!extends_on_boundary("filesystem", "file", &['_']));
    assert!(!extends_on_boundary("docs.read", "docs", &['_']));
    // Not a prefix at all.
    assert!(!extends_on_boundary("web.fetch", "docs", &['.']));
    // A shorter name never covers a longer prefix.
    assert!(!extends_on_boundary("doc", "docs", &['.']));
}
