//! Enumeration of the company's directory installs, for the agent side.
//!
//! The registry bridge tools address an install by a `server_id` the agent has
//! to have got from somewhere. OpenHuman's own `mcp_registry_installed_list`
//! answers with the install record serialised whole, which carries the dial
//! string (`transport`, `command`, `args`) and the opaque `config` blob the
//! install was created with — an HTTP-remote URL can carry a query-parameter
//! credential, and a stdio install's arguments can carry a flag one.
//! [`OcMcpRegistryInstalledListTool`] answers the same question from an
//! allowlist instead.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use tinytools::{PermissionLevel, Tool, ToolResult};

use crate::runtime::tools::grants_cover_registry_server;
use openhuman_core::mcp::registry::types::{InstalledServer, Transport};

use crate::harness::mcp::McpRuntime;

/// One install as an agent may see it.
///
/// An **allowlist**, never a projection of the record with fields removed: a
/// field added upstream must be opted in here rather than arriving on the
/// agent's side because nobody noticed it. `transport` is named by kind alone —
/// the dial string is exactly what must not travel.
fn row(install: &InstalledServer) -> Value {
    json!({
        "server_id": install.server_id,
        "qualified_name": install.qualified_name,
        "display_name": install.display_name,
        "description": install.description,
        "enabled": install.enabled,
        "transport": match install.transport {
            Transport::Stdio => "stdio",
            Transport::HttpRemote { .. } => "http_remote",
            _ => "unknown",
        },
        "last_connected_at": install.last_connected_at,
    })
}

/// The installs this agent's grants reach, in the shape [`row`] allows.
///
/// Scoped by the same predicate the call path uses, so enumeration cannot be
/// the way around it: an agent holding `mcp_registry.<one-id>` learns about that
/// install and no other.
pub fn installed_rows(installs: &[InstalledServer], grants: &[String]) -> Vec<Value> {
    installs
        .iter()
        .filter(|install| grants_cover_registry_server(grants, &install.server_id))
        .map(row)
        .collect()
}

/// The markdown rendering of the same rows.
fn markdown(rows: &[Value]) -> String {
    if rows.is_empty() {
        return "# Installed MCP servers\n\nYou can reach no installed MCP servers.".to_string();
    }
    let mut out = String::from("# Installed MCP servers\n");
    for row in rows {
        let name = row["display_name"].as_str().unwrap_or_default();
        let id = row["server_id"].as_str().unwrap_or_default();
        let state = if row["enabled"].as_bool().unwrap_or(false) {
            "enabled"
        } else {
            "disabled"
        };
        out.push_str(&format!("\n- **{name}** (`{id}`) — {state}"));
        if let Some(description) = row["description"].as_str() {
            out.push_str(&format!("\n  {description}"));
        }
    }
    out
}

/// Lists the directory installs this agent may call.
///
/// Keeps OpenHuman's tool name so an agent prompt naming it is unchanged; the
/// answer is this one because the upstream tool's is not one an agent may hold.
pub struct OcMcpRegistryInstalledListTool {
    runtime: Arc<McpRuntime>,
    grants: Vec<String>,
}

impl OcMcpRegistryInstalledListTool {
    /// Wraps the company's registry store, scoped to `grants`.
    pub fn new(runtime: Arc<McpRuntime>, grants: Vec<String>) -> Self {
        Self { runtime, grants }
    }
}

#[async_trait]
impl Tool for OcMcpRegistryInstalledListTool {
    fn name(&self) -> &str {
        "mcp_registry_installed_list"
    }

    fn description(&self) -> &str {
        "List the MCP servers installed for this company that you can reach. Use this to find a \
         server_id before listing or calling its tools."
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    fn supports_markdown(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _args: &Value) -> bool {
        true
    }

    async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
        let installs = match self.runtime.list() {
            Ok(installs) => installs,
            Err(error) => {
                return Ok(ToolResult::error(format!(
                    "The installed MCP server list could not be read: {error}"
                )));
            }
        };
        let rows = installed_rows(&installs, &self.grants);
        let md = markdown(&rows);
        Ok(ToolResult::success_with_markdown(
            json!({ "installed": rows }),
            md,
        ))
    }
}

#[cfg(test)]
#[path = "registry_list_tests.rs"]
mod tests;
