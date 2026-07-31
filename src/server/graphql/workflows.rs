//! Workflow reads: `Company.workflows` summaries (from the manifest's enabled
//! list) and `Company.workflow(id)` graphs.
//!
//! Graph bodies come from the union of the company's two sources — the seed
//! files at `{company}/workflows/<id>.toml` and the runtime-authored bodies on
//! the [`CompanyRecord`](crate::ports::types::CompanyRecord) overlay — via
//! [`load_workflow_union`]. A hosted tenant has no source directory, so all of
//! its graphs are overlay bodies; resolving only the seed side used to render
//! them as bare ids with no graph (issue #168).

use std::sync::Arc;

use async_graphql::{Context, ID, SimpleObject};

use crate::company::runtime::CompanyRuntime;
use crate::company::{WorkflowFile, load_workflow_union};
use crate::ports::types::OverlayWorkflow;

/// A one-line workflow summary for the workflows list.
#[derive(SimpleObject)]
#[graphql(name = "WorkflowSummary")]
pub struct WorkflowSummaryGql {
    /// The workflow id.
    pub id: ID,
    /// The workflow display name.
    pub name: String,
    /// Whether the workflow is enabled in the manifest.
    pub enabled: bool,
}

/// A full workflow graph.
#[derive(SimpleObject)]
#[graphql(name = "Workflow")]
pub struct WorkflowGql {
    /// The workflow id.
    pub id: ID,
    /// The workflow display name.
    pub name: String,
    /// The graph nodes.
    pub nodes: Vec<WorkflowNodeGql>,
    /// The graph edges.
    pub edges: Vec<WorkflowEdgeGql>,
}

/// One node in a workflow graph.
#[derive(SimpleObject)]
#[graphql(name = "WorkflowNode")]
pub struct WorkflowNodeGql {
    /// The node id.
    pub id: ID,
    /// The node kind (`trigger`, `agent`, `toolCall`, ...).
    pub kind: String,
    /// The node display name.
    pub name: String,
    /// An optional one-line summary.
    pub summary: Option<String>,
    /// Free-form, kind-specific node config (P1), exposed as a JSON scalar so
    /// `Company.workflow(id)` does not drop model data.
    pub config: Option<async_graphql::Json<serde_json::Value>>,
    /// Per-node error policy: `stop` / `continue` / `route`.
    pub on_error: Option<String>,
    /// Per-node retry policy (attempts + backoff), as a JSON scalar. Keys are
    /// camelCase (`maxAttempts` / `backoffMs`) to match the REST read shape;
    /// see [`RetryGql`].
    pub retry: Option<async_graphql::Json<RetryGql>>,
    /// Whether the node pauses awaiting operator approval before it runs.
    pub requires_approval: Option<bool>,
}

/// The camelCase retry shape the console reads back over GraphQL, mirroring the
/// REST `WorkflowRetryOut` (`maxAttempts` / `backoffMs`). The model/TOML type
/// [`crate::company::WorkflowRetryDef`] stays snake_case; without this mirror
/// the GraphQL JSON scalar would leak snake_case keys and diverge from REST.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryGql {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backoff_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backoff: Option<String>,
}

impl From<crate::company::WorkflowRetryDef> for RetryGql {
    fn from(r: crate::company::WorkflowRetryDef) -> Self {
        Self {
            max_attempts: r.max_attempts,
            backoff_ms: r.backoff_ms,
            backoff: r.backoff,
        }
    }
}

/// One directed edge in a workflow graph.
#[derive(SimpleObject)]
#[graphql(name = "WorkflowEdge")]
pub struct WorkflowEdgeGql {
    /// The source node id.
    pub from: ID,
    /// The target node id.
    pub to: ID,
    /// An optional edge label.
    pub label: Option<String>,
}

