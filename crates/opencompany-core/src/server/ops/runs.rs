//! Run reads: `GET …/runs?task=&status=&limit=` and `GET …/runs/{run_id}`,
//! under both scope forms (issue #242).
//!
//! These are the read half of the `Run` object. The write half already exists:
//! [`CompanyRuntime::dispatch_task`](crate::company::runtime::CompanyRuntime)
//! mints a row per attempt, [`CycleRunner`](crate::runtime::CycleRunner) begins
//! and settles it, and
//! [`RunTraceSink`](crate::harness::run_trace::RunTraceSink) writes the step
//! trace *during* the turn. Nothing here computes anything the store does not
//! already hold — these routes project it.
//!
//! ## Why this is a store read and not a journal fold
//!
//! The sibling run-history route ([`super::workflows::list_runs`]) folds the
//! company's whole event log on every request, and its own doc says so. That
//! cost is exactly why [`RunStore`](crate::ports::RunStore) exists: a run is
//! *queryable state*, indexed on task and status by every backend, so
//! `GET …/runs` hands its predicates to
//! [`RunStore::list_runs`](crate::ports::RunStore::list_runs) and reads only the
//! rows it will return. No route in this module iterates events.
//!
//! ## What the wire shape refuses to imply
//!
//! Three honesty constraints, all of them states the write path really
//! produces:
//!
//! * **A missing `finishedAtMillis` does not mean "still running."** It is
//!   absent for a *parked* run too, because a parked run can still resume and
//!   stamping it would make a resumable attempt read as over. So every summary
//!   carries [`RunStatus::phase`] — `active` / `parked` / `terminal` — and the
//!   console keys off that, never off the timestamp.
//! * **`stepCount` is the high-water ordinal actually persisted**, capped at
//!   [`MAX_RUN_STEPS`]. On a long attempt it is not "how many steps the agent
//!   took", so `stepCountCapped` says when the cap was reached instead of
//!   letting the number quietly lie.
//! * **A step left `running` is in flight, not broken.** Killing a host
//!   mid-tool-call leaves that row exactly as the trace sink wrote it — which
//!   is the entire point of an incremental trace — so the step's
//!   [`TurnStepStatus`] rides the wire beside its kind and the console renders
//!   it as in-flight rather than as a failure.
//!
//! ## Steps reuse the console's timeline contract
//!
//! [`RunStepEntry`]'s wire form is the `TimelineEntry` shape the Task Detail
//! screen already renders (`super::tasks::TimelineEntry`, and `TimelineEntry`
//! in `frontend/src/api/tasks.ts`): `seq` / `atMillis` / `kind` / `label` /
//! `detail`, plus the two fields a step has and a journal entry does not
//! (`status`, `elapsedMs`). Reusing the contract means the grouped-timeline
//! renderer is reused rather than reinvented — `kind` simply widens additively
//! from the journal's five words to include the three [`TurnStepKind`] words.

use axum::extract::{Path, Query};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::OpenCompanyError;
use crate::ports::runs::{RunFilter, RunRecord, RunStatus, RunStepRecord};
use crate::ports::types::{TokenUsage, TurnStepFailure, TurnStepKind, TurnStepStatus};
use crate::server::error::ApiError;
use crate::server::ops::{ScopedCompany, scoped};

/// The per-attempt step ceiling the trace sink enforces, re-stated here so the
/// read side can flag a capped `stepCount` without depending on the harness.
///
/// The sink is compiled only under `feature = "openhuman"`; these read routes
/// are not, and a store written by a harness build is read back by any build.
/// `the_cap_matches_the_trace_sink` pins the two together on a build that has
/// both.
const MAX_RUN_STEPS: u32 = 500;

/// How many attempts `GET …/runs` returns when the caller names no `?limit=`,
/// and the ceiling a larger one is clamped to.
const DEFAULT_RUN_LIMIT: usize = 50;
const MAX_RUN_LIMIT: usize = 200;

/// How many attempts ride along on `GET …/tasks/{id}`.
///
/// Bounded because a card can be re-dispatched without limit and the task
/// detail read must stay one cheap call. The console's attempts section pages
/// through `GET …/runs?task=` when it wants more.
pub(crate) const TASK_DETAIL_RUN_LIMIT: usize = 50;

/// Builds the run route fragment.
pub fn router() -> Router<AppState> {
    // No static/dynamic ordering hazard here: `/runs` has no static child
    // segment, so `{run_id}` cannot shadow anything.
    scoped("/runs", get(list_runs)).merge(scoped("/runs/{run_id}", get(run_detail)))
}

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

