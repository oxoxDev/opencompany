//! The [`WorkflowRevisionStore`] port: a bounded snapshot ring per edited
//! workflow (issue #274).
//!
//! Issue #259 made a saved workflow *replaceable* (`PUT …/workflows/{wid}`),
//! guarded by an optimistic-concurrency token — but it did not keep the graph it
//! replaced. An edit that drops a node or mangles a cron was unrecoverable
//! except by re-authoring from memory. This port keeps the prior body.
//!
//! ## Why its own store, not a field on the record
//!
//! Overlay workflow bodies live on
//! [`CompanyRecord`](crate::ports::types::CompanyRecord), which is loaded and
//! saved **whole** on every write. A 20-snapshot ring per workflow riding that
//! hot path would bloat every unrelated company save. So revisions get their own
//! durable surface — a port plus the three backends — exactly the shape the
//! issue calls for, mirroring OpenHuman's dedicated `flow_revisions` table.
//!
//! ## The capture moment
//!
//! A snapshot can only be taken race-free at the one instant the update path
//! already holds the prior TOML under the per-company write lock — immediately
//! before it overwrites the overlay body. [`push_revision`](WorkflowRevisionStore::push_revision)
//! is therefore called *there*, and **before** the record save, so a failed push
//! aborts the edit and nothing is lost (save-first would risk the exact data
//! loss this port fixes).
//!
//! ## Minted ids, not content hashes
//!
//! A revision's id is [`generate_id`]-minted, deliberately **not** a hash of its
//! body. The workflow's version token *is* a content hash, so an A → B → A edit
//! sequence produces two revisions with identical bodies — a hash id would
//! collide them and lose one. A minted id keeps every distinct snapshot
//! addressable even when two share a body.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::ports::generate_id;
use crate::ports::types::CompanyId;

/// How many snapshots a single workflow keeps. Older ones are pruned on each new
/// capture, so a hot, frequently-edited workflow cannot grow unbounded. Mirrors
/// OpenHuman's `MAX_REVISIONS_PER_FLOW`.
pub const MAX_WORKFLOW_REVISIONS: usize = 20;

/// One immutable snapshot of a workflow graph as it stood before an edit
/// replaced it.
///
/// The body is stored as the exact overlay TOML that was persisted — the same
/// string [`workflow_version`](crate::company::workflow_version) hashes — so a
/// rollback re-feeds it through the ordinary update path and gets the identical
/// re-validation a hand-authored edit does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRevisionRecord {
    /// Stable, **minted** id within the company (see the module docs for why it
    /// is not a content hash).
    pub id: String,
    /// The workflow this snapshot belongs to. All of a workflow's revisions
    /// share this, and it is what [`list_revisions`](WorkflowRevisionStore::list_revisions)
    /// and [`delete_revisions`](WorkflowRevisionStore::delete_revisions) scope on.
    pub workflow_id: String,
    /// The workflow's display name at snapshot time, for the history list. Falls
    /// back to the workflow id when the captured body no longer parsed.
    pub name: String,
    /// The overlay TOML exactly as it was stored before the edit — the whole
    /// point of the record.
    pub toml: String,
    /// Epoch-millis the snapshot was captured.
    pub created_at_millis: u64,
}

impl WorkflowRevisionRecord {
    /// Builds a revision with a freshly minted id and the given capture time.
    pub fn new(
        workflow_id: impl Into<String>,
        name: impl Into<String>,
        toml: impl Into<String>,
        at_millis: u64,
    ) -> Self {
        Self {
            id: generate_id(),
            workflow_id: workflow_id.into(),
            name: name.into(),
            toml: toml.into(),
            created_at_millis: at_millis,
        }
    }
}

