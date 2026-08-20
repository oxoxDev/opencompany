//! Projections of what the two upstream directories hand back, and of a live
//! connection's state.
//!
//! Split from the parent so that module keeps only the merge — the part every
//! build runs. Everything here is a pure `Value` / `&str` → DTO function with no
//! feature-gated type in its signature, which is what lets the parent's tests
//! exercise it in the ungated lane (issue #770).
//!
//! **Nothing upstream sends is forwarded blind.** Both of upstream's catalogue
//! DTOs end in a `#[serde(flatten)] extra` map that round-trips every key the
//! registries emit, so each projection below names the fields it emits and drops
//! the rest. That is what keeps an upstream payload change from silently
//! becoming an OpenCompany API change.

use serde::Serialize;
use serde_json::Value;

use crate::company::mcp::{McpHealth, McpStatus, stdio_install_refusal};

// ---------------------------------------------------------------------------
// Connection state → health — always compiled
// ---------------------------------------------------------------------------

/// Maps OpenHuman's connection status onto the health badge the console already
/// renders for List A, so one server does not get two vocabularies.
///
/// # Why `last_error` is dropped, not scrubbed
///
/// Upstream's `ConnStatus` carries a raw `last_error` from the transport. List A
/// runs its equivalent through [`scrub`](crate::harness::mcp_probe::scrub),
/// whose redaction pass needs **the credential values** to replace them with
/// `•••`. A registry install's credentials are its env values, which this
/// surface deliberately never loads — so there is no known-secret set to scrub
/// against, and the pass would degrade to stripping query strings and hoping.
/// An env value can be interpolated into a header or a URL by the server's own
/// config schema, so "hoping" is not a security posture. The stable `auth_hint`
/// code, which upstream documents as never carrying the raw challenge, plus a
/// fixed sentence per status, is the whole safe surface.
///
/// `checked_at_millis` is a parameter rather than a clock read so the mapping
/// stays a pure function.
pub(in crate::server::ops) fn health_from_status(
    status: &str,
    tool_count: u32,
    auth_hint: Option<&str>,
    checked_at_millis: u64,
) -> Option<McpHealth> {
    let (status, message) = match status {
        "connected" => (
            McpStatus::Ok,
            match tool_count {
                1 => "Connected — 1 tool available.".to_string(),
                n => format!("Connected — {n} tools available."),
            },
        ),
        "unauthorized" => (
            McpStatus::NeedsConfig,
            match auth_hint {
                Some("oauth_required") => {
                    "This server needs a browser sign-in — a pasted token will not work."
                }
                Some("token_rejected") => "The stored credential was rejected by this server.",
                _ => "This server needs a credential before it can be used.",
            }
            .to_string(),
        ),
        "error" => (
            McpStatus::Error,
            "The last connection attempt to this server failed. Reconnect to retry.".to_string(),
        ),
        "disabled" => (
            McpStatus::Unknown,
            "Disabled — this server is not connected and its tools are hidden.".to_string(),
        ),
        "connecting" => (McpStatus::Unknown, "Connecting…".to_string()),
        "disconnected" => (McpStatus::Unknown, "Not connected.".to_string()),
        // A status string this build does not know is not a badge we can render
        // honestly.
        _ => return None,
    };
    Some(McpHealth {
        status,
        message,
        tool_count,
        checked_at_millis,
        auth_hint: auth_hint.map(str::to_string),
    })
}

// ---------------------------------------------------------------------------
// Catalogue projections — always compiled
// ---------------------------------------------------------------------------

/// One directory listing, projected field by field out of upstream's summary.
///
/// Upstream's `SmitheryServerSummary` ends in `#[serde(flatten)] extra`, so it
/// round-trips every key the two registries emit. Naming what we forward is what
/// keeps an upstream payload change from becoming an OpenCompany API change.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(in crate::server::ops) struct CatalogueEntryDto {
    pub(in crate::server::ops) qualified_name: String,
    pub(in crate::server::ops) display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::server::ops) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::server::ops) icon_url: Option<String>,
    /// Which upstream directory this row came from (`smithery` / `mcp_official`).
    pub(in crate::server::ops) source: String,
    /// Upstream's canonical-first-party badge.
    pub(in crate::server::ops) official: bool,
    pub(in crate::server::ops) use_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::server::ops) website_url: Option<String>,
}

/// A page of directory results.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(in crate::server::ops) struct CatalogueSearchDto {
    pub(in crate::server::ops) servers: Vec<CatalogueEntryDto>,
    pub(in crate::server::ops) page: u32,
    pub(in crate::server::ops) total_pages: u32,
}

