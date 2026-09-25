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

use super::mcp::McpServerDecl;

use crate::Result;
use crate::error::OpenCompanyError;
use crate::ports::SecretStore;
use crate::ports::types::{CompanyId, SecretValue};

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
    /// The stable wire string, matching the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolTier::Interactive => "interactive",
            ToolTier::ReadOnly => "read_only",
            ToolTier::WriteDelete => "write_delete",
        }
    }

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

/// Verb prefixes that suggest a tool only reads.
///
/// Deliberately short. A suggested `ReadOnly` is the one tier whose nominal
/// default runs without a human, so every entry added here widens what a guess
/// about a name can reach. `query` and `fetch` are absent for that reason — a
/// "query" can carry a `DELETE`.
const READ_VERBS: &[&str] = &["get", "list", "read", "search"];

/// Verb prefixes that suggest a tool destroys something. Escalating into this
/// tier never loosens anything: it and `Interactive` share a nominal default.
const DESTRUCTIVE_VERBS: &[&str] = &["delete", "remove", "drop", "destroy", "purge"];

/// The leading verb of a tool name, lowercased. Handles the three shapes remote
/// servers use: `search_pages`, `search-pages`/`notion.search`, and
/// `searchPages`.
fn leading_verb(name: &str) -> String {
    let head = name
        .trim()
        .split(['_', '-', '.', ':', '/'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("");
    let boundary = head
        .char_indices()
        .skip(1)
        .find(|(_, c)| c.is_ascii_uppercase())
        .map(|(i, _)| i)
        .unwrap_or(head.len());
    head[..boundary].to_ascii_lowercase()
}

/// Suggests a tier for a remote tool from its name, and its description only
/// where that cannot make the suggestion more permissive.
///
/// **Non-authoritative.** This answers what a console should pre-select and
/// group under, not what the gate enforces — see [`resolve_policy`], where a
/// suggestion alone never grants [`ApprovalMode::AlwaysAllow`].
///
/// The name decides. A description is consulted only to escalate an otherwise
/// unclassified tool into [`ToolTier::WriteDelete`]; it can never pull one down
/// to [`ToolTier::ReadOnly`], because prose is written by whoever runs the
/// remote server and that direction is the one that loosens a boundary.
pub fn suggest_tool_tier(name: &str, description: Option<&str>) -> ToolTier {
    let verb = leading_verb(name);
    if DESTRUCTIVE_VERBS.contains(&verb.as_str()) {
        return ToolTier::WriteDelete;
    }
    if READ_VERBS.contains(&verb.as_str()) {
        return ToolTier::ReadOnly;
    }
    let described = description.unwrap_or("").to_ascii_lowercase();
    if DESTRUCTIVE_VERBS.iter().any(|v| {
        described
            .split_whitespace()
            .any(|word| word.trim_matches(|c: char| !c.is_ascii_alphabetic()) == *v)
    }) {
        return ToolTier::WriteDelete;
    }
    ToolTier::Interactive
}

/// One tool's resolved policy: what the gate enforces, plus what a console
/// needs to render the row honestly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedPolicy {
    /// The tier the row is grouped and defaulted under.
    pub tier: ToolTier,
    /// The enforced mode.
    pub mode: ApprovalMode,
    /// Whether an operator decided anything about this row, as opposed to it
    /// inheriting. Derived, never stored — storing it lets the two disagree,
    /// and the gate is the consumer of that disagreement.
    pub is_override: bool,
}

/// Resolves one tool's policy from the stored document and a suggested tier.
///
/// Two ladders. The tier: an operator's reclassification, else the suggestion,
/// else the conservative middle. The mode: this tool's own override, else the
/// tier's stored bulk default, else a hardcoded fallback.
///
/// The hardcoded fallback is where this departs from a plain reading of the
/// tier table. It grants [`ApprovalMode::AlwaysAllow`] only when an operator
/// **confirmed** the tier; a tier that is merely *suggested* falls to
/// [`ApprovalMode::NeedsApproval`] no matter what the suggestion says. A name
/// heuristic is a guess, and letting a guess skip the approval gate would mean
/// the day the heuristic gains a verb, calls that used to park stop parking.
/// Bulk allow is still one action away — it is `tier_defaults`, which an
/// operator writes deliberately.
pub fn resolve_policy(
    policies: &McpToolPolicies,
    tool: &str,
    suggested: Option<ToolTier>,
) -> ResolvedPolicy {
    let stored = policies.overrides.get(tool).copied().unwrap_or_default();
    let tier = stored.tier.or(suggested).unwrap_or(ToolTier::Interactive);
    let fallback = if stored.tier.is_some() {
        default_mode_for(tier)
    } else {
        ApprovalMode::NeedsApproval
    };
    let mode = stored
        .mode
        .or_else(|| policies.tier_defaults.get(&tier).copied())
        .unwrap_or(fallback);
    ResolvedPolicy {
        tier,
        mode,
        is_override: !stored.is_empty(),
    }
}

/// Restates a server's legacy `read_only_tools` list as a policy document.
///
/// Both fields are written concretely. A tier alone would resolve through the
/// hardcoded fallback and a mode alone would leave the row grouped under the
/// middle tier, and either way the restatement would stop being one.
pub fn migrate_read_only_tools(read_only_tools: &[String]) -> McpToolPolicies {
    let mut policies = McpToolPolicies::default();
    for tool in read_only_tools {
        let tool = tool.trim();
        if tool.is_empty() {
            continue;
        }
        policies.overrides.insert(
            tool.to_string(),
            ToolPolicy {
                tier: Some(ToolTier::ReadOnly),
                mode: Some(ApprovalMode::AlwaysAllow),
            },
        );
    }
    policies
}

/// What a read of a server's policy key found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredPolicies {
    /// Nothing written yet — the server's declaration is the whole policy.
    Absent,
    /// A document the operator's edits produced.
    Stored(McpToolPolicies),
    /// Present but unreadable. Distinct from [`Self::Absent`] because the two
    /// must resolve differently: an unreadable document may have carried a
    /// refusal, so falling back to the declaration would restore an allow the
    /// operator had taken away.
    Unreadable,
}

