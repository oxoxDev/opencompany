//! Promotion of a native file write into the company workspace.
//!
//! Two file-writing surfaces reach an agent. The `workspace_*` tools mint a
//! node in the shared tree and register a [`ChatOutput`], so the console can
//! open what was written. The OpenHuman-native `file_write`/`edit` pair writes
//! into the agent's own sandbox directory, which nothing outside the process
//! addresses — an agent that used them handed back a path no reader could
//! follow.
//!
//! [`PromotingFileTool`] closes that gap from the outside: it wraps the two
//! writers, and after a successful call copies the file into the workspace
//! under `agents/<agent id>/`, then registers the node on the current turn.
//! A decorator rather than a change inside the vendored tools, matching the
//! store-layer decorators in [`crate::runtime`]: the belt is a
//! `Vec<Box<dyn Tool>>`, so the whole feature stays in this crate.
//!
//! # Per-agent folders are the point, not a detail
//!
//! Several seats routinely write the same filename in their own sandboxes.
//! Promoting into one flat namespace would turn that into silent overwrite —
//! this fix becoming its own data-loss path. The per-agent prefix gives each
//! seat a distinct destination, which is also the honest description: they are
//! different documents.
//!
//! # Every failure here is non-fatal
//!
//! The agent asked to write a file and the file was written. Promotion is an
//! addition on top of that, so an unreadable path, a binary payload, an
//! oversized body or a refusing store all leave the inner result untouched and
//! log a warning. Promotion also runs only inside a claimed turn scope, so a
//! background or console-driven write cannot land a chip on the next reply.
//!
//! [`ChatOutput`]: crate::ports::types::ChatOutput

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tinytools::{
    PermissionLevel, Tool, ToolCallOptions, ToolCategory, ToolResult, ToolScope, ToolSpec,
    ToolTimeout,
};

use crate::company::workspace_names::kebab_name;
use crate::company::workspace_paths::{render_path, split_logical_path};
use crate::company::workspace_scaffold::ensure_agent_folder;
use crate::harness::turn_outputs::{TurnOutputCollector, in_claimed_turn};
use crate::harness::workspace_tools::MAX_WRITE_BYTES;
use crate::ports::types::CompanyId;
use crate::ports::workspace::{NodeKind, WorkspaceNode, WorkspaceOrigin, WorkspaceStore};

/// The tools whose successful call leaves a new file body on disk.
///
/// `file_read`, `list`, `grep` and `glob` produce nothing, and wrapping them
/// would buy a tree read per read-only call.
const PROMOTED_TOOLS: [&str; 2] = ["file_write", "edit"];

/// Everything the wrapped writers need to land a copy in the workspace.
pub(crate) struct WritePromotion {
    store: Arc<dyn WorkspaceStore>,
    company: CompanyId,
    agent_id: String,
    outputs: TurnOutputCollector,
    sandbox: PathBuf,
}

impl WritePromotion {
    pub(crate) fn new(
        store: Arc<dyn WorkspaceStore>,
        company: CompanyId,
        agent_id: String,
        outputs: TurnOutputCollector,
        sandbox: PathBuf,
    ) -> Self {
        Self {
            store,
            company,
            agent_id,
            outputs,
            sandbox,
        }
    }

    /// Wraps the writers in `tools` and returns the belt with its order intact.
    pub(crate) fn wrap_writers(self: &Arc<Self>, tools: Vec<Box<dyn Tool>>) -> Vec<Box<dyn Tool>> {
        tools
            .into_iter()
            .map(|inner| {
                if PROMOTED_TOOLS.contains(&inner.name()) {
                    Box::new(PromotingFileTool {
                        inner,
                        promotion: self.clone(),
                    }) as Box<dyn Tool>
                } else {
                    inner
                }
            })
            .collect()
    }

    async fn promote(&self, args: &Value) {
        if !in_claimed_turn() {
            return;
        }
        let Some(requested) = args.get("path").and_then(Value::as_str) else {
            return;
        };
        let Some(relative) = self.sandbox_relative(requested) else {
            return;
        };
        let Ok(segments) = split_logical_path(&relative) else {
            return;
        };
        let Some(body) = read_promotable(&self.sandbox.join(&relative)).await else {
            return;
        };
        let names: Vec<String> = segments.iter().map(|segment| kebab_name(segment)).collect();
        let Some((file_name, folders)) = names.split_last() else {
            return;
        };

        match self.land(file_name, folders, &body).await {
            Ok((node_id, path)) => self.outputs.workspace_node(node_id, &path),
            Err(error) => tracing::warn!(
                company = %self.company,
                agent = %self.agent_id,
                path = %relative,
                %error,
                "[file-promotion] could not copy the written file into the workspace"
            ),
        }
    }