impl From<WorkflowFile> for WorkflowGql {
    fn from(file: WorkflowFile) -> Self {
        Self {
            id: ID(file.id),
            name: file.name,
            nodes: file
                .nodes
                .into_iter()
                .map(|node| WorkflowNodeGql {
                    id: ID(node.id),
                    kind: node.kind.as_str().to_string(),
                    name: node.name,
                    summary: node.summary,
                    config: node.config.map(async_graphql::Json),
                    on_error: node.on_error,
                    retry: node.retry.map(|r| async_graphql::Json(RetryGql::from(r))),
                    requires_approval: node.requires_approval,
                })
                .collect(),
            edges: file
                .edges
                .into_iter()
                .map(|edge| WorkflowEdgeGql {
                    from: ID(edge.from),
                    to: ID(edge.to),
                    label: edge.label,
                })
                .collect(),
        }
    }
}

/// Best-effort load of one workflow graph from the seed ∪ overlay union.
/// Yields `None` when neither source has the id, or the body it finds is
/// invalid — a resolver never fails the whole query over one bad graph.
fn load_one(
    runtime: &Arc<CompanyRuntime>,
    overlays: &[OverlayWorkflow],
    id: &str,
) -> Option<WorkflowFile> {
    load_workflow_union(runtime.source_dir(), overlays, id)
        .ok()
        .flatten()
}

/// The enabled workflow ids from the company manifest.
async fn enabled_ids(runtime: &Arc<CompanyRuntime>) -> async_graphql::Result<Vec<String>> {
    Ok(runtime.enabled_workflow_ids().await?)
}

/// The company's runtime-authored graph bodies, read once per resolve. A
/// company with no persisted record contributes none.
async fn overlays(runtime: &Arc<CompanyRuntime>) -> async_graphql::Result<Vec<OverlayWorkflow>> {
    Ok(runtime
        .store()
        .load(runtime.id())
        .await?
        .map(|record| record.overlay_workflows)
        .unwrap_or_default())
}

/// Resolves `Company.workflows`.
pub(crate) async fn resolve_summaries(
    _ctx: &Context<'_>,
    runtime: &Arc<CompanyRuntime>,
) -> async_graphql::Result<Vec<WorkflowSummaryGql>> {
    let overlays = overlays(runtime).await?;
    let ids = enabled_ids(runtime).await?;
    Ok(ids
        .into_iter()
        .map(|id| {
            let name = load_one(runtime, &overlays, &id)
                .map(|file| file.name)
                .unwrap_or_else(|| id.clone());
            WorkflowSummaryGql {
                id: ID(id),
                name,
                enabled: true,
            }
        })
        .collect())
}

/// Resolves `Company.workflow(id)`, returning null when the graph is unavailable.
pub(crate) async fn resolve_one(
    _ctx: &Context<'_>,
    runtime: &Arc<CompanyRuntime>,
    id: &str,
) -> async_graphql::Result<Option<WorkflowGql>> {
    let overlays = overlays(runtime).await?;
    Ok(load_one(runtime, &overlays, id).map(WorkflowGql::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::company::parse_workflow;
    use serde_json::json;

    #[test]
    fn node_conversion_preserves_p1_fields_and_camelcases_retry() {
        let file = parse_workflow(
            r#"
            id = "wf"
            name = "Workflow"
            [[node]]
            id = "start"
            kind = "trigger"
            name = "Start"
            on_error = "continue"
            requires_approval = true
            [node.config]
            message = "hello"
            [node.retry]
            max_attempts = 3
            backoff_ms = 250
            backoff = "exponential"
            "#,
        )
        .expect("workflow parses");

        let gql = WorkflowGql::from(file);
        let node = &gql.nodes[0];
        assert_eq!(node.config.as_ref().unwrap().0, json!({"message": "hello"}));
        assert_eq!(node.on_error.as_deref(), Some("continue"));
        assert_eq!(node.requires_approval, Some(true));
        assert_eq!(
            serde_json::to_value(&node.retry.as_ref().unwrap().0).unwrap(),
            json!({
                "maxAttempts": 3,
                "backoffMs": 250,
                "backoff": "exponential"
            })
        );
    }
}
