//! [`WorkflowScheduler`]: fires saved workflows whose trigger carries a cron.
//!
//! A workflow graph's `trigger` node may carry a `schedule` — a standard 5-field
//! cron expression, always UTC (issue #169). Without this scheduler that field
//! would be inert: a workflow would still only run when an operator clicked Run.
//! This module is the half that makes a saved schedule actually fire.
//!
//! It is deliberately shaped like the manifest-cron
//! [`CompanyScheduler`](super::scheduler::CompanyScheduler) — same injectable
//! [`Clock`], same minute-boundary sleep loop, same
//! [`spawn`](WorkflowScheduler::spawn) shape — and reuses the same
//! [`CronExpr`] matcher, so the two schedules speak one dialect and no new
//! dependency is introduced. Three things differ, each for a reason:
//!
//! * **One process-global task, not one per company.** Workflow schedules
//!   mutate at runtime (creating a workflow adds a cron with no reboot) and a
//!   hosted tenant can be provisioned after boot, so the tick re-reads
//!   [`CompanyRegistry::list`] every minute rather than snapshotting companies
//!   at boot.
//! * **Enumeration goes through the seed ∪ overlay union** (issue #168's
//!   [`list_workflows_union`]), never a raw `source_dir` scan and never the
//!   manifest's `[workflows].enabled` list — a console-created workflow lives
//!   only on the record overlay, and it is exactly the kind an operator attaches
//!   a schedule to. See [`WorkflowScheduler::tick`] for why the enabled list is
//!   not a filter.
//! * **Each fire runs on its own tokio task**, so one long agent run cannot
//!   starve every other company's schedule, with an in-flight guard so a slow
//!   run is never overlapped by its own next tick.
//!
//! Missed runs are skipped, never caught up — identical to the manifest cron
//! scheduler, so a restart after downtime does not burst.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::company::{WorkflowFile, list_workflows_union};
use crate::ports::types::CompanyId;
use crate::runtime::CompanyRegistry;
use crate::runtime::cron::{CivilTime, CronExpr};
use crate::runtime::scheduler::{Clock, MINUTE_MS, millis_to_next_minute};

/// Identifies one schedulable workflow: which company, which graph.
type WorkflowKey = (CompanyId, String);

/// Drives the cron schedules authored on saved workflow graphs.
pub struct WorkflowScheduler {
    registry: CompanyRegistry,
    clock: Arc<dyn Clock>,
    /// Per-workflow last-fired epoch minute, so a workflow fires at most once
    /// per minute no matter how often [`tick`](Self::tick) is called.
    last_fired: HashMap<WorkflowKey, u64>,
    /// Scheduled runs currently executing. Shared with the spawned run tasks,
    /// which remove their key on completion, so a run that outlives its minute
    /// suppresses its own next fire instead of stacking up.
    in_flight: Arc<Mutex<HashSet<WorkflowKey>>>,
    /// Companies already warned about having scheduled workflows but no
    /// [`WorkflowRunner`](crate::ports::WorkflowRunner) to run them on. The tick
    /// is once a minute forever, so the warning is latched here and re-armed
    /// only when the situation changes — see [`note_unwired`](Self::note_unwired).
    warned_unwired: HashSet<CompanyId>,
    /// How many unwired-company warnings have been emitted, so a test can assert
    /// the latch actually suppresses the repeat.
    #[cfg(test)]
    unwired_warnings: usize,
}

impl WorkflowScheduler {
    /// Builds a scheduler over every company in `registry`, driven by `clock`.
    pub fn new(registry: CompanyRegistry, clock: Arc<dyn Clock>) -> Self {
        Self {
            registry,
            clock,
            last_fired: HashMap::new(),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            warned_unwired: HashSet::new(),
            #[cfg(test)]
            unwired_warnings: 0,
        }
    }

