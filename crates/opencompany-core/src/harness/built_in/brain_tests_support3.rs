use super::*;
use crate::company::steer::InflightRegistry;
use crate::ports::tasks::TaskTitle;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;
use tinyinference::message::Message;
use tinyinference::model::{ChatModel, ModelRequest, ModelResponse};

/// A host that fails to park the *first* effect it is handed, then behaves.
/// Models a transient journal/IO fault mid-batch.
#[derive(Default)]
pub(super) struct FlakyParkingHost {
    pub(super) parked: std::sync::Mutex<Vec<Effect>>,
    pub(super) seen: std::sync::atomic::AtomicUsize,
}

impl FlakyParkingHost {
    pub(super) fn parked(&self) -> Vec<Effect> {
        self.parked.lock().expect("parked").clone()
    }
}

#[async_trait]
impl CycleHost for FlakyParkingHost {
    async fn call_tool(&self, _call: ToolCall) -> Result<ToolResult> {
        Ok(ToolResult {
            ok: true,
            output: serde_json::Value::Null,
        })
    }
    async fn context_op(&self, _op: ContextOp) -> Result<ContextOpResult> {
        Ok(ContextOpResult::Text(String::new()))
    }
    async fn emit_effect(&self, _effect: Effect) -> Result<EffectDisposition> {
        panic!("an approval request must be parked, never re-evaluated as an effect");
    }
    async fn park_effect(&self, effect: Effect) -> Result<ApprovalId> {
        if self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            return Err(crate::OpenCompanyError::Store(
                "journal on fire".to_string(),
            ));
        }
        let mut parked = self.parked.lock().expect("parked");
        parked.push(effect);
        Ok(ApprovalId::new(format!("appr-{}", parked.len())))
    }
}

// --- Re-dispatching a granted call (issue #243) --------------------------

/// A brain over the offline mock provider, wired to a real event log and a
/// shared approval queue (whose grant set the runtime would mint into).
pub(super) fn brain_with_queue_and_events(
    dir: &std::path::Path,
    requests: crate::harness::policy::ApprovalRequestQueue,
    events: Arc<dyn crate::ports::EventLog>,
) -> HarnessBrain {
    let deps = HarnessDeps {
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(MockProvider::new("mock: ")),
        provider_slug: "mock".to_string(),
        serves: None,
        context: Arc::new(FsContextStore::new(dir)),
        store: Arc::new(FsCompanyStore::new(dir)),
        meter: None,
        workspace_root: dir.to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.to_path_buf(),
        model_override: None,
        tasks: None,
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events: Some(events),
        delegations: orchestrator::DelegationQueue::default(),
        workflow_runner: orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: crate::harness::mcp_probe::McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_revisions: None,
        approval_requests: requests,
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
        search: None,
        tenant_search: None,
        workspace: None,
        workflow_runs: None,
        deep_trace: None,
    };
    HarnessBrain::new(Arc::new(HarnessPool::new()), deps, record())
}

/// As [`brain_with_queue_and_events`], but every model call fails with a
/// budget-exhausted body via [`BudgetExhaustedProvider`] (issue #1846
/// review, Codex #3869725683) — otherwise byte-identical, so the only
/// variable a test built on this exercises is how the approval-
/// continuation redispatch path reacts to that one failure shape.
pub(super) fn brain_with_queue_and_events_and_budget_exhausted_provider(
    dir: &std::path::Path,
    requests: crate::harness::policy::ApprovalRequestQueue,
    events: Arc<dyn crate::ports::EventLog>,
) -> HarnessBrain {
    let deps = HarnessDeps {
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(BudgetExhaustedProvider),
        provider_slug: "scripted".to_string(),
        serves: None,
        context: Arc::new(FsContextStore::new(dir)),
        store: Arc::new(FsCompanyStore::new(dir)),
        meter: None,
        workspace_root: dir.to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.to_path_buf(),
        model_override: None,
        tasks: None,
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events: Some(events),
        delegations: orchestrator::DelegationQueue::default(),
        workflow_runner: orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: crate::harness::mcp_probe::McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_revisions: None,
        approval_requests: requests,
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
        search: None,
        tenant_search: None,
        workspace: None,
        workflow_runs: None,
        deep_trace: None,
    };
    HarnessBrain::new(Arc::new(HarnessPool::new()), deps, record())
}

pub(super) fn approval_resolved(id: &str, verdict: Verdict) -> CompanyEvent {
    CompanyEvent::ApprovalResolved {
        approval_id: ApprovalId::new(id),
        verdict,
        by: crate::ports::types::Actor {
            kind: crate::ports::types::ActorKind::Operator,
            id: "owner".into(),
        },
    }
}

pub(super) fn cycle_over(events: Vec<CompanyEvent>) -> CycleRequest {
    CycleRequest {
        cycle_id: "cyc-1".to_string(),
        company_id: CompanyId::new("acme"),
        events,
        event_seqs: Vec::new(),
        policy: None,
    }
}

