use super::tests_core2::*;
use super::*;

/// Two assignments that read the same card revision admit one writer and
/// explicitly refuse the stale one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_assignments_of_the_same_card_admit_exactly_one_writer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backing: Arc<dyn TaskStore> = Arc::new(FsOps::new(dir.path()));
    let record = record();
    backing
        .upsert(&record.id, &card_in("card-real", COLUMN_TODO))
        .await
        .expect("seed the real card");
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let tasks: Arc<dyn TaskStore> = Arc::new(BothReadBeforeEitherWritesStore {
        inner: backing.clone(),
        barrier,
    });
    let queue = DelegationQueue::default();
    let steer = InflightRegistry::default();
    let idle_turns_fx = Fixture::new();
    let idle_turns = ScriptedTurns::new(&idle_turns_fx, vec![]);
    let runner_a = DelegationRunner::new(
        &idle_turns,
        &record,
        Some(&tasks),
        &steer,
        &record.id,
        &queue,
        orchestrator::MAX_DELEGATIONS_PER_TURN,
    );
    let runner_b = DelegationRunner::new(
        &idle_turns,
        &record,
        Some(&tasks),
        &steer,
        &record.id,
        &queue,
        orchestrator::MAX_DELEGATIONS_PER_TURN,
    );

    let (a, b) = tokio::join!(
        runner_a.run_delegation(
            Delegation::AssignTask {
                task_id: "card-real".to_string(),
                assignee: "chief".to_string(),
                note: Some("from A".to_string()),
            },
            None,
            MessageContext::default(),
        ),
        runner_b.run_delegation(
            Delegation::AssignTask {
                task_id: "card-real".to_string(),
                assignee: "engineer".to_string(),
                note: Some("from B".to_string()),
            },
            None,
            MessageContext::default(),
        ),
    );
    let a = a.expect("A's assignment completes");
    let b = b.expect("B's assignment completes");
    let refused = usize::from(a.refused_card.is_some()) + usize::from(b.refused_card.is_some());
    assert_eq!(
        refused, 1,
        "exactly one stale assignment must be refused after both read the same revision"
    );

    let cards = backing.list(&record.id).await.unwrap();
    assert_eq!(cards.len(), 1);
    let card = &cards[0];
    assert!(
        card.assignee == "chief" || card.assignee == "engineer",
        "exactly one writer's assignment must be the one left standing: {card:?}"
    );
    let note = card.note.as_deref().unwrap_or_default();
    assert!(
        (card.assignee == "chief") == note.contains("from A")
            && (card.assignee == "engineer") == note.contains("from B"),
        "the surviving note must belong to the admitted assignee: {card:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_reviews_of_the_same_card_admit_exactly_one_writer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backing: Arc<dyn TaskStore> = Arc::new(FsOps::new(dir.path()));
    let record = record();
    backing
        .upsert(&record.id, &card_in("card-real", COLUMN_IN_REVIEW))
        .await
        .expect("seed the real card");
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let tasks: Arc<dyn TaskStore> = Arc::new(BothReadBeforeEitherWritesStore {
        inner: backing.clone(),
        barrier,
    });
    let queue = DelegationQueue::default();
    let steer = InflightRegistry::default();
    let idle_turns_fx = Fixture::new();
    let idle_turns = ScriptedTurns::new(&idle_turns_fx, vec![]);
    let runner_a = DelegationRunner::new(
        &idle_turns,
        &record,
        Some(&tasks),
        &steer,
        &record.id,
        &queue,
        orchestrator::MAX_DELEGATIONS_PER_TURN,
    );
    let runner_b = DelegationRunner::new(
        &idle_turns,
        &record,
        Some(&tasks),
        &steer,
        &record.id,
        &queue,
        orchestrator::MAX_DELEGATIONS_PER_TURN,
    );

    let (a, b) = tokio::join!(
        runner_a.run_delegation(
            Delegation::ReviewTask {
                task_id: "card-real".to_string(),
                decision: lifecycle::ReviewDecision::Approve,
                note: Some("approved by A".to_string()),
            },
            None,
            MessageContext::default(),
        ),
        runner_b.run_delegation(
            Delegation::ReviewTask {
                task_id: "card-real".to_string(),
                decision: lifecycle::ReviewDecision::Revise,
                note: Some("sent back by B".to_string()),
            },
            None,
            MessageContext::default(),
        ),
    );
    let a = a.expect("A's review completes");
    let b = b.expect("B's review completes");
    let refused = usize::from(a.refused_card.is_some()) + usize::from(b.refused_card.is_some());
    assert_eq!(
        refused, 1,
        "exactly one stale review must be refused after both read the same revision"
    );

    let cards = backing.list(&record.id).await.unwrap();
    assert_eq!(cards.len(), 1);
    let card = &cards[0];
    assert!(
        card.column == COLUMN_DONE || card.column == COLUMN_TODO,
        "the card must land wherever the admitted verdict sent it: {card:?}"
    );
    let note = card.note.as_deref().unwrap_or_default();
    assert!(
        (card.column == COLUMN_DONE) == note.contains("approved by A")
            && (card.column == COLUMN_TODO) == note.contains("sent back by B"),
        "the surviving note must belong to the admitted verdict: {card:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_same_column_assignment_cannot_be_overwritten_by_a_stale_review() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backing: Arc<dyn TaskStore> = Arc::new(FsOps::new(dir.path()));
    let record = record();
    backing
        .upsert(&record.id, &card_in("card-real", COLUMN_IN_REVIEW))
        .await
        .expect("seed the real card");
    let tasks: Arc<dyn TaskStore> = Arc::new(AssignmentBeforeReviewStore {
        inner: backing.clone(),
        both_read: Arc::new(tokio::sync::Barrier::new(2)),
        assignment_written: Arc::new(tokio::sync::Barrier::new(2)),
    });
    let queue = DelegationQueue::default();
    let steer = InflightRegistry::default();
    let idle_turns_fx = Fixture::new();
    let idle_turns = ScriptedTurns::new(&idle_turns_fx, vec![]);
    let assigner = DelegationRunner::new(
        &idle_turns,
        &record,
        Some(&tasks),
        &steer,
        &record.id,
        &queue,
        orchestrator::MAX_DELEGATIONS_PER_TURN,
    );
    let reviewer = DelegationRunner::new(
        &idle_turns,
        &record,
        Some(&tasks),
        &steer,
        &record.id,
        &queue,
        orchestrator::MAX_DELEGATIONS_PER_TURN,
    );

    let (assigned, reviewed) = tokio::join!(
        assigner.run_delegation(
            Delegation::AssignTask {
                task_id: "card-real".to_string(),
                assignee: "chief".to_string(),
                note: Some("assigned concurrently".to_string()),
            },
            None,
            MessageContext::default(),
        ),
        reviewer.run_delegation(
            Delegation::ReviewTask {
                task_id: "card-real".to_string(),
                decision: lifecycle::ReviewDecision::Approve,
                note: Some("reviewed concurrently".to_string()),
            },
            None,
            MessageContext::default(),
        ),
    );
    let assigned = assigned.expect("assignment completes");
    let reviewed = reviewed.expect("review completes");
    assert_eq!(
        usize::from(assigned.refused_card.is_some()),
        0,
        "the assignment ordered first must succeed"
    );
    assert_eq!(
        usize::from(reviewed.refused_card.is_some()),
        1,
        "the stale review ordered second must refuse"
    );

    let cards = backing.list(&record.id).await.unwrap();
    let card = &cards[0];
    let note = card.note.as_deref().unwrap_or_default();
    assert_eq!(
        card.column, COLUMN_IN_REVIEW,
        "the assignment must leave the card in review"
    );
    assert_eq!(
        card.assignee, "chief",
        "the stored card must retain the assignment's assignee"
    );
    assert!(
        note.contains("assigned concurrently"),
        "the stored card must retain the assignment note: {card:?}"
    );
}
