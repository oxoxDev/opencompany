//! Per-tool permission routes for MCP servers.
//!
//! Ungated, like the `/mcp/servers` management routes this sits beside: editing
//! what a tool is allowed to do is configuration, not a runtime capability, and
//! a build without the harness must still be able to express it. On such a
//! build no probe runs, so the inventory is empty and a read honestly returns
//! only the rows an operator already decided about.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::company::mcp::resolve_effective;
use crate::company::mcp_policy;
use crate::company::runtime::CompanyRuntime;
use crate::server::error::ApiError;
use crate::server::ops::mcp::{NamePath, manifest_servers};
use crate::server::ops::{AdminScopedCompany, ScopedCompany, scoped};

/// One tool's permission row as the console renders it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolPolicyRowDto {
    pub tool: String,
    /// The tier this row is grouped and defaulted under.
    pub effective_tier: crate::company::mcp_policy::ToolTier,
    /// What discovery suggested, when it reached this tool. May legitimately
    /// disagree with `effectiveTier` — an operator can reclassify a row, and the
    /// console must render that without looking broken.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_tier: Option<crate::company::mcp_policy::ToolTier>,
    pub mode: crate::company::mcp_policy::ApprovalMode,
    /// Whether an operator decided anything about this row, as opposed to it
    /// inheriting. Derived here; never stored.
    pub is_override: bool,
}

/// One tier's bulk default, and whether an operator actually wrote it.
///
/// `stored` is what stops the console presenting a nominal value as a live one.
/// An unstored tier carries [`default_mode_for`]'s nominal mode for reference,
/// but `resolve_policy` does not apply it to a merely-suggested tier — so a row
/// under that tier reads `NeedsApproval` while this says `AlwaysAllow`. Naming
/// which of the two an operator is looking at is the difference between
/// confirming a displayed value and unknowingly granting a bulk allow.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TierDefaultDto {
    pub mode: crate::company::mcp_policy::ApprovalMode,
    pub stored: bool,
}

/// A server's whole permission document as the console reads it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolPolicyDto {
    pub server: String,
    /// Every tier's bulk default — **total**, so the console never ships its own
    /// copy of the fallbacks and cannot drift from them.
    pub tier_defaults: std::collections::BTreeMap<String, TierDefaultDto>,
    pub tools: Vec<ToolPolicyRowDto>,
    /// When discovery last succeeded, if ever. `0` reads as never.
    pub discovered_at_millis: u64,
}

/// Renders a resolved policy document for one server.
pub fn tool_policy_dto(
    server: &str,
    policies: &crate::company::mcp_policy::McpToolPolicies,
    inventory: &crate::company::mcp_policy::McpToolInventory,
) -> ToolPolicyDto {
    use crate::company::mcp_policy::{
        ToolTier, default_mode_for, policy_tool_names, resolve_policy,
    };

    let tier_defaults = ToolTier::ALL
        .iter()
        .map(|tier| {
            let stored = policies.tier_defaults.get(tier).copied();
            let dto = TierDefaultDto {
                mode: stored.unwrap_or_else(|| default_mode_for(*tier)),
                stored: stored.is_some(),
            };
            (tier.as_str().to_string(), dto)
        })
        .collect();

    let tools = policy_tool_names(policies, inventory)
        .map(|tool| {
            let suggested = inventory.suggested(&tool);
            let resolved = resolve_policy(policies, &tool, suggested);
            ToolPolicyRowDto {
                tool,
                effective_tier: resolved.tier,
                suggested_tier: suggested,
                mode: resolved.mode,
                is_override: resolved.is_override,
            }
        })
        .collect();

    ToolPolicyDto {
        server: server.to_string(),
        tier_defaults,
        tools,
        discovered_at_millis: inventory.discovered_at_millis,
    }
}

