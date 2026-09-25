//! Issue #416 — the **confined turn**: an agent that reaches nothing but the
//! message it was given.
//!
//! ## What was wrong
//!
//! The per-workflow copilot (#303) sends its question to the company
//! orchestrator on a dedicated thread. The question is *grounded* — the console
//! inlines that workflow's graph and its recorded runs — and the exchange is
//! isolated in the journal. But the thread only ever selected the responder and
//! the journal thread; it did not narrow what the responder could reach. The
//! teammate answering held the whole company's context and its full tool
//! surface, so "answer only about this workflow" was a sentence in a prompt,
//! and a sentence is advice.
//!
//! ## What a confined turn is
//!
//! An ephemeral agent, built for one turn and dropped afterwards, whose reach is
//! the message and nothing else:
//!
//! * **No tools.** Its toolbelt is empty — not even the intrinsic memory tools
//!   every roster agent carries, and none of the orchestrator's `query_company`
//!   / `spawn_task` / `delegate_to_desk`. It cannot read the board, the roster,
//!   the workspace tree, another workflow, an MCP server, the web, or a file.
//! * **No company memory.** No memory tool is wired, and [`ConfinedContext`]
//!   — an in-process store that holds nothing and answers every read empty —
//!   is what any later memory seam must be pointed at, so the company
//!   [`ContextStore`] is not reachable through recall. The pool
//!   additionally skips the retrieve→inject step and the memory writeback for a
//!   confined turn, so no prior task outcome is prepended to the message and the
//!   exchange leaves nothing behind for a later turn to retrieve.
//! * **Enforced, not requested.** [`ConfinedToolPolicy`] denies **every** tool
//!   call by name whatever the belt holds. An empty belt already means the model
//!   is offered nothing; the policy is what makes that a boundary rather than an
//!   absence — a tool wired here by a later change, or a name the model invents,
//!   is refused by the host with a reason, not run.
//! * **No delegation.** The brain runs this turn directly rather than through
//!   the delegation runner, so a confined turn cannot hand off to a desk and
//!   have the desk do what it was not allowed to.
//!
//! ## What a copilot turn can still reach
//!
//! Exactly what the console put in the message: the workflow's name, id,
//! description, its nodes and edges, and the summaries of its own recorded runs.
//! Plus the operator's question, and the earlier turns of this same thread that
//! the provider carries. Nothing else — and when a question genuinely needs
//! company-wide context, the persona tells it to say so and point at the Chat
//! tab rather than answer from material it does not have.
//!
//! Compiled only under `feature = "openhuman"`, with the rest of the harness.

use std::ops::Range;

use async_trait::async_trait;
use openhuman_core as oh;

use oh::agent::tool_policy::{ToolPolicy, ToolPolicyDecision, ToolPolicyRequest};
use tinytools::Tool;

use crate::harness::HarnessDeps;
use crate::harness::build::{AgentBlueprint, ensure_agent_workspace, model_for_tier};
use crate::harness::policy::ApprovalPolicy;
use crate::ports::ContextStore;
use crate::ports::types::{ChunkAddr, ChunkHit, ChunkMeta, CompanyId, ContextChunk};

/// The agent id a confined turn runs under. Deliberately not a roster id: it
/// names no teammate, carries no manifest grants, and cannot be addressed.
///
/// Defined in [`crate::ports::ids`] and re-exported here (issue #966): the
/// attribution audit needs it in the default build, where this module does not
/// compile. Every `confine::CONFINED_AGENT_ID` call site keeps working.
pub use crate::ports::CONFINED_AGENT_ID;

/// What one confined turn is confined **to**.
///
/// One variant today. It is an enum rather than a bare workflow id because the
/// boundary is the reusable half of this issue: the next thing that should
/// reason about one object without the rest of the company in scope adds a
/// variant here and reuses the whole path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confinement {
    /// A copilot turn about exactly one saved workflow.
    Workflow { id: String },
}

impl Confinement {
    /// The confinement for a workflow copilot thread.
    pub fn workflow(id: impl Into<String>) -> Self {
        Self::Workflow { id: id.into() }
    }

    /// The subject, for logs and for the refusal a denied tool call carries.
    pub fn subject(&self) -> &str {
        match self {
            Self::Workflow { id } => id,
        }
    }
}

