//! What an episode lends a teammate for the turns it runs as a seat.
//!
//! A teammate's belt is composed once per turn by the factory its
//! [`AgentSpec`](openhuman_embed::AgentSpec) carries, and that factory is
//! built when the agent is registered -- long before any episode opens. So an
//! episode cannot hand its tools to the agent directly. It leaves them here,
//! under the conversation its seat will run in, and the factory looks them up
//! when a turn on that conversation arrives.
//!
//! This is what lets one teammate be both itself and a seat without existing
//! twice. Before, an episode built a second session around its tools; now the
//! tools reach the handle the pool already holds.
//!
//! # Why a conversation id is the key
//!
//! Because it is what the factory is told. A turn arrives with a
//! [`TurnContext`](openhuman_core::agent::TurnContext) naming the agent and
//! the conversation, and nothing else -- deliberately, since a belt keyed on
//! anything the host keeps mutably would be a race the moment one agent
//! serves two conversations.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tinyhivemind_openhuman::EpisodeBeltSource;

use crate::ports::events::EventLog;
use crate::ports::types::CompanyId;

/// What a guest seat needs to claim work in the operator's own line.
///
/// Lent beside the belt rather than reached for, because the tool it feeds
/// only exists on a seated turn and only for a guest: the teammate whose DM
/// this is answers the operator directly and has nothing to announce.
#[derive(Clone)]
pub struct TakeoverLoan {
    /// Where the announcement row is appended.
    pub events: Arc<dyn EventLog>,
    /// The company the row belongs to.
    pub company: CompanyId,
    /// The seat claiming the work — whose line the announcement lands in.
    pub agent: String,
}

impl std::fmt::Debug for TakeoverLoan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TakeoverLoan")
            .field("company", &self.company)
            .field("agent", &self.agent)
            .finish_non_exhaustive()
    }
}

/// What one episode lends one seat for the turns it runs there.
#[derive(Clone, Debug)]
pub struct SeatLoan {
    /// The room's own tools, rebuilt per turn.
    pub source: EpisodeBeltSource,
    /// Set only for a guest seat in an operator DM. `None` everywhere else,
    /// and that `None` is what withholds `take_over`: a seat with nobody to
    /// announce to is never offered the verb.
    pub takeover: Option<TakeoverLoan>,
    /// Whether this episode runs in an operator's direct line rather than on
    /// a desk. See [`broadcast_withheld_in`].
    pub dm: bool,
}

/// The episode tool a seat does **not** get in an operator's direct line.
///
/// # Why `broadcast` has no place in a DM
///
/// Because there is nobody in the room to broadcast to. A DM binds the whole
/// roster so that `ask` has legal targets (`resolve_dm` refuses a
/// non-member), and `broadcast` then reads that membership as an audience.
/// The router is already withheld here on the theory that "a broadcast falls
/// back to lead-and-mention, which is what a DM means anyway" -- but the lead
/// *is* the author of the broadcast, so the fallback must pick somebody else,
/// and the somebody is a teammate that was only ever there to be askable.
///
/// Two live runs show the cost. In both, the seat answering the operator
/// broadcast instead of asking, a teammate woke to a message addressed to
/// nobody, and the operator was told work had been handed to a third agent
/// that never ran a turn. Withholding the verb leaves `ask` as the only way
/// to reach a teammate, which is the way that actually transfers anything.
///
/// A desk keeps it: a desk is a real room, and that is what it is for.
#[must_use]
pub fn broadcast_withheld_in(dm: bool, prefix: &str) -> Option<String> {
    dm.then(|| format!("{prefix}broadcast"))
}

/// The belts episodes have lent one teammate, by conversation.
///
/// Cheap to clone: the map is shared, so the copy the factory closed over at
/// registration is the copy an episode writes to later.
#[derive(Clone, Default)]
pub struct EpisodeBelts {
    lent: Arc<Mutex<HashMap<String, SeatLoan>>>,
}

impl std::fmt::Debug for EpisodeBelts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let lent = self.lent.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("EpisodeBelts")
            .field("conversations", &lent.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl EpisodeBelts {
    /// Lend `source` to every turn that runs on `conversation`.
    ///
    /// Replaces what was there. A conversation is one episode's, and an
    /// episode that opens again on the same id means the new one.
    pub fn lend(&self, conversation: impl Into<String>, loan: SeatLoan) {
        self.lent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(conversation.into(), loan);
    }

    /// The belt lent to `conversation`, if an episode is running there.
    ///
    /// `None` for an ordinary turn, which is most of them: a teammate
    /// answering its operator is not seated in anything.
    #[must_use]
    pub fn lent_to(&self, conversation: Option<&str>) -> Option<SeatLoan> {
        let conversation = conversation?;
        self.lent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(conversation)
            .cloned()
    }

    /// Take the belt back when the episode ends.
    pub fn reclaim(&self, conversation: &str) {
        self.lent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(conversation);
    }
}

#[cfg(test)]
#[path = "seating_tests.rs"]
mod tests;
