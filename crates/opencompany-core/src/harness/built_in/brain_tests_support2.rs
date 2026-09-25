use super::*;
use crate::ports::TaskStore;
use crate::ports::artifacts::ArtifactRecord;

// -- issue #552: the write ordering, proven by failure injection ---------

/// An [`ArtifactStore`](crate::ports::artifacts::ArtifactStore) that refuses
/// `upsert` from the Nth call onward, delegating everything else.
///
/// The instrument the ordering tests need: with the artifact write made to
/// fail at a chosen point, what the *tree* holds afterwards says
/// unambiguously which surface was written first.
pub(super) struct FailingArtifacts {
    pub(super) inner: Arc<FsOps>,
    /// How many `upsert` calls succeed before the rest refuse.
    pub(super) allowed: std::sync::atomic::AtomicUsize,
    pub(super) seen: std::sync::atomic::AtomicUsize,
}

impl FailingArtifacts {
    pub(super) fn new(inner: Arc<FsOps>, allowed: usize) -> Arc<Self> {
        Arc::new(Self {
            inner,
            allowed: std::sync::atomic::AtomicUsize::new(allowed),
            seen: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Let every later `upsert` through again, so a test can publish
    /// normally after the injected failure and watch the repair.
    pub(super) fn heal(&self) {
        self.allowed
            .store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl crate::ports::artifacts::ArtifactStore for FailingArtifacts {
    async fn list(
        &self,
        company: &CompanyId,
        task_id: Option<&str>,
    ) -> crate::Result<Vec<ArtifactRecord>> {
        crate::ports::artifacts::ArtifactStore::list(&*self.inner, company, task_id).await
    }
    async fn get(&self, company: &CompanyId, id: &str) -> crate::Result<Option<ArtifactRecord>> {
        crate::ports::artifacts::ArtifactStore::get(&*self.inner, company, id).await
    }
    async fn upsert(&self, company: &CompanyId, artifact: &ArtifactRecord) -> crate::Result<()> {
        let n = self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n >= self.allowed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::error::OpenCompanyError::Store(
                "artifact store is down".to_string(),
            ));
        }
        crate::ports::artifacts::ArtifactStore::upsert(&*self.inner, company, artifact).await
    }
    async fn delete(&self, company: &CompanyId, id: &str) -> crate::Result<bool> {
        crate::ports::artifacts::ArtifactStore::delete(&*self.inner, company, id).await
    }
}

pub(super) fn publish_of(source: &str, body: &str) -> crate::harness::publish::PendingPublish {
    crate::harness::publish::PendingPublish {
        agent: "maya".to_string(),
        source: source.to_string(),
        title: "Launch spec".to_string(),
        kind: crate::ports::artifacts::ArtifactKind::Markdown,
        note: None,
        payload: crate::harness::publish::PublishPayload::Text(body.to_string()),
    }
}

/// The named node under `agents/maya/t-1/`, with its body — the tree's own
/// answer, read without going through the artifact chain at all.
pub(super) async fn note_in_tree(
    ops: &FsOps,
    company: &CompanyId,
    name: &str,
) -> Option<(String, String)> {
    use crate::ports::workspace::WorkspaceStore;
    let nodes = WorkspaceStore::tree(ops, company).await.unwrap();
    let found = nodes.iter().find(|n| n.name == name)?;
    let (_, body) = WorkspaceStore::read(ops, company, &found.id)
        .await
        .unwrap()?;
    Some((found.id.clone(), body))
}

// ── Issue #151 §3.2: a finished card answers where it was asked ──────

// The post-back's *text* rules — title, landing status, note folding,
// whitespace-only notes — moved with the renderer to
// `crate::harness::lifecycle` (issue #186), which owns them now and covers
// each case plus the new assignee-credit rule. What stays here is the
// wiring: that `run_task` reaches the relay at all, and attributes it to
// the orchestrator.

pub(super) async fn only_card(tasks: &Arc<FsOps>) -> TaskRecord {
    tasks
        .list(&CompanyId::new("acme"))
        .await
        .expect("list")
        .into_iter()
        .next()
        .expect("one card")
}

// ── Issue #337: every finished card stops for a person ────────────────

// ── Issue #242: the attempt row records what the dispatch actually did ──

/// Wires a run store onto a task-capable brain, mints the `Pending` row the
/// dispatch choke point would have minted, and returns both.
pub(super) async fn brain_with_a_pending_run(
    dir: &std::path::Path,
    assignee: &str,
) -> (HarnessBrain, Arc<FsOps>, Arc<dyn crate::ports::RunStore>) {
    use crate::ports::runs::NewRun;

    let (brain, tasks) = brain_with_tasks(dir);
    let runs: Arc<dyn crate::ports::RunStore> = Arc::new(FsOps::new(dir));
    let company = CompanyId::new("acme");
    tasks
        .upsert(&company, &card("t-1", assignee))
        .await
        .expect("seed");
    runs.create_run(&company, NewRun::for_task("run-1", "t-1", assignee))
        .await
        .expect("mint");
    (brain.with_runs(Arc::clone(&runs)), tasks, runs)
}

// ── Issue #205: the working agent is linked, and a bad assignee is refused ──

// --- Orchestrator routing + delegation ----------------------------------

/// A roster with an `orchestrator`-tier agent (not first) and a desk.
pub(super) fn record_with_desk() -> CompanyRecord {
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
id = "chief"
role = "Chief of Staff"
tier = "orchestrator"
description = "Coordinates the company."

[[agent]]
id = "engineer"
role = "Engineer"
description = "Builds it."

[[group_chat]]
id = "eng_desk"
name = "Engineering"
members = ["engineer"]
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

/// A roster with a desk whose id collides with a teammate id — the exact
/// shape `runtime::delegation_tools::a_prefixed_dm_reaches_the_teammate_
/// even_when_a_desk_shares_the_id` (issue #1743) exercises for
/// `chat_responder`. Manifest validation does not forbid the collision.
pub(super) fn record_with_colliding_desk_and_teammate_id() -> CompanyRecord {
    let manifest = toml::from_str(
        r#"
[company]
name = "Acme"

[[agent]]
id = "chief"
role = "Chief of Staff"
tier = "orchestrator"
description = "Coordinates the company."

[[agent]]
id = "engineer"
role = "Engineer"
description = "Builds it."

[[group_chat]]
id = "engineer"
name = "Engineering desk"
members = ["chief"]
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

/// A brain over `record`, wired to a real task store.
pub(super) fn brain_over(
    dir: &std::path::Path,
    record: CompanyRecord,
) -> (HarnessBrain, Arc<FsOps>) {
    let tasks = Arc::new(FsOps::new(dir));
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
        HarnessBrain::new(Arc::new(HarnessPool::new()), deps, record),
        tasks,
    )
}

/// A brain over the desk-bearing record, wired to a real task store.
pub(super) fn brain_with_desk(dir: &std::path::Path) -> (HarnessBrain, Arc<FsOps>) {
    brain_over(dir, record_with_desk())
}

// -----------------------------------------------------------------------
// Mention routing: naming somebody outranks the desk lead
// -----------------------------------------------------------------------

pub(super) fn mention_of(id: &str) -> crate::ports::types::Mention {
    crate::ports::types::Mention {
        target: crate::ports::types::MentionTarget::Agent { id: id.to_string() },
        text: format!("@{id}"),
        offset: 0,
        quiet: false,
    }
}

// ── Issue #151 §3.3: a DM thread reaches the teammate it names ──

// ── Issue #1743: who answers the built-in `#general` channel ──

// ── Issue #884 D2: an unresolvable chat key is no longer silent ──

/// Captures everything logged on **this thread** while `body` runs.
///
/// Thread-local (`with_default`) rather than a global default on purpose:
/// `workflow_scheduler`'s capture already claims the process-wide slot in
/// this same test binary and asserts it wins that race, so installing a
/// second global here would turn its test red for an unrelated reason.
pub(super) fn logs_from(body: impl FnOnce()) -> String {
    use std::io::Write;
    use std::sync::Mutex;

    #[derive(Clone)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    struct Writer(Arc<Mutex<Vec<u8>>>);
    impl Write for Writer {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log sink").extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
        type Writer = Writer;
        fn make_writer(&'a self) -> Self::Writer {
            Writer(self.0.clone())
        }
    }

    let sink = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Sink(sink.clone()))
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, body);
    let bytes = sink.lock().expect("log sink").clone();
    String::from_utf8_lossy(&bytes).to_string()
}

// ── Issue #186 part b: orchestrator lifecycle authority ────────────────

// --- Approval parking (issue #172) --------------------------------------

/// A brain over `dir` whose deps carry `requests` as the shared
/// approval-request queue — the same handle every roster agent's
/// `ApprovalPolicy` pushes onto.
pub(super) fn brain_with_approval_queue(
    dir: &std::path::Path,
    requests: crate::harness::policy::ApprovalRequestQueue,
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
