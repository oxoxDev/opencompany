//! Per-install grant scoping for the vendored MCP registry bridge tools.
//!
//! The tools in `oh::mcp::registry::tools` address an install by a `server_id`
//! argument supplied at call time, so being wired onto an agent's belt is not
//! by itself a reach decision. [`OcMcpRegistryScopedTool`] resolves that
//! argument against the agent's effective grants before delegating.

use std::any::Any;

use async_trait::async_trait;
use serde_json::Value;

use tinytools::{
    PermissionLevel, Tool, ToolCallOptions, ToolCategory, ToolExposure, ToolResult, ToolRunContext,
    ToolScope, ToolTimeout,
};

use crate::policy::consequence::MCP_REGISTRY_SERVER_KEY;
use crate::runtime::tools::grants_cover_registry_server;

/// Scopes a directory-installed MCP server tool to the agent's own installs.
///
/// The vendored registry tools address an install by a `server_id` argument and
/// carry no per-agent scoping of their own: any agent holding an `mcp_registry`
/// grant could name any install the company had connected. This decorator wraps
/// one of them, checks the named install against the agent's effective grants
/// through [`grants_cover_registry_server`], and only then delegates.
///
/// Wraps rather than replaces because the vendored tools live in
/// `vendor/openhuman` — the name, schema and every other trait answer are
/// forwarded so the decorated tool is indistinguishable from the one it wraps.
pub struct OcMcpRegistryScopedTool {
    inner: Box<dyn Tool>,
    grants: Vec<String>,
}

impl OcMcpRegistryScopedTool {
    /// Wraps `inner`, gating it on the agent's *effective* grants.
    pub fn new(inner: Box<dyn Tool>, grants: Vec<String>) -> Self {
        Self { inner, grants }
    }

    /// The refusal for a call naming an install this agent's grants do not
    /// cover, naming the grant that would allow it.
    fn denied(&self, server_id: &str) -> ToolResult {
        ToolResult::error(format!(
            "This agent is not granted access to MCP server '{server_id}'. Add the tool grant \
             'mcp_registry.{server_id}' to reach it. Do not retry — surface this to the operator."
        ))
    }

    /// The refusal for a call whose `server_id` is missing or unusable.
    ///
    /// Deliberately worded apart from [`Self::denied`]: a malformed argument is
    /// a different problem from an ungranted one, and the agent should not read
    /// it as a grant it needs to ask for.
    fn unaddressed(&self) -> ToolResult {
        ToolResult::error(format!(
            "'{}' requires a non-empty '{}' string argument naming the installed MCP server.",
            self.inner.name(),
            MCP_REGISTRY_SERVER_KEY
        ))
    }

    /// Fails closed on anything but a grant-covered install.
    ///
    /// Reads the argument with trim-only semantics, matching the vendored
    /// tool's own extraction, so the string authorised here is byte-identical
    /// to the one the inner tool will dispatch on.
    fn authorize(&self, args: &Value) -> Option<ToolResult> {
        let server_id = args
            .get(MCP_REGISTRY_SERVER_KEY)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty());
        match server_id {
            None => Some(self.unaddressed()),
            Some(id) if grants_cover_registry_server(&self.grants, id) => None,
            Some(id) => Some(self.denied(id)),
        }
    }
}

#[async_trait]
impl Tool for OcMcpRegistryScopedTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> Value {
        self.inner.parameters_schema()
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        if let Some(refusal) = self.authorize(&args) {
            return Ok(refusal);
        }
        self.inner.execute(args).await
    }

    // The trait chains the three entry points by default, so each gates
    // independently rather than relying on a caller taking the one that does.
    async fn execute_with_options(
        &self,
        args: Value,
        options: ToolCallOptions,
    ) -> anyhow::Result<ToolResult> {
        if let Some(refusal) = self.authorize(&args) {
            return Ok(refusal);
        }
        self.inner.execute_with_options(args, options).await
    }

    async fn execute_with_context(
        &self,
        args: Value,
        options: ToolCallOptions,
        context: Option<&dyn ToolRunContext>,
    ) -> anyhow::Result<ToolResult> {
        if let Some(refusal) = self.authorize(&args) {
            return Ok(refusal);
        }
        self.inner
            .execute_with_context(args, options, context)
            .await
    }

    fn supports_markdown(&self) -> bool {
        self.inner.supports_markdown()
    }

    fn permission_level(&self) -> PermissionLevel {
        self.inner.permission_level()
    }

    fn permission_level_with_args(&self, args: &Value) -> PermissionLevel {
        self.inner.permission_level_with_args(args)
    }

    fn scope(&self) -> ToolScope {
        self.inner.scope()
    }

    fn category(&self) -> ToolCategory {
        self.inner.category()
    }

    fn exposure(&self) -> ToolExposure {
        self.inner.exposure()
    }

    fn is_concurrency_safe(&self, args: &Value) -> bool {
        self.inner.is_concurrency_safe(args)
    }

    fn external_effect(&self) -> bool {
        self.inner.external_effect()
    }

    fn external_effect_with_args(&self, args: &Value) -> bool {
        self.inner.external_effect_with_args(args)
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        self.inner.max_result_size_chars()
    }

    fn timeout_policy(&self, args: &Value) -> ToolTimeout {
        self.inner.timeout_policy(args)
    }

    fn host_extension(&self) -> Option<&(dyn Any + Send + Sync)> {
        self.inner.host_extension()
    }

    fn host_call_extension(&self, args: &Value) -> Option<Box<dyn Any + Send + Sync>> {
        self.inner.host_call_extension(args)
    }

    fn display_label(&self, args: &Value) -> Option<String> {
        self.inner.display_label(args)
    }

    fn display_detail(&self, args: &Value) -> Option<String> {
        self.inner.display_detail(args)
    }
}

#[cfg(test)]
#[path = "registry_scoped_tests.rs"]
mod tests;
