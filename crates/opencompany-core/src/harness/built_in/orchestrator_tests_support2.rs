use super::super::*;
use crate::ports::tasks::TaskTitle;
use crate::ports::types::{CompanyRecord, CompanySummary, LedgerEntry};
use std::sync::Mutex as StdMutex;

// -----------------------------------------------------------------------
// Issue #661: the queue is scoped per claimant
// -----------------------------------------------------------------------

/// A card, titled so a drain can be identified by what it carried.
pub(super) fn card(title: &str) -> Delegation {
    Delegation::SpawnTask {
        title: title.to_string(),
        note: None,
        assignee: None,
    }
}

pub(super) fn hand_off() -> Delegation {
    Delegation::DelegateToDesk {
        desk: "design".to_string(),
        instruction: "have a look".to_string(),
    }
}

pub(super) fn titles(drained: Vec<Delegation>) -> Vec<String> {
    drained
        .into_iter()
        .map(|d| match d {
            Delegation::SpawnTask { title, .. } => title,
            other => panic!("expected a card, got {other:?}"),
        })
        .collect()
}

pub(super) fn stage(queue: &DelegationQueue, d: Delegation) -> Staged {
    queue.push_within_cap(d, MAX_DELEGATIONS_PER_TURN, NO_DEPTH_BOUND)
}

// -----------------------------------------------------------------------
// Issue #1859: the execution-state read trio (`list_tasks` / `read_task` /
// `read_run`) and `query_company`'s `## Board` section.
// -----------------------------------------------------------------------

