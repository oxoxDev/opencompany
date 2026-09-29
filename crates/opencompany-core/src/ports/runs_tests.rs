use super::*;
use crate::ports::types::{TurnStepKind, TurnStepStatus};

/// Every status, so a table-driven test cannot silently miss a new variant
/// (adding one without extending this list fails the exhaustiveness check
/// in [`all_statuses_are_listed`]).
const ALL: [RunStatus; 9] = [
    RunStatus::Pending,
    RunStatus::Running,
    RunStatus::WaitingApproval,
    RunStatus::Paused,
    RunStatus::Blocked,
    RunStatus::Succeeded,
    RunStatus::Failed,
    RunStatus::Cancelled,
    RunStatus::Declined,
];

#[test]
fn all_statuses_are_listed() {
    // A `match` with no wildcard: adding a variant breaks compilation here
    // first, which is the reminder to extend `ALL`.
    for status in ALL {
        match status {
            RunStatus::Pending
            | RunStatus::Running
            | RunStatus::WaitingApproval
            | RunStatus::Paused
            | RunStatus::Blocked
            | RunStatus::Succeeded
            | RunStatus::Failed
            | RunStatus::Cancelled
            | RunStatus::Declined => (),
        };
    }
    let mut seen: Vec<&str> = ALL.iter().map(|s| s.as_str()).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), ALL.len(), "status literals must be unique");
}

/// Issue #1861: a blocker waits on a person, so it is **parked** — not
/// terminal, and not active. A terminal blocker could never be answered;
/// an active one would be reclaimed by the boot reaper as an orphan while
/// somebody was still deciding what to say.
#[test]
fn a_blocker_is_parked_not_terminal() {
    assert!(RunStatus::Blocked.is_parked());
    assert!(!RunStatus::Blocked.is_terminal());
    assert!(!RunStatus::Blocked.is_active());
    assert_eq!(RunStatus::Blocked.phase(), "parked");
}

/// The answer resumes the work, and an unanswered blocker settles through
/// the TTL — so both edges out of `Blocked` have to exist.
#[test]
fn a_blocker_can_resume_or_settle() {
    assert!(RunStatus::Blocked.can_transition_to(RunStatus::Running));
    assert!(RunStatus::Blocked.can_transition_to(RunStatus::Failed));
    assert!(RunStatus::Running.can_transition_to(RunStatus::Blocked));
    assert!(
        !RunStatus::Succeeded.can_transition_to(RunStatus::Blocked),
        "a finished attempt cannot start waiting on somebody"
    );
}

#[test]
fn status_literals_match_their_serde_form() {
    for status in ALL {
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(
            json,
            format!("\"{}\"", status.as_str()),
            "the indexed column literal and the wire form must not drift"
        );
    }
}

/// The `?status=` filter parses the exact literal the backend indexes —
/// pinned in both directions so neither table can drift alone.
#[test]
fn from_wire_round_trips() {
    for status in ALL {
        assert_eq!(
            RunStatus::from_wire(status.as_str()),
            Some(status),
            "{status} does not parse back from its own literal"
        );
    }
    assert_eq!(RunStatus::from_wire("Succeeded"), None, "case is exact");
    assert_eq!(RunStatus::from_wire("waitingApproval"), None);
    assert_eq!(RunStatus::from_wire(""), None);
}

/// `phase()` is only sound if the three predicates partition the enum:
/// exactly one must hold for every status, or a run would report a phase
/// that contradicts one of them.
#[test]
fn phases_partition_every_status() {
    for status in ALL {
        let held = [status.is_active(), status.is_parked(), status.is_terminal()]
            .into_iter()
            .filter(|b| *b)
            .count();
        assert_eq!(held, 1, "{status} is in {held} phases, not exactly one");
    }
    assert_eq!(RunStatus::Pending.phase(), "active");
    assert_eq!(RunStatus::Running.phase(), "active");
    assert_eq!(RunStatus::WaitingApproval.phase(), "parked");
    assert_eq!(RunStatus::Paused.phase(), "parked");
    assert_eq!(RunStatus::Succeeded.phase(), "terminal");
    assert_eq!(RunStatus::Failed.phase(), "terminal");
    assert_eq!(RunStatus::Cancelled.phase(), "terminal");
    assert_eq!(RunStatus::Declined.phase(), "terminal");
}