pub(super) async fn assert_explicit_decision_continues(verdict: Verdict, expected: &str) {
    let dir = tempfile::tempdir().unwrap();
    let log: Arc<dyn crate::ports::EventLog> =
        Arc::new(crate::store::FsEventLog::new(dir.path().to_path_buf()));
    let requests = crate::harness::policy::ApprovalRequestQueue::default();
    requests
        .grants()
        .continue_approval(crate::runtime::grants::ApprovalContinuation {
            call: crate::runtime::grants::GrantedCall {
                approval_id: ApprovalId::new("appr-explicit"),
                agent: "ceo".into(),
                tool: crate::harness::approval_tool::REQUEST_APPROVAL_TOOL.into(),
                args: serde_json::json!({
                    "title": "Publish the announcement",
                    "question": "May I publish it?"
                }),
                at_millis: now_millis(),
                origin_thread: None,
                origin_parent: None,
                origin_task: None,
            },
            verdict,
            by: crate::ports::types::Actor {
                kind: crate::ports::types::ActorKind::User,
                id: "operator".into(),
            },
        });
    let grants = requests.grants();
    let brain = brain_with_queue_and_events(dir.path(), requests, log);

    let result = brain
        .run_cycle(
            cycle_over(vec![approval_resolved("appr-explicit", verdict)]),
            &NoopHost,
        )
        .await
        .unwrap();

    assert_eq!(result.channel_responses.len(), 1);
    let text = &result.channel_responses[0].text;
    assert!(text.contains(expected), "{text}");
    assert!(text.contains("Publish the announcement"), "{text}");
    assert!(!text.contains("Re-issue it"), "{text}");
    assert!(
        grants
            .peek_continuation(&ApprovalId::new("appr-explicit"))
            .is_none()
    );
}

/// No continuation reply was journaled by the brain itself (issue #469).
pub(super) async fn no_replies_journaled(log: &Arc<dyn crate::ports::EventLog>) -> bool {
    log.read_from(&CompanyId::new("acme"), crate::ports::EventSeq::new(0), 100)
        .await
        .unwrap()
        .iter()
        .all(|e| !matches!(e.event, CompanyEvent::AgentReply { .. }))
}

// --- Issue #453: a re-dispatch drains what its turn queued ---------------

