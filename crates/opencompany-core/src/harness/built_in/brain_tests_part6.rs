use super::*;

/// The other side of the same contract: a turn that opened no card must
/// leave the field empty, so no bubble grows a "card opened" chip it has
/// not earned.
#[tokio::test]
async fn a_turn_that_opens_no_card_reports_none() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, _provider) = brain_that_delegates(dir.path(), Vec::new());

    let result = brain
        .run_cycle(
            request(vec![CompanyEvent::OperatorMessage {
                mentions: Vec::new(),
                parent: None,
                text: "status?".into(),
                by: None,
                chat: None,
                deliverable: None,
                attachments: Vec::new(),
            }]),
            &NoopHost,
        )
        .await
        .expect("cycle runs");

    assert!(
        result.channel_responses[0].task_id.is_none(),
        "an ordinary chat turn must not claim a card"
    );
}

/// `assign_task` changes who owns an existing card, records the change in
/// the orchestrator's voice, and — deliberately — does **not** touch the
/// column: dispatch fires from `CompanyRuntime::upsert_task`, which the
/// `TaskStore` port this drain writes through cannot reach.
#[tokio::test]
async fn assign_task_reassigns_the_card_without_dispatching_it() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, tasks) = brain_with_tasks(dir.path());
    let mut c = card("t-assign", "engineer");
    c.column = COLUMN_TODO.to_string();
    tasks.upsert(&CompanyId::new("acme"), &c).await.unwrap();

    let out = brain
        .run_delegation(
            Delegation::AssignTask {
                task_id: "t-assign".to_string(),
                assignee: "ceo".to_string(),
                note: Some("closer to the customer".to_string()),
            },
            None,
        )
        .await
        .expect("delegation runs");
    assert!(
        out.bubble.is_none() && out.desk_reply.is_none(),
        "the orchestrator is mid-turn; a second voice here would be it talking to itself"
    );

    let after = only_card(&tasks).await;
    assert_eq!(after.assignee, "ceo");
    assert_eq!(
        after.column, COLUMN_TODO,
        "assignment records ownership; it must not start the work"
    );
    let note = after.note.expect("note");
    assert!(note.contains("assigned to ceo"), "{note}");
    assert!(note.contains("closer to the customer"), "{note}");
    assert!(
        note.contains(&format!("[{}]", brain.orchestrator())),
        "the assignment is recorded in the orchestrator's voice: {note}"
    );
}

/// #205: `assign_task` takes its `assignee` from an LLM tool call, so it can
/// name somebody the company does not have just as easily as the operator's
/// free-text field can. The bad name must not reach the card — the previous
/// owner stays, and the refusal is recorded on the note.
#[tokio::test]
async fn assign_task_refuses_an_off_roster_assignee_and_keeps_the_current_owner() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, tasks) = brain_with_tasks(dir.path());
    let mut c = card("t-assign", "engineer");
    c.column = COLUMN_TODO.to_string();
    tasks.upsert(&CompanyId::new("acme"), &c).await.unwrap();

    brain
        .run_delegation(
            Delegation::AssignTask {
                task_id: "t-assign".to_string(),
                assignee: "Shane".to_string(),
                note: None,
            },
            None,
        )
        .await
        .expect("delegation runs");

    let after = only_card(&tasks).await;
    assert_eq!(
        after.assignee, "engineer",
        "a name nobody answers to must not displace the real owner"
    );
    let note = after.note.expect("note");
    assert!(note.contains("could not assign to Shane"), "{note}");
}

/// #214 review: a blank `assignee` resolves to `Unassigned`, whose canonical
/// form is `""`. Clearing the owner is correct — unassigning is a real
/// request — but the note must say so. It used to fall through the named
/// arm and record `assigned to ` with nothing after it.
#[tokio::test]
async fn assign_task_with_a_blank_assignee_clears_the_owner_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, tasks) = brain_with_tasks(dir.path());
    let mut c = card("t-assign", "engineer");
    c.column = COLUMN_TODO.to_string();
    tasks.upsert(&CompanyId::new("acme"), &c).await.unwrap();

    brain
        .run_delegation(
            Delegation::AssignTask {
                task_id: "t-assign".to_string(),
                assignee: "   ".to_string(),
                note: None,
            },
            None,
        )
        .await
        .expect("delegation runs");

    let after = only_card(&tasks).await;
    assert_eq!(
        after.assignee, "",
        "a blank assignee unassigns the card, which is a legitimate write"
    );
    let note = after.note.expect("note");
    assert!(
        note.contains("cleared the assignee"),
        "the note names the effect rather than trailing off: {note}"
    );
    assert!(
        !note.contains("assigned to "),
        "the truncated 'assigned to <nothing>' note must not come back: {note}"
    );
}

