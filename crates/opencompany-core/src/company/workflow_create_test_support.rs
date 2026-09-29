//! Shared in-memory test doubles and fixtures for the `workflow_create` split test files.

use super::*;
use std::sync::Mutex as StdMutex;

pub(super) use crate::company::{CompanyManifest, RawEdge, RawNode, load_workflow_union};
pub(super) use crate::ports::types::{
    CompanyRecord, CompanySummary, EventSeq, LedgerEntry, OverlayDesk, ResponderMode, StoredEvent,
};
use async_trait::async_trait;
use futures::stream::{self, BoxStream};

// --- test doubles --------------------------------------------------------

/// An in-memory `CompanyStore` seeded with one record; `save` can be told to
/// fail so the file-rollback path is exercised.
#[derive(Default)]
pub(super) struct MemStore {
    record: StdMutex<Option<CompanyRecord>>,
    fail_save: bool,
}

impl MemStore {
    pub(super) fn seeded(record: CompanyRecord) -> Self {
        Self {
            record: StdMutex::new(Some(record)),
            fail_save: false,
        }
    }
    pub(super) fn failing(record: CompanyRecord) -> Self {
        Self {
            record: StdMutex::new(Some(record)),
            fail_save: true,
        }
    }
}

#[async_trait]
impl CompanyStore for MemStore {
    async fn load(&self, _id: &CompanyId) -> Result<Option<CompanyRecord>> {
        Ok(self.record.lock().unwrap().clone())
    }
    async fn save(&self, record: &CompanyRecord) -> Result<()> {
        if self.fail_save {
            return Err(OpenCompanyError::InvalidRequest("save boom".into()));
        }
        *self.record.lock().unwrap() = Some(record.clone());
        Ok(())
    }
    async fn list(&self) -> Result<Vec<CompanySummary>> {
        Ok(Vec::new())
    }
    async fn append_ledger(&self, _id: &CompanyId, _entry: LedgerEntry) -> Result<()> {
        Ok(())
    }
}

/// An in-memory `EventLog` that records appended events so the audit journal
/// can be asserted.
#[derive(Default)]
pub(super) struct MemLog {
    pub(super) events: StdMutex<Vec<CompanyEvent>>,
}

