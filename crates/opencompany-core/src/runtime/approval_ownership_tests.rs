use super::*;
use crate::runtime::types::ApprovalSummary;

fn summary(id: &str, task: Option<TaskLink>) -> ApprovalSummary {
    ApprovalSummary {
        id: crate::ports::types::ApprovalId::new(id),
        kind: "web_fetch".to_string(),
        group: crate::ports::types::EffectGroup::Other,
        amount_usd: None,
        at_millis: 0,
        expires_at_millis: None,
        task,
        agent: None,
        payload: None,
        thread: None,
        workflow_run_id: None,
        workflow_id: None,
        broadly_grantable: false,
        broadly_deniable: false,
        contents_hidden: false,
        batch: None,
        group_key: None,
        blocker_step_kind: None,
        episode: None,
    }
}

fn attempts(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(a, r)| (a.to_string(), r.to_string()))
        .collect()
}

fn owners(pairs: &[(&str, Option<&str>)]) -> HashMap<String, Option<String>> {
    pairs
        .iter()
        .map(|(r, t)| (r.to_string(), t.map(str::to_string)))
        .collect()
}

/// The fallback arm. Nothing to outrank the stamp, so the stamp stands —
/// a chat turn, a scheduler tick, a park from before #242.
#[test]
fn a_park_with_no_attempt_keeps_the_link_it_was_stamped_with() {
    let mut rows = vec![
        summary("a", Some(TaskLink::Task { id: "t-1".into() })),
        summary("b", Some(TaskLink::Unlinked)),
        summary("c", None),
    ];
    resolve_owners(&mut rows, &attempts(&[]), &owners(&[]));
    assert_eq!(rows[0].task, Some(TaskLink::Task { id: "t-1".into() }));
    assert_eq!(rows[1].task, Some(TaskLink::Unlinked));
    assert_eq!(rows[2].task, None);
}

/// The correction. The park stamped `t-1`; the attempt behind it belongs to
/// `t-other`, so `t-other` owns it — which is what the task detail read has
/// always said, and what the queue used to contradict.
#[test]
fn the_attempt_outranks_the_stamped_link() {
    let mut rows = vec![summary(
        "appr-elsewhere",
        Some(TaskLink::Task { id: "t-1".into() }),
    )];
    resolve_owners(
        &mut rows,
        &attempts(&[("appr-elsewhere", "run-c")]),
        &owners(&[("run-c", Some("t-other"))]),
    );
    assert_eq!(
        rows[0].task,
        Some(TaskLink::Task {
            id: "t-other".into()
        })
    );
}

/// The same rule pointing the other way: stamped `Unlinked`, but parked
/// under this card's second attempt, so it lands on the card.
#[test]
fn an_attempt_claims_a_park_that_was_stamped_unlinked() {
    let mut rows = vec![summary("appr-attempt-2", Some(TaskLink::Unlinked))];
    resolve_owners(
        &mut rows,
        &attempts(&[("appr-attempt-2", "run-b")]),
        &owners(&[("run-b", Some("t-1"))]),
    );
    assert_eq!(rows[0].task, Some(TaskLink::Task { id: "t-1".into() }));
}

/// An attempt that belongs to no card takes the approval off every card,
/// rather than letting the stale stamp put it on one. A card-level key
/// never overrides an attempt-level one.
#[test]
fn an_attempt_owned_by_no_card_unlinks_the_approval() {
    let mut rows = vec![summary("a", Some(TaskLink::Task { id: "t-1".into() }))];
    resolve_owners(
        &mut rows,
        &attempts(&[("a", "run-chat")]),
        &owners(&[("run-chat", None)]),
    );
    assert_eq!(rows[0].task, Some(TaskLink::Unlinked));
}

/// An attempt the store *says* does not exist is a definite answer — no
/// card claims it — and falling back to the stamp there would restore the
/// misattribution on exactly the rows least able to prove otherwise.
#[test]
fn an_attempt_the_store_denies_unlinks_rather_than_trusting_the_stamp() {
    let mut rows = vec![summary("a", Some(TaskLink::Task { id: "t-1".into() }))];
    resolve_owners(
        &mut rows,
        &attempts(&[("a", "run-gone")]),
        &owners(&[("run-gone", None)]),
    );
    assert_eq!(rows[0].task, Some(TaskLink::Unlinked));
}

/// A read that never succeeded is **not** that answer (#1895 review).
///
/// This is the one with teeth. Unlinking here drops the approval out of the
/// console's per-card join, and the board card then re-enables Resume over
/// something nobody decided — a transient store failure handing the
/// operator the re-dispatch. The stamp is kept instead: possibly a wrong
/// label, never a card that claims to be free while it is blocked.
#[test]
fn an_attempt_the_store_could_not_be_asked_about_keeps_its_stamp() {
    let mut rows = vec![
        summary("a", Some(TaskLink::Task { id: "t-1".into() })),
        summary("b", Some(TaskLink::Unlinked)),
    ];
    // Neither run id is in `owners` — the reads failed rather than answered.
    resolve_owners(
        &mut rows,
        &attempts(&[("a", "run-a"), ("b", "run-b")]),
        &owners(&[]),
    );
    assert_eq!(rows[0].task, Some(TaskLink::Task { id: "t-1".into() }));
    assert_eq!(rows[1].task, Some(TaskLink::Unlinked));
}

/// One failed read must not take its neighbours' answers with it.
#[test]
fn a_failed_read_does_not_disturb_the_attempts_that_answered() {
    let mut rows = vec![
        summary("a", Some(TaskLink::Task { id: "t-1".into() })),
        summary("b", Some(TaskLink::Task { id: "t-1".into() })),
    ];
    resolve_owners(
        &mut rows,
        &attempts(&[("a", "run-ok"), ("b", "run-failed")]),
        &owners(&[("run-ok", Some("t-other"))]),
    );
    assert_eq!(
        rows[0].task,
        Some(TaskLink::Task {
            id: "t-other".into()
        })
    );
    assert_eq!(rows[1].task, Some(TaskLink::Task { id: "t-1".into() }));
}

/// Two attempts at one card both land on it — #183 settled that repeat
/// trips through review are normal, so this must not read as two owners.
#[test]
fn two_attempts_at_one_card_both_resolve_to_it() {
    let mut rows = vec![
        summary("a", Some(TaskLink::Unlinked)),
        summary("b", Some(TaskLink::Unlinked)),
    ];
    resolve_owners(
        &mut rows,
        &attempts(&[("a", "run-a"), ("b", "run-b")]),
        &owners(&[("run-a", Some("t-1")), ("run-b", Some("t-1"))]),
    );
    assert_eq!(rows[0].task, Some(TaskLink::Task { id: "t-1".into() }));
    assert_eq!(rows[1].task, Some(TaskLink::Task { id: "t-1".into() }));
}