/// Layers what a read found over the baseline a server's `read_only_tools`
/// declares.
///
/// Field-level, not entry-level: a stored entry that names only a mode keeps
/// the baseline's tier. The declaration stays a live input rather than a
/// one-shot seed, so the first operator edit to any row cannot silently retire
/// the manifest's remaining read-only declarations.
///
/// An unreadable document drops the baseline too, and resolves to all-park.
pub fn effective_policies(read_only_tools: &[String], stored: StoredPolicies) -> McpToolPolicies {
    let stored = match stored {
        StoredPolicies::Unreadable => return McpToolPolicies::default(),
        StoredPolicies::Absent => McpToolPolicies::default(),
        StoredPolicies::Stored(policies) => policies,
    };
    let mut out = migrate_read_only_tools(read_only_tools);
    out.tier_defaults.extend(stored.tier_defaults);
    for (tool, policy) in stored.overrides {
        let entry = out.overrides.entry(tool).or_default();
        entry.tier = policy.tier.or(entry.tier);
        entry.mode = policy.mode.or(entry.mode);
    }
    out
}

fn parse_policies(raw: &str) -> std::result::Result<McpToolPolicies, String> {
    if raw.trim().is_empty() {
        return Ok(McpToolPolicies::default());
    }
    serde_json::from_str(raw).map_err(|e| e.to_string())
}

/// Reads a policy document for the gate, reporting an unreadable one as such
/// rather than as an error.
///
/// Surfacing an error instead would travel up through MCP resolution, which a
/// caller already treats as "this company gets no MCP servers at all", so one
/// unreadable key would strip every server from every agent behind one warn.
pub async fn load_tool_policies(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    key: &str,
) -> StoredPolicies {
    let raw = match secrets.get(company, key).await {
        Ok(Some(SecretValue(raw))) => raw,
        Ok(None) => return StoredPolicies::Absent,
        Err(err) => {
            tracing::warn!(
                company = %company,
                key = %key,
                error = %err,
                "reading MCP tool policy failed; every tool on this server parks for approval"
            );
            return StoredPolicies::Unreadable;
        }
    };
    match parse_policies(&raw) {
        Ok(policies) => StoredPolicies::Stored(policies),
        Err(err) => {
            tracing::warn!(
                company = %company,
                key = %key,
                error = %err,
                "MCP tool policy is not valid JSON; every tool on this server parks for approval"
            );
            StoredPolicies::Unreadable
        }
    }
}

/// Reads a policy document, surfacing an unreadable one.
///
/// The face an operator-facing read uses: a console that rendered the degraded
/// empty document would show permissions nobody chose, and an edit saved from
/// that view would make them real.
pub async fn load_tool_policies_strict(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    key: &str,
) -> Result<Option<McpToolPolicies>> {
    let Some(SecretValue(raw)) = secrets.get(company, key).await? else {
        return Ok(None);
    };
    parse_policies(&raw).map(Some).map_err(|e| {
        OpenCompanyError::Store(format!("mcp tool policy at `{key}` is not valid JSON: {e}"))
    })
}

/// Persists a policy document, dropping entries that decide nothing.
pub async fn save_tool_policies(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    key: &str,
    policies: &McpToolPolicies,
) -> Result<()> {
    let mut policies = policies.clone();
    policies.prune();
    let raw = serde_json::to_string(&policies)
        .map_err(|e| OpenCompanyError::Store(format!("serializing mcp tool policy: {e}")))?;
    secrets.set(company, key, SecretValue(raw)).await
}