/// Approving finishes a board-created card: this is #171's `in_review →
/// done` write (PR #179) for the card shape #179's own origin rule cannot
/// reach, with the verdict recorded on the note.
#[tokio::test]
async fn review_approve_records_the_verdict_and_completes_the_card() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, tasks) = brain_with_tasks(dir.path());
    let mut c = card("t-review", "engineer");
    c.column = "in_review".to_string();
    tasks.upsert(&CompanyId::new("acme"), &c).await.unwrap();

    brain
        .run_delegation(
            Delegation::ReviewTask {
                task_id: "t-review".to_string(),
                decision: lifecycle::ReviewDecision::Approve,
                note: Some("ships as-is".to_string()),
            },
            None,
        )
        .await
        .expect("delegation runs");

    let after = only_card(&tasks).await;
    assert_eq!(
        after.column, "done",
        "an approving verdict is the in_review -> done transition (#171)"
    );
    let note = after.note.expect("note");
    assert!(note.contains("reviewed: approved"), "{note}");
    assert!(note.contains("ships as-is"), "{note}");
}

/// `revise` is a transition #186 does own: the card goes back to the
/// To-do so it can be picked up and re-dispatched.
#[tokio::test]
async fn review_revise_sends_the_card_back_to_todo() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, tasks) = brain_with_tasks(dir.path());
    let mut c = card("t-revise", "engineer");
    c.column = "in_review".to_string();
    tasks.upsert(&CompanyId::new("acme"), &c).await.unwrap();

    brain
        .run_delegation(
            Delegation::ReviewTask {
                task_id: "t-revise".to_string(),
                decision: lifecycle::ReviewDecision::Revise,
                note: None,
            },
            None,
        )
        .await
        .expect("delegation runs");

    let after = only_card(&tasks).await;
    assert_eq!(after.column, COLUMN_TODO);
    assert!(
        after.note.expect("note").contains("needs another pass"),
        "the verdict must be recorded even without a reviewer comment"
    );
}

/// A card that has since been deleted is a silent no-op, matching every
/// other task path in this file — never an error that kills the turn.
#[tokio::test]
async fn a_lifecycle_delegation_for_a_missing_card_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, tasks) = brain_with_tasks(dir.path());

    for delegation in [
        Delegation::AssignTask {
            task_id: "ghost".to_string(),
            assignee: "ceo".to_string(),
            note: None,
        },
        Delegation::ReviewTask {
            task_id: "ghost".to_string(),
            decision: lifecycle::ReviewDecision::Approve,
            note: None,
        },
    ] {
        let out = brain
            .run_delegation(delegation, None)
            .await
            .expect("a missing card must not error");
        assert!(out.bubble.is_none() && out.desk_reply.is_none());
    }
    assert!(
        tasks
            .list(&CompanyId::new("acme"))
            .await
            .unwrap()
            .is_empty()
    );
}

/// A `delegate_to_desk` delegation runs the desk lead and hands its reply
/// back to relay (a `DeskReply` attributed to the lead, no standalone
/// bubble); an unknown desk yields nothing.
#[tokio::test]
async fn delegate_to_desk_delegation_answers_as_the_desk_lead() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, _tasks) = brain_with_desk(dir.path());
    // The pool must have the roster before a member turn can run.
    brain
        .pool
        .ensure(&brain.record(), &brain.deps)
        .await
        .expect("roster");

    let out = brain
        .run_delegation(
            Delegation::DelegateToDesk {
                desk: "eng_desk".to_string(),
                instruction: "ship-marker".to_string(),
            },
            None,
        )
        .await
        .expect("delegation runs");
    // The answer comes back as a DeskReply to relay — not a standalone
    // bubble — attributed to the desk lead, and the mock provider echoes the
    // instruction, proving the member's turn ran.
    assert!(
        out.bubble.is_none(),
        "the desk reply is relayed, not bubbled"
    );
    let desk = out.desk_reply.expect("desk lead replies");
    assert_eq!(desk.member, "engineer");
    assert!(desk.reply.contains("ship-marker"), "{:?}", desk.reply);

    // An unknown desk delegates to nobody.
    let none = brain
        .run_delegation(
            Delegation::DelegateToDesk {
                desk: "ghost".to_string(),
                instruction: "hello".to_string(),
            },
            None,
        )
        .await
        .expect("delegation runs");
    assert!(
        none.bubble.is_none() && none.desk_reply.is_none(),
        "an unknown desk yields nothing"
    );
}

