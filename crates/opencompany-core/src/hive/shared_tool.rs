//! A shared tool handed out as an owned belt entry.
//!
//! This crate builds a teammate's belt **once**, when the roster is built, and
//! shares it as `Arc<dyn Tool>` ([`share_belt`](super::tools::share_belt)).
//! `AgentSpec::tools` asks for an owned `Vec<Box<dyn Tool>>` and asks for it
//! **once per turn**, because the session behind a spec is rebuilt from config
//! every turn and a `Box<dyn Tool>` cannot survive in between.
//!
//! [`SharedTool`] bridges the two: a thin `Box` around the `Arc`, minted per
//! turn, delegating every call to the one shared instance. No tool is rebuilt,
//! no state is duplicated, and a tool holding a connection or a cache keeps
//! holding exactly one.
//!
//! # Every method, deliberately
//!
//! `tinytools::Tool` has twenty-six methods and all but four carry a default.
//! A wrapper that implemented only the four required ones would compile, and
//! would silently answer the other twenty-two for the tool it wraps — a
//! `PermissionLevel::Write` tool would read as the default, an admission gate
//! would see the wrong `external_effect`, and a hidden tool would advertise
//! itself. Nothing in the type system catches that, so every method is
//! forwarded here explicitly and `shared_tool_tests` pins the ones that change
//! behaviour rather than presentation.

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tinytools::{
    PermissionLevel, Tool, ToolCallOptions, ToolCategory, ToolInjectedArgument, ToolPolicy,
    ToolResult, ToolScope, ToolSpec, ToolTimeout,
};

/// One shared tool, owned for the length of a turn.
pub struct SharedTool(Arc<dyn Tool>);

// `dyn Tool` is not `Debug`, and the name is the only part of a tool worth
// printing anyway — a belt in a log line should read as its names.
impl std::fmt::Debug for SharedTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SharedTool").field(&self.0.name()).finish()
    }
}

impl SharedTool {
    /// Wraps one shared tool.
    #[must_use]
    pub fn new(inner: Arc<dyn Tool>) -> Self {
        Self(inner)
    }
}

/// The shared belt as an owned one, for a single turn.
///
/// Call it per turn from an `AgentSpec::tools` factory; the tools themselves
/// are not rebuilt.
#[must_use]
pub fn owned_belt(shared: &[Arc<dyn Tool>]) -> Vec<Box<dyn Tool>> {
    shared
        .iter()
        .map(|tool| Box::new(SharedTool::new(Arc::clone(tool))) as Box<dyn Tool>)
        .collect()
}

#[async_trait]
impl Tool for SharedTool {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn description(&self) -> &str {
        self.0.description()
    }

    fn parameters_schema(&self) -> Value {
        self.0.parameters_schema()
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        self.0.execute(args).await
    }

    async fn execute_with_options(
        &self,
        args: Value,
        options: ToolCallOptions,
    ) -> anyhow::Result<ToolResult> {
        self.0.execute_with_options(args, options).await
    }

    async fn execute_with_context(
        &self,
        args: Value,
        options: ToolCallOptions,
        context: Option<&dyn tinytools::ToolRunContext>,
    ) -> anyhow::Result<ToolResult> {
        self.0.execute_with_context(args, options, context).await
    }

    fn policy(&self) -> ToolPolicy {
        self.0.policy()
    }

    fn injected_arguments(&self) -> Vec<ToolInjectedArgument> {
        self.0.injected_arguments()
    }

    fn supports_markdown(&self) -> bool {
        self.0.supports_markdown()
    }

    fn permission_level(&self) -> PermissionLevel {
        self.0.permission_level()
    }

    fn permission_level_with_args(&self, args: &Value) -> PermissionLevel {
        self.0.permission_level_with_args(args)
    }

    fn scope(&self) -> ToolScope {
        self.0.scope()
    }

    fn category(&self) -> ToolCategory {
        self.0.category()
    }

    fn exposure(&self) -> tinytools::ToolExposure {
        self.0.exposure()
    }

    fn family(&self) -> Option<&str> {
        self.0.family()
    }

    fn is_concurrency_safe(&self, args: &Value) -> bool {
        self.0.is_concurrency_safe(args)
    }

    fn external_effect(&self) -> bool {
        self.0.external_effect()
    }

    fn external_effect_with_args(&self, args: &Value) -> bool {
        self.0.external_effect_with_args(args)
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        self.0.max_result_size_chars()
    }

    fn timeout_policy(&self, args: &Value) -> ToolTimeout {
        self.0.timeout_policy(args)
    }

    fn host_extension(&self) -> Option<&(dyn Any + Send + Sync)> {
        self.0.host_extension()
    }

    fn host_call_extension(&self, args: &Value) -> Option<Box<dyn Any + Send + Sync>> {
        self.0.host_call_extension(args)
    }

    fn spec(&self) -> ToolSpec {
        self.0.spec()
    }

    fn display_label(&self, args: &Value) -> Option<String> {
        self.0.display_label(args)
    }

    fn display_detail(&self, args: &Value) -> Option<String> {
        self.0.display_detail(args)
    }

    fn return_direct(&self) -> bool {
        self.0.return_direct()
    }
}

#[cfg(test)]
#[path = "shared_tool_tests.rs"]
mod tests;
