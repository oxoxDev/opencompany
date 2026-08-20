//! A workflow run's terminal reading, as one word (issue #981).
//!
//! # Why the host owns this
//!
//! A run's outcome was spread across six fields — `running`, `error`,
//! `cancelled`, `blockedNodes`, `deliveries`, `pendingApprovals` — and no
//! surface said what they added up to. The only place that answered "did this
//! run succeed?" was the console's TypeScript, so every other reader had to
//! re-derive it and the obvious derivation is wrong: a run whose nodes all
//! reported `ok` looks green even when its report never left the process,
//! because delivery is host-side and post-engine (`crate::workflows::delivery`)
//! and never touches a node's status.
//!
//! That is not hypothetical. The QA pass on issue #981 watched a run paint its
//! `output` node **`DONE`, green**, list it as **`ok`** in the Steps panel, and
//! score PASS in a harness folding `nodes[].status` — while the run's own
//! delivery row said `channel-not-wired` and the report was gone. Three readers,
//! three transcriptions of the same ladder, and the one fact that mattered
//! lived in none of them.
//!
//! [`WorkflowRunVerdict`] is that ladder, once, on the host. Both run DTOs
//! serialize it, so a client reads the verdict instead of inventing one.
//!
//! # Derived, never stored
//!
//! Nothing journals a verdict. It is a pure function of fields already on the
//! wire, computed at serialization time, which buys three things:
//!
//! * **No migration.** Every run already in a company's journal re-scores on
//!   deploy, including the ones written before this existed.
//! * **No third state to keep in sync.** The read-side settle (issue #1081)
//!   rewrites `running` and `error` on a run it finds dead; a stored verdict
//!   would have to be rewritten alongside them, and the one that was forgotten
//!   would be the bug. A derived one is correct by construction.
//! * **No new failure mode.** A verdict cannot disagree with the rows it was
//!   read from, because there is only ever one reading.
//!
//! # What it deliberately does NOT do
//!
//! It does not populate a run's `error`, and it does not flip any
//! `nodes[].status`. A dropped report is not a broken graph: the nodes ran, the
//! work is valid, and the fix is a destination or a runtime wiring, not a node.
//! Marking the run failed would send the copilot's fix-from-run at a graph that
//! was fine, inflate the failure count, and collapse the three terminal
//! readings issue #383 kept apart. So `undelivered` is its **own** reading —
//! neither `failed` nor `ok` — and every existing consumer of `error`,
//! `cancelled`, `running` and `nodes[].status` sees exactly what it saw before.

use serde::{Deserialize, Serialize};

use crate::ports::workflow_runner::{DeliveryReason, DeliveryReport, DeliveryStatus};

/// What a workflow run adds up to, as a closed set (issue #981).
///
/// The order of the variants is the **precedence order** in which they are
/// tested, and the order is the whole content of the type — see
/// [`WorkflowRunVerdict::of`]. Every arm below the first exists because the
/// state it names had been scoring green on some surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowRunVerdict {
    /// Started and not yet settled. Neither succeeded nor failed, and painting
    /// it as either is a claim the host has not made.
    Running,
    /// The run carries an error — a genuine break, or the boot sweep's
    /// [`INTERRUPTED_BY_RESTART`](crate::runtime::INTERRUPTED_BY_RESTART).
    Failed,
    /// An operator stopped it (issue #383). Not a fault, and deliberately not
    /// grouped with `failed`: nothing about the graph went wrong.
    Stopped,
    /// A node stopped short because a tool call inside its turn is waiting on a
    /// person (issue #881). Not a failure and not a pause — the run will not
    /// continue on its own.
    Blocked,
    /// The run did its work and at least one report **did not reach its
    /// destination and will not without a change** (issue #981).
    ///
    /// The one this type was added for. It outranks
    /// [`AwaitingApproval`](Self::AwaitingApproval) because a report that needs
    /// a fix is worse news than one waiting on a human, and it ranks *below*
    /// [`Failed`](Self::Failed) and [`Blocked`](Self::Blocked) because those
    /// describe a run that did not finish its work at all.
    Undelivered,
    /// Something about this run is waiting on a person: a gate it paused at, or
    /// a report parked in Approvals (issue #846).
    AwaitingApproval,
    /// Finished, delivered what it routed, and is waiting on nobody.
    Ok,
}

