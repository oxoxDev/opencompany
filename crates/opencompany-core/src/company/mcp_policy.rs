//! Per-tool approval policy for MCP servers: the tier vocabulary, the operator's
//! stored overrides, and the resolution ladder the approval gate reads.
//!
//! Two layers, deliberately separate:
//!
//! 1. A **suggested** tier, computed from a tool's own name and description by
//!    [`suggest_tool_tier`]. Non-authoritative — it is a starting point a
//!    console renders, never something the gate trusts on its own.
//! 2. The **operator's** decision, persisted as [`McpToolPolicies`] and resolved
//!    by [`resolve_policy`]. This is what the gate enforces.
//!
//! A server's own `readOnlyHint`/`destructiveHint` annotations are not a source
//! here. They are self-reported by whoever runs the remote server, and a
//! directory install can come from an unvetted publisher, so keying an approval
//! *bypass* off them would put the trust boundary in the wrong place.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A remote tool's risk tier.
///
/// `Interactive` is the conservative middle: it is what an unclassified tool
/// resolves to, so a tool nobody has looked at parks for approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolTier {
    Interactive,
    ReadOnly,
    WriteDelete,
}

impl ToolTier {
    /// Every tier, in the order a console lists them.
    pub const ALL: [ToolTier; 3] = [
        ToolTier::ReadOnly,
        ToolTier::Interactive,
        ToolTier::WriteDelete,
    ];
}

/// What happens when a tool is called, whatever tier it sits in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Runs without parking for a human.
    AlwaysAllow,
    /// Parks under the standing approval rules, as every bridge call does today.
    NeedsApproval,
    /// Refused before the call reaches the transport. Distinct from
    /// [`Self::NeedsApproval`] in that no approver can wave it through.
    Blocked,
}

/// One tool's stored policy. Both fields are absent-by-default: an absent field
/// inherits, and an entry with neither is indistinguishable from no entry at
/// all, which is what makes "reset this row" expressible on the wire.
///
/// `tier` is optional rather than mandatory so that storage never freezes a
/// *suggestion*. A row the operator only changed the mode on keeps tracking an
/// improved heuristic instead of pinning whatever the heuristic said the day it
/// was written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<ToolTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ApprovalMode>,
}

impl ToolPolicy {
    /// Whether this entry carries any decision. An empty entry is pruned on
    /// write rather than stored, so a reset leaves no residue.
    pub fn is_empty(&self) -> bool {
        self.tier.is_none() && self.mode.is_none()
    }
}

/// One server's whole tool policy: per-tier bulk defaults plus per-tool
/// overrides that win over them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolPolicies {
    #[serde(default)]
    pub tier_defaults: HashMap<ToolTier, ApprovalMode>,
    #[serde(default)]
    pub overrides: HashMap<String, ToolPolicy>,
}

impl McpToolPolicies {
    /// Drops entries that decide nothing, so an empty override is never stored.
    pub fn prune(&mut self) {
        self.overrides.retain(|_, policy| !policy.is_empty());
    }
}

/// The [`SecretStore`](crate::ports::SecretStore) key holding a declared server's [`McpToolPolicies`].
pub fn tool_policies_key(name: &str) -> String {
    format!("mcp/{name}/tool_policies")
}

/// The [`SecretStore`](crate::ports::SecretStore) key holding a directory install's [`McpToolPolicies`],
/// keyed by the install's `server_id` rather than a slug.
pub fn registry_tool_policies_key(server_id: &str) -> String {
    format!("mcp_registry/{server_id}/tool_policies")
}

/// The mode a tier carries when neither an override nor a stored tier default
/// says otherwise.
pub fn default_mode_for(tier: ToolTier) -> ApprovalMode {
    match tier {
        ToolTier::ReadOnly => ApprovalMode::AlwaysAllow,
        ToolTier::Interactive | ToolTier::WriteDelete => ApprovalMode::NeedsApproval,
    }
}

#[cfg(test)]
#[path = "mcp_policy_tests.rs"]
mod tests;