/// One directory entry in full, with the install decision already made.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(in crate::server::ops) struct CatalogueDetailDto {
    pub(in crate::server::ops) qualified_name: String,
    pub(in crate::server::ops) display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::server::ops) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::server::ops) icon_url: Option<String>,
    pub(in crate::server::ops) source: String,
    /// The hosted endpoint an install would dial. `None` ⇒ nothing dialable here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::server::ops) endpoint: Option<String>,
    /// The env keys the install dialog must collect, as upstream derived them
    /// from the connection the install will actually use.
    pub(in crate::server::ops) required_env_keys: Vec<String>,
    /// Whether `POST …/mcp/registry/install` would accept this entry.
    pub(in crate::server::ops) installable: bool,
    /// Why not, when it would not. Present iff `installable` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::server::ops) refusal: Option<String>,
}

/// The connection kinds that mean "dial an HTTPS endpoint". Mirrors upstream's
/// `ConnectionKind::transport_kind`, which is `pub(super)` to its own crate and
/// so cannot be called from here.
const HTTP_CONNECTION_KINDS: [&str; 3] = ["http", "http_remote", "sse"];

/// The endpoint an install of this entry would dial, or `None` when the entry
/// offers only a local subprocess.
///
/// Published connections win, matching upstream's picker, so the entry detail
/// names the URL the install will actually use rather than an unpublished one.
pub(in crate::server::ops) fn http_deployment_url(server: &Value) -> Option<String> {
    let connections = server.get("connections")?.as_array()?;
    let dialable = |conn: &&Value| -> Option<String> {
        let kind = conn.get("type").and_then(Value::as_str).unwrap_or_default();
        if !HTTP_CONNECTION_KINDS.contains(&kind) {
            return None;
        }
        let url = conn
            .get("deployment_url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty())?;
        Some(url.to_string())
    };
    connections
        .iter()
        .filter(|conn| conn.get("published").and_then(Value::as_bool) == Some(true))
        .find_map(|conn| dialable(&conn))
        .or_else(|| connections.iter().find_map(|conn| dialable(&conn)))
}

/// Projects `{ servers, page, total_pages }` as upstream's search returns it.
pub(in crate::server::ops) fn catalogue_search(raw: &Value) -> CatalogueSearchDto {
    let servers = raw
        .get("servers")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter_map(catalogue_entry).collect())
        .unwrap_or_default();
    CatalogueSearchDto {
        servers,
        page: raw.get("page").and_then(Value::as_u64).unwrap_or(1) as u32,
        total_pages: raw.get("total_pages").and_then(Value::as_u64).unwrap_or(0) as u32,
    }
}

/// One summary row. A row without a qualified name cannot be installed and is
/// dropped rather than rendered as an un-actionable card.
fn catalogue_entry(raw: &Value) -> Option<CatalogueEntryDto> {
    let qualified_name = text(raw, "qualified_name")?;
    Some(CatalogueEntryDto {
        display_name: text(raw, "display_name").unwrap_or_else(|| qualified_name.clone()),
        qualified_name,
        description: text(raw, "description"),
        icon_url: text(raw, "icon_url"),
        source: text(raw, "source").unwrap_or_default(),
        official: raw
            .get("official")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        use_count: raw.get("use_count").and_then(Value::as_u64).unwrap_or(0),
        website_url: text(raw, "website_url"),
    })
}

/// Projects `{ server: … }` as upstream's `registry_get` returns it, deciding
/// installability from the connections it lists.
pub(in crate::server::ops) fn catalogue_detail(raw: &Value) -> Option<CatalogueDetailDto> {
    let server = raw.get("server")?;
    let qualified_name = text(server, "qualified_name")?;
    let endpoint = http_deployment_url(server);
    let refusal = endpoint
        .is_none()
        .then(|| stdio_install_refusal(&qualified_name));
    Some(CatalogueDetailDto {
        display_name: text(server, "display_name").unwrap_or_else(|| qualified_name.clone()),
        qualified_name,
        description: text(server, "description"),
        icon_url: text(server, "icon_url"),
        source: text(server, "source").unwrap_or_default(),
        installable: endpoint.is_some(),
        endpoint,
        required_env_keys: server
            .get("required_env_keys")
            .and_then(Value::as_array)
            .map(|keys| {
                keys.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        refusal,
    })
}

/// A non-blank string field.
fn text(raw: &Value, key: &str) -> Option<String> {
    raw.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