/// An attempt's token/cost totals, in the console's casing.
///
/// A projection of [`TokenUsage`] rather than the type itself, because
/// `TokenUsage` carries **no** `rename_all`: it is embedded in
/// [`CycleResult`](crate::ports::types::CycleResult) and therefore in
/// already-journaled events, where its field names are the durable decode
/// contract for every row ever written. Renaming it there to suit a REST
/// response would be a migration, not a rename.
///
/// So the wire casing is fixed here instead. Without this the response would
/// be the worst of both: `input` and `output` looking camelCase by accident of
/// being single words, beside a snake_case `cached_input` and `cost_usd` — an
/// object no console type could describe honestly.
/// `usage_is_camel_case_on_the_wire` pins it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunUsage {
    input: u64,
    output: u64,
    cached_input: u64,
    /// Zero when the path reports tokens but bills elsewhere (the managed
    /// passthrough echoes no USD), so tokens stay comparable across attempts.
    cost_usd: f64,
}

impl From<TokenUsage> for RunUsage {
    fn from(usage: TokenUsage) -> Self {
        Self {
            input: usage.input,
            output: usage.output,
            cached_input: usage.cached_input,
            cost_usd: usage.cost_usd,
        }
    }
}

/// One attempt, as the console's attempts list renders it.
///
/// Everything is projected from the stored [`RunRecord`]; the company is
/// dropped because it is already the scope of the request.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunSummary {
    /// Stable id for the attempt.
    id: String,
    /// The card this is an attempt at.
    ///
    /// **Omitted, never `null`**, when the attempt is at no card — an operator
    /// chat turn (issue #983) — matching how every other absent field in this
    /// module is written, so a reader's "is there a card?" test is a presence
    /// check rather than a null check.
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    /// The conversation this attempt belongs to, when one raised it
    /// (issue #983). Omitted for a dispatch, which is reachable through its
    /// card instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_id: Option<String>,
    /// The desk/teammate it was dispatched to.
    agent_id: String,
    /// Which attempt at the card this is, 1-based.
    attempt: u32,
    /// Where it stands: `pending` · `running` · `waiting_approval` · `paused` ·
    /// `succeeded` · `failed` · `cancelled` · `declined`.
    status: RunStatus,
    /// The coarse phase of `status`: `active`, `parked`, or `terminal`.
    ///
    /// Derived server-side by [`RunStatus::phase`] so the terminal set is
    /// enumerated once, in the module that owns the state machine. A reader
    /// must decide liveness from this and never from `finishedAtMillis`, which
    /// is absent for a parked run as well as a live one.
    phase: &'static str,
    /// The event-log seq of the `TaskDispatched` that drove the attempt.
    /// Absent while the run is still `pending` (the row is written before the
    /// event, on purpose) and for a dispatch that failed before the append.
    #[serde(skip_serializing_if = "Option::is_none")]
    trigger_event_seq: Option<u64>,
    /// The conversation **thread** this attempt is in, within `chatId`.
    ///
    /// `chatId` names the channel, and a channel holds many threads since
    /// #1890 — so it cannot say which of them a running turn belongs to. This
    /// is the event seq of the message that started the thread, which is also
    /// what the console addresses a thread by.
    ///
    /// Absent for a card dispatch and a workflow node, which belong to no
    /// conversation, and for a row written before the field existed — both of
    /// which read as "rooted at the channel".
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_root: Option<u64>,
    /// The desk episode this attempt is a seat turn of (plan hive-desks). The
    /// console lanes it under its round; absent for every other attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    episode_id: Option<String>,
    /// The round revision inside that episode (raw, 0-based).
    #[serde(skip_serializing_if = "Option::is_none")]
    round_revision: Option<u64>,
    /// Epoch-millis the row was minted.
    created_at_millis: u64,
    /// Epoch-millis the cycle began. Absent while `pending`.
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at_millis: Option<u64>,
    /// Epoch-millis the attempt reached a **terminal** status. Absent for a
    /// parked run, which can still resume — see `phase`.
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at_millis: Option<u64>,
    /// Why the attempt failed, in plain language, when the settle carried one.
    ///
    /// One value here is worth recognising rather than special-casing away: a
    /// run whose `begin_run` never landed stays `pending`, a `waiting_approval`
    /// settle on it is refused by the state machine, and the cycle's
    /// terminality backstop then closes it `failed` with the reason it could
    /// not settle. That row is rare but it is the honest record of a dispatch
    /// that died in the gap.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Tokens and cost folded across the attempt's turns.
    ///
    /// Written on the settle, so a live attempt's figures are **provisional**
    /// until it reaches a terminal or parked status — which `phase` says.
    usage: RunUsage,
    /// The high-water step ordinal actually persisted — not "how many steps the
    /// agent took" once `stepCountCapped` is set. Also written on the settle.
    step_count: u32,
    /// Whether `stepCount` hit the per-attempt ceiling, meaning the trace is a
    /// prefix of the run rather than the whole of it.
    step_count_capped: bool,
}