/// A [`ContextStore`] that stores nothing and finds nothing.
///
/// Stands in for the company's real store so a confined agent's memory is
/// structurally present (openhuman requires one) and substantively empty. Reads
/// answer empty rather than erroring: a confined turn that *fails* on recall
/// would be a turn whose confinement is visible to the model as a fault, and
/// "there is nothing here" is the truth anyway.
#[derive(Debug, Default)]
pub struct ConfinedContext;

#[async_trait]
impl ContextStore for ConfinedContext {
    async fn put(&self, _id: &CompanyId, chunk: ContextChunk) -> crate::Result<ChunkAddr> {
        // Accepted and dropped. The address is derived from the label so a
        // caller that stores and immediately re-reads gets a coherent answer
        // (nothing), rather than a store that refuses writes and turns a
        // confined turn into an error path.
        Ok(ChunkAddr::new(format!("confined/{}", chunk.label)))
    }

    async fn list(&self, _id: &CompanyId, _prefix: &str) -> crate::Result<Vec<ChunkMeta>> {
        Ok(Vec::new())
    }

    async fn delete(
        &self,
        _id: &CompanyId,
        _addr: &crate::ports::types::ChunkAddr,
    ) -> crate::Result<bool> {
        // Nothing is ever stored, so there is never anything to delete —
        // `false` is the truth, same as the empty reads above.
        Ok(false)
    }

    async fn delete_label(
        &self,
        _id: &CompanyId,
        _addr: &crate::ports::types::ChunkAddr,
        _label: &str,
    ) -> crate::Result<bool> {
        // Same truth as `delete`: no claim was ever stored.
        Ok(false)
    }

    async fn peek(
        &self,
        _id: &CompanyId,
        _addr: &ChunkAddr,
        _range: Option<Range<usize>>,
    ) -> crate::Result<String> {
        Ok(String::new())
    }

    async fn search(
        &self,
        _id: &CompanyId,
        _query: &str,
        _limit: usize,
    ) -> crate::Result<Vec<ChunkHit>> {
        Ok(Vec::new())
    }
}

/// Denies every tool call, whatever the belt holds.
///
/// The confined agent is built with an empty toolbelt, so in the ordinary case
/// this policy is never consulted — nothing is offered to call. It exists for
/// the two cases that are not ordinary: a tool wired onto this path by a later
/// change (the belt is code, and code drifts), and a model that emits a call for
/// a name it was never given. Both are refused here, by the host, with a reason
/// the model can read and repeat to the operator.
#[derive(Debug)]
pub struct ConfinedToolPolicy {
    confinement: Confinement,
}

impl ConfinedToolPolicy {
    /// A policy that refuses everything outside `confinement`.
    pub fn new(confinement: Confinement) -> Self {
        Self { confinement }
    }

    /// The refusal a denied call carries. Names the boundary rather than saying
    /// "denied", so a model can tell the operator what it could not do and why
    /// instead of retrying the same call.
    pub fn refusal(&self, tool_name: &str) -> String {
        match &self.confinement {
            Confinement::Workflow { id } => format!(
                "`{tool_name}` is not available here. This turn is confined to the workflow \
                 `{id}`: it answers from the workflow description in this message and reaches \
                 nothing else in the company. Say what you would need and that it has to be \
                 asked in the company chat instead."
            ),
        }
    }
}

#[async_trait]
impl ToolPolicy for ConfinedToolPolicy {
    fn name(&self) -> &str {
        "workflow_confined"
    }

    async fn check(&self, request: &ToolPolicyRequest) -> ToolPolicyDecision {
        tracing::info!(
            tool = %request.tool_name,
            subject = %self.confinement.subject(),
            "[confine] refusing a tool call on a confined turn"
        );
        ToolPolicyDecision::deny(self.refusal(&request.tool_name))
    }
}