#[async_trait]
impl EventLog for MemLog {
    async fn append(&self, _id: &CompanyId, event: CompanyEvent) -> Result<EventSeq> {
        let mut guard = self.events.lock().unwrap();
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

/// An in-memory [`EventLog`] whose [`subscribe`](EventLog::subscribe) stream
/// actually delivers what [`append`](EventLog::append) writes — the property
/// [`MemLog`] above deliberately lacks (its `subscribe` is empty). This is
/// what lets a test stand in for the live SSE fan-out: the console's picker
/// re-reads off exactly this broadcast (issue #1045), so a create/delete
/// that reaches a live subscriber here is evidence that in-process delivery
/// is intact and a stale picker is a console-side defect, not a lost frame.
pub(super) struct BroadcastMemLog {
    tx: tokio::sync::broadcast::Sender<StoredEvent>,
    next_seq: StdMutex<u64>,
}

impl BroadcastMemLog {
    pub(super) fn new() -> Self {
        Self {
            tx: tokio::sync::broadcast::channel(64).0,
            next_seq: StdMutex::new(0),
        }
    }
}

#[async_trait]
impl EventLog for BroadcastMemLog {
    async fn append(&self, id: &CompanyId, event: CompanyEvent) -> Result<EventSeq> {
        let seq = {
            let mut n = self.next_seq.lock().unwrap();
            *n += 1;
            *n
        };
        let stored = StoredEvent {
            seq: EventSeq::new(seq),
            company: id.clone(),
            event,
            at_millis: now_millis(),
        };
        // No live subscriber is not an error — a send with zero receivers
        // just means nobody is watching yet.
        let _ = self.tx.send(stored);
        Ok(EventSeq::new(seq))
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
        let rx = self.tx.subscribe();
        Box::pin(stream::unfold(rx, |mut rx| async move {
            // Each call to this closure produces exactly one item and hands
            // the receiver back as continuation state, so there is no loop
            // here.
            match rx.recv().await {
                Ok(event) => Some((crate::ports::events::EventStreamItem::Event(event), rx)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    Some((crate::ports::events::EventStreamItem::Gap { missed }, rx))
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
            }
        }))
    }
}

/// An in-memory [`WorkflowRevisionStore`] so the capture, prune, cascade and
/// rollback behaviour can be asserted without a real backend. Pruning to the
/// cap is applied on push, mirroring the durable backends.
#[derive(Default)]
pub(super) struct MemRevisions {
    rows: StdMutex<Vec<WorkflowRevisionRecord>>,
}

#[async_trait]
impl WorkflowRevisionStore for MemRevisions {
    async fn push_revision(
        &self,
        _company: &CompanyId,
        revision: &WorkflowRevisionRecord,
    ) -> Result<()> {
        use crate::ports::workflow_revisions::{MAX_WORKFLOW_REVISIONS, sort_newest_first};
        let mut rows = self.rows.lock().unwrap();
        rows.push(revision.clone());
        let mut mine: Vec<WorkflowRevisionRecord> = rows
            .iter()
            .filter(|r| r.workflow_id == revision.workflow_id)
            .cloned()
            .collect();
        if mine.len() > MAX_WORKFLOW_REVISIONS {
            sort_newest_first(&mut mine);
            let keep: std::collections::HashSet<String> = mine
                .into_iter()
                .take(MAX_WORKFLOW_REVISIONS)
                .map(|r| r.id)
                .collect();
            rows.retain(|r| r.workflow_id != revision.workflow_id || keep.contains(&r.id));
        }
        Ok(())
    }
    async fn list_revisions(
        &self,
        _company: &CompanyId,
        workflow_id: &str,
    ) -> Result<Vec<WorkflowRevisionRecord>> {
        use crate::ports::workflow_revisions::sort_newest_first;
        let mut mine: Vec<WorkflowRevisionRecord> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.workflow_id == workflow_id)
            .cloned()
            .collect();
        sort_newest_first(&mut mine);
        Ok(mine)
    }
    async fn get_revision(
        &self,
        _company: &CompanyId,
        workflow_id: &str,
        revision_id: &str,
    ) -> Result<Option<WorkflowRevisionRecord>> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.workflow_id == workflow_id && r.id == revision_id)
            .cloned())
    }
    async fn delete_revisions(&self, _company: &CompanyId, workflow_id: &str) -> Result<u64> {
        let mut rows = self.rows.lock().unwrap();
        let before = rows.len();
        rows.retain(|r| r.workflow_id != workflow_id);
        Ok((before - rows.len()) as u64)
    }
}

/// A throwaway revision store for tests that do not assert on revision
/// capture — the common case. Tests that DO assert capture/prune/rollback
/// hold their own `Arc<MemRevisions>` so they can read it back.
pub(super) fn revs() -> Arc<dyn WorkflowRevisionStore> {
    Arc::new(MemRevisions::default())
}

/// An in-memory [`ScheduleFireStore`] so the delete-time fire-ledger purge
/// (issue #708) can be asserted without a real backend. Only the verbs the
/// delete path exercises need real behaviour; `claim_fire` seeds a ledger
/// and `delete_schedule_fires` purges one schedule's rows.
#[derive(Default)]
pub(super) struct MemFires {
    /// `(company, schedule_id) -> claimed minutes`.
    rows: StdMutex<std::collections::HashMap<(String, String), std::collections::HashSet<u64>>>,
    /// Arm the next `delete_schedule_fires` call to error, to prove the
    /// delete succeeds even when the purge cascade fails.
    fail_delete: std::sync::atomic::AtomicBool,
}

