use super::*;
pub(super) use crate::ports::tasks::TaskTitle;
use tinyinference::model::{ChatModel, ModelRequest, ModelResponse};

pub(super) use crate::company::CompanyManifest;
pub(super) use crate::harness::provider::{HarnessModel, MockProvider};
pub(super) use crate::ports::brain::CycleHost;
// Issue #301: every lifecycle return now lands in To-do (the `backlog` pool
// is gone), so these assertions read the const rather than a literal.
pub(super) use crate::ports::tasks::{COLUMN_PAUSED, COLUMN_TODO};
pub(super) use crate::ports::types::{
    ApprovalId, CompanyId, ContextOp, ContextOpResult, Effect, EffectDisposition, OverlayAgent,
    ToolCall, ToolResult,
};
pub(super) use crate::store::{FsCompanyStore, FsContextStore, FsOps};

/// A minimal card, used across the brain tests wherever the assertion is
/// about dispatch/lifecycle plumbing rather than the card's own content.
pub(super) fn card(id: &str, assignee: &str) -> TaskRecord {
    TaskRecord {
        id: id.to_string(),
        title: TaskTitle::authored("Ship the thing"),
        note: None,
        column: "in_progress".to_string(),
        priority: "high".to_string(),
        assignee: assignee.to_string(),
        updated_at_millis: 0,
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

/// A `CycleHost` that auto-executes anything the brain asks for and swallows
/// anything it parks; used by every test that isn't about approvals.
#[derive(Default)]
pub(super) struct NoopHost;

#[async_trait]
impl CycleHost for NoopHost {
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
        Ok(EffectDisposition::Executed)
    }
    async fn park_effect(&self, _effect: Effect) -> Result<ApprovalId> {
        Ok(ApprovalId::new("appr-parked"))
    }
}

/// A `CycleHost` that records every effect parked for approval, so the
/// approval drain can be asserted on (issue #172). Anything else it does is
/// inert.
#[derive(Default)]
pub(super) struct ParkingHost {
    pub(super) parked: std::sync::Mutex<Vec<Effect>>,
}

impl ParkingHost {
    /// The effects parked through `park_effect`, in order.
    pub(super) fn parked(&self) -> Vec<Effect> {
        self.parked.lock().expect("parked").clone()
    }
}

#[async_trait]
impl CycleHost for ParkingHost {
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
        let mut parked = self.parked.lock().expect("parked");
        parked.push(effect);
        Ok(ApprovalId::new(format!("appr-{}", parked.len())))
    }
}