    /// Creates or overwrites `agents/<agent id>/<folders>/<file_name>`,
    /// answering the node's id and its rendered workspace path.
    async fn land(
        &self,
        file_name: &str,
        folders: &[String],
        body: &str,
    ) -> crate::error::Result<(String, String)> {
        let origin = WorkspaceOrigin::Agent {
            id: self.agent_id.clone(),
        };
        let store = self.store.as_ref();
        let mut parent = ensure_agent_folder(store, &self.company, &self.agent_id).await?;
        for folder in folders {
            parent = store
                .adopt_or_create_folder(&self.company, Some(&parent), folder, origin.clone())
                .await?
                .into_node()
                .id;
        }

        let nodes = store.tree(&self.company).await?;
        let by_id: HashMap<&str, &WorkspaceNode> =
            nodes.iter().map(|node| (node.id.as_str(), node)).collect();
        let parent_path = by_id
            .get(parent.as_str())
            .and_then(|node| render_path(node, &by_id))
            .unwrap_or_else(|| parent.clone());

        let taken: Vec<&WorkspaceNode> = nodes
            .iter()
            .filter(|node| {
                node.parent_id.as_deref() == Some(parent.as_str())
                    && node.name.eq_ignore_ascii_case(file_name)
            })
            .collect();

        let node = match taken.as_slice() {
            [one] if one.kind == NodeKind::File && !one.is_binary() => {
                store.write(&self.company, &one.id, body, origin).await?
            }
            [] => {
                let node = WorkspaceNode {
                    id: crate::ports::generate_id(),
                    name: file_name.to_string(),
                    kind: NodeKind::File,
                    parent_id: Some(parent),
                    updated_at_millis: crate::ports::now_millis(),
                    created_by: origin.clone(),
                    updated_by: origin,
                    mime: None,
                    size: None,
                    sha256: None,
                    adopted: false,
                };
                store.create(&self.company, &node, Some(body)).await?;
                node
            }
            _ => {
                return Err(crate::error::OpenCompanyError::Conflict(format!(
                    "`{file_name}` under `{parent_path}` does not name one note this write can \
                     replace"
                )));
            }
        };

        let path = format!("{parent_path}/{name}", name = node.name);
        Ok((node.id, path))
    }

    /// The sandbox-relative form of what the agent asked to write, or `None`
    /// when the path does not name a file inside the sandbox.
    fn sandbox_relative(&self, requested: &str) -> Option<String> {
        let path = Path::new(requested);
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.sandbox).ok()?
        } else {
            path
        };
        relative.to_str().map(str::to_string)
    }
}

/// Reads a just-written file when it is small enough and prose.
///
/// Binary payloads and bodies over the workspace write ceiling are skipped
/// rather than truncated: a promoted note must stay one an agent can read back
/// and revise through `workspace_read`/`workspace_write`.
async fn read_promotable(path: &Path) -> Option<String> {
    let bytes = tokio::fs::read(path).await.ok()?;
    if bytes.len() > MAX_WRITE_BYTES {
        return None;
    }
    let body = String::from_utf8(bytes).ok()?;
    (!body.contains('\0')).then_some(body)
}

/// A file writer that also lands what it wrote in the company workspace.
pub(crate) struct PromotingFileTool {
    inner: Box<dyn Tool>,
    promotion: Arc<WritePromotion>,
}

impl PromotingFileTool {
    async fn promote_after(
        &self,
        args: &Value,
        result: anyhow::Result<ToolResult>,
    ) -> anyhow::Result<ToolResult> {
        if matches!(&result, Ok(out) if !out.is_error) {
            self.promotion.promote(args).await;
        }
        result
    }
}

#[async_trait]
impl Tool for PromotingFileTool {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn description(&self) -> &str {
        self.inner.description()
    }
    fn parameters_schema(&self) -> Value {
        self.inner.parameters_schema()
    }
    fn supports_markdown(&self) -> bool {
        self.inner.supports_markdown()
    }
    fn spec(&self) -> ToolSpec {
        self.inner.spec()
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
    fn is_concurrency_safe(&self, args: &Value) -> bool {
        self.inner.is_concurrency_safe(args)
    }
    fn external_effect(&self) -> bool {
        self.inner.external_effect()
    }
    fn external_effect_with_args(&self, args: &Value) -> bool {
        self.inner.external_effect_with_args(args)
    }
    fn host_extension(&self) -> Option<&(dyn std::any::Any + Send + Sync)> {
        self.inner.host_extension()
    }
    fn host_call_extension(&self, args: &Value) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        self.inner.host_call_extension(args)
    }
    fn max_result_size_chars(&self) -> Option<usize> {
        self.inner.max_result_size_chars()
    }
    fn timeout_policy(&self, args: &Value) -> ToolTimeout {
        self.inner.timeout_policy(args)
    }
    fn display_label(&self, args: &Value) -> Option<String> {
        self.inner.display_label(args)
    }
    fn display_detail(&self, args: &Value) -> Option<String> {
        self.inner.display_detail(args)
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let result = self.inner.execute(args.clone()).await;
        self.promote_after(&args, result).await
    }

    async fn execute_with_options(
        &self,
        args: Value,
        options: ToolCallOptions,
    ) -> anyhow::Result<ToolResult> {
        let result = self.inner.execute_with_options(args.clone(), options).await;
        self.promote_after(&args, result).await
    }

    async fn execute_with_context(
        &self,
        args: Value,
        options: ToolCallOptions,
        context: Option<&dyn tinytools::ToolRunContext>,
    ) -> anyhow::Result<ToolResult> {
        let result = self
            .inner
            .execute_with_context(args.clone(), options, context)
            .await;
        self.promote_after(&args, result).await
    }
}

#[cfg(test)]
#[path = "file_tool_outputs_tests.rs"]
mod tests;
