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

use crate::ports::workflow_runner::{DeliveryReport, DeliveryStatus};

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

/// Reports that did **not** reach their destination and will not without a
/// change — the count worth acting on.
///
/// `pending` is excluded on purpose: it is a report parked for an operator's
/// approval, so counting it here would score a working approvals queue as a
/// failure. It is counted by [`awaiting_count`] instead.
///
/// Read off [`DeliveryStatus`] rather than off
/// [`DeliveryReason`](crate::ports::DeliveryReason), which keeps this identical
/// to the reading every existing surface already performs — the console's
/// "N not delivered" badge, the scheduler's log line, the SSE toast. Three of
/// the reasons that land on [`Skipped`](DeliveryStatus::Skipped) arguably do not
/// belong in this count at all (`already-delivered`, `no-destination-configured`
/// and `dry-run` each describe a report that was never owed to an address), but
/// reclassifying them would move the badge and the verdict apart unless every
/// surface moved together. That is its own change; this one moves the *ladder*
/// to the host without moving the rungs.
pub fn undelivered_count(deliveries: &[DeliveryReport]) -> usize {
    deliveries
        .iter()
        .filter(|d| !matches!(d.status, DeliveryStatus::Sent | DeliveryStatus::Pending))
        .count()
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
}