/// Resets a server's policy to the empty document.
///
/// Writes rather than deletes, because the store has no delete: an empty
/// document is both the repair for an unparseable one and, resolving to
/// all-park, the safe thing to leave behind.
pub async fn clear_tool_policies(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    key: &str,
) -> Result<()> {
    save_tool_policies(company, secrets, key, &McpToolPolicies::default()).await
}
/// Flattens a company's effective MCP servers into the `(server, tool)` set the
/// approval gate lets run without parking, resolved through each server's tool
/// policy.
///
/// The successor to the test-only `mcp_read_set`, which reads the flat
/// declaration directly. Both produce the same shape, so one can be diffed
/// against the other over a fixture.
///
/// Enumerates the union of the policy document's own entries and the tools
/// discovery last saw. Both halves are needed: an entry names a tool the
/// operator decided about, which discovery may not have reached; the inventory
/// names the tools a per-tier bulk default is *for*, which have no entry by
/// definition. A disabled server contributes nothing: it hands out no tool, so
/// a call through it could not have been made.
///
/// Never consults [`suggest_tool_tier`] directly: a suggestion reaches the
/// resolution only through the stored inventory, and only as a tier. It can
/// still reach [`ApprovalMode::AlwaysAllow`] from there, through a stored
/// `tier_defaults` entry — see [`resolve_policy`], where that is the deliberate
/// bulk-allow an operator writes. What a suggestion cannot do is reach it on
/// its own: with no bulk default stored, a merely-suggested tier resolves to
/// [`ApprovalMode::NeedsApproval`].
pub fn mcp_allow_set(servers: &[McpServerDecl]) -> crate::policy::McpReadSet {
    crate::policy::McpReadSet::from_pairs(servers.iter().filter(|s| s.enabled).flat_map(|server| {
        policy_tool_names(&server.tool_policies, &server.tool_inventory)
            .filter(|tool| {
                resolve_policy(
                    &server.tool_policies,
                    tool,
                    server.tool_inventory.suggested(tool),
                )
                .mode
                    == ApprovalMode::AlwaysAllow
            })
            .map(move |tool| (server.name.clone(), tool))
    }))
}

/// Every tool name a policy decision can be stated about: the ones an operator
/// already decided, plus the ones discovery found.
pub fn policy_tool_names(
    policies: &McpToolPolicies,
    inventory: &McpToolInventory,
) -> impl Iterator<Item = String> {
    let mut names: std::collections::BTreeSet<String> =
        policies.overrides.keys().cloned().collect();
    names.extend(inventory.tools.keys().cloned());
    names.into_iter()
}

/// Whether one server's stored policy refuses `tool` outright.
///
/// The single definition of "blocked": both the declared-server set and the
/// registry decorator resolve through this, so a tool refused on one path is
/// refused identically on the other.
pub fn blocks_tool(policies: &McpToolPolicies, inventory: &McpToolInventory, tool: &str) -> bool {
    resolve_policy(policies, tool, inventory.suggested(tool)).mode == ApprovalMode::Blocked
}

/// Every granted server's resolved policy, addressed by server name.
///
/// The call-time face of the same documents [`mcp_allow_set`] flattens. The
/// allow set answers a question the approval gate asks *before* a call; this
/// answers one the bridge tool asks at the point it would dial.
#[derive(Clone, Debug, Default)]
pub struct McpToolPolicySet {
    by_server: HashMap<String, (McpToolPolicies, McpToolInventory)>,
}

impl McpToolPolicySet {
    /// Collects the enabled servers' policies. A disabled server hands out no
    /// tool, so a call through it could not have been made.
    ///
    /// Takes an iterator so a caller can narrow the set first — the harness
    /// hands it only the servers an agent's grants reach, the way it already
    /// narrows the credential substrings it collects.
    pub fn from_declarations<'a>(servers: impl IntoIterator<Item = &'a McpServerDecl>) -> Self {
        Self {
            by_server: servers
                .into_iter()
                .filter(|server| server.enabled)
                .map(|server| {
                    (
                        server.name.clone(),
                        (server.tool_policies.clone(), server.tool_inventory.clone()),
                    )
                })
                .collect(),
        }
    }

    /// Whether this call is refused outright.
    ///
    /// A server with no policy answers `false`: blocking is an explicit
    /// operator act, and the absence of one is the absence of that act, not a
    /// reason to refuse. Nothing is granted by answering `false` either — the
    /// approval gate has already decided separately whether the call parks.
    pub fn is_blocked(&self, server: &str, tool: &str) -> bool {
        self.by_server
            .get(server)
            .is_some_and(|(policies, inventory)| blocks_tool(policies, inventory, tool))
    }
}

