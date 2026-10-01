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
