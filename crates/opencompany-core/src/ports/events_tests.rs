use super::*;
use crate::ports::types::{Actor, ActorKind, WorkflowNodeStatus};

/// A fixed base far from epoch 0 so cutoff arithmetic stays positive.
const BASE: u64 = 1_000_000_000_000;
const DAY: u64 = 86_400_000;

fn run_started(n: u64) -> CompanyEvent {
    CompanyEvent::WorkflowRunStarted {
        workflow_id: "wf".into(),
        run_id: format!("run-{n}"),
        scheduled: false,
        started_by: None,
        resume_semantic: None,
    }
}

fn node_finished(n: u64) -> CompanyEvent {
    CompanyEvent::WorkflowNodeFinished {
        workflow_id: "wf".into(),
        run_id: format!("run-{n}"),
        node_id: format!("node-{n}"),
        status: WorkflowNodeStatus::Ok,
        elapsed_ms: 1,
        diagnostics: Vec::new(),
        agent_run_id: None,
    }
}

fn lifecycle(n: u64) -> CompanyEvent {
    CompanyEvent::LifecycleChanged {
        from: "running".into(),
        to: format!("paused-{n}"),
        by: Actor {
            kind: ActorKind::Operator,
            id: "operator".into(),
        },
    }
}

/// Builds a log from `(seq, at_millis, event)` triples.
fn log(entries: Vec<(u64, u64, CompanyEvent)>) -> Vec<StoredEvent> {
    entries
        .into_iter()
        .map(|(seq, at_millis, event)| StoredEvent {
            seq: EventSeq::new(seq),
            company: CompanyId::new("acme"),
            event,
            at_millis,
        })
        .collect()
}

fn seqs(raw: &[u64]) -> Vec<EventSeq> {
    raw.iter().copied().map(EventSeq::new).collect()
}

#[test]
fn default_policy_is_a_no_op() {
    let events = log(vec![
        (0, BASE, run_started(0)),
        (1, BASE + DAY, run_started(1)),
        (2, BASE + 2 * DAY, run_started(2)),
    ]);
    assert!(RetentionPolicy::default().is_noop());
    assert_eq!(plan_prune(&events, &RetentionPolicy::default()), vec![]);
}

#[test]
fn empty_log_prunes_nothing() {
    assert_eq!(
        plan_prune(&[], &RetentionPolicy::with_max_entries_per_kind(0)),
        vec![]
    );
}

#[test]
fn age_bound_is_anchored_to_the_newest_entry_not_a_clock() {
    // Newest is 10 days after BASE; a 5-day window keeps everything from
    // BASE+5d onward. No wall clock is consulted, so this test is stable
    // whenever it runs.
    let events = log(vec![
        (0, BASE, run_started(0)),
        (1, BASE + 4 * DAY, run_started(1)),
        (2, BASE + 5 * DAY, run_started(2)),
        (3, BASE + 10 * DAY, run_started(3)),
    ]);
    let policy = RetentionPolicy::with_max_age_millis(5 * DAY);
    assert_eq!(plan_prune(&events, &policy), seqs(&[0, 1]));
}

#[test]
fn an_entry_exactly_at_the_window_edge_is_kept() {
    let events = log(vec![
        (0, BASE, run_started(0)),
        (1, BASE + 5 * DAY, run_started(1)),
    ]);
    // Newest is BASE+5d, so the cutoff is exactly BASE. Seq 0 sits on it
    // and is inside the window.
    let policy = RetentionPolicy::with_max_age_millis(5 * DAY);
    assert_eq!(plan_prune(&events, &policy), vec![]);
}

#[test]
fn permanent_kinds_survive_any_policy() {
    let events = log(vec![
        (0, BASE, lifecycle(0)),
        (1, BASE, lifecycle(1)),
        (2, BASE, lifecycle(2)),
        (3, BASE + 1000 * DAY, run_started(0)),
    ]);
    let policy = RetentionPolicy {
        max_age_millis: Some(1),
        max_entries_per_kind: Some(0),
    };
    // Only the watermark is prunable-by-kind, and rule 3 protects it.
    assert_eq!(plan_prune(&events, &policy), vec![]);
}