/// Every tool this server refuses outright, sorted.
///
/// The attachment-time face of [`blocks_tool`]: a company agent reaches a
/// declared server through OpenHuman's native `mcp_call_tool`, and the only
/// lever on that path is the attached server's deny list.
///
/// Reads the inventory as well as the overrides, so a tier default blocks the
/// tools discovery found rather than only the ones an operator has already
/// named one by one.
pub fn blocked_tool_names(policies: &McpToolPolicies, inventory: &McpToolInventory) -> Vec<String> {
    let mut names: Vec<String> = policy_tool_names(policies, inventory)
        .filter(|tool| blocks_tool(policies, inventory, tool))
        .collect();
    names.sort();
    names
}

/// The agent-facing refusal for a blocked call. One function so the two bridge
/// families cannot come to word it differently.
pub fn blocked_refusal(server: &str, tool: &str) -> String {
    format!(
        "The tool '{tool}' on MCP server '{server}' is blocked by this company's tool \
         permissions, so the call was not made. This is not an approval that can be granted \
         in the moment — do not retry, and surface it to the operator if the work needs it."
    )
}

/// A server's discovered tools and the tier each one is suggested under.
///
/// Persisted because the suggestion is only obtainable while the server
/// answers. Without it a transient outage would empty the console's row set and
/// drop every tool a stored tier default reaches — the policy would appear to
/// change because the network hiccuped.
///
/// Holds names and tiers only. A tool's description is what the suggestion was
/// computed *from*, is marked untrusted by the transport that carries it, and
/// is not needed again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInventory {
    /// Discovered tool name to its suggested tier. Ordered so the document is
    /// byte-stable across writes and a re-probe that discovers nothing new is a
    /// no-op diff.
    #[serde(default)]
    pub tools: std::collections::BTreeMap<String, ToolTier>,
    /// When discovery last succeeded, in unix milliseconds.
    #[serde(default)]
    pub discovered_at_millis: u64,
}

impl McpToolInventory {
    /// The tier discovery suggested for `tool`, if it was seen.
    pub fn suggested(&self, tool: &str) -> Option<ToolTier> {
        self.tools.get(tool).copied()
    }
}

/// The [`SecretStore`](crate::ports::SecretStore) key holding a declared
/// server's [`McpToolInventory`].
pub fn tool_inventory_key(name: &str) -> String {
    format!("mcp/{name}/tool_inventory")
}

/// The [`SecretStore`](crate::ports::SecretStore) key holding a directory
/// install's [`McpToolInventory`].
pub fn registry_tool_inventory_key(server_id: &str) -> String {
    format!("mcp_registry/{server_id}/tool_inventory")
}

/// Builds an inventory from a discovery pass, suggesting a tier for each tool.
pub fn inventory_from_discovery<'a>(
    tools: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    discovered_at_millis: u64,
) -> McpToolInventory {
    McpToolInventory {
        tools: tools
            .into_iter()
            .filter_map(|(name, description)| {
                let name = name.trim();
                (!name.is_empty()).then(|| (name.to_string(), suggest_tool_tier(name, description)))
            })
            .collect(),
        discovered_at_millis,
    }
}

/// Reads a server's tool inventory, degrading an unreadable one to the empty
/// inventory.
///
/// Fail-closed: with no suggestion every tool resolves under the conservative
/// middle tier and parks. Never surfaces an error, for the same reason
/// [`load_tool_policies`] does not.
pub async fn load_tool_inventory(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    key: &str,
) -> McpToolInventory {
    let raw = match secrets.get(company, key).await {
        Ok(Some(SecretValue(raw))) => raw,
        Ok(None) => return McpToolInventory::default(),
        Err(err) => {
            tracing::warn!(
                company = %company,
                key = %key,
                error = %err,
                "reading MCP tool inventory failed; this server suggests no tiers"
            );
            return McpToolInventory::default();
        }
    };
    if raw.trim().is_empty() {
        return McpToolInventory::default();
    }
    serde_json::from_str(&raw).unwrap_or_else(|err| {
        tracing::warn!(
            company = %company,
            key = %key,
            error = %err,
            "MCP tool inventory is not valid JSON; this server suggests no tiers"
        );
        McpToolInventory::default()
    })
}

/// Persists a server's tool inventory.
pub async fn save_tool_inventory(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    key: &str,
    inventory: &McpToolInventory,
) -> Result<()> {
    let raw = serde_json::to_string(inventory)
        .map_err(|e| OpenCompanyError::Store(format!("serializing mcp tool inventory: {e}")))?;
    secrets.set(company, key, SecretValue(raw)).await
}

#[cfg(test)]
#[path = "mcp_policy_tests.rs"]
mod tests;