/// The trap this whole projection exists for: a parked run has **no**
/// finish time, exactly like a live one, so a reader that inferred
/// liveness from the timestamp would call a run waiting on a person "still
/// running" forever.
#[test]
fn a_parked_run_has_no_finish_time_and_is_not_active() {
    for parked in [RunStatus::WaitingApproval, RunStatus::Paused] {
        let mut record = run("r1", 10, 1);
        record.status = RunStatus::Running;
        record.started_at_millis = Some(11);
        // What `finish_run` would stamp: only a terminal settle is a finish.
        record.status = parked;
        record.finished_at_millis = parked.is_terminal().then_some(12);
        assert_eq!(record.finished_at_millis, None);
        assert!(!record.is_active(), "{parked} must not read as live");
        assert_eq!(record.status.phase(), "parked");
    }
}

#[test]
fn terminal_statuses_are_final() {
    for from in ALL.into_iter().filter(|s| s.is_terminal()) {
        for to in ALL {
            assert!(
                !from.can_transition_to(to),
                "{from} is terminal but claims it can move to {to}"
            );
        }
    }
}

#[test]
fn pending_only_starts_or_dies() {
    assert!(RunStatus::Pending.can_transition_to(RunStatus::Running));
    assert!(RunStatus::Pending.can_transition_to(RunStatus::Failed));
    assert!(RunStatus::Pending.can_transition_to(RunStatus::Cancelled));
    // A run that never began cannot have succeeded... but it also cannot
    // park: nothing has run to hit an approval or a rate limit.
    assert!(!RunStatus::Pending.can_transition_to(RunStatus::WaitingApproval));
    assert!(!RunStatus::Pending.can_transition_to(RunStatus::Paused));
    assert!(!RunStatus::Pending.can_transition_to(RunStatus::Pending));
}

#[test]
fn running_cannot_restart() {
    assert!(!RunStatus::Running.can_transition_to(RunStatus::Running));
    assert!(!RunStatus::Running.can_transition_to(RunStatus::Pending));
}

/// Epic #183 decision 3: an attempt may enter review many times, because
/// #243 grants are single-use and argument-exact.
#[test]
fn waiting_approval_is_re_enterable() {
    assert!(RunStatus::Running.can_transition_to(RunStatus::WaitingApproval));
    assert!(RunStatus::WaitingApproval.can_transition_to(RunStatus::Running));
    // …and round again.
    assert!(RunStatus::Running.can_transition_to(RunStatus::WaitingApproval));
}

/// Epic #183 decision 2: who unblocks it decides where it parks, so the two
/// parked states are distinct and both are non-terminal and resumable.
#[test]
fn both_parked_states_resume() {
    for parked in [RunStatus::WaitingApproval, RunStatus::Paused] {
        assert!(parked.is_parked());
        assert!(!parked.is_terminal());
        assert!(!parked.is_active());
        assert!(parked.can_transition_to(RunStatus::Running));
        assert!(parked.can_transition_to(RunStatus::Succeeded));
    }
    // A run waiting on a person can turn into one waiting on a dependency
    // (and back) without a terminal hop in between.
    assert!(RunStatus::WaitingApproval.can_transition_to(RunStatus::Paused));
    assert!(RunStatus::Paused.can_transition_to(RunStatus::WaitingApproval));
}

#[test]
fn only_pending_and_running_are_reapable() {
    for status in ALL {
        assert_eq!(
            status.is_active(),
            matches!(status, RunStatus::Pending | RunStatus::Running),
            "{status} is misclassified for the boot reaper"
        );
    }
}