/// The canonical [`list_revisions`](WorkflowRevisionStore::list_revisions)
/// ordering: newest first, by `created_at_millis` descending, ties broken by
/// `id` descending.
///
/// Shared by every backend rather than left to each one's natural order, because
/// the conformance suite asserts an exact sequence and two revisions captured in
/// the same millisecond are the common case (a burst of edits), not the exotic
/// one — a timestamp-only sort would make the suite pass or fail on scheduler
/// luck. Minted ids sort in mint order, so the tie-break is itself newest-first.
pub fn sort_newest_first(revisions: &mut [WorkflowRevisionRecord]) {
    revisions.sort_by(|a, b| {
        b.created_at_millis
            .cmp(&a.created_at_millis)
            .then_with(|| b.id.cmp(&a.id))
    });
}

/// Bounded, per-workflow revision history. Company A's revisions MUST be
/// invisible to company B, and one workflow's revisions MUST be invisible to
/// another's.
#[async_trait]
pub trait WorkflowRevisionStore: Send + Sync {
    /// Records a snapshot and prunes the workflow's history down to
    /// [`MAX_WORKFLOW_REVISIONS`], **atomically** where the backend can (one
    /// SQLite/Mongo transaction; under the fs per-path lock otherwise).
    ///
    /// The prune is part of the write on purpose: a caller must never see a
    /// 21-deep ring, and doing it here keeps the cap a property of the port
    /// rather than of every writer remembering to trim.
    async fn push_revision(
        &self,
        company: &CompanyId,
        revision: &WorkflowRevisionRecord,
    ) -> Result<()>;

    /// Lists one workflow's revisions, **newest first** (see
    /// [`sort_newest_first`]).
    async fn list_revisions(
        &self,
        company: &CompanyId,
        workflow_id: &str,
    ) -> Result<Vec<WorkflowRevisionRecord>>;

    /// Fetches one revision by id, scoped to its workflow, or `None`.
    ///
    /// Scoped to `workflow_id` so a rollback cannot restore one workflow's body
    /// onto another by naming a revision id that belongs elsewhere.
    async fn get_revision(
        &self,
        company: &CompanyId,
        workflow_id: &str,
        revision_id: &str,
    ) -> Result<Option<WorkflowRevisionRecord>>;

    /// Drops every revision of a workflow; returns how many were removed. The
    /// delete cascade a workflow deletion runs so a removed workflow leaves no
    /// orphaned history behind.
    async fn delete_revisions(&self, company: &CompanyId, workflow_id: &str) -> Result<u64>;
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn new_mints_a_distinct_id_each_time() {
        let a = WorkflowRevisionRecord::new("wf", "Greeter", "id = \"wf\"", 1);
        let b = WorkflowRevisionRecord::new("wf", "Greeter", "id = \"wf\"", 1);
        assert_ne!(a.id, b.id, "identical bodies must still get distinct ids");
        assert_eq!(a.workflow_id, "wf");
        assert_eq!(a.toml, "id = \"wf\"");
    }

    #[test]
    fn a_record_round_trips_as_camel_case_json() {
        let rev = WorkflowRevisionRecord::new("wf", "Greeter", "id = \"wf\"", 7);
        let json = serde_json::to_string(&rev).unwrap();
        assert!(json.contains("\"workflowId\""), "{json}");
        assert!(json.contains("\"createdAtMillis\""), "{json}");
        assert_eq!(rev, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn sort_is_newest_first_with_id_tiebreak() {
        // Three revisions, two sharing a millisecond — the burst-of-edits case.
        let mut revs = vec![
            WorkflowRevisionRecord {
                id: "a".to_string(),
                workflow_id: "wf".to_string(),
                name: "n".to_string(),
                toml: String::new(),
                created_at_millis: 10,
            },
            WorkflowRevisionRecord {
                id: "c".to_string(),
                workflow_id: "wf".to_string(),
                name: "n".to_string(),
                toml: String::new(),
                created_at_millis: 20,
            },
            WorkflowRevisionRecord {
                id: "b".to_string(),
                workflow_id: "wf".to_string(),
                name: "n".to_string(),
                toml: String::new(),
                created_at_millis: 20,
            },
        ];
        sort_newest_first(&mut revs);
        let ids: Vec<&str> = revs.iter().map(|r| r.id.as_str()).collect();
        // 20/c and 20/b outrank 10/a; within the tick the higher id wins.
        assert_eq!(ids, ["c", "b", "a"]);
    }
}