/// The body of a tool-policy PUT.
///
/// **A partial merge, at two levels.** A field this body does not name is left
/// as stored; an entry naming no field at all is the reset for that tool. The
/// second level is the one that matters: `{tool, mode}` sets the mode and
/// leaves any tier reclassification intact, because replace-semantics would
/// make every press of a three-way control silently revert the operator's tier
/// decision.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutToolPolicy {
    /// A named tier is set; a tier named with `null` is cleared back to unset.
    /// Same shape as a `tools` entry naming neither field — naming a thing is
    /// how it is changed, and naming it as nothing is how it is undone.
    #[serde(default)]
    pub tier_defaults:
        Option<std::collections::HashMap<String, Option<crate::company::mcp_policy::ApprovalMode>>>,
    #[serde(default)]
    pub tools: Option<Vec<PutToolPolicyEntry>>,
}

/// One tool's patch. Absent is "leave alone"; an entry with neither field is
/// the reset.
///
/// The wire form and the stored form are different objects, deliberately.
/// `{tool}` is a meaningful instruction — reset this row — while the
/// `ToolPolicy` it produces, with both fields `None`, is the meaningless result
/// that gets pruned. One word covering both is how this contract reads as
/// self-contradictory.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutToolPolicyEntry {
    pub tool: String,
    #[serde(default)]
    pub tier: Option<crate::company::mcp_policy::ToolTier>,
    #[serde(default)]
    pub mode: Option<crate::company::mcp_policy::ApprovalMode>,
}

/// Applies a PUT body to a stored document, returning the merged result or the
/// operator-facing reason it cannot be applied.
pub fn apply_tool_policy_patch(
    mut stored: crate::company::mcp_policy::McpToolPolicies,
    patch: PutToolPolicy,
) -> Result<crate::company::mcp_policy::McpToolPolicies, String> {
    use crate::company::mcp_policy::ToolTier;

    if patch.tier_defaults.is_none() && patch.tools.is_none() {
        return Err(
            "name `tierDefaults`, `tools`, or both — a body naming neither changes nothing."
                .to_string(),
        );
    }

    if let Some(defaults) = patch.tier_defaults {
        for (tier, mode) in defaults {
            let parsed = ToolTier::ALL
                .iter()
                .find(|candidate| candidate.as_str() == tier.trim())
                .copied()
                .ok_or_else(|| format!("`{tier}` is not a tool tier."))?;
            match mode {
                Some(mode) => stored.tier_defaults.insert(parsed, mode),
                None => stored.tier_defaults.remove(&parsed),
            };
        }
    }

    for entry in patch.tools.unwrap_or_default() {
        let tool = entry.tool.trim();
        if tool.is_empty() {
            return Err("every entry in `tools` needs a `tool` name.".to_string());
        }
        match (entry.tier, entry.mode) {
            // Names no field: the reset.
            (None, None) => {
                stored.overrides.remove(tool);
            }
            (tier, mode) => {
                let row = stored.overrides.entry(tool.to_string()).or_default();
                if tier.is_some() {
                    row.tier = tier;
                }
                if mode.is_some() {
                    row.mode = mode;
                }
            }
        }
    }

    stored.prune();
    Ok(stored)
}

/// Builds the per-tool permission route fragment.
pub fn router() -> Router<AppState> {
    scoped(
        "/mcp/servers/{name}/tools/policy",
        get(read_policy).put(write_policy).delete(reset_policy),
    )
}

/// Resolves the named server, or the response explaining why it could not be.
async fn decl_for(
    runtime: &CompanyRuntime,
    name: &str,
) -> Result<crate::company::mcp::McpServerDecl, Box<Response>> {
    let manifest = manifest_servers(runtime)
        .await
        .map_err(|err| Box::new(err.into_response()))?;
    let decls = resolve_effective(
        runtime.id(),
        runtime.default_mcp_servers(),
        &manifest,
        runtime.secrets().as_ref(),
    )
    .await
    .map_err(|err| Box::new(ApiError(err).into_response()))?;
    decls
        .into_iter()
        .find(|d| d.name == name)
        .ok_or_else(|| Box::new(not_found(name)))
}

fn not_found(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": format!("no MCP server named `{name}`"),
            "code": "not_found",
        })),
    )
        .into_response()
}

/// The one repair a merge cannot perform: a document that will not parse has no
/// fields to merge into, so the reset is the only way back.
pub fn policy_unreadable(name: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": format!(
                "the stored tool permissions for `{name}` cannot be read. Clearing them restores \
                 the defaults; every tool parks for approval until you do."
            ),
            "code": "policy_unreadable",
        })),
    )
        .into_response()
}