pub(super) fn record() -> CompanyRecord {
    let manifest = toml::from_str(
        r#"
[company]
name = "Acme"

[policy]
mode = "full"

[[agent]]
id = "ceo"
role = "Chief Executive"
description = "Runs Acme."
"#,
    )
    .expect("valid manifest");
    CompanyRecord {
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: CompanyId::new("acme"),
        manifest,
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

pub(super) fn brain_over_mock(dir: &std::path::Path) -> HarnessBrain {
    brain_over_mock_with(dir, record())
}

/// [`brain_over_mock`] over a chosen record, so a test can vary the roster
/// (and its `[[harness]]` block) without restating the whole deps literal.
pub(super) fn brain_over_mock_with(dir: &std::path::Path, record: CompanyRecord) -> HarnessBrain {
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(MockProvider::new("mock: ")),
        provider_slug: "mock".to_string(),
        serves: None,
        context: Arc::new(FsContextStore::new(dir)),
        store: Arc::new(FsCompanyStore::new(dir)),
        meter: Some(Arc::new(FsOps::new(dir))),
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
        events: None,
        delegations: orchestrator::DelegationQueue::default(),
        workflow_runner: orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: crate::harness::mcp_probe::McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_revisions: None,
        approval_requests: crate::harness::policy::ApprovalRequestQueue::default(),
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
    HarnessBrain::new(Arc::new(HarnessPool::new()), deps, record)
}

pub(super) fn request(events: Vec<CompanyEvent>) -> CycleRequest {
    CycleRequest {
        cycle_id: "cycle-1".to_string(),
        company_id: CompanyId::new("acme"),
        events,
        event_seqs: Vec::new(),
        policy: None,
    }
}

// --- Task dispatch ------------------------------------------------------

/// A two-agent record so assignee routing has somewhere to route.
pub(super) fn record_two() -> CompanyRecord {
    let manifest = toml::from_str(
        r#"
[company]
name = "Acme"

[policy]
mode = "full"

[[agent]]
id = "ceo"
role = "Chief Executive"
description = "Runs Acme."

[[agent]]
id = "engineer"
role = "Engineer"
description = "Builds it."
"#,
    )
    .expect("valid manifest");
    CompanyRecord {
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: CompanyId::new("acme"),
        manifest,
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

/// A brain wired to a real task store (shared handle returned for seeding /
/// asserting), over the offline mock provider.
pub(super) fn brain_with_tasks(dir: &std::path::Path) -> (HarnessBrain, Arc<FsOps>) {
    brain_with_tasks_notified(dir, false)
}

/// As [`brain_with_tasks`], but with the journal wired too — so a test can
/// seed a card, settle it, and read back the `DeskTaskCompleted` the settle
/// wrote (issue #1890 B). [`FsOps`] is not an [`EventLog`], so the log is a
/// second store over the same directory.
pub(super) fn brain_with_tasks_and_events(
    dir: &std::path::Path,
) -> (HarnessBrain, Arc<FsOps>, Arc<dyn crate::ports::EventLog>) {
    let events: Arc<dyn crate::ports::EventLog> = Arc::new(crate::store::FsEventLog::new(dir));
    let (brain, tasks) = brain_with_tasks_notified_logging(dir, false, Some(events.clone()));
    (brain, tasks, events)
}

/// Same as [`brain_with_tasks`], but also wires the task store as the
/// notification store (issue #1865, PR #1883 review comment 3878668326):
/// [`FsOps`] implements both, so a test can seed a card, drive a cycle,
/// and then read back any `dispatch_failed` row a refusal filed.
pub(super) fn brain_with_tasks_notified(
    dir: &std::path::Path,
    notify: bool,
) -> (HarnessBrain, Arc<FsOps>) {
    brain_with_tasks_notified_logging(dir, notify, None)
}

/// As [`brain_with_tasks_notified`], but with the journal optionally wired
/// — so a test can seed a card, settle it, and read back the
/// `DeskTaskCompleted` the settle wrote (issue #1890 B). `None` is the
/// shape every caller had before, and `HarnessBrain` holds its deps behind
/// an `Arc`, so this has to be a build-time choice rather than a mutation
/// after the fact.
pub(super) fn brain_with_tasks_notified_logging(
    dir: &std::path::Path,
    notify: bool,
    events: Option<Arc<dyn crate::ports::EventLog>>,
) -> (HarnessBrain, Arc<FsOps>) {
    let tasks = Arc::new(FsOps::new(dir));
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: if notify { Some(tasks.clone()) } else { None },
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(MockProvider::new("mock: ")),
        provider_slug: "mock".to_string(),
        serves: None,
        context: Arc::new(FsContextStore::new(dir)),
        store: Arc::new(FsCompanyStore::new(dir)),
        meter: Some(Arc::new(FsOps::new(dir))),
        workspace_root: dir.to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.to_path_buf(),
        model_override: None,
        tasks: Some(tasks.clone()),
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events,
        delegations: orchestrator::DelegationQueue::default(),
        workflow_runner: orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: crate::harness::mcp_probe::McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_revisions: None,
        approval_requests: crate::harness::policy::ApprovalRequestQueue::default(),
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
    (
        HarnessBrain::new(Arc::new(HarnessPool::new()), deps, record_two()),
        tasks,
    )
}

/// A model whose every call fails with the exact wire shape
/// `is_top_level_budget_exhausted` recognises (issue #1846 review, Codex
/// #3864988168) — the same body `a_top_level_budget_exhaustion_pauses_
/// gracefully_and_parks_a_reissue_marker` in `mod.rs` scripts, reused here
/// to prove the DISPATCHED-CARD path settles on the pause rather than
/// completing.
pub(super) struct BudgetExhaustedProvider;

#[async_trait]
impl ChatModel<()> for BudgetExhaustedProvider {
    async fn invoke(
        &self,
        _state: &(),
        _request: ModelRequest,
    ) -> tinyinference::Result<ModelResponse> {
        Err(tinyinference::Error::Model(
            "USER_INSUFFICIENT_CREDITS: insufficient budget for this account — add credits \
             to continue"
                .to_string(),
        ))
    }
}

impl HarnessModel for BudgetExhaustedProvider {
    fn telemetry_provider_id(&self) -> String {
        "scripted".to_string()
    }
}

/// As [`brain_with_tasks`], but every model call fails with a
/// budget-exhausted body (issue #1846 review, Codex #3864988168) —
/// otherwise byte-identical, so the only variable a test built on this
/// exercises is how the dispatch path reacts to that one failure shape.
pub(super) fn brain_with_tasks_and_budget_exhausted_provider(
    dir: &std::path::Path,
) -> (HarnessBrain, Arc<FsOps>) {
    let tasks = Arc::new(FsOps::new(dir));
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(BudgetExhaustedProvider),
        provider_slug: "scripted".to_string(),
        serves: None,
        context: Arc::new(FsContextStore::new(dir)),
        store: Arc::new(FsCompanyStore::new(dir)),
        meter: Some(Arc::new(FsOps::new(dir))),
        workspace_root: dir.to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.to_path_buf(),
        model_override: None,
        tasks: Some(tasks.clone()),
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
        approval_requests: crate::harness::policy::ApprovalRequestQueue::default(),
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
    (
        HarnessBrain::new(Arc::new(HarnessPool::new()), deps, record_two()),
        tasks,
    )
}

/// As [`brain_with_tasks`], but the roster also carries an `eng` desk led by
/// the engineer — the shape `delegate_to_desk` writes into a card's
/// `assignee` (issue #205).
pub(super) fn brain_with_desk_tasks(dir: &std::path::Path) -> (HarnessBrain, Arc<FsOps>) {
    let (brain, tasks) = brain_with_tasks(dir);
    let group_chats = toml::from_str::<CompanyManifest>(
        r#"
[company]
name = "Acme"

[[agent]]
id = "engineer"
role = "Engineer"

[[group_chat]]
id = "eng"
name = "Engineering desk"
members = ["engineer"]
"#,
    )
    .expect("valid manifest")
    .group_chats;
    brain.mutate_record(|r| r.manifest.group_chats = group_chats);
    (brain, tasks)
}

/// As [`brain_with_tasks`], but with the artifact store wired to the same
/// [`FsOps`] handle (it implements both), so a dispatch's versioned output
/// is observable.
pub(super) fn brain_with_artifacts(dir: &std::path::Path) -> (HarnessBrain, Arc<FsOps>) {
    brain_with_stores(dir, false)
}

/// As [`brain_with_artifacts`], but with the workspace store wired to the
/// same [`FsOps`] handle too (it implements all three), so issue #552's
/// dual write into the shared tree is observable.
///
/// A separate constructor rather than a change to the one above: leaving
/// `brain_with_artifacts` workspace-less is what keeps every pre-existing
/// publish test on the artifact-only path, which is the guarantee that an
/// unwired workspace behaves exactly as it did before this cell.
pub(super) fn brain_with_artifacts_and_workspace(
    dir: &std::path::Path,
) -> (HarnessBrain, Arc<FsOps>) {
    brain_with_stores(dir, true)
}

pub(super) fn brain_with_stores(
    dir: &std::path::Path,
    with_workspace: bool,
) -> (HarnessBrain, Arc<FsOps>) {
    let ops = Arc::new(FsOps::new(dir));
    let artifacts = ops.clone() as Arc<dyn crate::ports::artifacts::ArtifactStore>;
    brain_with_injected_artifacts(dir, ops, artifacts, with_workspace)
}

/// As [`brain_with_stores`], but with the artifact store supplied by the
/// caller — so a test can make `upsert` refuse and observe what the publish
/// drain did to the *tree* before it got there.
///
/// That is the only way to pin issue #552's write ordering. An ordering
/// described in a comment is not an ordering: the next refactor reorders it
/// and nothing objects.
pub(super) fn brain_with_injected_artifacts(
    dir: &std::path::Path,
    ops: Arc<FsOps>,
    artifacts: Arc<dyn crate::ports::artifacts::ArtifactStore>,
    with_workspace: bool,
) -> (HarnessBrain, Arc<FsOps>) {
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(MockProvider::new("mock: ")),
        provider_slug: "mock".to_string(),
        serves: None,
        context: Arc::new(FsContextStore::new(dir)),
        store: Arc::new(FsCompanyStore::new(dir)),
        meter: Some(Arc::new(FsOps::new(dir))),
        workspace_root: dir.to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.to_path_buf(),
        model_override: None,
        tasks: Some(ops.clone()),
        artifacts: Some(artifacts),
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
        approval_requests: crate::harness::policy::ApprovalRequestQueue::default(),
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
        workspace: with_workspace.then(|| ops.clone() as Arc<dyn crate::ports::WorkspaceStore>),
        workflow_runs: None,
        deep_trace: None,
    };
    (
        HarnessBrain::new(Arc::new(HarnessPool::new()), deps, record_two()),
        ops,
    )
}