    /// Runs one tick: fires every saved workflow whose trigger schedule matches
    /// the current UTC minute. Returns how many runs were started.
    ///
    /// Per company, in order, the tick skips:
    ///
    /// * a company whose `ensure_running` guard rejects (paused or archived) —
    ///   the same guard [`CompanyScheduler::tick`](super::scheduler::CompanyScheduler::tick)
    ///   uses, so schedules resume cleanly on unpause;
    /// * a company with no [`WorkflowRunner`](crate::ports::WorkflowRunner)
    ///   wired — the default build has none, so it stays inert through the same
    ///   port seam the run route reports `not_wired` on. If that company *does*
    ///   have scheduled workflows the skip is announced once (see
    ///   [`note_unwired`](Self::note_unwired)), because a configured build whose
    ///   inference source failed to resolve lands here too and would otherwise
    ///   look identical to a working one;
    /// * a workflow whose graph is malformed (skipped by the union loader with a
    ///   warning, so one bad graph never silences the rest);
    /// * a workflow already fired this minute, or whose previous scheduled run
    ///   is still in flight.
    ///
    /// **Why the manifest's `[workflows].enabled` list is not a filter here.**
    /// A trigger `schedule` is itself the operator's explicit "run this on a
    /// cron" statement, and the enabled list does not survive a restart today
    /// (a boot rebuild overwrites the persisted record's manifest from the seed
    /// `company.toml` — issue #208). Filtering on it would mean every scheduled
    /// workflow silently stopped firing after a restart, which is the exact
    /// failure this feature exists to prevent. The overlay graph bodies the
    /// union reads *do* survive a rebuild, so enumeration through them is
    /// restart-durable.
    pub async fn tick(&mut self) -> usize {
        let now = self.clock.now_millis();
        let minute = now / MINUTE_MS;
        let civil = CivilTime::from_unix_millis(now);

        let mut fired = 0;
        for company in self.registry.list() {
            let Some(runtime) = self.registry.get(&company) else {
                continue; // removed between listing and lookup (archive)
            };
            // Not accepting work: paused or archived.
            if runtime.ensure_running().await.is_err() {
                continue;
            }
            // The record's runtime-authored graph bodies. A company with no
            // persisted record contributes none; a store failure is logged and
            // skipped rather than aborting every other company's schedules.
            let overlays = match runtime.store().load(&company).await {
                Ok(Some(record)) => record.overlay_workflows,
                Ok(None) => Vec::new(),
                Err(err) => {
                    tracing::warn!(%company, %err, "workflow scheduler: cannot read company record");
                    continue;
                }
            };

            // Enumerate the company's scheduled workflows BEFORE checking for a
            // runner: whether any exist is exactly what decides if an unwired
            // company is misconfigured or simply has nothing to run.
            let mut scheduled: Vec<(WorkflowFile, String, CronExpr)> = Vec::new();
            for file in list_workflows_union(runtime.source_dir(), &overlays) {
                let Some(cron) = trigger_schedule(&file) else {
                    continue; // no schedule: manual-run only
                };
                // Validation already accepted this expression; a parse failure
                // here would mean a body written by an older/looser path, so
                // skip that one workflow loudly rather than panicking.
                let Ok(expr) = CronExpr::parse(&cron) else {
                    tracing::warn!(
                        %company,
                        workflow = %file.id,
                        schedule = %cron,
                        "workflow scheduler: skipping an unparsable schedule"
                    );
                    continue;
                };
                scheduled.push((file, cron, expr));
            }

            // No execution wired. This is the default build's inert seam, but it
            // is ALSO what a configured build looks like when its inference
            // source failed to resolve at boot — an operator-visible
            // misconfiguration in which a saved schedule is indistinguishable
            // from a broken one. Say so, once.
            let runner = match runtime.workflow_runner().cloned() {
                Some(runner) => {
                    // A runner appeared: re-arm the warning for this company.
                    self.warned_unwired.remove(&company);
                    runner
                }
                None => {
                    self.note_unwired(&company, scheduled.len());
                    continue;
                }
            };

            for (file, cron, expr) in scheduled {
                if !expr.matches(&civil) {
                    continue;
                }

                let key = (company.clone(), file.id.clone());
                if self.last_fired.get(&key) == Some(&minute) {
                    continue; // already fired this minute
                }
                self.last_fired.insert(key.clone(), minute);

                // Overlap guard: a previous scheduled run still executing keeps
                // this fire from stacking a second copy on top of it. Manual
                // runs go through the run route and are unaffected.
                let Some(claim) = self.claim(&key) else {
                    tracing::info!(
                        %company,
                        workflow = %file.id,
                        schedule = %cron,
                        "workflow scheduler: previous scheduled run still in flight, skipping"
                    );
                    continue;
                };

                let input = json!({
                    // `request` is what `run_request_text` reads, so every agent
                    // turn in the run knows it was started by a schedule rather
                    // than by an operator typing a topic.
                    "request": format!("Scheduled run (cron `{cron}`)"),
                    "scheduled": true,
                    "cron": cron,
                    "firedAtMs": now,
                });

                let runner = runner.clone();
                let workflow = file;
                // A FRESH TASK PER FIRE IS CORRECT HERE, and is not a hole in
                // the `WORKFLOW_DEPTH` re-entry guard
                // (`crate::workflows::runner`). That guard is a task-local
                // counting one *causal chain*: a run, the agent turns inside
                // it, and the tools those turns call all stay on one task, so a
                // workflow that reaches back into itself is bounded. A
                // scheduled fire starts no such chain — it is a new root, at
                // depth 0, exactly like an operator clicking Run — so spawning
                // it here is the same as any other entry point. What would
                // break the guard is spawning *inside* an existing run's chain,
                // which would reset the depth mid-chain; this scheduler never
                // runs inside a run. Spawning also keeps one slow agent run
                // from starving every other company's schedule on the tick
                // loop.
                tokio::spawn(async move {
                    // Held for the whole run so the slot is released on EVERY
                    // exit path, including an unwind. Releasing after the
                    // `await` instead would leak the claim when the runner
                    // panics — and a leaked claim is permanent, because nothing
                    // else ever removes it, so one panic would retire that
                    // schedule for the life of the process with no log line.
                    let _claim = claim;
                    let (company, workflow_id) = key;
                    match runner.run(&company, &workflow, input).await {
                        Ok(run) => tracing::info!(
                            %company,
                            workflow = %workflow_id,
                            pending_approvals = run.pending_approvals.len(),
                            "workflow scheduler: scheduled run finished"
                        ),
                        Err(err) => tracing::warn!(
                            %company,
                            workflow = %workflow_id,
                            %err,
                            "workflow scheduler: scheduled run failed"
                        ),
                    }
                });
                fired += 1;
            }
        }

        // --- sweep stale per-company/per-workflow state -------------------
        // Both maps are keyed on things that can disappear (a workflow is
        // deleted, a company is archived out of the registry), so both need a
        // sweep or they grow for the life of the process. One place, so there
        // is a single obvious point where this happens.

        // Only the CURRENT minute can dedupe a fire, so every older entry is
        // dead weight.
        self.last_fired.retain(|_, fired_at| *fired_at >= minute);

        // A company removed from the registry is never visited again, so
        // NEITHER re-arm path in `note_unwired` (a runner appearing, or the
        // scheduled count dropping to zero) can ever clear its latch — the
        // entry would be orphaned forever.
        let registry = &self.registry;
        self.warned_unwired
            .retain(|company| registry.get(company).is_some());

        fired
    }