impl From<RunRecord> for RunSummary {
    fn from(run: RunRecord) -> Self {
        Self {
            id: run.id,
            task_id: run.task_id,
            chat_id: run.chat_id,
            agent_id: run.agent_id,
            attempt: run.attempt,
            status: run.status,
            phase: run.status.phase(),
            trigger_event_seq: run.trigger_event_seq.map(|s| s.value()),
            thread_root: run.thread_root.map(|s| s.value()),
            episode_id: run.episode_id,
            round_revision: run.round_revision,
            created_at_millis: run.created_at_millis,
            started_at_millis: run.started_at_millis,
            finished_at_millis: run.finished_at_millis,
            error: run.error,
            usage: run.usage.into(),
            step_count: run.step_count,
            step_count_capped: run.step_count >= MAX_RUN_STEPS,
        }
    }
}

/// One step of an attempt's trace, in the console's `TimelineEntry` shape.
///
/// See the module docs: this is deliberately the same wire contract the Task
/// Detail timeline already renders, widened additively rather than duplicated.
/// `kind` and `status` serialize straight from their enums, so the literals
/// cannot drift from the ones the harness scrubber produces.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunStepEntry {
    /// Position within the attempt: 0-based, dense, run-scoped. The render key.
    ///
    /// Deliberately **not** an event seq — an event seq is company-wide and
    /// shared with chat and audit. Two runs both have a step `0`.
    seq: u32,
    /// Epoch-millis the step was recorded.
    at_millis: u64,
    /// `tool_call` · `thinking` · `note`.
    kind: TurnStepKind,
    /// `ok` · `error` · `running` · `awaiting_approval`.
    ///
    /// `running` is a real, expected terminal state of a *row*: a killed host
    /// leaves the tool call that was in flight exactly as the sink wrote it.
    /// It means in-flight-when-the-trace-stopped, never failed.
    ///
    /// `awaiting_approval` (issue #411) is the other state that is not a
    /// failure: the call was gated and is waiting on a person. It used to
    /// arrive here as `error`, which made the one step an operator could act on
    /// look like the one thing that had crashed.
    status: TurnStepStatus,
    /// A short, server-computed human label. Never derived from tool arguments
    /// or output.
    label: String,
    /// **What the step was doing** — its arguments, already put through issue
    /// #372's host-side redactor and bounded by the harness (issue #411).
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    /// **What came back** — a shape summary, an intrinsic tool's own message,
    /// or a failure's plain-language cause (issue #411). Never a remote body.
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<String>,
    /// The typed reason the step did not succeed (issue #411). Absent on a
    /// success, on a step still `running`, and on a parked one.
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<TurnStepFailure>,
    /// The result was cut before the agent could read all of it (issue #410).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
    /// How long the step took, when known (tool calls report it; thinking and
    /// note steps do not).
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_ms: Option<u64>,
}

impl From<RunStepRecord> for RunStepEntry {
    fn from(record: RunStepRecord) -> Self {
        Self {
            seq: record.step_seq,
            at_millis: record.at_millis,
            kind: record.step.kind,
            status: record.step.status,
            label: record.step.label,
            detail: record.step.detail,
            result: record.step.result,
            failure: record.step.failure,
            truncated: record.step.truncated,
            elapsed_ms: record.step.elapsed_ms,
        }
    }
}

/// One attempt with its full persisted trace.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunDetail {
    /// The attempt itself — the same shape the list returns per row.
    run: RunSummary,
    /// The step trace, oldest first.
    ///
    /// **Refresh-on-read.** Steps persist incrementally, so re-reading a live
    /// attempt shows the progress made since the last read; there is no live
    /// stream here on purpose, because streaming would mean widening the
    /// harness turn stream for a surface a poll already serves.
    steps: Vec<RunStepEntry>,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// The `?task=` / `?agent=` / `?status=` / `?limit=` selectors on the run list.