impl WorkflowRunVerdict {
    /// The wire token, matching this type's serde rendering exactly.
    ///
    /// Serde owns the wire, and this is here for the surfaces that are not
    /// JSON — the orchestrator's run summary, a log line, a test message — so
    /// they cannot drift into a second spelling of the same word.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::Blocked => "blocked",
            Self::Undelivered => "undelivered",
            Self::AwaitingApproval => "awaiting-approval",
            Self::Ok => "ok",
        }
    }

    /// Reads a run's verdict off the facts, **in precedence order**.
    ///
    /// The order is the check, and each arm records a fall-through that used to
    /// score green:
    ///
    /// * `running` first — an unsettled run has no deliveries yet, no error and
    ///   no cancel, so without this arm it falls all the way to `ok` and the
    ///   host claims a run that has not finished succeeded.
    /// * `failed` next, so **a run that broke mid-graph and also dropped a
    ///   report reports the break**. The more serious fact wins; the delivery
    ///   rows are still on the response for whoever wants both.
    /// * `stopped` before the delivery reads (issue #383) — a stop somebody
    ///   asked for is not a fault, and a cancelled run has no deliveries to
    ///   weigh anyway.
    /// * `blocked` before the delivery reads (issue #881) — a blocked run
    ///   carries no error, is not cancelled, is not running and routed no
    ///   report, which is precisely the shape that fell through every check.
    /// * `undelivered` before `awaiting-approval` (issue #981) — a report that
    ///   will not go out without a change outranks one waiting on a human.
    /// * `awaiting-approval` reads the **gates too**, not the delivery rows
    ///   alone (issue #846): a run that paused at a `requires_approval` node
    ///   never reached an `output` node, so a delivery-only read scored the
    ///   gated case — the common one — as clean.
    pub fn of(facts: RunVerdictFacts<'_>) -> Self {
        if facts.running {
            return Self::Running;
        }
        if facts.failed() {
            return Self::Failed;
        }
        if facts.cancelled {
            return Self::Stopped;
        }
        if facts.blocked_nodes > 0 {
            return Self::Blocked;
        }
        if undelivered_count(facts.deliveries) > 0 {
            return Self::Undelivered;
        }
        if awaiting_count(facts.deliveries, facts.pending_approvals) > 0 {
            return Self::AwaitingApproval;
        }
        Self::Ok
    }
}

impl std::fmt::Display for WorkflowRunVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The facts a verdict is read from — **exactly** the fields both run DTOs
/// already serialize, and nothing else.
///
/// A named struct rather than six positional arguments, because five of them
/// are `bool`/`usize` and a transposed pair would compile silently into a wrong
/// verdict on one surface only.
#[derive(Clone, Copy, Debug)]
pub struct RunVerdictFacts<'a> {
    /// The run has started and not settled.
    pub running: bool,
    /// The run's `error`, when it has one.
    pub error: Option<&'a str>,
    /// An operator stopped the run (issue #383).
    pub cancelled: bool,
    /// How many nodes blocked on a human (issue #881).
    pub blocked_nodes: usize,
    /// One row per delivery attempt this run made.
    pub deliveries: &'a [DeliveryReport],
    /// How many nodes the run is waiting on a human for.
    pub pending_approvals: usize,
}

impl RunVerdictFacts<'_> {
    /// Whether the run carries a failure.
    ///
    /// An **empty** error string is not one. No producer writes one today, and
    /// the console's `if (run.error)` has always read it as falsy — so the host
    /// agreeing costs nothing and removes a way for the two to disagree about a
    /// run neither of them can explain.
    fn failed(&self) -> bool {
        self.error.is_some_and(|e| !e.is_empty())
    }
}