    /// Records that `company` has `scheduled` workflows but no runner to fire
    /// them on, warning at most once per company.
    ///
    /// Two rules, both deliberate:
    ///
    /// * **Silence when `scheduled == 0`.** A company with no scheduled
    ///   workflows and no runner is not misconfigured — it simply has nothing to
    ///   run. The latch is cleared in that case, so if a schedule is saved later
    ///   while the company is still unwired, the operator does get told.
    /// * **Once per company, not once per tick.** The tick is every minute
    ///   forever; logging there would be ~1440 lines a day per tenant, which
    ///   buries the signal it is trying to raise. The latch is re-armed in
    ///   [`tick`](Self::tick) as soon as a runner appears, and swept there when
    ///   the company leaves the registry — neither re-arm path can reach a
    ///   company that is no longer visited.
    fn note_unwired(&mut self, company: &CompanyId, scheduled: usize) {
        if scheduled == 0 {
            self.warned_unwired.remove(company);
            return;
        }
        if !self.warned_unwired.insert(company.clone()) {
            return; // already warned for this company
        }
        #[cfg(test)]
        {
            self.unwired_warnings += 1;
        }
        tracing::warn!(
            %company,
            scheduled_workflows = scheduled,
            "workflow scheduler: {scheduled} scheduled workflow(s) will not fire — no workflow \
             runner is wired for this company (inference source unresolved?)"
        );
    }

    /// Claims `key` for a run, or `None` when a run already holds it.
    ///
    /// The returned [`Claim`] IS the hold: dropping it releases the slot, so a
    /// caller that takes a claim and then bails out cannot strand it.
    fn claim(&self, key: &WorkflowKey) -> Option<Claim> {
        if lock_in_flight(&self.in_flight).insert(key.clone()) {
            Some(Claim {
                in_flight: self.in_flight.clone(),
                key: key.clone(),
            })
        } else {
            None
        }
    }

    /// Whether any scheduled run is currently executing.
    #[cfg(test)]
    fn is_running_any(&self) -> bool {
        !lock_in_flight(&self.in_flight).is_empty()
    }

    /// Spawns a background task that ticks on every minute boundary until
    /// `shutdown` is notified. Boot holds the join handle and the shared
    /// `shutdown` so the scheduler stops cleanly when the server does.
    pub fn spawn(mut self, shutdown: Arc<Notify>) -> JoinHandle<()> {
        tokio::spawn(async move {
            // The `Notified` future is built ONCE and pinned across iterations,
            // not rebuilt inside the `select!`. Boot signals with
            // `notify_waiters()`, which wakes only the waiters registered at
            // that instant — a future created fresh each iteration is not
            // registered while `tick` is running, so a shutdown arriving
            // mid-tick would be dropped and the scheduler would sleep another
            // full minute before noticing. Polled once here, this one stays
            // registered, and a notification delivered during `tick` is
            // latched: the next `select!` sees it immediately.
            let notified = shutdown.notified();
            tokio::pin!(notified);
            loop {
                let sleep_ms = millis_to_next_minute(self.clock.now_millis());
                tokio::select! {
                    _ = &mut notified => break,
                    _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {
                        self.tick().await;
                    }
                }
            }
        })
    }
}

/// An RAII hold on one workflow's in-flight slot, released on drop.
///
/// A hold released by a statement after the `await` would survive a panic in
/// the run: the task unwinds, the statement never executes, and the key stays
/// in the set forever — permanently retiring that schedule. `Drop` runs while
/// unwinding, so tying the release to a guard closes that path.
struct Claim {
    in_flight: Arc<Mutex<HashSet<WorkflowKey>>>,
    key: WorkflowKey,
}

impl Drop for Claim {
    fn drop(&mut self) {
        lock_in_flight(&self.in_flight).remove(&self.key);
    }
}

