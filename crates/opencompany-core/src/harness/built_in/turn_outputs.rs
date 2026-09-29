//! Turn-scoped collection of addressable files produced by agent tools.
//!
//! Workspace tools are built once per cached agent while a chat reply varies
//! per turn. A company-wide vector therefore cannot answer which reply wrote a
//! node: concurrent turns can interleave, and even sequential turns in one
//! cycle would let a forgotten drain cross-attribute the first turn's output
//! to the second. The task-local scope below gives every turn its own bucket on
//! one cheap shared handle.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::ports::types::{ChatOutput, ChatOutputKind};
use crate::runtime::approval_display;

tokio::task_local! {
    /// The collector bucket owned by the agent turn executing in this task.
    static CURRENT_OUTPUT_SCOPE: u64;
}

/// A shared handle whose writes are partitioned by the current turn.
#[derive(Clone, Default)]
pub struct TurnOutputCollector {
    inner: Arc<Mutex<BTreeMap<u64, Vec<ChatOutput>>>>,
    next_scope: Arc<AtomicU64>,
}

impl TurnOutputCollector {
    /// Opens an isolated output bucket for one turn.
    #[must_use = "the claim removes its bucket on drop"]
    pub fn claim(&self) -> TurnOutputClaim {
        // Zero means "unscoped" in the overflow fallback below, so real
        // claims start at one.
        let scope = self
            .next_scope
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.clear(scope);
        TurnOutputClaim {
            collector: self.clone(),
            scope,
        }
    }

    /// Registers one successful workspace create/write against the active
    /// turn. Calls outside a claim are ignored: background and console writes
    /// must never leak onto the next chat reply.
    pub fn workspace_node(&self, node_id: impl Into<String>, title: &str) {
        self.push(ChatOutput {
            kind: ChatOutputKind::WorkspaceNode,
            target_id: node_id.into(),
            title: redacted_text("path", title),
            task_id: None,
            version: None,
        });
    }

    /// Removes a workspace node from the active turn after it was deleted.
    pub fn remove_workspace_node(&self, node_id: &str) {
        let Ok(scope) = CURRENT_OUTPUT_SCOPE.try_with(|scope| *scope) else {
            return;
        };
        let mut buckets = self.inner.lock().expect("turn output collector");
        if let Some(outputs) = buckets.get_mut(&scope) {
            outputs.retain(|output| {
                output.kind != ChatOutputKind::WorkspaceNode || output.target_id != node_id
            });
        }
    }

    /// Registers one artifact revision recorded for the active chat turn.
    /// Calls from dispatched cards and other non-chat contexts are unscoped
    /// and intentionally ignored.
    pub fn artifact(
        &self,
        artifact_id: impl Into<String>,
        task_id: impl Into<String>,
        version: u32,
        title: &str,
    ) {
        self.push(ChatOutput {
            kind: ChatOutputKind::Artifact,
            target_id: artifact_id.into(),
            title: redacted_text("title", title),
            task_id: Some(task_id.into()),
            version: Some(version),
        });
    }

    fn push(&self, output: ChatOutput) {
        let Ok(scope) = CURRENT_OUTPUT_SCOPE.try_with(|scope| *scope) else {
            return;
        };
        let mut buckets = self.inner.lock().expect("turn output collector");
        let outputs = buckets.entry(scope).or_default();
        // Target identity is kind + id. Replacing in place retains stable
        // button order while keeping the final label/state from a repeated
        // write in the same turn.
        if let Some(existing) = outputs
            .iter_mut()
            .find(|item| item.kind == output.kind && item.target_id == output.target_id)
        {
            *existing = output;
        } else {
            outputs.push(output);
        }
    }

    fn take(&self, scope: u64) -> Vec<ChatOutput> {
        self.inner
            .lock()
            .expect("turn output collector")
            .remove(&scope)
            .unwrap_or_default()
    }

    fn clear(&self, scope: u64) {
        self.inner
            .lock()
            .expect("turn output collector")
            .remove(&scope);
    }
}

/// One turn's live claim on the shared collector.
pub struct TurnOutputClaim {
    collector: TurnOutputCollector,
    scope: u64,
}

impl TurnOutputClaim {
    /// Runs `future` with this claim as the ambient tool-write destination.
    pub async fn scoped<F, T>(&self, future: F) -> T
    where
        F: Future<Output = T>,
    {
        CURRENT_OUTPUT_SCOPE.scope(self.scope, future).await
    }

    /// Drains only this turn's outputs.
    pub fn drain(&self) -> Vec<ChatOutput> {
        self.collector.take(self.scope)
    }
}

impl Drop for TurnOutputClaim {
    fn drop(&mut self) {
        self.collector.clear(self.scope);
    }
}

/// Whether the current task runs inside a claimed turn scope.
///
/// [`TurnOutputCollector::workspace_node`] already drops an unscoped call, so
/// this exists for the writer that must decide *before* doing work whose only
/// purpose is the output — promoting an agent's file into the workspace, whose
/// node would otherwise be minted for a reply that will never show it.
pub fn in_claimed_turn() -> bool {
    CURRENT_OUTPUT_SCOPE.try_with(|_| ()).is_ok()
}

/// Applies the same redaction and string bound used by turn-step details.
pub fn redacted_text(key: &str, text: &str) -> String {
    approval_display::redact(&serde_json::json!({ (key): text }))
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or(approval_display::UNRENDERABLE)
        .to_string()
}

#[cfg(test)]
#[path = "turn_outputs_tests.rs"]
mod tests;
