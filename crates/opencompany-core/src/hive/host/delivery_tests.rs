//! What a seat's turn hands over, and which row carries it.

use std::sync::Arc;

use std::sync::atomic::AtomicBool;

use async_trait::async_trait;
use tinyhivemind::aside::Viewer;
use tinyhivemind::{Conversation, SESSION_WINDOW, SessionQuery, project_session};
use tinyhivemind_driver::Commit;
use tinyhivemind_openhuman::Journal;

use super::*;
use crate::hive::test_support::MemoryLog;
use crate::ports::events::{EventLog, EventStreamItem};
use crate::ports::types::{ChatOutput, ChatOutputKind, CompanyId, EventSeq, StoredEvent};
use crate::{OpenCompanyError, Result};

/// A journal that refuses the first append whose event matches `refuse_when`,
/// then behaves like an ordinary [`MemoryLog`] afterwards.
struct FlakyLog {
    inner: MemoryLog,
    refused: AtomicBool,
}

impl FlakyLog {
    fn new() -> Self {
        Self {
            inner: MemoryLog::default(),
            refused: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl EventLog for FlakyLog {
    async fn append(&self, id: &CompanyId, event: CompanyEvent) -> Result<EventSeq> {
        let is_standalone_delivery_row = matches!(
            &event,
            CompanyEvent::AgentReply { text, outputs, .. }
                if text.is_empty() && !outputs.is_empty()
        );
        if is_standalone_delivery_row && !self.refused.swap(true, Ordering::SeqCst) {
            return Err(OpenCompanyError::Harness(
                "journal refused the write (test)".to_owned(),
            ));
        }
        self.inner.append(id, event).await
    }

    async fn read_from(
        &self,
        id: &CompanyId,
        seq: EventSeq,
        limit: usize,
    ) -> Result<Vec<StoredEvent>> {
        self.inner.read_from(id, seq, limit).await
    }

    fn subscribe(&self, id: &CompanyId) -> futures::stream::BoxStream<'static, EventStreamItem> {
        self.inner.subscribe(id)
    }
}

fn host(events: Arc<dyn EventLog>) -> DeskHost {
    DeskHost::new(
        CompanyId::new("acme"),
        "engineering".to_owned(),
        "Engineering".to_owned(),
        events,
        vec!["one".to_owned(), "two".to_owned()],
    )
    .episode("ep1")
    .in_thread(Some(EventSeq::new(7)))
}

fn commit(author: &str, utterance: serde_json::Value) -> Commit {
    serde_json::from_value(serde_json::json!({
        "author": author,
        "utterance": utterance,
        "thread": null,
        "only_for": null,
        "conversation": null,
        "purpose": { "kind": "desk" },
    }))
    .expect("a commit the driver would make")
}

fn post(author: &str, message: &str) -> Commit {
    commit(
        author,
        serde_json::json!({ "kind": "post", "message": message }),
    )
}

fn artifact(id: &str) -> ChatOutput {
    ChatOutput {
        kind: ChatOutputKind::Artifact,
        target_id: id.to_owned(),
        title: "Slide outline".to_owned(),
        task_id: Some("card-1".to_owned()),
        version: Some(1),
    }
}

fn delivered(id: &str) -> Delivery {
    Delivery {
        outputs: vec![artifact(id)],
        task_id: Some("card-1".to_owned()),
    }
}

/// Every reply on the journal, as `(author, text, outputs, task, episode kind)`.
async fn replies(
    log: &MemoryLog,
) -> Vec<(
    String,
    String,
    Vec<ChatOutput>,
    Option<String>,
    Option<UtteranceKind>,
)> {
    let rows: Vec<StoredEvent> = log
        .read_from(&MemoryLog::company(), EventSeq::new(0), 64)
        .await
        .expect("reads");
    rows.into_iter()
        .filter_map(|stored| match stored.event {
            CompanyEvent::AgentReply {
                agent_id,
                text,
                outputs,
                task_id,
                episode,
                ..
            } => Some((
                agent_id,
                text,
                outputs,
                task_id,
                episode.map(|episode| episode.kind),
            )),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_seats_delivery_rides_its_next_desk_row_once() {
    let log = Arc::new(MemoryLog::default());
    let host = host(log.clone() as Arc<dyn EventLog>);
    host.hold_delivery("one", delivered("a-1"));

    host.commit(&post("two", "not mine")).expect("commits");
    host.commit(&commit(
        "one",
        serde_json::json!({ "kind": "complete_episode", "message": "outline is ready" }),
    ))
    .expect("commits");
    host.commit(&post("one", "anything else?"))
        .expect("commits");
    host.settle_deliveries();
    host.settle_deliveries();

    let rows = replies(&log).await;
    assert_eq!(rows.len(), 3, "no extra row: {rows:?}");
    assert!(rows[0].2.is_empty(), "another seat's row carries nothing");
    assert_eq!(rows[1].1, "outline is ready");
    assert_eq!(rows[1].2, vec![artifact("a-1")]);
    assert_eq!(rows[1].3.as_deref(), Some("card-1"));
    assert!(rows[2].2.is_empty(), "handed over once: {rows:?}");
    assert_eq!(rows[2].3, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delivery_with_no_desk_row_gets_its_own_row_when_the_wave_ends() {
    let log = Arc::new(MemoryLog::default());
    let host = host(log.clone() as Arc<dyn EventLog>);
    host.hold_delivery("one", delivered("a-1"));

    host.commit(&post("two", "still thinking"))
        .expect("commits");
    host.settle_deliveries();
    assert_eq!(
        replies(&log).await.len(),
        1,
        "a commit's checkpoint is not the end of the wave"
    );

    host.settle_deliveries();
    let rows = replies(&log).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    let (author, text, outputs, task, kind) = &rows[1];
    assert_eq!(author, "one");
    assert!(text.is_empty());
    assert_eq!(outputs, &vec![artifact("a-1")]);
    assert_eq!(task.as_deref(), Some("card-1"));
    assert_eq!(
        *kind,
        Some(UtteranceKind::Post),
        "it belongs to the episode"
    );

    host.settle_deliveries();
    assert_eq!(replies(&log).await.len(), 2, "written once");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ask_does_not_carry_a_delivery_into_a_private_conversation() {
    let log = Arc::new(MemoryLog::default());
    let host = host(log.clone() as Arc<dyn EventLog>);
    host.hold_delivery("one", delivered("a-1"));

    host.commit(&commit(
        "one",
        serde_json::json!({ "kind": "ask", "to": "two", "message": "does the outline hold?" }),
    ))
    .expect("commits");
    host.settle_deliveries();
    host.flush_deliveries();

    let rows = replies(&log).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].2.is_empty(), "the ask stays between the pair");
    assert_eq!(rows[1].2, vec![artifact("a-1")], "the desk gets it instead");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_row_that_only_carries_outputs_is_not_shown_to_seats_as_speech() {
    let log = Arc::new(MemoryLog::default());
    let host = host(log.clone() as Arc<dyn EventLog>);
    host.commit(&post("two", "the plan is on the desk"))
        .expect("commits");
    host.hold_delivery("one", delivered("a-1"));
    host.flush_deliveries();

    let shown = project_session(
        host.log(),
        &SessionQuery {
            conversation: Conversation {
                desk_id: "engineering".to_owned(),
                desk_name: "Engineering".to_owned(),
                thread_root: None,
            },
            viewer: Viewer::Agent { id: "two".into() },
            before: None,
            window: SESSION_WINDOW,
        },
    )
    .await
    .expect("reads");
    let readable: Vec<&str> = shown.iter().filter_map(|row| row.readable()).collect();
    assert_eq!(readable, vec!["the plan is on the desk"]);
    assert_eq!(replies(&log).await.len(), 2, "the row is still journaled");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delivery_survives_a_failed_journal_write_and_is_retried() {
    let log = Arc::new(FlakyLog::new());
    let host = host(log.clone() as Arc<dyn EventLog>);
    host.hold_delivery("one", delivered("a-1"));

    host.flush_deliveries();
    assert!(
        replies(&log.inner).await.is_empty(),
        "a refused write must not be recorded, and the delivery must not be dropped"
    );

    host.flush_deliveries();
    let rows = replies(&log.inner).await;
    assert_eq!(rows.len(), 1, "the retried flush lands the row: {rows:?}");
    assert_eq!(
        rows[0].2,
        vec![artifact("a-1")],
        "the outputs survive the retry"
    );
}