/// Locks the in-flight set, recovering rather than panicking on a poisoned
/// mutex.
///
/// Two reasons this never unwraps. First, [`Claim::drop`] can run while
/// unwinding from the very panic that poisoned the lock, and a panic inside a
/// `Drop` during an unwind aborts the process. Second, the alternative — a
/// tolerant `if let Ok(..)` — would *skip* the release on a poisoned lock and
/// reintroduce the leak this guard exists to prevent. Recovering is safe here
/// because every critical section is a single `insert` / `remove` / `is_empty`
/// on a `HashSet`, none of which can leave it half-updated.
fn lock_in_flight(
    in_flight: &Mutex<HashSet<WorkflowKey>>,
) -> std::sync::MutexGuard<'_, HashSet<WorkflowKey>> {
    in_flight
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The cron a graph's trigger schedules itself on, if any.
///
/// Validation allows `schedule` only on a `trigger` node and permits at most one
/// scheduled trigger per graph, so the node this finds is the only one there is
/// — a graph with two schedules is rejected at parse rather than silently
/// resolving to whichever came first.
fn trigger_schedule(file: &WorkflowFile) -> Option<String> {
    file.nodes
        .iter()
        .find(|node| {
            node.kind == crate::company::WorkflowNodeKind::Trigger && node.schedule.is_some()
        })
        .and_then(|node| node.schedule.clone())
}

#[cfg(test)]
mod test {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use serde_json::Value;
    use tokio::sync::Semaphore;

    use super::*;
    use crate::company::CompanyManifest;
    use crate::ports::types::{CompanyRecord, OverlayWorkflow};
    use crate::ports::{WorkflowRun, WorkflowRunner};
    use crate::runtime::{FakeClock, RuntimeBuilder};