#[derive(Debug, Deserialize)]
struct RunsQuery {
    /// Only attempts at this card. Absent = every card.
    task: Option<String>,
    /// Only attempts spawned by this workflow run's `agent` nodes. Absent =
    /// every attempt, workflow-spawned or not.
    ///
    /// The join a run inspector needs: a workflow node's turn has neither a card
    /// nor a conversation, so before this there was no selector that could
    /// reach it.
    workflow_run: Option<String>,
    /// Only attempts dispatched to this desk/teammate. Absent = every desk.
    ///
    /// Not validated against the roster on purpose: a teammate can be removed
    /// while its attempts remain, and refusing to show that history would erase
    /// the record of work that did happen. An id nobody ran simply answers
    /// `[]`, which is the truth about it.
    agent: Option<String>,
    /// A comma-separated status list (`?status=failed,cancelled`). Absent =
    /// any status. An unknown word is a 400 rather than a silent empty page:
    /// a typo'd filter that returns `[]` looks exactly like "nothing matched".
    status: Option<String>,
    /// Cap the page. Clamped to [`MAX_RUN_LIMIT`]; `0` falls back to the
    /// default rather than returning an empty page, which is never what a
    /// caller means.
    limit: Option<usize>,
}

impl RunsQuery {
    /// Turns the query string into the store's own filter — the whole read
    /// plan, handed to the backend rather than applied to a scan.
    fn into_filter(self) -> Result<RunFilter, ApiError> {
        let mut statuses = Vec::new();
        for word in self
            .status
            .iter()
            .flat_map(|raw| raw.split(','))
            .map(str::trim)
            .filter(|word| !word.is_empty())
        {
            let status = RunStatus::from_wire(word).ok_or_else(|| {
                ApiError(OpenCompanyError::InvalidRequest(format!(
                    "unknown run status '{word}': expected one of pending, running, \
                     waiting_approval, paused, succeeded, failed, cancelled, declined"
                )))
            })?;
            statuses.push(status);
        }
        Ok(RunFilter {
            task_id: self.task,
            workflow_run_id: self.workflow_run,
            agent_id: self.agent,
            statuses,
            limit: Some(match self.limit {
                Some(0) | None => DEFAULT_RUN_LIMIT,
                Some(n) => n.min(MAX_RUN_LIMIT),
            }),
        })
    }
}

/// `GET …/runs?task=&agent=&status=&limit=` — the company's attempts, newest
/// first.
///
/// An indexed store read: the `task`/`agent`/`status`/`limit` predicates go to
/// [`RunStore::list_runs`](crate::ports::RunStore::list_runs), which every
/// backend answers from its own index, and the ordering is the port's shared
/// [`sort_newest_first`](crate::ports::runs::sort_newest_first) so all three
/// backends agree. Nothing here reads the event log.
async fn list_runs(
    company: ScopedCompany,
    Query(query): Query<RunsQuery>,
) -> Result<Json<Vec<RunSummary>>, ApiError> {
    let filter = query.into_filter()?;
    let runs = company
        .runtime
        .runs()
        .list_runs(company.id(), &filter)
        .await
        .map_err(ApiError)?;
    Ok(Json(runs.into_iter().map(RunSummary::from).collect()))
}

/// The `{run_id}` path segment. Named separately from the scope's `{id}` so
/// both scope forms extract cleanly (the platform form carries two params).
#[derive(Debug, Deserialize)]
struct RunPath {
    run_id: String,
}

/// `GET …/runs/{run_id}` — one attempt and its full persisted trace.
///
/// 404s when the id names no attempt *in this company*, which is also what
/// keeps a run id minted for company A from resolving under company B: the
/// store read is company-scoped, so a cross-company id simply is not found.
async fn run_detail(
    company: ScopedCompany,
    Path(RunPath { run_id }): Path<RunPath>,
) -> Result<Json<RunDetail>, ApiError> {
    let runs = company.runtime.runs();
    let run = runs
        .get_run(company.id(), &run_id)
        .await
        .map_err(ApiError)?
        .ok_or_else(|| ApiError(OpenCompanyError::NotFound(format!("no run '{run_id}'"))))?;
    let steps = runs
        .list_run_steps(company.id(), &run_id)
        .await
        .map_err(ApiError)?;
    Ok(Json(RunDetail {
        run: run.into(),
        steps: steps.into_iter().map(RunStepEntry::from).collect(),
    }))
}

/// The attempts at one card, newest first — the `runs[]` that rides along on
/// `GET …/tasks/{task_id}`.
///
/// Lives here rather than in [`super::tasks`] so the projection has one home:
/// the attempts list on the task screen and the standalone run list must not be
/// able to disagree about what a run looks like.
pub(crate) async fn runs_for_task(
    company: &ScopedCompany,
    task_id: &str,
) -> Result<Vec<RunSummary>, ApiError> {
    let filter = RunFilter::for_task(task_id).with_limit(TASK_DETAIL_RUN_LIMIT);
    let runs = company
        .runtime
        .runs()
        .list_runs(company.id(), &filter)
        .await
        .map_err(ApiError)?;
    Ok(runs.into_iter().map(RunSummary::from).collect())
}

#[cfg(test)]
#[path = "runs_tests.rs"]
mod tests;
