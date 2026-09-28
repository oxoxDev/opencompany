//! The company-wide `#general` channel: its identity, its stored membership,
//! and the read-time decode that maps legacy spellings of it onto its id.

use serde::{Deserialize, Deserializer, Serialize};

use crate::ports::types::{Actor, CompanyEvent, CompanyRecord, OverlayAgent};

/// The id of the company-wide channel, stamped on every message written to it.
pub const GENERAL_CHANNEL_ID: &str = "general";

/// The display name of the company-wide channel.
pub const GENERAL_CHANNEL_NAME: &str = "General";

/// The company-wide channel as a stored entity.
///
/// Created with the company and kept on its record. Its membership is always
/// the non-retired roster, maintained by the record's roster writers through
/// [`CompanyRecord::sync_general_members`]; nothing edits it by hand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralChannel {
    /// Always [`GENERAL_CHANNEL_ID`].
    pub id: String,
    /// Always [`GENERAL_CHANNEL_NAME`].
    pub name: String,
    /// Every non-retired roster teammate, manifest order then overlay order.
    #[serde(default)]
    pub members: Vec<String>,
}

impl Default for GeneralChannel {
    fn default() -> Self {
        Self {
            id: GENERAL_CHANNEL_ID.to_string(),
            name: GENERAL_CHANNEL_NAME.to_string(),
            members: Vec::new(),
        }
    }
}

/// How a roster change moved `#general`'s membership.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GeneralMembershipDelta {
    /// Ids that joined.
    pub added: Vec<String>,
    /// Ids that left.
    pub removed: Vec<String>,
}

impl GeneralMembershipDelta {
    /// Whether nothing moved.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }

    /// The journal row announcing this change, or `None` when nothing moved.
    pub fn into_event(self, by: Option<Actor>) -> Option<CompanyEvent> {
        (!self.is_empty()).then(|| CompanyEvent::DeskMembersChanged {
            desk_id: GENERAL_CHANNEL_ID.to_string(),
            added: self.added,
            removed: self.removed,
            by,
        })
    }
}

impl CompanyRecord {
    /// Recomputes `#general`'s members from the roster and restores its
    /// identity, returning who joined and who left.
    pub fn sync_general_members(&mut self) -> GeneralMembershipDelta {
        let mut roster: Vec<String> = Vec::new();
        for id in self
            .manifest
            .agents
            .iter()
            .map(|agent| agent.id.as_str())
            .chain(self.overlay_agents.iter().map(|agent| agent.id.as_str()))
        {
            if !self.is_retired(id) && !roster.iter().any(|seen| seen == id) {
                roster.push(id.to_string());
            }
        }
        let previous = std::mem::take(&mut self.general_channel.members);
        let delta = GeneralMembershipDelta {
            added: roster
                .iter()
                .filter(|id| !previous.contains(id))
                .cloned()
                .collect(),
            removed: previous
                .iter()
                .filter(|id| !roster.contains(id))
                .cloned()
                .collect(),
        };
        self.general_channel.id = GENERAL_CHANNEL_ID.to_string();
        self.general_channel.name = GENERAL_CHANNEL_NAME.to_string();
        self.general_channel.members = roster;
        delta
    }

    /// Adds an operator teammate to the roster and seats it in `#general`.
    pub fn hire_overlay_agent(&mut self, agent: OverlayAgent) -> GeneralMembershipDelta {
        self.overlay_agents.push(agent);
        self.sync_general_members()
    }

    /// Removes an operator teammate from the roster and from `#general`,
    /// returning its row when one existed.
    pub fn remove_overlay_agent(
        &mut self,
        agent_id: &str,
    ) -> (Option<OverlayAgent>, GeneralMembershipDelta) {
        let removed = self
            .overlay_agents
            .iter()
            .position(|agent| agent.id == agent_id)
            .map(|index| self.overlay_agents.remove(index));
        self.overlay_agents.retain(|agent| agent.id != agent_id);
        (removed, self.sync_general_members())
    }

    /// Replaces the operator teammates and the removal tombstones wholesale,
    /// as a reload from the store does, and resyncs `#general`.
    pub fn install_roster_overlay(
        &mut self,
        agents: Vec<OverlayAgent>,
        retired: Vec<String>,
    ) -> GeneralMembershipDelta {
        self.overlay_agents = agents;
        self.overlay_retired_agents = retired;
        self.sync_general_members()
    }
}

/// Whether `chat` is a legacy or current spelling of `#general`: its id, its
/// display name, `main`, or the empty string, in any case.
pub fn is_general_spelling(chat: &str) -> bool {
    chat.is_empty()
        || chat.eq_ignore_ascii_case(GENERAL_CHANNEL_ID)
        || chat.eq_ignore_ascii_case("main")
}

/// Maps any spelling of `#general` onto [`GENERAL_CHANNEL_ID`], leaving every
/// other chat id as written.
pub fn decode_general_chat_id(chat: String) -> String {
    if is_general_spelling(&chat) {
        GENERAL_CHANNEL_ID.to_string()
    } else {
        chat
    }
}

/// [`decode_general_chat_id`] over an optional id. `None` stays `None`.
pub fn decode_general_chat_opt(chat: Option<String>) -> Option<String> {
    chat.map(decode_general_chat_id)
}

/// Serde `deserialize_with` for a required chat id.
pub fn deserialize_general_chat<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(decode_general_chat_id)
}

/// Serde `deserialize_with` for an optional chat id where `None` means "no
/// conversation" and must stay `None`.
pub fn deserialize_general_chat_opt<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(decode_general_chat_opt)
}

#[cfg(test)]
#[path = "general_channel_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "general_channel_decode_tests.rs"]
mod decode_tests;
