//! Tests for the #661 (M7) workflow admin tools.
//!
//! The company layer's `update_company_workflow` / `delete_company_workflow`
//! are covered in `workflow_create.rs`; nothing here re-tests them. What is
//! tested is the part that is new — the tool surface: the read projection the
//! writer accepts back, the required version token, the agent-surface refusals
//! that do NOT exist in the company layer, and that the company layer's own
//! gates (seeds, #682 validation, optimistic concurrency) still bite when a
//! tool is what is calling them.

use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use serde_json::{Value, json};

use super::*;
use crate::company::CompanyManifest;
use crate::error::Result;
use crate::ports::types::{
    CompanyEvent, CompanyRecord, CompanySummary, EventSeq, LedgerEntry, OverlayWorkflow,
    StoredEvent,
};
use crate::ports::workflow_revisions::WorkflowRevisionRecord;
use openhuman_core as oh;

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(crate) struct MemStore {
    record: StdMutex<Option<CompanyRecord>>,
}

impl MemStore {
    pub(crate) fn seeded(record: CompanyRecord) -> Self {
        Self {
            record: StdMutex::new(Some(record)),
        }
    }
}

#[async_trait]
impl CompanyStore for MemStore {
    async fn load(&self, _id: &CompanyId) -> Result<Option<CompanyRecord>> {
        Ok(self.record.lock().unwrap().clone())
    }
    async fn save(&self, record: &CompanyRecord) -> Result<()> {
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

/// Wraps a [`MemStore`], counting `load` calls and failing every one after
/// the first.
///
/// A stand-in for a transient store hiccup between two reads of what should
/// be "the same" record. `read_workflow`'s overlay body and its
/// `[globals].disable` list must come from **one** load — a second, later
/// load disagreeing with the first is exactly the shape that let a
/// company-disabled global slip through a fallback that turned that second
/// load's failure into an empty (not-disabled) list.
#[derive(Default)]
pub(crate) struct FailsAfterFirstLoadStore {
    inner: MemStore,
    calls: StdMutex<u32>,
}

impl FailsAfterFirstLoadStore {
    pub(crate) fn seeded(record: CompanyRecord) -> Self {
        Self {
            inner: MemStore::seeded(record),
            calls: StdMutex::new(0),
        }
    }
}

#[async_trait]
impl CompanyStore for FailsAfterFirstLoadStore {
    async fn load(&self, id: &CompanyId) -> Result<Option<CompanyRecord>> {
        let count = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls
        };
        if count > 1 {
            return Err(crate::error::OpenCompanyError::Store(
                "simulated store failure on a second read".to_string(),
            ));
        }
        self.inner.load(id).await
    }
    async fn save(&self, record: &CompanyRecord) -> Result<()> {
        self.inner.save(record).await
    }
    async fn list(&self) -> Result<Vec<CompanySummary>> {
        self.inner.list().await
    }
    async fn append_ledger(&self, id: &CompanyId, entry: LedgerEntry) -> Result<()> {
        self.inner.append_ledger(id, entry).await
    }
}

#[derive(Default)]
pub(crate) struct MemLog {
    events: StdMutex<Vec<CompanyEvent>>,
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

#[derive(Default)]
pub(crate) struct MemRevisions {
    rows: StdMutex<Vec<WorkflowRevisionRecord>>,
}

#[async_trait]
impl WorkflowRevisionStore for MemRevisions {
    async fn push_revision(
        &self,
        _company: &CompanyId,
        revision: &WorkflowRevisionRecord,
    ) -> Result<()> {
        self.rows.lock().unwrap().push(revision.clone());
        Ok(())
    }
    async fn list_revisions(
        &self,
        _company: &CompanyId,
        workflow_id: &str,
    ) -> Result<Vec<WorkflowRevisionRecord>> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.workflow_id == workflow_id)
            .cloned()
            .collect())
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

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A harness holding every double, so a test can read back what the tools wrote.
pub(crate) struct Fixture {
    pub(crate) company: CompanyId,
    pub(crate) dir: tempfile::TempDir,
    pub(crate) store: Arc<MemStore>,
    pub(crate) revisions: Arc<MemRevisions>,
    pub(crate) log: Arc<MemLog>,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let company = CompanyId::new("acme");
        let manifest: CompanyManifest = toml::from_str(
            "[company]\nname = \"Acme\"\n[[agent]]\nid = \"assistant\"\nrole = \"Assistant\"\n",
        )
        .expect("valid manifest");
        let record = CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: company.clone(),
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
        };
        Self {
            company,
            dir: tempfile::tempdir().expect("tempdir"),
            store: Arc::new(MemStore::seeded(record)),
            revisions: Arc::new(MemRevisions::default()),
            log: Arc::new(MemLog::default()),
        }
    }

    pub(crate) fn source_dir(&self) -> &Path {
        self.dir.path()
    }

    pub(crate) fn admin(&self) -> WorkflowAdmin {
        let store: Arc<dyn CompanyStore> = self.store.clone();
        let revisions: Arc<dyn WorkflowRevisionStore> = self.revisions.clone();
        let events: Arc<dyn EventLog> = self.log.clone();
        WorkflowAdmin::new(
            self.company.clone(),
            Some(self.dir.path().to_path_buf()),
            store,
            Some(revisions),
            Some(events),
        )
    }

    /// The same handle with no revision store, for the degraded-deployment case.
    pub(crate) fn admin_without_revisions(&self) -> WorkflowAdmin {
        let store: Arc<dyn CompanyStore> = self.store.clone();
        WorkflowAdmin::new(
            self.company.clone(),
            Some(self.dir.path().to_path_buf()),
            store,
            None,
            None,
        )
    }

    /// Put a body straight onto the record's overlay, bypassing the tools —
    /// for the shapes the agent schema cannot author (a schedule, node policy,
    /// a corrupt body).
    pub(crate) async fn put_overlay(&self, id: &str, toml_src: &str) {
        let mut record = self
            .store
            .load(&self.company)
            .await
            .unwrap()
            .expect("record");
        record.overlay_workflows.push(OverlayWorkflow {
            id: id.to_string(),
            toml: toml_src.to_string(),
        });
        record.manifest.workflows.enabled.push(id.to_string());
        self.store.save(&record).await.unwrap();
    }

    pub(crate) async fn overlays(&self) -> Vec<OverlayWorkflow> {
        self.store
            .load(&self.company)
            .await
            .unwrap()
            .map(|r| r.overlay_workflows)
            .unwrap_or_default()
    }

    pub(crate) async fn enabled(&self) -> Vec<String> {
        self.store
            .load(&self.company)
            .await
            .unwrap()
            .map(|r| r.manifest.workflows.enabled)
            .unwrap_or_default()
    }

    pub(crate) fn events(&self) -> Vec<CompanyEvent> {
        self.log.events.lock().unwrap().clone()
    }

    /// Write a seed file into `workflows/`, so an id is source-defined.
    pub(crate) fn write_seed(&self, id: &str, toml_src: &str) {
        let dir = self.dir.path().join("workflows");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join(format!("{id}.toml")), toml_src).expect("write seed");
    }
}