/// A recorded MCP failure re-skins into an **error step** on the operator
/// bubble's timeline AND a scrubbed `McpCallFailed` audit event when the
/// event log is wired (the Activity-trace re-skin of the old warning bubble).
#[tokio::test]
async fn mcp_failures_surface_as_error_steps_and_event() {
    use crate::harness::mcp_probe::McpFailure;
    use crate::ports::EventLog;
    use crate::ports::types::EventSeq;
    use crate::store::FsEventLog;

    let dir = tempfile::tempdir().unwrap();
    let events: Arc<dyn EventLog> = Arc::new(FsEventLog::new(dir.path()));
    let failures = crate::harness::mcp_probe::McpFailureQueue::default();
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(MockProvider::new("mock: ")),
        provider_slug: "mock".to_string(),
        serves: None,
        context: Arc::new(FsContextStore::new(dir.path())),
        store: Arc::new(FsCompanyStore::new(dir.path())),
        meter: None,
        workspace_root: dir.path().to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.path().to_path_buf(),
        model_override: None,
        tasks: None,
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events: Some(events.clone()),
        delegations: orchestrator::DelegationQueue::default(),
        workflow_runner: orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: failures.clone(),
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
    let brain = HarnessBrain::new(Arc::new(HarnessPool::new()), deps, record());

    // A failure recorded during the turn (its message already scrubbed).
    failures.push(McpFailure {
        server: "browserbase".into(),
        tool: "browse".into(),
        status: "tool_call_rejected".into(),
        hint: None,
        scrubbed_message: "server rejected the call".into(),
    });

    let mut steps: Vec<TurnStep> = Vec::new();
    // `None` — this is the chat-turn drain, which journals no `task_id`
    // (#185). The dispatch drain passes the card id; see `run_task`.
    brain
        .surface_mcp_failures(&mut steps, None)
        .await
        .expect("drain surfaces failures");

    assert_eq!(steps.len(), 1, "one error step");
    assert_eq!(steps[0].kind, TurnStepKind::Note);
    assert_eq!(steps[0].status, TurnStepStatus::Error);
    assert!(
        steps[0].label.contains("browserbase"),
        "{:?}",
        steps[0].label
    );
    assert_eq!(steps[0].detail.as_deref(), Some("server rejected the call"));

    let logged = events
        .read_from(&CompanyId::new("acme"), EventSeq::new(0), usize::MAX)
        .await
        .expect("read events");
    assert!(
        logged.iter().any(|e| matches!(
            &e.event,
            CompanyEvent::McpCallFailed { server, status, .. }
                if server == "browserbase" && status == "tool_call_rejected"
        )),
        "an McpCallFailed audit event was journaled"
    );
}

/// #185 review follow-up: one bad journal write must not swallow the rest of
/// the batch.
///
/// `McpFailureQueue::drain` is a `mem::take` — by the time the loop runs the
/// queue is empty and the batch exists only in that iterator. Propagating
/// the first append error with `?` therefore did not merely skip one audit
/// event, it discarded every failure behind it with nothing left to retry
/// from. Journaling is per-item best-effort so the drain always completes.
#[tokio::test]
async fn a_failed_journal_write_does_not_swallow_the_rest_of_the_drain() {
    use crate::harness::mcp_probe::McpFailure;
    use crate::ports::EventLog;
    use crate::ports::types::{EventSeq, StoredEvent};
    use futures::stream::{self, BoxStream};

    /// An event log whose FIRST append fails and whose later appends
    /// succeed, recording what got through.
    #[derive(Default)]
    struct FailFirstLog {
        seen: StdMutex<Vec<CompanyEvent>>,
        appends: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl EventLog for FailFirstLog {
        async fn append(&self, _id: &CompanyId, event: CompanyEvent) -> Result<EventSeq> {
            let nth = self
                .appends
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if nth == 0 {
                return Err(crate::error::OpenCompanyError::Store(
                    "journal unavailable".to_string(),
                ));
            }
            let mut guard = self.seen.lock().unwrap();
            guard.push(event);
            Ok(EventSeq::new(guard.len() as u64))
        }
        async fn read_from(
            &self,
            _id: &CompanyId,
            _seq: EventSeq,
            _limit: usize,
        ) -> Result<Vec<StoredEvent>> {
            Ok(Vec::new())
        }
        fn subscribe(
            &self,
            _id: &CompanyId,
        ) -> BoxStream<'static, crate::ports::events::EventStreamItem> {
            Box::pin(stream::empty())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(FailFirstLog::default());
    let failures = crate::harness::mcp_probe::McpFailureQueue::default();
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(MockProvider::new("mock: ")),
        provider_slug: "mock".to_string(),
        serves: None,
        context: Arc::new(FsContextStore::new(dir.path())),
        store: Arc::new(FsCompanyStore::new(dir.path())),
        meter: None,
        workspace_root: dir.path().to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.path().to_path_buf(),
        model_override: None,
        tasks: None,
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events: Some(log.clone()),
        delegations: orchestrator::DelegationQueue::default(),
        workflow_runner: orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: failures.clone(),
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
    let brain = HarnessBrain::new(Arc::new(HarnessPool::new()), deps, record());

    for server in ["first", "second", "third"] {
        failures.push(McpFailure {
            server: server.into(),
            tool: "browse".into(),
            status: "tool_call_rejected".into(),
            hint: None,
            scrubbed_message: "server rejected the call".into(),
        });
    }

    let mut steps: Vec<TurnStep> = Vec::new();
    brain
        .surface_mcp_failures(&mut steps, Some("t1"))
        .await
        .expect("a journal error is best-effort, not fatal");

    // Every failure is re-skinned onto the timeline regardless…
    assert_eq!(steps.len(), 3, "all three failures surfaced as steps");
    // …and the two after the failed write still reached the journal. Before
    // this fix `seen` was empty: the `?` returned on `first` and `second` /
    // `third` were dropped with the drained batch.
    let seen = log.seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "the drain continued past the failed append");
    assert!(
        seen.iter().any(|e| matches!(
            e,
            CompanyEvent::McpCallFailed { server, .. } if server == "third"
        )),
        "the last failure in the batch was still journaled"
    );
}