/// Reads the stored document strictly, so an unreadable one is a `409` rather
/// than silently rendered as "no overrides".
///
/// The gate's own loader degrades instead. The two disagree on purpose: a
/// console that rendered the degraded document would show permissions nobody
/// chose, and an edit saved from that view would make them real.
async fn stored_strict(
    runtime: &CompanyRuntime,
    name: &str,
) -> Result<crate::company::mcp_policy::McpToolPolicies, Box<Response>> {
    mcp_policy::load_tool_policies_strict(
        runtime.id(),
        runtime.secrets().as_ref(),
        &mcp_policy::tool_policies_key(name),
    )
    .await
    .map_err(|_| Box::new(policy_unreadable(name)))
    .map(Option::unwrap_or_default)
}

async fn read_policy(company: ScopedCompany, Path(NamePath { name }): Path<NamePath>) -> Response {
    let runtime = company.runtime.as_ref();
    let name = name.trim().to_string();
    let decl = match decl_for(runtime, &name).await {
        Ok(decl) => decl,
        Err(response) => return *response,
    };
    let stored = match stored_strict(runtime, &name).await {
        Ok(stored) => stored,
        Err(response) => return *response,
    };
    let policies = mcp_policy::effective_policies(
        &decl.read_only_tools,
        mcp_policy::StoredPolicies::Stored(stored),
    );
    Json(tool_policy_dto(&name, &policies, &decl.tool_inventory)).into_response()
}

async fn write_policy(
    company: AdminScopedCompany,
    Path(NamePath { name }): Path<NamePath>,
    body: Option<Json<PutToolPolicy>>,
) -> Response {
    let runtime = company.runtime.as_ref();
    let name = name.trim().to_string();
    let decl = match decl_for(runtime, &name).await {
        Ok(decl) => decl,
        Err(response) => return *response,
    };
    let stored = match stored_strict(runtime, &name).await {
        Ok(stored) => stored,
        Err(response) => return *response,
    };

    let patch = match body {
        Some(Json(patch)) => patch,
        None => return bad_request("send a JSON body naming `tierDefaults`, `tools`, or both."),
    };
    let merged = match apply_tool_policy_patch(stored, patch) {
        Ok(merged) => merged,
        Err(reason) => return bad_request(&reason),
    };

    if let Err(err) = mcp_policy::save_tool_policies(
        runtime.id(),
        runtime.secrets().as_ref(),
        &mcp_policy::tool_policies_key(&name),
        &merged,
    )
    .await
    {
        return ApiError(err).into_response();
    }

    // The echoed document is the resolved one, not the patch: every field the
    // console renders is host-resolved, and predicting them there would be a
    // second implementation of a rule this crate owns.
    let policies = mcp_policy::effective_policies(
        &decl.read_only_tools,
        mcp_policy::StoredPolicies::Stored(merged),
    );
    Json(tool_policy_dto(&name, &policies, &decl.tool_inventory)).into_response()
}

async fn reset_policy(
    company: AdminScopedCompany,
    Path(NamePath { name }): Path<NamePath>,
) -> Response {
    let runtime = company.runtime.as_ref();
    let name = name.trim().to_string();
    let decl = match decl_for(runtime, &name).await {
        Ok(decl) => decl,
        Err(response) => return *response,
    };
    // Does not read the stored document first: this is the repair for one that
    // cannot be read, so requiring it to parse would lock the operator out of
    // the only way back.
    if let Err(err) = mcp_policy::clear_tool_policies(
        runtime.id(),
        runtime.secrets().as_ref(),
        &mcp_policy::tool_policies_key(&name),
    )
    .await
    {
        return ApiError(err).into_response();
    }
    let policies =
        mcp_policy::effective_policies(&decl.read_only_tools, mcp_policy::StoredPolicies::Absent);
    Json(tool_policy_dto(&name, &policies, &decl.tool_inventory)).into_response()
}

fn bad_request(reason: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": reason, "code": "invalid_request" })),
    )
        .into_response()
}

#[cfg(test)]
#[path = "mcp_tool_policy_tests.rs"]
mod tests;