/// The graph the `create_workflow`/`update_workflow` schema accepts, as JSON.
pub(crate) fn graph_args(id: &str, name: &str, worker_name: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "description": "A tiny graph.",
        "nodes": [
            { "id": "start", "kind": "trigger", "name": "Start" },
            { "id": "worker", "kind": "agent", "name": worker_name, "agent": "assistant" },
            { "id": "done", "kind": "output", "name": "Report" }
        ],
        "edges": [
            { "from": "start", "to": "worker" },
            { "from": "worker", "to": "done" }
        ]
    })
}

/// A graph with an owning desk already set (issue #1862 prerequisite). Tests
/// put it on the record directly rather than via a tool call so a fixture can
/// start from "already owned" without depending on `UpdateWorkflowTool`'s own
/// desk handling — the same reason `SCHEDULED_TOML` does.
pub(crate) const OWNED_TOML: &str = r#"
id = "owned"
name = "Owned flow"
owner_desk = "engineering"
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

pub(crate) const SEED_TOML: &str = r#"
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

/// A graph whose trigger carries a cron. Only an operator can author this, so
/// tests put it on the record directly.
pub(crate) const SCHEDULED_TOML: &str = r#"
id = "nightly"
name = "Nightly flow"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
schedule = "0 3 * * *"
[[node]]
id = "done"
kind = "output"
name = "Done"
[[edge]]
from = "start"
to = "done"
"#;

/// A graph with an operator's approval gate on a node.
pub(crate) const GATED_TOML: &str = r#"
id = "gated"
name = "Gated flow"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
[[node]]
id = "worker"
kind = "agent"
name = "Worker"
agent = "assistant"
requires_approval = true
[[node]]
id = "done"
kind = "output"
name = "Done"
[[edge]]
from = "start"
to = "worker"
[[edge]]
from = "worker"
to = "done"
"#;

/// The markdown a tool result puts in front of the model.
pub(crate) fn md(result: &ToolResult) -> String {
    result.markdown_formatted.clone().unwrap_or_default()
}

/// The text of an error result.
pub(crate) fn err_text(result: &ToolResult) -> String {
    assert!(result.is_error, "expected an error result");
    result.output_for_llm(false)
}

/// The JSON payload a successful result carries.
pub(crate) fn data(result: &ToolResult) -> Value {
    assert!(!result.is_error, "expected a success result: {result:?}");
    for block in &result.content {
        if let oh::skills::types::ToolContent::Json { data } = block {
            return data.clone();
        }
    }
    panic!("no JSON block in {result:?}");
}