/// A minimal board card, for fixtures below. Named `task_card` rather than
/// `card` — that name is already the `Delegation` fixture above.
pub(super) fn task_card(id: &str, title: &str, column: &str, assignee: &str) -> TaskRecord {
    TaskRecord {
        id: id.to_string(),
        title: TaskTitle::authored(title),
        note: None,
        column: column.to_string(),
        priority: "medium".to_string(),
        assignee: assignee.to_string(),
        updated_at_millis: 1,
        origin: crate::ports::TaskOrigin::new(None, None),
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

/// A task board that cannot answer, so a read failure never collapses
/// into an empty or missing board.
pub(super) struct BrokenTaskStore;

#[async_trait]
impl TaskStore for BrokenTaskStore {
    async fn list(&self, _company: &CompanyId) -> crate::Result<Vec<TaskRecord>> {
        Err(OpenCompanyError::Store(
            "simulated board read failure".into(),
        ))
    }
    async fn upsert(&self, _company: &CompanyId, _task: &TaskRecord) -> crate::Result<()> {
        unimplemented!("not exercised by these tests")
    }
    async fn update_if_column(
        &self,
        _company: &CompanyId,
        _task: &TaskRecord,
        _observed: &TaskRecord,
        _expected_column: &str,
    ) -> crate::Result<bool> {
        unimplemented!("not exercised by these tests")
    }
    async fn delete(&self, _company: &CompanyId, _id: &str) -> crate::Result<bool> {
        unimplemented!("not exercised by these tests")
    }
}

/// A run store that cannot answer, so a run-history read failure never
/// collapses into "no attempts" or a missing run — the same distinction
/// `list_tasks`/`read_task`'s board read already makes for [`TaskStore`].
pub(super) struct BrokenRunStore;

#[async_trait]
impl RunStore for BrokenRunStore {
    async fn create_run(
        &self,
        _company: &CompanyId,
        _spec: crate::ports::runs::NewRun,
    ) -> crate::Result<RunRecord> {
        unimplemented!("not exercised by these tests")
    }
    async fn get_run(&self, _company: &CompanyId, _id: &str) -> crate::Result<Option<RunRecord>> {
        Err(OpenCompanyError::Store(
            "simulated run-store read failure".into(),
        ))
    }
    async fn put_run(&self, _company: &CompanyId, _run: &RunRecord) -> crate::Result<()> {
        unimplemented!("not exercised by these tests")
    }
    async fn list_runs(
        &self,
        _company: &CompanyId,
        _filter: &RunFilter,
    ) -> crate::Result<Vec<RunRecord>> {
        Err(OpenCompanyError::Store(
            "simulated run-history read failure".into(),
        ))
    }
    async fn append_run_step(
        &self,
        _company: &CompanyId,
        _step: &crate::ports::runs::RunStepRecord,
    ) -> crate::Result<()> {
        unimplemented!("not exercised by these tests")
    }
    async fn list_run_steps(
        &self,
        _company: &CompanyId,
        _run_id: &str,
    ) -> crate::Result<Vec<crate::ports::runs::RunStepRecord>> {
        unimplemented!("not exercised by these tests")
    }
}

/// A run store that answers `get_run` but never `list_runs`, to isolate
/// [`ReadRunTool`]'s agent-attempt lookup from its journal fallback.
pub(super) struct FailingGetRun;

#[async_trait]
impl RunStore for FailingGetRun {
    async fn create_run(
        &self,
        _company: &CompanyId,
        _spec: crate::ports::runs::NewRun,
    ) -> crate::Result<RunRecord> {
        unimplemented!("not exercised by these tests")
    }
    async fn get_run(&self, _company: &CompanyId, _id: &str) -> crate::Result<Option<RunRecord>> {
        Err(OpenCompanyError::Store(
            "simulated run-store read failure".into(),
        ))
    }
    async fn put_run(&self, _company: &CompanyId, _run: &RunRecord) -> crate::Result<()> {
        unimplemented!("not exercised by these tests")
    }
    async fn list_runs(
        &self,
        _company: &CompanyId,
        _filter: &RunFilter,
    ) -> crate::Result<Vec<RunRecord>> {
        unimplemented!("not exercised by these tests")
    }
    async fn append_run_step(
        &self,
        _company: &CompanyId,
        _step: &crate::ports::runs::RunStepRecord,
    ) -> crate::Result<()> {
        unimplemented!("not exercised by these tests")
    }
    async fn list_run_steps(
        &self,
        _company: &CompanyId,
        _run_id: &str,
    ) -> crate::Result<Vec<crate::ports::runs::RunStepRecord>> {
        unimplemented!("not exercised by these tests")
    }
}

/// An event log that always fails `read_from`, to prove
/// [`ReadRunTool`]'s workflow-run fallback distinguishes a journal read
/// failure from a genuinely absent run.
pub(super) struct BrokenEventLog;

#[async_trait]
impl EventLog for BrokenEventLog {
    async fn append(&self, _id: &CompanyId, _event: CompanyEvent) -> crate::Result<EventSeq> {
        unreachable!("read_run only reads")
    }
    async fn read_from(
        &self,
        _id: &CompanyId,
        _seq: EventSeq,
        _limit: usize,
    ) -> crate::Result<Vec<crate::ports::types::StoredEvent>> {
        Err(OpenCompanyError::Store(
            "simulated event-log read failure".into(),
        ))
    }
    fn subscribe(
        &self,
        _id: &CompanyId,
    ) -> futures::stream::BoxStream<'static, crate::ports::events::EventStreamItem> {
        Box::pin(futures::stream::empty())
    }
}

/// An artifact store that cannot answer, so an output-surface read
/// failure never collapses into "nothing published".
pub(super) struct BrokenArtifactStore;

#[async_trait]
impl ArtifactStore for BrokenArtifactStore {
    async fn list(
        &self,
        _company: &CompanyId,
        _task_id: Option<&str>,
    ) -> crate::Result<Vec<crate::ports::artifacts::ArtifactRecord>> {
        Err(OpenCompanyError::Store(
            "simulated artifact-store read failure".into(),
        ))
    }
    async fn get(
        &self,
        _company: &CompanyId,
        _id: &str,
    ) -> crate::Result<Option<crate::ports::artifacts::ArtifactRecord>> {
        unimplemented!("not exercised by these tests")
    }
    async fn upsert(
        &self,
        _company: &CompanyId,
        _artifact: &crate::ports::artifacts::ArtifactRecord,
    ) -> crate::Result<()> {
        unimplemented!("not exercised by these tests")
    }
    async fn delete(&self, _company: &CompanyId, _id: &str) -> crate::Result<bool> {
        unimplemented!("not exercised by these tests")
    }
}

// -- FAIL-axis: cross-tenant reach, ungrounded targets, unbounded growth -

/// A `RunStore` that genuinely partitions by company — the shape every
/// real backend promises — so a lookup under one company can never answer
/// with a row filed under another.
pub(super) struct TenantScopedRunStore {
    pub(super) rows: std::sync::Mutex<Vec<RunRecord>>,
}

#[async_trait]
impl RunStore for TenantScopedRunStore {
    async fn create_run(
        &self,
        _company: &CompanyId,
        _spec: crate::ports::runs::NewRun,
    ) -> crate::Result<RunRecord> {
        unimplemented!("not exercised by this test")
    }
    async fn get_run(&self, company: &CompanyId, id: &str) -> crate::Result<Option<RunRecord>> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .find(|r| &r.company == company && r.id == id)
            .cloned())
    }
    async fn put_run(&self, _company: &CompanyId, _run: &RunRecord) -> crate::Result<()> {
        unimplemented!("not exercised by this test")
    }
    async fn list_runs(
        &self,
        _company: &CompanyId,
        _filter: &RunFilter,
    ) -> crate::Result<Vec<RunRecord>> {
        unimplemented!("not exercised by this test")
    }
    async fn append_run_step(
        &self,
        _company: &CompanyId,
        _step: &crate::ports::runs::RunStepRecord,
    ) -> crate::Result<()> {
        unimplemented!("not exercised by this test")
    }
    async fn list_run_steps(
        &self,
        _company: &CompanyId,
        _run_id: &str,
    ) -> crate::Result<Vec<crate::ports::runs::RunStepRecord>> {
        unimplemented!("not exercised by this test")
    }
}