/// The confined agent's persona.
///
/// Says three things, and each is load-bearing:
///
/// 1. what this turn is about (one workflow, named);
/// 2. that its whole world is the message — which is *true* now, so the model is
///    not being asked to pretend a limit it could step outside of;
/// 3. what to do when the question needs more than that. The issue's own design
///    question: refusing is honest and unhelpful, answering unconfined defeats
///    the purpose, so it answers what it can and **says which part it could
///    not** rather than guessing from an absence.
pub fn confined_persona(company_name: &str, confinement: &Confinement) -> String {
    match confinement {
        Confinement::Workflow { id } => format!(
            "You are the workflow copilot for {company_name}, answering about ONE saved \
             workflow (`{id}`).\n\n\
             Everything you know about this workflow is in the message you were sent: its \
             description, its nodes and edges, and the summaries of its own recorded runs. You \
             have no tools and no access to the rest of the company — not its board, its \
             teammates, its other workflows, its files, or its memory. That is deliberate: an \
             answer about this workflow is meant to be an answer about this workflow.\n\n\
             Answer from that material. Where the question needs something you were not given \
             — another workflow, what a teammate is doing, the company's data — say plainly \
             which part you cannot answer and that it has to be asked in the company chat, then \
             answer whatever the rest of the question allows. Do not guess at company context, \
             and do not claim to have looked anything up.\n\n\
             You cannot change the workflow yourself, and you have no way to try: this turn \
             calls nothing. What you can do is PROPOSE a change in the format the message asks \
             for (issue #415) — the operator reads it as a diff against their graph and applies \
             it, or throws it away. A proposal is text in your reply, not an action, so never \
             say you have made a change, and never propose one that was not asked for."
        ),
    }
}

/// Builds the ephemeral agent a confined turn runs on.
///
/// Deliberately **not** [`build_agent`](crate::harness::build::build_agent) with
/// empty grants: that path wires the intrinsic memory tools onto every agent
/// regardless of grants, and even an empty-grants call still gets the approval,
/// thread-read, and escalate-to-human tools — reach this turn must not have.
/// Nothing is cached — the agent is built per turn and dropped with it, so a
/// confined turn cannot accumulate state that a later one reads.
pub fn build_confined_agent(
    company: &CompanyId,
    company_name: &str,
    confinement: &Confinement,
    deps: &HarnessDeps,
) -> crate::Result<AgentBlueprint> {
    // Its own sandbox directory, so nothing here can resolve into another
    // agent's workspace. Best-effort exactly as on the roster path: an agent
    // with no file tools runs a perfectly good turn without the directory, and
    // this one has no file tools by construction.
    let workspace = match ensure_agent_workspace(&deps.workspace_root, company, CONFINED_AGENT_ID) {
        Ok(workspace) => workspace,
        Err(error) => {
            tracing::warn!(
                company = %company,
                %error,
                "[confine] could not create the confined agent's workspace directory"
            );
            crate::harness::build::agent_workspace(&deps.workspace_root, company, CONFINED_AGENT_ID)
        }
    };

    // The conversational workload, not the orchestrator's agentic one: this turn
    // explains a graph it was handed and calls nothing. A host-wide
    // `model_override` still wins, exactly as it does for the roster.
    let model = deps
        .model_override
        .clone()
        .unwrap_or_else(|| model_for_tier(None));

    // An empty belt renders as an empty `ToolScopeSpec::Named`, so the runtime
    // offers the model nothing — the boundary [`ConfinedToolPolicy`] enforced
    // in-process is now the runtime's own tool scope, and the confined
    // context store is simply never wired (no memory tool reaches this turn).
    // The policy type stays for the Phase 3 tool handler, which is where a
    // host-served tool would otherwise reach this turn.
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let policy = ApprovalPolicy::new(&crate::company::Policy::default(), None)
        .with_policy_hitl_disabled()
        .with_requests(deps.approval_requests.clone())
        .with_agent(CONFINED_AGENT_ID.to_string());

    super::tool_posture::declare();
    Ok(AgentBlueprint {
        // A confined turn delegates nothing; nothing to withhold.
        unadvertised: Vec::new(),
        system_prompt: confined_persona(company_name, confinement),
        tools,
        native_tool_names: Vec::new(),
        #[cfg(feature = "mcp")]
        company_mcp_servers: Vec::new(),
        chat_model: deps.provider.clone(),
        model,
        workspace,
        policy: std::sync::Arc::new(policy),
        definition_name: CONFINED_AGENT_ID.to_string(),
    })
}

#[cfg(test)]
#[path = "confine_tests.rs"]
mod tests;
