//! Run a company's workflows on the **tinyflows** engine (issue #29, epic #26).
//!
//! A company's workflow graphs live on disk as
//! [`WorkflowFile`](crate::company::workflow_file::WorkflowFile)s — a data-only
//! node/edge model whose accepted node kinds are
//! [`WORKFLOW_NODE_KINDS`](crate::company::workflow_file::WORKFLOW_NODE_KINDS)
//! (the authoring contract; see `docs/spec/runtime/workflow-vocabulary.md`).
//! This module runs one directly on the embedded [`tinyflows`] engine, with
//! **agent** nodes routed to the company's
//! [`HarnessPool`](crate::harness::HarnessPool) so a step inherits the roster's
//! persona / model / memory / approval policy / metering (never a second pool).
//!
//! Compiled only under `feature = "openhuman"`. tinyflows is host-agnostic and
//! `Config`-free by design, so nothing here boots an OpenHuman global `Config`,
//! registry, or backend-proxied model — the shared architecture rule for this
//! epic. The default build links none of it.

/// Issue #782: end-to-end proof that an upstream node's output reaches a
/// downstream agent node's turn (an `agent -> agent` pipeline passes data).
#[cfg(test)]
mod agent_upstream_input_tests;
/// Issue #899 (Stage 1): end-to-end proof that approving a call gated inside an
/// agent node's own tool loop AUTO-CONTINUES the blocked run — one continuation,
/// after the last decision, and none for a wholly refused block.
/// Issues #881 / #880: end-to-end proof that a node whose deliverable was
/// parked for approval reports `blocked`, stops its branch instead of handing
/// its apology downstream, and that the run says what it parked.
#[cfg(test)]
#[path = "blocked_node_pipeline_tests.rs"]
mod blocked_node_pipeline_tests;
#[cfg(test)]
#[path = "blocked_node_pure_tests.rs"]
mod blocked_node_pure_tests;
/// Issue #661 (M5): end-to-end proof that a workflow node can open and re-own a
/// board card, and that everything it may not do stays refused.
#[cfg(test)]
mod board_turn_tests;
pub mod caps;
pub mod checkpoint_store;
pub mod delivery;
/// Issue #460: the company's `ApprovalPolicy` decides which `tool_call` nodes
/// stop for an operator, before the run reaches them.
pub mod gate;
/// Issue #460: end-to-end proof that a `tool_call` node the company's policy
/// stops does not execute, and leaves a decidable card.
#[cfg(test)]
mod gated_tool_call_tests;
/// Issue #395: end-to-end proof that a tool call gated inside a workflow agent
/// node reaches the Approvals page and survives the next chat cycle.
#[cfg(test)]
pub(crate) mod gated_tool_turn_tests;
/// Issue #1866: one tool-less semantic sufficiency pass after deterministic checks.
pub mod judge;
/// Issue #978: a run that fans out to N gated nodes is cleared by approving,
/// not multiplied by it — the composition of #395, #243 and #469 that each of
/// their own suites is blind to.
/// Issue #1192: a node whose `publish_artifact` was refused for want of a
/// destination says so on the run, instead of the refusal reaching the operator
/// only as whatever prose the model wrote about it.
#[cfg(test)]
mod publish_refusal_notice_tests;
/// Issue #846: a continuation replays the outward calls its lineage already
/// made, instead of making them a second time.
pub mod replay;
pub mod runner;
pub mod translate;
/// Issue #1098: a scheduled workflow granted a standing permission stops
/// re-asking on every run — two runs, because a single-run test cannot see it.
pub use caps::{HarnessAgentRunner, build_capabilities};
pub use checkpoint_store::WorkflowCheckpointStore;
pub use delivery::{DeliveryParking, WorkflowDeliveryDeps, deliver_outputs, deliver_outputs_dry};
pub use runner::{HarnessWorkflowRunner, run_workflow};
pub use translate::translate;

/// Compile-only proof (P0) that the vendored `tinyflows` engine links under the
/// `openhuman` feature and its public API is reachable from this crate.
///
/// Naming a `tinyflows` public item here forces the dependency to resolve and
/// the version to align at build time; it is exercised by
/// [`tinyflows_engine_is_linked`](tests) so it is not dead code.
pub fn tinyflows_engine_name() -> &'static str {
    tinyflows::product_name()
}

#[cfg(test)]
#[path = "workflows_tests.rs"]
mod tests;
