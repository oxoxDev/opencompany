//! The [`WorkflowRunner`] port: execute a company's workflow graph.
//!
//! A company's workflows are data-only
//! [`WorkflowFile`](crate::company::workflow_file::WorkflowFile) graphs. Running
//! one is dependency-inverted behind this port so the kernel and the HTTP layer
//! depend only on the trait: the concrete engine-backed implementation
//! (`crate::workflows::HarnessWorkflowRunner`, which drives the graph on the
//! embedded `tinyflows` engine with agent nodes on the harness pool) is compiled
//! only under `feature = "openhuman"`. The default build compiles this trait and
//! its result type but wires no implementation — a runtime with no runner leaves
//! the run route reporting "not wired", exactly like the other networked seams.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Result;
use crate::company::WorkflowFile;
use crate::ports::types::CompanyId;

/// The outcome of running one workflow to completion.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowRun {
    /// The final run state after the terminal node(s) completed. Its shape is
    /// the engine's `{ "run": …, "nodes": { "<id>": { "items": [ … ] } } }` map.
    pub output: Value,
    /// Node ids that paused the run awaiting human approval. Empty for a run
    /// that reached its terminal node(s) without gating.
    pub pending_approvals: Vec<String>,
    /// One row per attempt to route a reached `output` node's report to its
    /// configured destination (issue #170), in graph order.
    ///
    /// Empty for a graph whose `output` nodes name no destination — the
    /// pre-#170 shape — which is why it is `#[serde(default)]`: a `WorkflowRun`
    /// deserialized from an older payload still loads.
    ///
    /// A delivery failure is reported here rather than failing the run: the work
    /// the run did is still valid. An output node the run never reached
    /// contributes no row at all, so an absent row means "not reached", never
    /// "silently dropped".
    #[serde(default)]
    pub deliveries: Vec<DeliveryReport>,
}

/// What became of one attempt to deliver an `output` node's report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryStatus {
    /// The transport accepted the report.
    Sent,
    /// Deliberately not attempted — a policy precondition was unmet (a cold
    /// email recipient, no mailbox configured). Not an error; the report simply
    /// was not owed to that address under the current rules.
    Skipped,
    /// Refused by policy: the company does not grant what the destination needs.
    Denied,
    /// Attempted (or attemptable) and did not work — a transport error, an
    /// unwired channel, or a runtime with no delivery ports at all.
    Failed,
}

/// One attempt to route a reached `output` node's report somewhere.
///
/// On an on-demand run these rows ride the run response into the console's
/// run-result panel, so an operator can tell a delivered report from an
/// undelivered one without reading a log. A scheduled run is not persisted, so
/// its rows reach only the scheduler's log until issue #228 lands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryReport {
    /// The `output` node whose report this was.
    pub node: String,
    /// The destination kind as authored (`owner` / `email` / `channel`).
    pub kind: String,
    /// The address or channel actually addressed. For `owner` this is the
    /// server-resolved recipient, not something the graph named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// What became of the attempt.
    pub status: DeliveryStatus,
    /// An operator-readable reason, always populated — including on success, so
    /// a `sent` row still says *how* it was sent (which matters for `owner`,
    /// whose recipient the graph never named).
    pub detail: String,
}

/// Runs a company's workflow graph to completion.
///
/// `company` names the tenant whose roster the run's agent nodes execute on;
/// `workflow` is the parsed graph; `input` is the trigger payload (an arbitrary
/// JSON value seeded as the trigger node's item).
#[async_trait]
pub trait WorkflowRunner: Send + Sync {
    /// Runs `workflow` for `company` with the trigger `input`, returning the
    /// final state and any nodes left pending approval.
    async fn run(
        &self,
        company: &CompanyId,
        workflow: &WorkflowFile,
        input: Value,
    ) -> Result<WorkflowRun>;
}
