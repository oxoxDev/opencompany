//! Directory ranking, the featured backfill, and icon inlining.

use std::cell::RefCell;

use serde_json::json;

use super::*;

const OFFICIAL: &[&str] = &["com.notion/mcp", "app.linear/linear"];

fn entry(qualified_name: &str, use_count: u64) -> CatalogueEntryDto {
    CatalogueEntryDto {
        qualified_name: qualified_name.to_string(),
        display_name: qualified_name.to_string(),
        description: None,
        icon_url: None,
        source: "mcp_official".to_string(),
        official: false,
        use_count,
        website_url: None,
    }
}

fn names(servers: &[CatalogueEntryDto]) -> Vec<&str> {
    servers.iter().map(|s| s.qualified_name.as_str()).collect()
}

#[test]
fn ranking_badges_only_exact_official_names() {
    let mut servers = vec![
        entry("com.notion/mcp", 0),
        entry("ai.smithery/notion", 0),
        entry("com.notion/mcp-fork", 0),
    ];
    rank_catalogue(&mut servers, OFFICIAL);
    let official: Vec<_> = servers.iter().filter(|s| s.official).collect();
    assert_eq!(official.len(), 1);
    assert_eq!(official[0].qualified_name, "com.notion/mcp");
}

#[test]
fn ranking_clears_an_upstream_official_claim_not_on_the_list() {
    let mut claimed = entry("io.example/unknown", 0);
    claimed.official = true;
    let mut servers = vec![claimed];
    rank_catalogue(&mut servers, OFFICIAL);
    assert!(!servers[0].official);
}

#[test]
fn ranking_puts_official_first_then_most_installed_and_keeps_ties_in_order() {
    let mut servers = vec![
        entry("io.a/first-tie", 5),
        entry("io.b/popular", 900),
        entry("app.linear/linear", 0),
        entry("io.c/second-tie", 5),
        entry("com.notion/mcp", 3),
    ];
    rank_catalogue(&mut servers, OFFICIAL);
    assert_eq!(
        names(&servers),
        [
            "com.notion/mcp",
            "app.linear/linear",
            "io.b/popular",
            "io.a/first-tie",
            "io.c/second-tie",
        ]
    );
}

#[test]
fn featured_rows_lead_and_are_not_repeated_from_the_page() {
    let merged = merge_featured(
        vec![entry("com.notion/mcp", 0), entry("app.linear/linear", 0)],
        vec![entry("io.x/other", 1), entry("com.notion/mcp", 0)],
    );
    assert_eq!(
        names(&merged),
        ["com.notion/mcp", "app.linear/linear", "io.x/other"]
    );
}

#[test]
fn a_featured_lookup_needs_a_dialable_endpoint() {
    let hosted = json!({ "server": {
        "qualified_name": "com.notion/mcp",
        "display_name": "Notion",
        "icon_url": "https://notion.example/icon.png",
        "connections": [{ "type": "http", "deployment_url": "https://mcp.notion.com/mcp" }],
    }});
    let local_only = json!({ "server": {
        "qualified_name": "io.example/stdio",
        "connections": [{ "type": "stdio" }],
    }});
    let featured = featured_entry(&hosted).expect("hosted entry is featured");
    assert_eq!(featured.display_name, "Notion");
    assert!(featured_entry(&local_only).is_none());
    assert!(featured_entry(&json!({})).is_none());
}

#[tokio::test]
async fn an_inline_icon_is_kept_without_fetching() {
    let fetched = RefCell::new(Vec::new());
    let fetch = |url: String| {
        fetched.borrow_mut().push(url);
        async { Some("data:image/png;base64,FETCHED".to_string()) }
    };
    let icon = inline_icon(Some("data:image/png;base64,AAAA".to_string()), &fetch).await;
    assert_eq!(icon.as_deref(), Some("data:image/png;base64,AAAA"));
    assert!(fetched.borrow().is_empty());
}

#[tokio::test]
async fn a_remote_icon_is_replaced_by_what_the_host_fetched() {
    let fetch = |_url: String| async { Some("data:image/png;base64,FETCHED".to_string()) };
    let icon = inline_icon(Some("https://icons.example/a.png".to_string()), &fetch).await;
    assert_eq!(icon.as_deref(), Some("data:image/png;base64,FETCHED"));
}

