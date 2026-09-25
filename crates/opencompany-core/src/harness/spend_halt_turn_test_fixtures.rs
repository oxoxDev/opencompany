//! Shared fixtures for the spend-halt turn tests: the scripted model,
//! company/manifest/record builders, and the harness deps wiring. Split
//! out of `spend_halt_turn_tests.rs` because the combined inline module
//! exceeded the 750-line file limit.
//!
//! See [`super::spend_halt_turn_tests`] for what each test proves.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Json;
use axum::routing::post;
use serde_json::{Value, json};

use crate::company::CompanyManifest;
use crate::company::credentials::Credential;
use crate::harness::HarnessDeps;
use crate::harness::mcp_probe::McpFailureQueue;
use crate::harness::memory_loop;
use crate::harness::orchestrator::{DelegationQueue, WorkflowRunnerHandle};
use crate::harness::policy::ApprovalRequestQueue;
use crate::harness::provider::{HostedProvider, HostedProviderConfig};
use crate::ports::ContextStore;
use crate::ports::brain::CycleHost;
use crate::ports::types::{
    ApprovalId, CompanyEvent, CompanyId, CompanyRecord, ContextOp, ContextOpResult, CycleRequest,
    Effect, EffectDisposition, OutboundMessage, ToolCall, ToolResult,
};
use crate::store::{FsCompanyStore, FsContextStore, FsOps};

/// The agent every test here talks to.
pub(super) const AGENT: &str = "ceo";

/// The declared daily cap the in-turn brake arms at, in USD.
///
/// Small enough that one scripted call crosses it, and quoted in the notice —
/// so the assertions below can look for `$0.05` and know where it came from.
pub(super) const CAP_USD: f64 = 0.05;

/// `prompt_tokens` a *cheap* call reports — well inside [`CAP_USD`], so a turn
/// scripted with it finishes on its own.
pub(super) const CHEAP_TOKENS: u64 = 12;

/// `prompt_tokens` an *expensive* call reports. On the `chat-v1` tier this
/// estimates to roughly $0.14, so the very first iteration crosses
/// [`CAP_USD`] and the brake halts the turn.
pub(super) const EXPENSIVE_TOKENS: u64 = 1_000_000;

/// The tool-iteration cap a turn actually runs under, bound to the constant
/// rather than re-hardcoded so a vendor bump cannot quietly weaken the
/// cross-fire test below into scripting the wrong turn shape.
pub(super) const CAP: usize = crate::harness::build::MAX_TOOL_ITERATIONS;

/// The answer the scripted model gives when it is allowed to finish.
pub(super) const ANSWER: &str = "Spec published.";

/// A slice of the spend notice unique to the platform's voice, for proving the
/// notice is *absent* — from memory, and from a turn that finished.
///
/// A substring rather than the whole notice: an absence assertion on the full
/// string would pass the moment the wording changed by a comma.
pub(super) const SPEND_MARKER: &str = "reached its spend cap partway through";

// ---------------------------------------------------------------------------
// The scripted model
// ---------------------------------------------------------------------------

/// What the scripted model does on each successive call.
#[derive(Clone, Debug)]
pub(super) enum Turn {
    /// Emit a native tool call with these literal arguments.
    Call { tool: String, args: Value },
    /// Finish with plain assistant text.
    Say(String),
}

/// A scripted OpenAI-compatible `/chat/completions` endpoint.
pub(super) struct Script {
    turns: Mutex<Vec<Turn>>,
    seen: Mutex<Vec<Value>>,
    /// `prompt_tokens` echoed on every response — the knob that decides whether
    /// the budget hook fires, because the stop-hook middleware folds it into
    /// openhuman's turn cost.
    prompt_tokens: u64,
}

impl Script {
    /// How many model calls the turn actually made.
    pub(super) fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

/// One assistant message carrying a native `tool_calls` array — the shape the
/// provider's `tool_calling: true` profile puts the turn loop on.
fn tool_call_message(tool: &str, args: &Value) -> Value {
    json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{
            "id": format!("call-{tool}"),
            "type": "function",
            "function": { "name": tool, "arguments": args.to_string() }
        }]
    })
}