/// What the scripted model does on each successive `/chat/completions` call.
#[derive(Clone, Debug)]
pub(super) enum ScriptTurn {
    /// Emit a native tool call.
    Call {
        tool: &'static str,
        args: serde_json::Value,
    },
    /// Finish with plain assistant text.
    Say(&'static str),
}

/// Serves a scripted OpenAI-compatible endpoint on loopback and returns its
/// base URL.
///
/// `MockProvider` cannot express a tool call, and a tool call is the whole
/// point here: the defect is that a `review_task` made by a re-issued turn
/// was staged and never drained. Same shape `workspace_turn_test` and
/// `gated_tool_turn_test` established — stub exactly one boundary, the
/// model's choices, and run everything else for real.
pub(super) async fn spawn_model_script(turns: Vec<ScriptTurn>) -> String {
    use axum::Json;
    use axum::routing::post;

    let script = Arc::new(std::sync::Mutex::new(turns));
    let app = axum::Router::new().route(
        "/chat/completions",
        post(move |Json(_body): Json<serde_json::Value>| {
            let script = Arc::clone(&script);
            async move {
                let next = {
                    let mut turns = script.lock().unwrap();
                    if turns.is_empty() {
                        None
                    } else {
                        Some(turns.remove(0))
                    }
                };
                // Running off the end means the loop went round more times
                // than expected; end the turn rather than hang.
                let message = match next.unwrap_or(ScriptTurn::Say("done")) {
                    ScriptTurn::Say(text) => {
                        serde_json::json!({ "role": "assistant", "content": text })
                    }
                    ScriptTurn::Call { tool, args } => {
                        // Plan hive-desks Phase 3: a company tool is reached
                        // through `mcp_call_tool` on the `opencompany` server.
                        let (name, args) = crate::hive::tools::via_opencompany_mcp(tool, args);
                        serde_json::json!({
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": format!("call-{tool}"),
                                "type": "function",
                                "function": { "name": name, "arguments": args.to_string() }
                            }]
                        })
                    }
                };
                Json(serde_json::json!({
                    "choices": [{ "index": 0, "message": message }],
                    "usage": { "prompt_tokens": 12, "completion_tokens": 4 }
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// A brain over the scripted model with a **real task store**, so a
/// `review_task` the re-dispatched turn makes can actually move a card.
pub(super) fn brain_over_script(
    dir: &std::path::Path,
    requests: crate::harness::policy::ApprovalRequestQueue,
    base_url: String,
) -> HarnessBrain {
    use crate::company::credentials::Credential;
    use crate::harness::provider::{HostedProvider, HostedProviderConfig};

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
        meter: None,
        workspace_root: dir.to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.to_path_buf(),
        model_override: Some("stub-model".to_string()),
        tasks: Some(Arc::new(FsOps::new(dir))),
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events: None,
        delegations: orchestrator::DelegationQueue::default(),
        workflow_runner: orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: crate::harness::mcp_probe::McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_revisions: None,
        approval_requests: requests,
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
        search: None,
        tenant_search: None,
        workspace: None,
        workflow_runs: None,
        deep_trace: None,
    };
    HarnessBrain::new(Arc::new(HarnessPool::new()), deps, record())
}

/// A card sitting in review, waiting on the verdict the operator approved.
pub(super) fn card_in_review(id: &str) -> TaskRecord {
    TaskRecord {
        id: id.to_string(),
        title: TaskTitle::authored(&format!("Work item {id}")),
        note: None,
        column: COLUMN_IN_REVIEW.to_string(),
        priority: "medium".to_string(),
        assignee: "ceo".to_string(),
        updated_at_millis: now_millis(),
        origin: None,
        parent_task_id: None,
        output: None,
        plan: None,
        planning_attempts: Vec::new(),
        deliverable: crate::ports::tasks::TaskDeliverable::Once,
        workflow_proposal: None,
        origin_run_id: None,
        origin_workflow_id: None,
        origin_message_seq: None,
        bounced: None,
    }
}

pub(super) fn granted(approval: &str, tool: &str) -> crate::runtime::grants::GrantedCall {
    crate::runtime::grants::GrantedCall {
        approval_id: ApprovalId::new(approval),
        agent: "ceo".into(),
        tool: tool.into(),
        args: serde_json::json!({}),
        at_millis: now_millis(),
        origin_thread: None,
        origin_parent: None,
        origin_task: None,
    }
}

// --- Steer disposition (issue #111) -------------------------------------

/// A model that steers its OWN in-flight run on selected turns (via the
/// shared registry), so the disposition matrix can be driven deterministically
/// over an offline turn. It pops one queued action per [`invoke`](ChatModel::invoke)
/// call and applies it against `key`, then echoes the last user message.
pub(super) struct SteeringProvider {
    pub(super) steer: InflightRegistry,
    pub(super) company: CompanyId,
    pub(super) key: String,
    pub(super) actions: StdMutex<VecDeque<SteerAction>>,
    pub(super) calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl ChatModel<()> for SteeringProvider {
    async fn invoke(
        &self,
        _state: &(),
        request: ModelRequest,
    ) -> tinyinference::Result<ModelResponse> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(action) = self.actions.lock().unwrap().pop_front() {
            let key = if self.key.is_empty() {
                self.steer
                    .list(&self.company)
                    .into_iter()
                    .next()
                    .map(|entry| entry.key)
                    .unwrap_or_default()
            } else {
                self.key.clone()
            };
            let _ = self.steer.steer(&self.company, &key, action);
        }
        let message = request
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m, Message::User(_)))
            .map(|m| m.text())
            .unwrap_or_default();
        Ok(ModelResponse::assistant(format!("did: {message}")))
    }
}

impl HarnessModel for SteeringProvider {
    fn telemetry_provider_id(&self) -> String {
        "steering".to_string()
    }
}

/// A deterministic turn result for scheduled-cycle edge-case tests.
pub(super) struct FixedOutcomeTurn {
    pub(super) outcome: crate::harness::built_in::TurnOutcome,
    pub(super) approval_requests: Option<crate::harness::policy::ApprovalRequestQueue>,
}

#[async_trait]
impl crate::runtime::delegation::RunTurn for FixedOutcomeTurn {
    async fn run(
        &self,
        _company: &CompanyId,
        _agent_id: &str,
        _message: &str,
        _chat: crate::runtime::delegation::ChatTarget<'_>,
    ) -> Result<crate::harness::built_in::TurnOutcome> {
        if let Some(requests) = &self.approval_requests {
            for index in 0..(crate::harness::policy::MAX_APPROVAL_REQUESTS_PER_TURN + 1) {
                requests.push(crate::harness::policy::ApprovalRequest {
                    tool: format!("test_tool_{index}"),
                    reason: "test approval".to_string(),
                    effect: Effect {
                        kind: format!("test_tool_{index}"),
                        group: crate::ports::types::EffectGroup::Other,
                        amount_usd: None,
                        established_thread: false,
                        first_time_counterparty: false,
                        payload: serde_json::json!({ "index": index }),
                        agent: None,
                        run_id: None,
                    },
                });
            }
        }
        Ok(self.outcome.clone())
    }

    async fn run_steered(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
        _control: &crate::company::steer::SteerControl,
        chat: crate::runtime::delegation::ChatTarget<'_>,
        _run_sink: Option<Arc<crate::harness::run_trace::RunTraceSink>>,
    ) -> Result<crate::harness::built_in::TurnOutcome> {
        self.run(company, agent_id, message, chat).await
    }

    async fn run_steered_background(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
        _control: &crate::company::steer::SteerControl,
        _chat: crate::runtime::delegation::ChatTarget<'_>,
        _run_sink: Option<Arc<crate::harness::run_trace::RunTraceSink>>,
    ) -> Result<crate::harness::built_in::TurnOutcome> {
        self.run(
            company,
            agent_id,
            message,
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await
    }
}