    fn tmp_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "opencompany-wfsched-{}",
            crate::ports::generate_id()
        ))
    }

    fn manifest() -> CompanyManifest {
        toml::from_str(
            r#"
            [company]
            name = "Acme"

            [[agent]]
            id = "ceo"
            role = "Chief"

            [policy]
            mode = "full"
            "#,
        )
        .expect("parse manifest")
    }

    /// Unix millis for a UTC civil minute, via the cron module's own conversion.
    fn millis_at(year: i64, month: u32, day: u32, hour: u32, minute: u32) -> u64 {
        let mut probe = 0u64;
        loop {
            let c = CivilTime::from_unix_millis(probe);
            if (c.year, c.month, c.day) == (year, month, day) {
                break;
            }
            probe += 86_400_000;
            if probe > 4_102_444_800_000 {
                panic!("date out of probe range");
            }
        }
        probe + (hour as u64) * 3_600_000 + (minute as u64) * MINUTE_MS
    }

    /// What one recorded scheduled run carries.
    #[derive(Clone, Debug)]
    struct Recorded {
        company: String,
        workflow: String,
        input: Value,
    }

    /// A [`WorkflowRunner`] that records every run instead of executing one, and
    /// can be held open so a test controls exactly when a run completes.
    struct RecordingRunner {
        started: Arc<Mutex<Vec<Recorded>>>,
        completed: Arc<AtomicUsize>,
        /// When set, a run parks until the test adds a permit.
        gate: Option<Arc<Semaphore>>,
    }

    impl RecordingRunner {
        fn new() -> (Arc<Self>, Arc<Mutex<Vec<Recorded>>>, Arc<AtomicUsize>) {
            let started = Arc::new(Mutex::new(Vec::new()));
            let completed = Arc::new(AtomicUsize::new(0));
            let runner = Arc::new(Self {
                started: started.clone(),
                completed: completed.clone(),
                gate: None,
            });
            (runner, started, completed)
        }

        fn gated(gate: Arc<Semaphore>) -> (Arc<Self>, Arc<Mutex<Vec<Recorded>>>, Arc<AtomicUsize>) {
            let started = Arc::new(Mutex::new(Vec::new()));
            let completed = Arc::new(AtomicUsize::new(0));
            let runner = Arc::new(Self {
                started: started.clone(),
                completed: completed.clone(),
                gate: Some(gate),
            });
            (runner, started, completed)
        }
    }

    #[async_trait]
    impl WorkflowRunner for RecordingRunner {
        async fn run(
            &self,
            company: &CompanyId,
            workflow: &WorkflowFile,
            input: Value,
        ) -> crate::Result<WorkflowRun> {
            self.started.lock().unwrap().push(Recorded {
                company: company.as_ref().to_string(),
                workflow: workflow.id.clone(),
                input,
            });
            if let Some(gate) = &self.gate {
                let permit = gate.acquire().await.expect("gate open");
                permit.forget();
            }
            self.completed.fetch_add(1, Ordering::SeqCst);
            Ok(WorkflowRun {
                output: Value::Null,
                pending_approvals: Vec::new(),
                deliveries: Vec::new(),
            })
        }
    }

    /// A minimal valid graph body, optionally scheduled on `cron`.
    fn body(id: &str, cron: Option<&str>) -> String {
        let schedule = cron
            .map(|c| format!("schedule = \"{c}\"\n"))
            .unwrap_or_default();
        format!(
            r#"
id = "{id}"
name = "{id}"
[[node]]
id = "start"
kind = "trigger"
name = "Start"
{schedule}
[[node]]
id = "done"
kind = "output"
name = "Done"
[[edge]]
from = "start"
to = "done"
"#
        )
    }

    fn overlay(id: &str, cron: Option<&str>) -> OverlayWorkflow {
        OverlayWorkflow {
            id: id.to_string(),
            toml: body(id, cron),
        }
    }

    /// Builds a registered company whose only workflows are record overlays —
    /// the console-created shape, with no source directory at all.
    async fn company_with_overlays(
        home: &std::path::Path,
        id: &str,
        overlays: Vec<OverlayWorkflow>,
        runner: Option<Arc<dyn WorkflowRunner>>,
        lifecycle: &str,
    ) -> CompanyRegistry {
        let company = CompanyId::new(id);
        let mut runtime = RuntimeBuilder::new(home.to_path_buf(), manifest())
            .with_id(company.clone())
            .build()
            .await
            .expect("builds");
        assert!(
            runtime.source_dir().is_none(),
            "the overlay-only case must have no source dir"
        );
        if let Some(runner) = runner {
            runtime.set_workflow_runner(runner);
        }

        // Persist the graph bodies (and lifecycle) the scheduler will read.
        let store = runtime.store().clone();
        let mut record: CompanyRecord = store
            .load(&company)
            .await
            .expect("loads")
            .expect("the builder materialized a record");
        record.overlay_workflows = overlays;
        record.lifecycle = lifecycle.to_string();
        store.save(&record).await.expect("saves");

        let registry = CompanyRegistry::new();
        registry.insert(company, Arc::new(runtime));
        registry
    }

    /// Yields until `predicate` holds, so a test can wait on a spawned run
    /// without a wall-clock sleep. Panics rather than hanging if it never does.
    async fn wait_for(mut predicate: impl FnMut() -> bool) {
        for _ in 0..10_000 {
            if predicate() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("condition never became true");
    }

    /// Waits until no scheduled run still holds its in-flight claim.
    ///
    /// **Any test that ticks a second time and expects another fire must call
    /// this first.** `tick` is not a synchronisation point: it spawns the run
    /// and returns without awaiting it, so whether the spawned task has been
    /// polled by the next tick depends on whether some await inside `tick`
    /// (the record load) happens to yield. When it does not, the previous run
    /// still holds the claim, the overlap guard correctly rejects the fire, and
    /// the tick returns 0 — which is the code working as designed and the test
    /// being wrong. That is exactly how
    /// `the_dedupe_map_does_not_grow_without_bound` passed locally and failed
    /// in CI.
    ///
    /// Waiting on the mock's `started` vec is NOT a substitute: it records the
    /// run's start, not its completion, so it proves nothing about the claim.
    /// This keys on the claim itself, which is the state the guard reads.
    async fn drain(scheduler: &WorkflowScheduler) {
        wait_for(|| !scheduler.is_running_any()).await;
    }

    async fn cleanup(home: &std::path::Path) {
        tokio::fs::remove_dir_all(home).await.ok();
    }

    /// The headline: a workflow that exists ONLY as a record overlay (no source
    /// file — the console-created shape #168 introduced) is picked up and fired
    /// on a matching minute, once. It does not re-fire in the same minute, stays
    /// silent on a non-matching minute, and fires again on the next match.
    #[tokio::test]
    async fn fires_an_overlay_only_workflow_once_per_matching_minute() {
        let home = tmp_home();
        let (runner, started, _completed) = RecordingRunner::new();
        let registry = company_with_overlays(
            &home,
            "acme",
            vec![overlay("digest", Some("0 9 * * MON"))],
            Some(runner),
            "running",
        )
        .await;

        // Monday 2026-07-13 09:00 UTC — the schedule matches.
        let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
        let mut scheduler = WorkflowScheduler::new(registry, clock.clone());

        assert_eq!(scheduler.tick().await, 1);
        wait_for(|| started.lock().unwrap().len() == 1).await;
        let run = started.lock().unwrap()[0].clone();
        assert_eq!(run.company, "acme");
        assert_eq!(run.workflow, "digest");

        // Same minute again: deduped.
        clock.advance(30_000);
        assert_eq!(scheduler.tick().await, 0);

        // A non-matching minute (09:01) is silent.
        clock.set(millis_at(2026, 7, 13, 9, 1));
        assert_eq!(scheduler.tick().await, 0);

        // The following Monday fires again — after the first run has let go of
        // its claim, or the overlap guard would rightly refuse.
        drain(&scheduler).await;
        clock.set(millis_at(2026, 7, 20, 9, 0));
        assert_eq!(scheduler.tick().await, 1);
        wait_for(|| started.lock().unwrap().len() == 2).await;

        cleanup(&home).await;
    }

    /// The seeded input tells the run — and every agent turn inside it — that a
    /// schedule started it. `request` is the key `run_request_text` reads.
    #[tokio::test]
    async fn the_seeded_input_marks_the_run_as_scheduled() {
        let home = tmp_home();
        let (runner, started, _completed) = RecordingRunner::new();
        let registry = company_with_overlays(
            &home,
            "acme",
            vec![overlay("digest", Some("* * * * *"))],
            Some(runner),
            "running",
        )
        .await;
        let fired_at = millis_at(2026, 7, 13, 9, 0);
        let clock = Arc::new(FakeClock::new(fired_at));
        let mut scheduler = WorkflowScheduler::new(registry, clock);

        assert_eq!(scheduler.tick().await, 1);
        wait_for(|| started.lock().unwrap().len() == 1).await;

        let input = started.lock().unwrap()[0].input.clone();
        assert_eq!(input["scheduled"], true);
        assert_eq!(input["cron"], "* * * * *");
        assert_eq!(input["firedAtMs"], fired_at);
        // `request` is exactly the key `workflows::caps::run_request_text`
        // reads, so this string reaches every agent turn in the run.
        let request = input["request"].as_str().expect("a request string");
        assert!(request.contains("Scheduled run"), "{request}");
        assert!(request.contains("* * * * *"), "{request}");
        assert!(!request.trim().is_empty());

        cleanup(&home).await;
    }

    /// A workflow with no schedule is never fired by the scheduler — it stays
    /// manual-run only.
    #[tokio::test]
    async fn an_unscheduled_workflow_never_fires() {
        let home = tmp_home();
        let (runner, started, _completed) = RecordingRunner::new();
        let registry = company_with_overlays(
            &home,
            "acme",
            vec![overlay("manual", None)],
            Some(runner),
            "running",
        )
        .await;
        let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
        let mut scheduler = WorkflowScheduler::new(registry, clock);

        assert_eq!(scheduler.tick().await, 0);
        assert!(started.lock().unwrap().is_empty());

        cleanup(&home).await;
    }

    /// A paused company fires nothing — the same `ensure_running` guard the
    /// manifest cron scheduler uses, so schedules resume on unpause.
    #[tokio::test]
    async fn a_paused_company_is_skipped() {
        let home = tmp_home();
        let (runner, started, _completed) = RecordingRunner::new();
        let registry = company_with_overlays(
            &home,
            "acme",
            vec![overlay("digest", Some("* * * * *"))],
            Some(runner),
            "paused",
        )
        .await;
        let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
        let mut scheduler = WorkflowScheduler::new(registry, clock);

        assert_eq!(scheduler.tick().await, 0);
        assert!(started.lock().unwrap().is_empty());

        cleanup(&home).await;
    }

    /// No runner wired (the default build) is a clean no-op, not an error — the
    /// same port seam the run route reports `not_wired` on. Because the company
    /// *does* have a scheduled workflow, the skip is announced — exactly once,
    /// no matter how many minutes pass, so a once-a-minute tick cannot bury the
    /// signal it is raising.
    #[tokio::test]
    async fn no_runner_wired_is_a_noop_and_warns_once() {
        let home = tmp_home();
        let registry = company_with_overlays(
            &home,
            "acme",
            vec![overlay("digest", Some("* * * * *"))],
            None,
            "running",
        )
        .await;
        let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
        let mut scheduler = WorkflowScheduler::new(registry, clock.clone());

        assert_eq!(scheduler.tick().await, 0);
        assert_eq!(scheduler.unwired_warnings, 1, "the first skip must be said");

        // Several more matching minutes: still skipped, still silent.
        for minute in 1..5 {
            clock.set(millis_at(2026, 7, 13, 9, minute));
            assert_eq!(scheduler.tick().await, 0);
        }
        assert_eq!(
            scheduler.unwired_warnings, 1,
            "the warning must not repeat every tick"
        );

        cleanup(&home).await;
    }

    /// A company with no runner AND no scheduled workflows is not
    /// misconfigured — it simply has nothing to run, so it stays silent.
    #[tokio::test]
    async fn no_runner_and_no_schedules_does_not_warn() {
        let home = tmp_home();
        let registry = company_with_overlays(
            &home,
            "acme",
            vec![overlay("manual", None)],
            None,
            "running",
        )
        .await;
        let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
        let mut scheduler = WorkflowScheduler::new(registry, clock);

        assert_eq!(scheduler.tick().await, 0);
        assert_eq!(
            scheduler.unwired_warnings, 0,
            "a company with nothing scheduled deserves silence"
        );

        cleanup(&home).await;
    }

    /// The latch is re-armed when the situation changes: a schedule saved onto a
    /// still-unwired company warns, even though an earlier tick already found
    /// that company unwired (with nothing scheduled) and said nothing.
    #[tokio::test]
    async fn a_schedule_added_later_still_warns() {
        let home = tmp_home();
        let company = CompanyId::new("acme");
        let registry = company_with_overlays(
            &home,
            "acme",
            vec![overlay("manual", None)],
            None,
            "running",
        )
        .await;
        let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
        let mut scheduler = WorkflowScheduler::new(registry.clone(), clock.clone());

        assert_eq!(scheduler.tick().await, 0);
        assert_eq!(scheduler.unwired_warnings, 0);

        // The operator saves a schedule on the (still unwired) company.
        let runtime = registry.get(&company).expect("registered");
        let store = runtime.store().clone();
        let mut record = store.load(&company).await.unwrap().unwrap();
        record.overlay_workflows = vec![overlay("digest", Some("* * * * *"))];
        store.save(&record).await.unwrap();

        clock.set(millis_at(2026, 7, 13, 9, 1));
        assert_eq!(scheduler.tick().await, 0);
        assert_eq!(
            scheduler.unwired_warnings, 1,
            "a schedule saved onto an unwired company must be reported"
        );

        cleanup(&home).await;
    }

    /// A malformed graph body skips only itself: the healthy scheduled workflow
    /// beside it still fires.
    #[tokio::test]
    async fn a_malformed_graph_skips_only_itself() {
        let home = tmp_home();
        let (runner, started, _completed) = RecordingRunner::new();
        let registry = company_with_overlays(
            &home,
            "acme",
            vec![
                OverlayWorkflow {
                    id: "broken".to_string(),
                    toml: "id = \"broken\"\nname =".to_string(),
                },
                overlay("healthy", Some("* * * * *")),
            ],
            Some(runner),
            "running",
        )
        .await;
        let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
        let mut scheduler = WorkflowScheduler::new(registry, clock);

        assert_eq!(scheduler.tick().await, 1);
        wait_for(|| started.lock().unwrap().len() == 1).await;
        assert_eq!(started.lock().unwrap()[0].workflow, "healthy");

        cleanup(&home).await;
    }

    /// A scheduled run still executing suppresses the next fire; once it
    /// completes, the workflow becomes schedulable again.
    #[tokio::test]
    async fn an_in_flight_run_suppresses_the_next_fire() {
        let home = tmp_home();
        let gate = Arc::new(Semaphore::new(0));
        let (runner, started, completed) = RecordingRunner::gated(gate.clone());
        let registry = company_with_overlays(
            &home,
            "acme",
            vec![overlay("slow", Some("* * * * *"))],
            Some(runner),
            "running",
        )
        .await;
        let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
        let mut scheduler = WorkflowScheduler::new(registry, clock.clone());

        // Minute 1: fires, and the run parks on the gate.
        assert_eq!(scheduler.tick().await, 1);
        wait_for(|| started.lock().unwrap().len() == 1).await;
        assert!(scheduler.is_running_any());

        // Minute 2: matches, but the previous run is still in flight.
        clock.set(millis_at(2026, 7, 13, 9, 1));
        assert_eq!(scheduler.tick().await, 0);
        assert_eq!(started.lock().unwrap().len(), 1);

        // Let the first run finish, then wait for its task to release the claim.
        gate.add_permits(1);
        wait_for(|| completed.load(Ordering::SeqCst) == 1).await;
        drain(&scheduler).await;

        // Minute 3: the workflow is schedulable again.
        gate.add_permits(1);
        clock.set(millis_at(2026, 7, 13, 9, 2));
        assert_eq!(
            scheduler.tick().await,
            1,
            "the workflow must be schedulable once its run completed"
        );
        wait_for(|| started.lock().unwrap().len() == 2).await;

        cleanup(&home).await;
    }

    /// A company removed from the registry (archived) is swept out of the
    /// warning latch. Neither re-arm path in `note_unwired` can reach it — both
    /// require the company to still be visited by a tick — so without the sweep
    /// its entry would be orphaned for the life of the process.
    #[tokio::test]
    async fn a_removed_company_is_swept_from_the_warning_latch() {
        let home = tmp_home();
        let company = CompanyId::new("acme");
        let registry = company_with_overlays(
            &home,
            "acme",
            vec![overlay("digest", Some("* * * * *"))],
            None, // no runner, so the company latches a warning
            "running",
        )
        .await;
        let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
        let mut scheduler = WorkflowScheduler::new(registry.clone(), clock.clone());

        assert_eq!(scheduler.tick().await, 0);
        assert_eq!(scheduler.unwired_warnings, 1);
        assert!(scheduler.warned_unwired.contains(&company));

        // The company is archived out of the registry.
        assert!(registry.remove(&company).is_some());

        clock.set(millis_at(2026, 7, 13, 9, 1));
        assert_eq!(scheduler.tick().await, 0);
        assert!(
            scheduler.warned_unwired.is_empty(),
            "a company that no longer exists must not be held in the latch"
        );

        cleanup(&home).await;
    }

    /// The dedupe map keeps only the current minute. Anything older can never
    /// dedupe again, and retaining it would grow the map forever — one entry per
    /// (company, workflow) ever fired, including deleted ones.
    #[tokio::test]
    async fn the_dedupe_map_does_not_grow_without_bound() {
        let home = tmp_home();
        let (runner, started, _completed) = RecordingRunner::new();
        let registry = company_with_overlays(
            &home,
            "acme",
            vec![overlay("digest", Some("* * * * *"))],
            Some(runner),
            "running",
        )
        .await;
        let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
        let mut scheduler = WorkflowScheduler::new(registry, clock.clone());

        assert_eq!(scheduler.tick().await, 1);
        // The entry for the minute just fired is kept — that is what dedupes a
        // second tick inside the same minute.
        assert_eq!(scheduler.last_fired.len(), 1);
        clock.advance(30_000);
        assert_eq!(scheduler.tick().await, 0, "same minute: deduped");

        // Many minutes later the map still holds exactly one entry, not one per
        // minute elapsed. Each iteration drains first: without that, the
        // previous run may still hold its claim and the overlap guard would
        // (correctly) refuse the fire.
        for minute in 1..10 {
            drain(&scheduler).await;
            clock.set(millis_at(2026, 7, 13, 9, minute));
            assert_eq!(scheduler.tick().await, 1, "minute {minute}");
            assert_eq!(scheduler.last_fired.len(), 1, "minute {minute}");
        }
        wait_for(|| started.lock().unwrap().len() == 10).await;

        cleanup(&home).await;
    }

    /// A runner that panics must not strand the in-flight claim. Releasing the
    /// slot after the `await` would leave the key set forever, silently retiring
    /// that schedule for the life of the process; the RAII [`Claim`] releases it
    /// on the unwind instead.
    #[tokio::test]
    async fn a_panicking_run_releases_its_claim() {
        struct PanickingRunner {
            calls: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl WorkflowRunner for PanickingRunner {
            async fn run(
                &self,
                _company: &CompanyId,
                _workflow: &WorkflowFile,
                _input: Value,
            ) -> crate::Result<WorkflowRun> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                panic!("the runner blew up mid-run");
            }
        }

        let home = tmp_home();
        let calls = Arc::new(AtomicUsize::new(0));
        let runner = Arc::new(PanickingRunner {
            calls: calls.clone(),
        });
        let registry = company_with_overlays(
            &home,
            "acme",
            vec![overlay("boom", Some("* * * * *"))],
            Some(runner),
            "running",
        )
        .await;
        let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
        let mut scheduler = WorkflowScheduler::new(registry, clock.clone());

        // Minute 1: fires, and the run panics inside its task.
        assert_eq!(scheduler.tick().await, 1);
        wait_for(|| calls.load(Ordering::SeqCst) == 1).await;
        // The unwind must have released the slot.
        drain(&scheduler).await;

        // Minute 2: the schedule is still alive. Before the RAII guard this
        // fired 0 forever.
        clock.set(millis_at(2026, 7, 13, 9, 1));
        assert_eq!(
            scheduler.tick().await,
            1,
            "a panicking run must not retire the schedule"
        );
        wait_for(|| calls.load(Ordering::SeqCst) == 2).await;

        cleanup(&home).await;
    }

    /// A workflow committed as a seed file (not an overlay) is scheduled too, so
    /// the union really is the read path.
    #[tokio::test]
    async fn a_seed_file_workflow_is_scheduled() {
        let home = tmp_home();
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("workflows")).unwrap();
        std::fs::write(
            source.path().join("workflows").join("seeded.toml"),
            body("seeded", Some("* * * * *")),
        )
        .unwrap();

        let company = CompanyId::new("acme");
        let (runner, started, _completed) = RecordingRunner::new();
        let mut runtime = RuntimeBuilder::new(home.clone(), manifest())
            .with_id(company.clone())
            .build()
            .await
            .unwrap();
        runtime.set_source_dir(Some(source.path().to_path_buf()));
        runtime.set_workflow_runner(runner);
        let registry = CompanyRegistry::new();
        registry.insert(company, Arc::new(runtime));

        let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
        let mut scheduler = WorkflowScheduler::new(registry, clock);
        assert_eq!(scheduler.tick().await, 1);
        wait_for(|| started.lock().unwrap().len() == 1).await;
        assert_eq!(started.lock().unwrap()[0].workflow, "seeded");

        cleanup(&home).await;
    }

    /// The helper reads the first scheduled trigger and ignores everything else.
    #[test]
    fn trigger_schedule_reads_the_trigger_only() {
        let scheduled = crate::company::parse_workflow(&body("wf", Some("0 * * * *"))).unwrap();
        assert_eq!(trigger_schedule(&scheduled).as_deref(), Some("0 * * * *"));

        let bare = crate::company::parse_workflow(&body("wf", None)).unwrap();
        assert!(trigger_schedule(&bare).is_none());
    }

    /// An empty registry ticks cleanly (the shape a server with no companies
    /// boots into).
    #[tokio::test]
    async fn an_empty_registry_ticks_to_zero() {
        let clock = Arc::new(FakeClock::new(millis_at(2026, 7, 13, 9, 0)));
        let mut scheduler = WorkflowScheduler::new(CompanyRegistry::new(), clock);
        assert_eq!(scheduler.tick().await, 0);
    }
}