pub(super) fn tenant_run(company: &str, id: &str) -> RunRecord {
    RunRecord {
        id: id.to_string(),
        company: CompanyId::new(company),
        task_id: None,
        chat_id: None,
        agent_id: "ceo".to_string(),
        attempt: 1,
        status: crate::ports::runs::RunStatus::Running,
        trigger_event_seq: None,
        thread_root: None,
        created_at_millis: 1_000,
        started_at_millis: None,
        finished_at_millis: None,
        error: None,
        usage: crate::ports::types::TokenUsage::default(),
        step_count: 0,
        workflow_run_id: None,
        node_id: None,
        episode_id: None,
        round_revision: None,
    }
}

/// A `CompanyStore` that genuinely partitions by company — unlike
/// `MemStore`, which ignores the `id` argument and answers for whichever
/// company it was seeded with regardless of who asks. Needed to prove
/// `spawn_task`'s grounding actually scopes its lookup to `self.company`
/// rather than happening to work because every test fixture only ever
/// holds one company's record.
pub(super) struct TenantScopedCompanyStore {
    pub(super) records: std::collections::HashMap<String, CompanyRecord>,
}

#[async_trait::async_trait]
impl CompanyStore for TenantScopedCompanyStore {
    async fn load(&self, id: &CompanyId) -> crate::Result<Option<CompanyRecord>> {
        Ok(self.records.get(id.as_ref()).cloned())
    }
    async fn save(&self, _record: &CompanyRecord) -> crate::Result<()> {
        unimplemented!("not exercised by this test")
    }
    async fn list(&self) -> crate::Result<Vec<CompanySummary>> {
        Ok(Vec::new())
    }
    async fn append_ledger(&self, _id: &CompanyId, _entry: LedgerEntry) -> crate::Result<()> {
        Ok(())
    }
}

/// A store that yields between reading a record and writing it back, so two
/// concurrent `add_agent` calls genuinely interleave their load → push →
/// save cycle rather than each running to completion uncontended.
pub(super) struct YieldingStore {
    pub(super) record: StdMutex<Option<CompanyRecord>>,
}

#[async_trait::async_trait]
impl CompanyStore for YieldingStore {
    async fn load(&self, _id: &CompanyId) -> crate::Result<Option<CompanyRecord>> {
        let snapshot = self.record.lock().expect("record").clone();
        tokio::task::yield_now().await;
        Ok(snapshot)
    }
    async fn save(&self, record: &CompanyRecord) -> crate::Result<()> {
        tokio::task::yield_now().await;
        *self.record.lock().expect("record") = Some(record.clone());
        Ok(())
    }
    async fn list(&self) -> crate::Result<Vec<CompanySummary>> {
        Ok(Vec::new())
    }
    async fn append_ledger(&self, _id: &CompanyId, _entry: LedgerEntry) -> crate::Result<()> {
        Ok(())
    }
}