#[tokio::test]
async fn a_failed_or_non_web_icon_never_reaches_the_browser() {
    let fetched = RefCell::new(Vec::new());
    let fetch = |url: String| {
        fetched.borrow_mut().push(url);
        async { None }
    };
    assert_eq!(
        inline_icon(Some("https://icons.example/gone.png".to_string()), &fetch).await,
        None
    );
    assert_eq!(
        inline_icon(Some("javascript:alert(1)".to_string()), &fetch).await,
        None
    );
    assert_eq!(
        inline_icon(Some("file:///etc/passwd".to_string()), &fetch).await,
        None
    );
    assert_eq!(*fetched.borrow(), ["https://icons.example/gone.png"]);
}

#[tokio::test]
async fn every_row_gets_its_own_inlined_icon() {
    let mut servers = vec![entry("io.a/a", 0), entry("io.b/b", 0), entry("io.c/c", 0)];
    servers[0].icon_url = Some("https://icons.example/a.png".to_string());
    servers[1].icon_url = Some("data:image/png;base64,BBBB".to_string());
    let fetch = |url: String| async move { Some(format!("data:image/png;base64,{}", url.len())) };
    inline_icons(&mut servers, fetch).await;
    assert_eq!(
        servers[0].icon_url.as_deref(),
        Some("data:image/png;base64,27")
    );
    assert_eq!(
        servers[1].icon_url.as_deref(),
        Some("data:image/png;base64,BBBB")
    );
    assert_eq!(servers[2].icon_url, None);
}

#[test]
fn an_official_server_is_named_and_given_a_logo() {
    assert_eq!(
        brand_name("com.notion/mcp", Some("mcp".to_string())),
        "Notion"
    );
    assert_eq!(brand_name("com.paypal.mcp/mcp", None), "PayPal");
    assert_eq!(
        brand_logo("com.notion/mcp").as_deref(),
        Some("https://avatars.githubusercontent.com/u/4792552?s=128")
    );
    assert_eq!(brand_logo("ai.smithery/smithery-notion"), None);
}

#[cfg(feature = "mcp")]
#[test]
fn every_official_server_has_a_brand() {
    for name in tinymcp::registry::curation::OFFICIAL_SERVERS {
        assert!(brand_logo(name).is_some(), "{name} has no logo");
    }
}

#[test]
fn a_generic_upstream_name_falls_back_to_the_publisher() {
    assert_eq!(brand_name("com.acme/mcp", Some("mcp".to_string())), "Acme");
    assert_eq!(
        brand_name(
            "io.github.acme/remote-mcp-server",
            Some("remote mcp server".to_string())
        ),
        "Acme"
    );
    assert_eq!(brand_name("dev.acme.mcp/mcp", None), "Acme");
    assert_eq!(brand_name("@acme/mcp", Some("mcp".to_string())), "Acme");
}

#[test]
fn a_descriptive_upstream_name_is_kept() {
    assert_eq!(
        brand_name(
            "io.github.someone/weather-tools",
            Some("weather tools".to_string())
        ),
        "weather tools"
    );
    assert_eq!(
        brand_name("com.acme/mcp-github", Some("mcp github".to_string())),
        "mcp github"
    );
}

#[test]
fn a_search_row_carries_the_brand() {
    let page = catalogue_search(&json!({
        "servers": [{ "qualified_name": "com.stripe/mcp", "display_name": "mcp" }],
        "page": 1,
        "total_pages": 1
    }));
    let row = &page.servers[0];
    assert_eq!(row.display_name, "Stripe");
    assert_eq!(
        row.icon_url.as_deref(),
        Some("https://avatars.githubusercontent.com/u/856813?s=128")
    );
}

#[test]
fn a_directory_install_is_named_by_a_slug_of_its_shown_name() {
    assert_eq!(directory_server_name("Notion").as_deref(), Some("notion"));
    assert_eq!(
        directory_server_name("Atlassian Rovo MCP Server").as_deref(),
        Some("atlassian-rovo-mcp-server")
    );
    assert_eq!(
        directory_server_name("inference.sh").as_deref(),
        Some("inference-sh")
    );
    assert_eq!(directory_server_name("  — ").as_deref(), None);
}
