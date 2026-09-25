//! Shared fixtures for the hive unit tests: an in-memory journal and row
//! builders. `#[cfg(test)]` only; nothing here ships.

use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream::{self, BoxStream};

use crate::Result;
use crate::ports::events::{EventLog, EventStreamItem};
use crate::ports::types::{
    CompanyEvent, CompanyId, CompanyRecord, EventSeq, ReplyEpisode, StoredEvent,
};

/// An in-memory journal, the smallest thing that satisfies the port.
///
/// `read_before` is implemented directly rather than inherited from the port's
/// forward-scan default, because the session adapter's paging is one of the
/// things under test and a default that reads the whole log would hide a
/// cursor bug rather than expose it.
#[derive(Default)]
pub(crate) struct MemoryLog {
    events: Mutex<Vec<StoredEvent>>,
}

impl MemoryLog {
    pub(crate) fn company() -> CompanyId {
        CompanyId::new("acme")
    }

    pub(crate) fn rows(&self) -> Vec<StoredEvent> {
        self.events.lock().expect("journal poisoned").clone()
    }

    /// Every `AgentReply` on `chat`, as `(author, text)` in journal order.
    ///
    /// Only `driver_tests.rs` (feature `openhuman`) calls this; without that
    /// feature it would be dead code under a plain `cargo clippy --all-targets`.
    #[cfg(feature = "openhuman")]
    #[allow(dead_code)]
    pub(crate) fn replies(&self, chat: &str) -> Vec<(String, String)> {
        self.rows()
            .into_iter()
            .filter_map(|stored| match stored.event {
                CompanyEvent::AgentReply {
                    chat_id,
                    agent_id,
                    text,
                    ..
                } if chat_id == chat => Some((agent_id, text)),
                _ => None,
            })
            .collect()
    }

    /// The `kind` of every journaled event, in order.
    ///
    /// Only `driver_tests.rs` (feature `openhuman`) calls this; see `replies`.
    #[cfg(feature = "openhuman")]
    #[allow(dead_code)]
    pub(crate) fn kinds(&self) -> Vec<&'static str> {
        self.rows()
            .iter()
            .map(|stored| stored.event.kind())
            .collect()
    }
}

#[async_trait]
impl EventLog for MemoryLog {
    async fn append(&self, _id: &CompanyId, event: CompanyEvent) -> Result<EventSeq> {
        let mut events = self.events.lock().expect("journal poisoned");
        let seq = EventSeq::new(events.len() as u64 + 1);
        events.push(StoredEvent {
            seq,
            company: MemoryLog::company(),
            event,
            at_millis: seq.value() * 1000,
        });
        Ok(seq)
    }

    async fn read_from(
        &self,
        _id: &CompanyId,
        seq: EventSeq,
        limit: usize,
    ) -> Result<Vec<StoredEvent>> {
        Ok(self
            .rows()
            .into_iter()
            .filter(|stored| stored.seq.value() >= seq.value())
            .take(limit)
            .collect())
    }

    async fn read_before(
        &self,
        _id: &CompanyId,
        before: Option<EventSeq>,
        limit: usize,
    ) -> Result<Vec<StoredEvent>> {
        let mut rows: Vec<StoredEvent> = self
            .rows()
            .into_iter()
            .filter(|stored| before.is_none_or(|cursor| stored.seq.value() < cursor.value()))
            .collect();
        rows.reverse();
        rows.truncate(limit);
        Ok(rows)
    }

    fn subscribe(&self, _id: &CompanyId) -> BoxStream<'static, EventStreamItem> {
        Box::pin(stream::empty())
    }
}

/// An operator message on `chat`.
pub(crate) fn operator_message(chat: &str, text: &str, parent: Option<u64>) -> CompanyEvent {
    CompanyEvent::OperatorMessage {
        text: text.into(),
        by: None,
        chat: Some(chat.into()),
        parent: parent.map(EventSeq::new),
        deliverable: None,
        mentions: Vec::new(),
        attachments: Vec::new(),
    }
}

/// A desk-visible agent reply on `chat`.
pub(crate) fn agent_reply(chat: &str, agent: &str, text: &str) -> CompanyEvent {
    agent_reply_in(chat, agent, text, Vec::new(), None)
}

/// An agent reply with an audience and episode metadata.
pub(crate) fn agent_reply_in(
    chat: &str,
    agent: &str,
    text: &str,
    audience: Vec<String>,
    episode: Option<ReplyEpisode>,
) -> CompanyEvent {
    CompanyEvent::AgentReply {
        chat_id: chat.into(),
        agent_id: agent.into(),
        text: text.into(),
        steps: Vec::new(),
        outputs: Vec::new(),
        task_id: None,
        parent: None,
        mentions: Vec::new(),
        mention_depth: 0,
        audience,
        episode,
    }
}

/// A record parsed from a manifest.
pub(crate) fn record(manifest: &str) -> CompanyRecord {
    let manifest: crate::company::CompanyManifest =
        toml::from_str(manifest).expect("test manifest parses");
    CompanyRecord::from_manifest(MemoryLog::company(), manifest)
}

/// Two desks of two sharing the CEO — the `hive_demo` shape.
pub(crate) const TWO_DESKS: &str = r#"
[company]
name = "Acme"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[agent]]
id = "engineer"
role = "Engineer"

[[agent]]
id = "writer"
role = "Writer"

[[group_chat]]
id = "engineering"
name = "Engineering desk"
description = "How things are built."
members = ["engineer", "ceo"]

[group_chat.routing]
round_width = 2

[group_chat.routing.referral]
enabled = true
max_hops = 1
returns = true

[[group_chat]]
id = "content"
name = "Content desk"
members = ["writer", "ceo"]

[group_chat.routing.referral]
enabled = true
max_hops = 1
returns = true
"#;