fn run(id: &str, created: u64, attempt: u32) -> RunRecord {
    RunRecord {
        id: id.to_string(),
        company: CompanyId::new("alpha"),
        task_id: Some("card".to_string()),
        chat_id: None,
        agent_id: "ceo".to_string(),
        attempt,
        status: RunStatus::Pending,
        trigger_event_seq: None,
        thread_root: None,
        created_at_millis: created,
        started_at_millis: None,
        finished_at_millis: None,
        error: None,
        usage: TokenUsage::default(),
        step_count: 0,
        workflow_run_id: None,
        node_id: None,
        episode_id: None,
        round_revision: None,
    }
}

#[test]
fn ordering_breaks_ties_deterministically() {
    // Same millisecond — the common case, not the exotic one.
    let mut runs = vec![run("a", 10, 1), run("c", 10, 3), run("b", 10, 2)];
    sort_newest_first(&mut runs);
    let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["c", "b", "a"], "newest attempt first within a tick");

    // A later timestamp outranks a higher ordinal.
    let mut runs = vec![run("old", 10, 9), run("new", 20, 1)];
    sort_newest_first(&mut runs);
    assert_eq!(runs[0].id, "new");
}

#[test]
fn filter_matches_task_and_status() {
    let mut pending = run("a", 10, 1);
    let mut done = run("b", 10, 2);
    done.status = RunStatus::Succeeded;
    done.task_id = Some("other".to_string());

    assert!(RunFilter::default().matches(&pending));
    assert!(RunFilter::default().matches(&done));

    assert!(RunFilter::for_task("card").matches(&pending));
    assert!(!RunFilter::for_task("card").matches(&done));

    assert!(RunFilter::active().matches(&pending));
    assert!(!RunFilter::active().matches(&done));

    pending.status = RunStatus::Running;
    assert!(RunFilter::active().matches(&pending));

    let only_succeeded = RunFilter::default().with_status(RunStatus::Succeeded);
    assert!(only_succeeded.matches(&done));
    assert!(!only_succeeded.matches(&pending));
}

#[test]
fn step_records_round_trip() {
    let record = RunStepRecord {
        run_id: "r1".to_string(),
        step_seq: 0,
        at_millis: 42,
        step: TurnStep {
            kind: TurnStepKind::ToolCall,
            status: TurnStepStatus::Ok,
            label: "Reading messages".to_string(),
            detail: None,
            elapsed_ms: Some(12),
            ..TurnStep::default()
        },
    };
    let json = serde_json::to_string(&record).unwrap();
    assert_eq!(record, serde_json::from_str(&json).unwrap());
    // The wire form is camelCase, like every other console-facing shape.
    assert!(json.contains("\"runId\""), "{json}");
    assert!(json.contains("\"stepSeq\""), "{json}");
}

#[test]
fn run_records_round_trip_and_omit_empty_optionals() {
    let mut record = run("r1", 100, 1);
    let json = serde_json::to_string(&record).unwrap();
    assert_eq!(record, serde_json::from_str(&json).unwrap());
    assert!(!json.contains("triggerEventSeq"), "{json}");
    assert!(!json.contains("finishedAtMillis"), "{json}");

    record.status = RunStatus::Failed;
    record.trigger_event_seq = Some(EventSeq::new(7));
    record.finished_at_millis = Some(200);
    record.error = Some(ORPHAN_ERROR.to_string());
    record.usage = TokenUsage {
        input: 10,
        output: 5,
        cached_input: 1,
        cost_usd: 0.25,
    };
    record.step_count = 3;
    let json = serde_json::to_string(&record).unwrap();
    assert_eq!(record, serde_json::from_str(&json).unwrap());
    assert!(json.contains("\"status\":\"failed\""), "{json}");
}