/// Whether **this one report** did not reach a destination and will not without
/// a change (issue #981).
///
/// The single rung every surface stands on: the verdict below, the scheduler's
/// alert number, the sidecar's and the orchestrator's summaries, the console's
/// "N not delivered" badge, the SSE toast, and — since this exists — the
/// per-node delivery marker the console paints on the `output` node itself.
/// Written once here because a rung that only some readers honour is worse than
/// no rung at all.
///
/// # Status alone is not the reading
///
/// `sent` obviously did land and `pending` is a report parked for an operator's
/// approval — counting the latter here would score a working approvals queue as
/// a failure, so it is counted by [`awaiting_count`] instead.
///
/// The interesting half is [`Skipped`](DeliveryStatus::Skipped), which the
/// delivery path writes for three genuinely different situations. The axis that
/// separates them is **whether the report's fate is accounted for**, not whether
/// it "was owed to an address":
///
/// * [`AlreadyDelivered`](DeliveryReason::AlreadyDelivered) — an earlier run in
///   this approval lineage **sent it** (issue #438). Approving a gate re-runs the
///   graph from the trigger, so every upstream `output` node is reached a second
///   time; the report is at its destination and re-counting it as lost would
///   paint every resumed gate red.
/// * [`DryRun`](DeliveryReason::DryRun) — a test run (issue #542). Nothing was
///   attempted, on purpose, in a mode the operator chose. Counting it made the
///   *only* safe way to try a graph report a failure every single time.
/// * [`NoDestinationConfigured`](DeliveryReason::NoDestinationConfigured) — the
///   report was **produced and then lost**, with nothing accounting for it
///   (issue #925). This row exists precisely so that "the author routed nothing
///   on purpose" and "the author never configured a destination" stop being the
///   same observation; excusing it here would restore the silence issues #947
///   and #963 were filed about. **It counts.**
///
/// The match on [`DeliveryStatus`] is exhaustive and only the `Skipped` arm
/// reads a reason, so a new delivery status cannot be added without classifying
/// it, and a hypothetical `failed`/`dry-run` pair still counts.
///
/// A row carrying [`Unspecified`](DeliveryReason::Unspecified) — the only
/// reachable value on a `WorkflowRunFinished` journaled before issue #248 added
/// the field — counts, which is the safe direction: an unreadable reason must
/// not excuse a report from the number an operator acts on.
pub fn is_undelivered(report: &DeliveryReport) -> bool {
    match report.status {
        DeliveryStatus::Sent | DeliveryStatus::Pending => false,
        DeliveryStatus::Skipped => !matches!(
            report.reason,
            DeliveryReason::AlreadyDelivered | DeliveryReason::DryRun
        ),
        DeliveryStatus::Denied | DeliveryStatus::Failed => true,
    }
}

/// How many of a run's reports did **not** reach their destination and will not
/// without a change — the count worth acting on.
///
/// A fold of [`is_undelivered`], which is where the reasoning lives.
pub fn undelivered_count(deliveries: &[DeliveryReport]) -> usize {
    deliveries.iter().filter(|d| is_undelivered(d)).count()
}

