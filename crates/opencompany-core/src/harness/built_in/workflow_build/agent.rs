//! The create-time copilot's builder agent (issue #840, PR-2).
//!
//! PR-1 wired the effective-tool set; PR-2 turned the copilot into a real
//! tool-using agent built fresh per request. Since plan hive-desks Phase 2 it
//! runs on the host-side tool loop ([`crate::harness::host_loop`]) rather
//! than an embedded OpenHuman agent: its three tools are in-process and their
//! side effect is the result, which the embedded runtime — whose tool set is
//! its own — cannot host. It reuses the roster's inference engine
//! (`deps.provider`, an `Arc<dyn HarnessModel>` that upcasts to the
//! tinyinference `ChatModel<()>`), so the copilot's spend is captured as
//! backend-charged USD on its own per-call usage, not the token-only total a
//! bare tinyflows/tinyagents runner would report.
//!
//! It carries exactly the three OC-native tools in [`super::tools`] and no
//! memory (nothing it does is worth persisting into the company's durable
//! memory — a create-time draft is reviewed and pressed Create, or discarded).
//! Its system prompt is the ported DSL persona ([`copilot_persona`]); the
//! tools are offered as native tool schemas — a model that narrates prose
//! instead calls nothing.

use std::sync::Arc;

use tinytools::Tool;

use crate::harness::HarnessDeps;
use crate::harness::build::model_for_tier;

use super::tools::{
    AcceptedCell, CheckWorkflowTool, CopilotContext, DiagCell, ListEffectiveToolsTool,
    ProposeWorkflowTool,
};
use super::{DESCRIPTION_NODE_KINDS, graph_contract};

/// The ported DSL half of OpenHuman's builder prompt (issue #840): the workflow
/// model, the `=`/jq + envelope rules, minimal-graph guidance, honest
/// check-result interpretation, and OC's SAFETY paragraph — with OpenHuman's
/// save/create/run/OAuth/inference-readiness/ask-and-STOP machinery cut.
const COPILOT_PERSONA: &str = include_str!("copilot_prompt.md");

/// The copilot's full system prompt: the ported persona, then the shared
/// [`graph_contract`] for `DESCRIPTION_NODE_KINDS` — the SINGLE source of the
/// node-kind vocabulary + JSON schema both the prompt and the propose tool's
/// refusal read, so the two cannot drift (the drift is pinned by a test).
pub(super) fn copilot_persona() -> String {
    format!(
        "{COPILOT_PERSONA}\n\n{}",
        graph_contract(DESCRIPTION_NODE_KINDS)
    )
}

/// Builds the create-time copilot agent over the harness deps (issue #840).
///
/// Mirrors [`build_agent`](crate::harness::build::build_agent)'s native-vs-XML
/// dispatcher choice: a provider that advertises native tool calling
/// (`profile().tool_calling` — the managed hosted surface) gets openhuman's
/// [`NativeToolDispatcher`] so the turn sends structured `tools` and reads
/// `tool_calls` back; anything else keeps the prompt-guided XML fallback. Native
/// is REQUIRED for the copilot to work — without it the model narrates a proposal
/// as prose and never calls [`ProposeWorkflowTool`], so no proposal is ever
/// accepted.
///
/// The tool-iteration cap is set AFTER construction (per the setter's contract)
/// to a small budget: list → check → propose, with room for one correction
/// round.
/// The copilot, ready to run: its tools, its prompt, and the model it runs
/// on. Built fresh per request and dropped afterwards.
pub(super) struct CopilotAgent {
    /// The three OC-native copilot tools.
    pub(super) tools: Vec<Box<dyn Tool>>,
    /// The model every call goes to (the roster's engine).
    pub(super) model: Arc<dyn tinyinference::model::ChatModel<()>>,
    /// The model name every request carries.
    pub(super) model_name: String,
}

impl CopilotAgent {
    /// One request: the loop until the model answers or the cap is hit.
    pub(super) async fn run_single(
        &self,
        user: &str,
    ) -> anyhow::Result<crate::harness::host_loop::LoopOutcome> {
        crate::harness::host_loop::run(
            &self.model,
            &self.model_name,
            &copilot_persona(),
            user,
            &self.tools,
            COPILOT_MAX_ITERATIONS,
        )
        .await
    }
}

/// How many model calls one copilot request may make.
const COPILOT_MAX_ITERATIONS: usize = 7;

pub(super) fn build_copilot_agent(
    deps: &HarnessDeps,
    ctx: Arc<CopilotContext>,
    accepted: AcceptedCell,
    diag: DiagCell,
) -> CopilotAgent {
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(ListEffectiveToolsTool::new(ctx.clone())),
        Box::new(CheckWorkflowTool::new(ctx.clone(), diag.clone())),
        Box::new(ProposeWorkflowTool::new(ctx, accepted, diag)),
    ];
    let model_name = deps
        .model_override
        .clone()
        .unwrap_or_else(|| model_for_tier(None));
    super::super::tool_posture::declare();
    CopilotAgent {
        tools,
        model: deps.provider.clone() as Arc<dyn tinyinference::model::ChatModel<()>>,
        model_name,
    }
}