#[test]
fn count_bound_keeps_the_newest_per_kind() {
    // Two prunable kinds interleaved: each gets its own budget of 1, so a
    // burst of one kind cannot evict the other.
    let events = log(vec![
        (0, BASE, run_started(0)),
        (1, BASE, node_finished(0)),
        (2, BASE, run_started(1)),
        (3, BASE, node_finished(1)),
        (4, BASE, run_started(2)),
        (5, BASE, lifecycle(0)),
    ]);
    let policy = RetentionPolicy::with_max_entries_per_kind(1);
    // Kept: seq 4 (newest run-start), seq 3 (newest node-finish), seq 5
    // (permanent, and the watermark).
    assert_eq!(plan_prune(&events, &policy), seqs(&[0, 1, 2]));
}

#[test]
fn the_sequence_watermark_is_never_removed() {
    // Every entry is prunable and every entry is over both bounds; the
    // highest sequence still survives, because fs and sqlite allocate the
    // next sequence from it.
    let events = log(vec![
        (0, BASE, run_started(0)),
        (1, BASE, run_started(1)),
        (2, BASE, run_started(2)),
    ]);
    let policy = RetentionPolicy {
        max_age_millis: Some(0),
        max_entries_per_kind: Some(0),
    };
    assert_eq!(plan_prune(&events, &policy), seqs(&[0, 1]));
}

#[test]
fn either_bound_alone_can_select_an_entry() {
    let events = log(vec![
        (0, BASE, run_started(0)),
        (1, BASE + 100 * DAY, run_started(1)),
        (2, BASE + 100 * DAY, run_started(2)),
    ]);
    // Age alone would take seq 0; count alone (keep 2) would also take
    // seq 0. Together they still take exactly seq 0 — the rule is a union,
    // not a double count.
    let policy = RetentionPolicy {
        max_age_millis: Some(DAY),
        max_entries_per_kind: Some(2),
    };
    assert_eq!(plan_prune(&events, &policy), seqs(&[0]));
}

#[test]
fn an_age_evicted_entry_does_not_consume_a_count_slot() {
    // Sequence order and timestamp order disagree here — seq 1 is the old
    // one — which is the only arrangement that can tell the two counting
    // rules apart. Backfill and clock skew both produce it.
    //
    // seq 1 is evicted by age. If it *also* consumed one of the two
    // per-kind slots, seq 0 would be evicted for being over the count.
    // It must not be: the budget is for what is kept.
    let events = log(vec![
        (0, BASE + 100 * DAY, run_started(0)),
        (1, BASE, run_started(1)),
        (2, BASE + 100 * DAY, run_started(2)),
        (3, BASE + 100 * DAY, lifecycle(0)),
    ]);
    let policy = RetentionPolicy {
        max_age_millis: Some(DAY),
        max_entries_per_kind: Some(2),
    };
    assert_eq!(plan_prune(&events, &policy), seqs(&[1]));
}

#[test]
fn input_order_does_not_change_the_outcome() {
    let mut events = log(vec![
        (0, BASE, run_started(0)),
        (1, BASE, run_started(1)),
        (2, BASE, run_started(2)),
        (3, BASE, lifecycle(0)),
    ]);
    let policy = RetentionPolicy::with_max_entries_per_kind(1);
    let forward = plan_prune(&events, &policy);
    events.reverse();
    assert_eq!(forward, plan_prune(&events, &policy));
    assert_eq!(forward, seqs(&[0, 1]));
}

#[test]
fn chat_kinds_addressed_by_sequence_are_permanent() {
    // The referents of a thread parent, a reaction, and #358's redaction
    // tombstone. Pruning any of them dangles a stored pointer.
    for event in [
        CompanyEvent::OperatorMessage {
            mentions: Vec::new(),
            text: "hi".into(),
            by: None,
            chat: None,
            parent: None,
            deliverable: None,
            attachments: Vec::new(),
        },
        CompanyEvent::AgentReply {
            audience: Vec::new(),
            mentions: Vec::new(),
            mention_depth: 0,
            chat_id: "desk".into(),
            agent_id: "ceo".into(),
            text: "hello".into(),
            steps: vec![],
            task_id: None,
            outputs: Vec::new(),
            parent: None,
            episode: None,
        },
    ] {
        assert_eq!(
            event.retention_class(),
            RetentionClass::Permanent,
            "{} must stay permanent: other entries address it by sequence",
            event.kind()
        );
    }
}

#[test]
fn kind_matches_the_serialized_tag() {
    // `kind()` is hand-written, so pin it against what serde actually
    // emits; a rename that misses one of the two would otherwise be silent.
    for event in [run_started(0), node_finished(0), lifecycle(0)] {
        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(
            json.get("kind").and_then(|k| k.as_str()),
            Some(event.kind()),
            "kind() disagrees with the serde tag"
        );
    }
}