/// Everything about a run that is waiting on a person: the gates it paused at
/// **and** the reports it parked (issue #846).
///
/// The two were never read together, which is what let a run report success
/// while a human had not answered it — a run that paused at a
/// `requires_approval` node never reaches an `output` node, so its deliveries
/// are empty and a delivery-only read scored it clean.
pub fn awaiting_count(deliveries: &[DeliveryReport], pending_approvals: usize) -> usize {
    pending_approvals
        + deliveries
            .iter()
            .filter(|d| matches!(d.status, DeliveryStatus::Pending))
            .count()
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::ports::DeliveryReason;

    fn row(status: DeliveryStatus, reason: DeliveryReason) -> DeliveryReport {
        DeliveryReport {
            node: "report".into(),
            kind: "channel".into(),
            target: Some("engineering".into()),
            status,
            detail: "detail".into(),
            reason,
        }
    }

    /// A run that finished cleanly and routed nothing — the base every case
    /// below varies by exactly one fact.
    fn clean() -> RunVerdictFacts<'static> {
        RunVerdictFacts {
            running: false,
            error: None,
            cancelled: false,
            blocked_nodes: 0,
            deliveries: &[],
            pending_approvals: 0,
        }
    }

    #[test]
    fn a_clean_run_is_ok() {
        assert_eq!(WorkflowRunVerdict::of(clean()), WorkflowRunVerdict::Ok);
    }

    /// The defect issue #981 filed: every node `ok`, no error, nothing
    /// cancelled — and the report is gone.
    #[test]
    fn a_run_whose_only_failure_is_delivery_is_not_ok() {
        let dropped = [row(DeliveryStatus::Failed, DeliveryReason::ChannelNotWired)];
        let verdict = WorkflowRunVerdict::of(RunVerdictFacts {
            deliveries: &dropped,
            ..clean()
        });
        assert_eq!(verdict, WorkflowRunVerdict::Undelivered);
        assert_ne!(verdict, WorkflowRunVerdict::Ok);
        // …and it is not reported as a failure either. The nodes ran.
        assert_ne!(verdict, WorkflowRunVerdict::Failed);
    }

    /// The other two refusals issue #981 names reach the same verdict, and
    /// through their own `DeliveryStatus` rather than through a shared one — so
    /// this pins that the count is not accidentally reading only `Failed`.
    #[test]
    fn a_denied_or_skipped_report_is_undelivered_too() {
        for status in [DeliveryStatus::Denied, DeliveryStatus::Skipped] {
            let rows = [row(status, DeliveryReason::EmailNotGranted)];
            assert_eq!(
                WorkflowRunVerdict::of(RunVerdictFacts {
                    deliveries: &rows,
                    ..clean()
                }),
                WorkflowRunVerdict::Undelivered,
                "{status:?} is a report that did not go out"
            );
        }
    }

    #[test]
    fn a_run_that_delivered_everything_is_unchanged() {
        let sent = [row(DeliveryStatus::Sent, DeliveryReason::ChannelPosted)];
        assert_eq!(
            WorkflowRunVerdict::of(RunVerdictFacts {
                deliveries: &sent,
                ..clean()
            }),
            WorkflowRunVerdict::Ok
        );
    }

    /// The more serious fact first: a run that broke mid-graph AND dropped its
    /// report reports the break, not the drop.
    #[test]
    fn a_failed_run_that_also_dropped_a_report_reads_failed() {
        let dropped = [row(DeliveryStatus::Failed, DeliveryReason::ChannelNotWired)];
        assert_eq!(
            WorkflowRunVerdict::of(RunVerdictFacts {
                error: Some("node `draft` errored"),
                deliveries: &dropped,
                ..clean()
            }),
            WorkflowRunVerdict::Failed
        );
    }

    #[test]
    fn the_precedence_order_is_the_whole_check() {
        let dropped = [row(DeliveryStatus::Failed, DeliveryReason::ChannelNotWired)];
        // Every arm asserted against a fact set that ALSO satisfies every arm
        // below it, so a reordering breaks this rather than passing by luck.
        let everything = RunVerdictFacts {
            running: true,
            error: Some("boom"),
            cancelled: true,
            blocked_nodes: 1,
            deliveries: &dropped,
            pending_approvals: 1,
        };
        assert_eq!(
            WorkflowRunVerdict::of(everything),
            WorkflowRunVerdict::Running
        );
        assert_eq!(
            WorkflowRunVerdict::of(RunVerdictFacts {
                running: false,
                ..everything
            }),
            WorkflowRunVerdict::Failed
        );
        assert_eq!(
            WorkflowRunVerdict::of(RunVerdictFacts {
                running: false,
                error: None,
                ..everything
            }),
            WorkflowRunVerdict::Stopped
        );
        assert_eq!(
            WorkflowRunVerdict::of(RunVerdictFacts {
                running: false,
                error: None,
                cancelled: false,
                ..everything
            }),
            WorkflowRunVerdict::Blocked
        );
        assert_eq!(
            WorkflowRunVerdict::of(RunVerdictFacts {
                running: false,
                error: None,
                cancelled: false,
                blocked_nodes: 0,
                ..everything
            }),
            WorkflowRunVerdict::Undelivered
        );
        assert_eq!(
            WorkflowRunVerdict::of(RunVerdictFacts {
                running: false,
                error: None,
                cancelled: false,
                blocked_nodes: 0,
                deliveries: &[],
                ..everything
            }),
            WorkflowRunVerdict::AwaitingApproval
        );
    }

    /// A parked report is waiting on a person, not on a fix — so it must never
    /// land in the undelivered count, which would badge a working approvals
    /// queue as a failure.
    #[test]
    fn a_parked_report_is_awaiting_not_undelivered() {
        let parked = [row(
            DeliveryStatus::Pending,
            DeliveryReason::ParkedForApproval,
        )];
        assert_eq!(undelivered_count(&parked), 0);
        assert_eq!(awaiting_count(&parked, 0), 1);
        assert_eq!(
            WorkflowRunVerdict::of(RunVerdictFacts {
                deliveries: &parked,
                ..clean()
            }),
            WorkflowRunVerdict::AwaitingApproval
        );
    }

    /// Issue #846: a gated run reaches no `output` node, so its verdict has to
    /// come off `pending_approvals` or it scores clean.
    #[test]
    fn a_gated_run_with_no_deliveries_is_awaiting() {
        assert_eq!(
            WorkflowRunVerdict::of(RunVerdictFacts {
                pending_approvals: 1,
                ..clean()
            }),
            WorkflowRunVerdict::AwaitingApproval
        );
    }

    /// An error string the host never writes, read the way the console reads
    /// it: empty is not a failure.
    #[test]
    fn an_empty_error_is_not_a_failure() {
        assert_eq!(
            WorkflowRunVerdict::of(RunVerdictFacts {
                error: Some(""),
                ..clean()
            }),
            WorkflowRunVerdict::Ok
        );
    }

    /// The wire tokens are the console's seven words, and `as_str` may not
    /// drift from them.
    #[test]
    fn the_wire_tokens_are_the_consoles_words() {
        for (verdict, token) in [
            (WorkflowRunVerdict::Running, "running"),
            (WorkflowRunVerdict::Failed, "failed"),
            (WorkflowRunVerdict::Stopped, "stopped"),
            (WorkflowRunVerdict::Blocked, "blocked"),
            (WorkflowRunVerdict::Undelivered, "undelivered"),
            (WorkflowRunVerdict::AwaitingApproval, "awaiting-approval"),
            (WorkflowRunVerdict::Ok, "ok"),
        ] {
            assert_eq!(
                serde_json::to_value(verdict).expect("serializes"),
                serde_json::Value::String(token.to_string())
            );
            assert_eq!(verdict.as_str(), token);
            assert_eq!(verdict.to_string(), token);
        }
    }

    /// Issue #981, the second half: a **test run** attempted nothing, on
    /// purpose, so its rows are not reports that went missing.
    ///
    /// This was a live false positive, not a theoretical one — `deliver_outputs_dry`
    /// writes one `skipped`/`dry-run` row per routed `output` node, so before
    /// this every single test run of a graph with a destination scored
    /// `undelivered` and the console badged the safest thing an operator can do
    /// as a failure.
    #[test]
    fn a_dry_run_is_not_undelivered() {
        let dry = [row(DeliveryStatus::Skipped, DeliveryReason::DryRun)];
        assert!(!is_undelivered(&dry[0]));
        assert_eq!(undelivered_count(&dry), 0);
        assert_eq!(
            WorkflowRunVerdict::of(RunVerdictFacts {
                deliveries: &dry,
                ..clean()
            }),
            WorkflowRunVerdict::Ok
        );
    }

    /// Issue #438: approving a gate re-runs the graph from the trigger, so an
    /// `output` node upstream of the gate is reached a second time and
    /// deliberately not sent again. The report is at its destination; the
    /// continuation is not a run that lost one.
    #[test]
    fn an_already_delivered_report_is_not_undelivered() {
        let again = [row(
            DeliveryStatus::Skipped,
            DeliveryReason::AlreadyDelivered,
        )];
        assert!(!is_undelivered(&again[0]));
        assert_eq!(
            WorkflowRunVerdict::of(RunVerdictFacts {
                deliveries: &again,
                ..clean()
            }),
            WorkflowRunVerdict::Ok
        );
    }

    /// The deliberate **non**-move, and the reason the other two could move at
    /// all: an `output` node with nowhere to send produced a report and lost it,
    /// with nothing accounting for it. Issue #925 added the row precisely so
    /// that case stops being indistinguishable from a graph that routed nothing
    /// on purpose; excusing it here restores the silence issues #947 and #963
    /// were filed about.
    #[test]
    fn an_output_node_with_no_destination_is_still_undelivered() {
        let nowhere = [row(
            DeliveryStatus::Skipped,
            DeliveryReason::NoDestinationConfigured,
        )];
        assert!(is_undelivered(&nowhere[0]));
        assert_eq!(
            WorkflowRunVerdict::of(RunVerdictFacts {
                deliveries: &nowhere,
                ..clean()
            }),
            WorkflowRunVerdict::Undelivered
        );
    }

    /// A row journaled before issue #248 added `reason` deserializes as
    /// `Unspecified`, and an unreadable reason must not excuse a report from the
    /// number an operator acts on.
    #[test]
    fn a_skipped_row_with_no_recorded_reason_still_counts() {
        let old = [row(DeliveryStatus::Skipped, DeliveryReason::Unspecified)];
        assert!(is_undelivered(&old[0]));
    }

    /// Only the `skipped` arm reads a reason. A `failed` row is a report that
    /// was attempted and did not work, whatever it claims about why — so the
    /// two exemptions cannot leak onto a status that means something broke.
    #[test]
    fn the_exemptions_are_scoped_to_skipped() {
        for status in [DeliveryStatus::Failed, DeliveryStatus::Denied] {
            for reason in [DeliveryReason::DryRun, DeliveryReason::AlreadyDelivered] {
                assert!(
                    is_undelivered(&row(status, reason)),
                    "{status:?}/{reason:?} is not a skip"
                );
            }
        }
    }

    /// `sent` and `pending` are excused by **status**, so no reason can pull
    /// them into the count either.
    #[test]
    fn sent_and_pending_are_never_undelivered() {
        for status in [DeliveryStatus::Sent, DeliveryStatus::Pending] {
            for reason in [
                DeliveryReason::ChannelPosted,
                DeliveryReason::ParkedForApproval,
                DeliveryReason::NoDestinationConfigured,
            ] {
                assert!(!is_undelivered(&row(status, reason)));
            }
        }
    }
}
