use super::*;

use crate::policy::ManifestApprovalGate;
use crate::ports::types::{EffectGroup, EventSeq};

struct Fixture {
    parker: ApprovalParker,
    journal: Arc<RuntimeJournal>,
    grants: GrantSet,
    continuations: ContinuationQueue,
    events: Arc<dyn EventLog>,
    company: CompanyId,
    _dir: tempfile::TempDir,
}

fn fixture(journal_writable: bool) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = toml::from_str("mode = \"supervised\"\n").expect("valid [policy] block");
    let gate: Arc<dyn ApprovalGate> = Arc::new(ManifestApprovalGate::new(policy));
    let journal_path = dir.path().join("journal.jsonl");
    if !journal_writable {
        std::fs::create_dir_all(&journal_path).expect("journal path occupied by a directory");
    }
    let journal = Arc::new(RuntimeJournal::new(journal_path));
    let grants = GrantSet::default();
    let continuations = ContinuationQueue::default();
    let events: Arc<dyn EventLog> = Arc::new(crate::store::FsEventLog::new(dir.path()));
    Fixture {
        parker: ApprovalParker::new(
            gate,
            journal.clone(),
            grants.clone(),
            continuations.clone(),
            events.clone(),
        ),
        journal,
        grants,
        continuations,
        events,
        company: CompanyId::new("acme"),
        _dir: dir,
    }
}

fn effect() -> Effect {
    Effect {
        kind: "shell".to_string(),
        group: EffectGroup::Other,
        amount_usd: None,
        established_thread: false,
        first_time_counterparty: false,
        payload: serde_json::json!({ "call": "shell" }),
        agent: Some("ceo".to_string()),
        run_id: None,
    }
}

async fn parked_events(fx: &Fixture) -> Vec<ApprovalId> {
    fx.events
        .read_from(&fx.company, EventSeq::new(0), usize::MAX)
        .await
        .expect("event log reads")
        .into_iter()
        .filter_map(|stored| match stored.event {
            CompanyEvent::ApprovalParked { approval_id, .. } => Some(approval_id),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_park_is_counted_journaled_held_and_announced() {
    let fx = fixture(true);
    let id = fx
        .parker
        .park(
            &fx.company,
            effect(),
            ParkSite {
                task: TaskLink::from_task_id(Some("card-1")),
                conversation: ApprovalConversation {
                    thread: Some("ops".to_string()),
                    parent: Some(EventSeq::new(7)),
                },
                turn: Some("cycle-1".to_string()),
            },
        )
        .await
        .expect("park succeeds");

    assert_eq!(fx.continuations.outstanding("cycle-1"), 1);
    let pending = fx.journal.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, id);
    assert_eq!(pending[0].thread.as_deref(), Some("ops"));
    assert!(
        fx.grants.any_for_task("card-1"),
        "the card's checkout is held while its approval waits"
    );
    assert_eq!(parked_events(&fx).await, vec![id]);
}

#[tokio::test]
async fn a_conversation_park_holds_the_threads_work_unit() {
    let fx = fixture(true);
    fx.parker
        .park(
            &fx.company,
            effect(),
            ParkSite {
                task: TaskLink::Unlinked,
                conversation: ApprovalConversation {
                    thread: Some("ops".to_string()),
                    parent: None,
                },
                turn: None,
            },
        )
        .await
        .expect("park succeeds");

    let key = crate::runtime::cycle::sanitize_work_segment("ops").expect("a usable segment");
    assert!(fx.grants.any_for_task(&key));
}

#[tokio::test]
async fn a_park_the_journal_refuses_leaves_nothing_behind() {
    let fx = fixture(false);
    let result = fx
        .parker
        .park(
            &fx.company,
            effect(),
            ParkSite {
                task: TaskLink::from_task_id(Some("card-1")),
                conversation: ApprovalConversation::default(),
                turn: Some("cycle-1".to_string()),
            },
        )
        .await;

    assert!(result.is_err(), "the journal failure is the caller's error");
    assert_eq!(
        fx.continuations.outstanding("cycle-1"),
        0,
        "the slot armed for a card that never became durable is released"
    );
    assert!(fx.journal.pending().is_empty());
    assert!(!fx.grants.any_for_task("card-1"));
    assert!(parked_events(&fx).await.is_empty());
}
