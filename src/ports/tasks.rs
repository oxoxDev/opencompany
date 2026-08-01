//! The [`TaskStore`] port: the company's durable Kanban board.
//!
//! Tasks are the operator-visible work items the console's board renders (see
//! [`BOARD_COLUMNS`]). They are hand-curated state, not cycle working memory —
//! the brain's per-cycle task results live in
//! [`MemoryStore`](crate::ports::MemoryStore). Each record is keyed by a stable
//! id within the company.
//!
//! ## The column vocabulary lives here
//!
//! The column ids are defined once, in this always-compiled port module, and
//! every other surface refers to them: the lifecycle decision layer
//! (`harness::lifecycle`, behind the `openhuman` feature), the dispatch edge
//! (`company::runtime`), and the write plane (`server::ops::tasks`). Before
//! issue #206 they were bare literals spread across those files plus a separate
//! feature-gated constant block, so adding a column meant finding every copy.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::ports::types::CompanyId;

/// Unqueued or returned work: cards nobody has committed to yet, plus the ones
/// a run sent back (a failed dispatch, or an orchestrator `revise` verdict).
pub const COLUMN_BACKLOG: &str = "backlog";
/// Queued to be worked next (issue #206). This is the board's manual-entry
/// column — the `+` button lives here alone, and it is what `POST …/tasks`
/// defaults to.
pub const COLUMN_TODO: &str = "todo";
/// The column a card must **enter** to be dispatched to its assignee. The
/// transition, not the state, is the trigger.
pub const COLUMN_IN_PROGRESS: &str = "in_progress";
/// Where a steered-to-pause run parks its card (issue #111). Resume is a plain
/// `column → in_progress` PATCH, which re-triggers dispatch.
pub const COLUMN_PAUSED: &str = "paused";
/// A card in review, awaiting the orchestrator's verdict.
pub const COLUMN_IN_REVIEW: &str = "in_review";
/// The terminal column — nothing dispatches out of it.
pub const COLUMN_DONE: &str = "done";

/// Every column the board renders, in the order the console draws them left to
/// right — the host's half of the console's `TASK_COLUMNS`
/// (`frontend/src/lib/tasks-sample.ts`).
///
/// `backlog` and `todo` are both "not started", and the split is deliberate:
/// `backlog` is the pool — and where the lifecycle returns work that needs
/// another pass — while `todo` is what has been picked up for the next stretch.
///
/// This is also the list the REST write boundary validates a card's `column`
/// against, which is why `todo` has to be in it: `POST …/tasks` defaults a new
/// card here, so a validator that did not know the column would reject every
/// card the board creates.
pub const BOARD_COLUMNS: [&str; 6] = [
    COLUMN_BACKLOG,
    COLUMN_TODO,
    COLUMN_IN_PROGRESS,
    COLUMN_PAUSED,
    COLUMN_IN_REVIEW,
    COLUMN_DONE,
];

/// Whether `column` names a column the board actually renders.
pub fn is_board_column(column: &str) -> bool {
    BOARD_COLUMNS.contains(&column)
}

/// One card on the company's task board.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord {
    /// Stable id for the task within the company.
    pub id: String,
    /// The task's title.
    pub title: String,
    /// An optional longer note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The board column — one of [`BOARD_COLUMNS`].
    pub column: String,
    /// The priority (`low`, `medium`, `high`).
    pub priority: String,
    /// Which desk/teammate owns it.
    pub assignee: String,
    /// Epoch-millis timestamp of the last update.
    pub updated_at_millis: u64,
    /// The chat/desk thread the task was created from, when it came from a
    /// delegation (issue #151 §3.2).
    ///
    /// A dispatched card runs asynchronously, long after the turn that spawned
    /// it has answered, so the completion reply has no ambient thread to post
    /// onto — without this it can only be written into `note`, where the
    /// operator has to go looking for it. Stamped by `spawn_task` from the
    /// delegating turn's chat id.
    ///
    /// `None` for a card created straight on the board (no originating
    /// conversation) and for every card written before this field existed —
    /// both simply get no post-back, exactly as today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_chat_id: Option<String>,
    /// The task whose dispatch turn spawned this card (issue #185) — the
    /// parent half of the Task Detail screen's lineage.
    ///
    /// An agent running a dispatched card can itself delegate, opening a new
    /// backlog card through the same `spawn_task` path. That makes the board a
    /// forest, but nothing recorded the edge: `origin_chat_id` names the
    /// *conversation* a card came from, which is shared by every sibling
    /// spawned in that thread and is `None` entirely for a board-native card.
    /// Lineage needs the task-to-task edge, so it gets its own field.
    ///
    /// Read back as: `parent` is the card with this id; `children` are the
    /// cards whose `parent_task_id` is this card's id.
    ///
    /// `None` for a card created straight on the board, for one delegated from
    /// an ordinary chat turn (no task in scope), and for every card written
    /// before this field existed — all three are lineage roots, exactly as
    /// today. Additive on the wire like [`Self::origin_chat_id`], so no stored
    /// board needs migrating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
}

/// Durable per-company task board. Company A's tasks MUST be invisible to
/// company B.
#[async_trait]
pub trait TaskStore: Send + Sync {
    /// Lists every task, most-recently-updated first.
    async fn list(&self, company: &CompanyId) -> Result<Vec<TaskRecord>>;
    /// Inserts or replaces a task by id.
    async fn upsert(&self, company: &CompanyId, task: &TaskRecord) -> Result<()>;
    /// Deletes a task by id; returns whether a task was removed.
    async fn delete(&self, company: &CompanyId, id: &str) -> Result<bool>;
}

#[cfg(test)]
mod test {
    use super::*;

    /// Pins the **Rust** list's ids and their order against a literal, so a
    /// reorder or a rename is a deliberate two-place edit rather than a
    /// side effect.
    ///
    /// It does **not** protect against drift from the console's mirror in
    /// `frontend/src/lib/tasks-sample.ts` — a Rust test cannot see the TS list,
    /// so a column added on one side and not the other keeps this green.
    /// Closing that gap means generating one list from the other (a build step
    /// this crate does not have, across a separate npm build), so for now the
    /// mirror is maintained by hand and the two lists are reviewed together.
    #[test]
    fn columns_are_ordered_and_unique() {
        assert_eq!(
            BOARD_COLUMNS,
            [
                "backlog",
                "todo",
                "in_progress",
                "paused",
                "in_review",
                "done"
            ]
        );
        let mut sorted = BOARD_COLUMNS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            BOARD_COLUMNS.len(),
            "column ids must be unique"
        );
    }

    #[test]
    fn is_board_column_accepts_only_board_columns() {
        for column in BOARD_COLUMNS {
            assert!(is_board_column(column), "{column} is a board column");
        }
        // Issue #206 added To-do as a column of its own; Backlog stays.
        assert!(is_board_column(COLUMN_TODO));
        assert!(is_board_column(COLUMN_BACKLOG));
        // Near-misses a typo'd client might send.
        assert!(!is_board_column("to_do"));
        assert!(!is_board_column("To-do"));
        assert!(!is_board_column("inprogress"));
        assert!(!is_board_column(""));
    }
}