impl MemFires {
    pub(super) fn seed(&self, company: &CompanyId, schedule_id: &str, minute: u64) {
        self.rows
            .lock()
            .unwrap()
            .entry((company.as_ref().to_string(), schedule_id.to_string()))
            .or_default()
            .insert(minute);
    }
    pub(super) fn minutes(&self, company: &CompanyId, schedule_id: &str) -> Vec<u64> {
        let rows = self.rows.lock().unwrap();
        let mut ms: Vec<u64> = rows
            .get(&(company.as_ref().to_string(), schedule_id.to_string()))
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        ms.sort_unstable();
        ms
    }
    pub(super) fn arm_delete_failure(&self) {
        self.fail_delete
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl ScheduleFireStore for MemFires {
    async fn claim_fire(&self, c: &CompanyId, s: &str, m: u64) -> Result<bool> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .entry((c.as_ref().to_string(), s.to_string()))
            .or_default()
            .insert(m))
    }
    async fn latest_fire(&self, c: &CompanyId, s: &str) -> Result<Option<u64>> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .get(&(c.as_ref().to_string(), s.to_string()))
            .and_then(|set| set.iter().max().copied()))
    }
    async fn prune_fires_before(&self, _c: &CompanyId, _m: u64) -> Result<usize> {
        Ok(0)
    }
    async fn delete_schedule_fires(&self, c: &CompanyId, s: &str) -> Result<usize> {
        if self
            .fail_delete
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(OpenCompanyError::Store("flaky fire-ledger purge".into()));
        }
        Ok(self
            .rows
            .lock()
            .unwrap()
            .remove(&(c.as_ref().to_string(), s.to_string()))
            .map_or(0, |set| set.len()))
    }
}

/// A throwaway fire store for delete tests that do not assert on the purge —
/// the common case. Tests that DO assert the purge hold their own
/// `Arc<MemFires>` so they can read it back.
pub(super) fn fires() -> Arc<dyn ScheduleFireStore> {
    Arc::new(MemFires::default())
}

// --- fixtures ------------------------------------------------------------

/// A committed seed graph, id `seeded` / name `Seeded flow`.
pub(super) const SEED_TOML: &str = r#"
id = "seeded"
name = "Seeded flow"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "done"
kind = "output"
name = "Done"
[[edge]]
from = "start"
to = "done"
"#;

/// A manifest with an `assistant` roster agent so `agent`-node graphs pass
/// the roster check.
pub(super) fn manifest_with_assistant() -> CompanyManifest {
    toml::from_str(
        "[company]\nname = \"Acme\"\n[[agent]]\nid = \"assistant\"\nrole = \"Assistant\"\n",
    )
    .expect("valid manifest")
}

pub(super) fn record(id: &CompanyId, manifest: CompanyManifest) -> CompanyRecord {
    CompanyRecord {
        general_channel: Default::default(),
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: id.clone(),
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

/// A valid trigger → agent → output draft naming the `assistant` teammate.
pub(super) fn valid_draft(id: &str, name: &str) -> RawWorkflow {
    RawWorkflow {
        id: id.to_string(),
        name: name.to_string(),
        description: Some("A tiny graph.".to_string()),
        owner_desk: None,
        nodes: vec![
            RawNode {
                id: "start".to_string(),
                kind: "trigger".to_string(),
                name: "Start".to_string(),
                summary: None,
                agent: None,
                schedule: None,
                config: None,
                on_error: None,
                retry: None,
                requires_approval: None,
                repeatable: None,
                destination: None,
                postcondition: None,
                verify: None,
            },
            RawNode {
                id: "worker".to_string(),
                kind: "agent".to_string(),
                name: "Worker".to_string(),
                summary: None,
                agent: Some("assistant".to_string()),
                schedule: None,
                config: None,
                on_error: None,
                retry: None,
                requires_approval: None,
                repeatable: None,
                destination: None,
                postcondition: None,
                verify: None,
            },
            RawNode {
                id: "done".to_string(),
                kind: "output".to_string(),
                name: "Report".to_string(),
                summary: None,
                agent: None,
                schedule: None,
                config: None,
                on_error: None,
                retry: None,
                requires_approval: None,
                repeatable: None,
                destination: None,
                postcondition: None,
                verify: None,
            },
        ],
        edges: vec![
            RawEdge {
                from: "start".to_string(),
                to: "worker".to_string(),
                label: None,
            },
            RawEdge {
                from: "worker".to_string(),
                to: "done".to_string(),
                label: Some("ok".to_string()),
            },
        ],
    }
}

pub(super) fn store_of(store: MemStore) -> Arc<dyn CompanyStore> {
    Arc::new(store)
}