/// Serve the script on loopback and return its base URL plus the shared handle.
pub(super) async fn spawn_script(turns: Vec<Turn>, prompt_tokens: u64) -> (String, Arc<Script>) {
    let script = Arc::new(Script {
        turns: Mutex::new(turns),
        seen: Mutex::new(Vec::new()),
        prompt_tokens,
    });
    let handle = Arc::clone(&script);
    let app = axum::Router::new().route(
        "/chat/completions",
        post(move |Json(body): Json<Value>| {
            let script = Arc::clone(&handle);
            async move {
                script.seen.lock().unwrap().push(body.clone());
                let next = {
                    let mut turns = script.turns.lock().unwrap();
                    if turns.is_empty() {
                        None
                    } else {
                        Some(turns.remove(0))
                    }
                };
                // Running off the end means the turn looped more than the script
                // expected. End it with text rather than hanging — the
                // call-count assertions are what report the mismatch.
                let next = next.unwrap_or_else(|| Turn::Say("ran off the script".to_string()));
                let message = match next {
                    Turn::Say(text) => json!({ "role": "assistant", "content": text }),
                    Turn::Call { tool, args } => tool_call_message(&tool, &args),
                };
                (
                    axum::http::StatusCode::OK,
                    Json(json!({
                        "choices": [{ "index": 0, "message": message }],
                        "usage": {
                            "prompt_tokens": script.prompt_tokens,
                            "completion_tokens": 4
                        }
                    })),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), script)
}

/// A script that writes `n` distinct files and then answers.
///
/// **Distinct paths and bodies** on purpose: an identical successful tool batch
/// reissued back to back is what openhuman's repeat-progress guard halts on, and
/// a run stopped by *that* would prove nothing about the brake under test. The
/// writes must also succeed every time, or the repeated-failure breaker halts
/// the run for a third unrelated reason.
pub(super) fn write_then_answer(n: usize) -> Vec<Turn> {
    let mut turns: Vec<Turn> = (1..=n)
        .map(|i| Turn::Call {
            tool: "file_write".to_string(),
            args: json!({ "path": format!("step-{i}.md"), "content": format!("step {i}") }),
        })
        .collect();
    turns.push(Turn::Say(ANSWER.to_string()));
    turns
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// An inert `CycleHost` — these tests are about the turn, not the effect gate.
pub(super) struct NoopHost;

#[async_trait]
impl CycleHost for NoopHost {
    async fn call_tool(&self, _call: ToolCall) -> crate::Result<ToolResult> {
        Ok(ToolResult {
            ok: true,
            output: Value::Null,
        })
    }
    async fn context_op(&self, _op: ContextOp) -> crate::Result<ContextOpResult> {
        Ok(ContextOpResult::Text(String::new()))
    }
    async fn emit_effect(&self, _effect: Effect) -> crate::Result<EffectDisposition> {
        Ok(EffectDisposition::Executed)
    }
    async fn park_effect(&self, _effect: Effect) -> crate::Result<ApprovalId> {
        Ok(ApprovalId::new("appr-parked"))
    }
}

pub(super) fn company() -> CompanyId {
    CompanyId::new("acme")
}

/// A one-agent company on `full` policy, so an ordinary turn is not parked for
/// approval — the gate under test is the spend brake, not the approval one.
///
/// `budget` is the teammate's declared `budget_usd_daily`. `None` renders the
/// key away entirely, which is the state in which #988 arms no hook at all —
/// the negative control this file needs, and not something a zero could stand
/// in for (a zero is a *malformed* cap, which `turn_spend_cap_usd` also ignores,
/// for a different reason).
pub(super) fn manifest(budget: Option<f64>) -> CompanyManifest {
    let budget_line = match budget {
        Some(usd) => format!("budget_usd_daily = {usd}\n"),
        None => String::new(),
    };
    toml::from_str(&format!(
        r#"
[company]
name = "Acme"

[policy]
mode = "full"

[tools]
allow = ["*"]

[[agent]]
id = "ceo"
role = "Chief Executive"
tier = "orchestrator"
{budget_line}"#
    ))
    .expect("manifest parses")
}

pub(super) fn record(budget: Option<f64>) -> CompanyRecord {
    CompanyRecord {
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: company(),
        manifest: manifest(budget),
        ledger: Vec::new(),
        lifecycle: "running".to_string(),
        overlay_agents: Vec::new(),
        overlay_desk_members: Vec::new(),
        overlay_desk_order: Vec::new(),
        overlay_desks: Vec::new(),
        overlay_workflows: Vec::new(),
        overlay_budgets: Vec::new(),
        overlay_policy: None,
        overlay_tool_grants: None,
        overlay_desk_tools: Default::default(),
        disabled_workflows: Vec::new(),
        template_provenance: None,
        setup: None,
        name_confirmed: false,
        activation_completed_at: None,
        created_at_millis: None,
    }
}

/// Real deps pointed at the scripted endpoint, with a workspace on disk so
/// `file_write` genuinely succeeds.
///
/// `meter: Some(ops)` on purpose. The **pre-dispatch** daily-spend gate reads
/// it, and with a fresh store it reports zero spend — so the turn is dispatched
/// and the *in-turn* brake is the thing that stops it. A `None` meter would let
/// the turn run too, but for the wrong reason (that gate fails open), and the
/// test would no longer distinguish the two controls it exists to separate.
pub(super) fn deps_for(base_url: String, dir: &std::path::Path) -> (HarnessDeps, Arc<FsOps>) {
    let ops = Arc::new(FsOps::new(dir));
    let deps = HarnessDeps {
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(HostedProvider::new(HostedProviderConfig {
            base_url,
            credential: Credential::from_value("stub-key"),
            extra_headers: Vec::new(),
        })),
        provider_slug: "managed".to_string(),
        serves: None,
        context: Arc::new(FsContextStore::new(dir)),
        store: Arc::new(FsCompanyStore::new(dir)),
        meter: Some(ops.clone()),
        workspace_root: dir.to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.to_path_buf(),
        // Keys rework, issue #2306, slice 2d: `HostedProvider` has no decl
        // and therefore no configured map, so a bare tier name is refused
        // outright rather than sent. This fixture's own assertions only ever
        // check that some spend was recorded (`> 0.0`) and the operator's
        // own declared cap, never a specific priced figure tied to the model
        // name, so a stub id changes nothing they check.
        model_override: Some("stub-model".to_string()),
        tasks: Some(ops.clone()),
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events: None,
        delegations: DelegationQueue::default(),
        workflow_runner: WorkflowRunnerHandle::default(),
        mcp_failures: McpFailureQueue::default(),
        pending_publishes: Default::default(),
        workflow_refs: Default::default(),
        run_outputs: Default::default(),
        run_output_store: None,
        workflow_revisions: None,
        approval_requests: ApprovalRequestQueue::default(),
        approval_parker: None,
        secrets: None,
        web_allowed_domains: Vec::new(),
        capabilities: crate::harness::toolbelt::CapabilityFilter::AllowAll,
        workflow_source_dir: None,
        plan: None,
        media: None,
        composio: None,
        #[cfg(feature = "chargebee")]
        chargebee: None,
        #[cfg(feature = "paypal")]
        paypal: None,
        hosting: None,
        steer: crate::company::steer::InflightRegistry::default(),
        run_supervisor: crate::runtime::RunSupervisor::default(),
        delivery: None,
        workspace: None,
        search: None,
        tenant_search: None,
        workflow_runs: None,
        deep_trace: None,
    };
    (deps, ops)
}

/// An operator message, the shape a chat turn arrives as.
pub(super) fn chat(text: &str) -> CycleRequest {
    CycleRequest {
        cycle_id: "cycle-1".to_string(),
        company_id: company(),
        events: vec![CompanyEvent::OperatorMessage {
            mentions: Vec::new(),
            text: text.to_string(),
            by: None,
            chat: None,
            parent: None,
            deliverable: None,
            attachments: Vec::new(),
        }],
        event_seqs: Vec::new(),
        policy: None,
    }
}

/// The operator-channel bubbles a cycle produced.
pub(super) fn operator_bubbles(responses: &[OutboundMessage]) -> Vec<&OutboundMessage> {
    responses
        .iter()
        .filter(|m| m.channel == "operator")
        .collect()
}

/// Everything the turn wrote back to memory.
pub(super) async fn memory_bodies(context: &FsContextStore) -> Vec<String> {
    let metas = context
        .list(&company(), memory_loop::OUTCOME_LABEL_PREFIX)
        .await
        .expect("list memory");
    let mut bodies = Vec::new();
    for meta in metas {
        bodies.push(
            context
                .peek(&company(), &meta.addr, None)
                .await
                .expect("peek memory"),
        );
    }
    bodies
}
