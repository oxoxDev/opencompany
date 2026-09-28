//! Cross-desk referral, read side: the reserved authors a crossing is
//! journaled under, the pair-conversation key, the attribution heads a
//! question and its answer carry, and the `ReturnAddress` a far episode's
//! checkpoint keeps.
//!
//! What crosses is the question and, later, the answer — never a vote, never
//! a seat. The far desk deliberates on its own terms with its own members; the
//! asking desk reads the answer as one more line on its own transcript.

use serde::{Deserialize, Serialize};

use crate::ports::types::EventSeq;

/// The `agent_id` an answer carried back from another desk — and the question
/// carried out to it — is journaled under.
///
/// Hyphenated so no roster id can spell it (`agent_slug` and `is_snake_case`
/// both reject a hyphen), exactly as
/// [`WORKFLOW_REPLY_AUTHOR`](crate::runtime::channel::WORKFLOW_REPLY_AUTHOR)
/// is. The session log reads it as a system row, so a seat sees "the content
/// desk answered" rather than a teammate that never sat here.
pub const HIVE_REFERRAL_AUTHOR: &str = "hive-referral";

/// Whether an `agent_id` is this module's reserved system author.
#[must_use]
pub fn is_hive_author(agent_id: &str) -> bool {
    agent_id == HIVE_REFERRAL_AUTHOR
}

/// The reserved authors the trace-grammar hive wrote its closing report and
/// its failure notices under, before plan hive-desks Phase 4 retired it.
///
/// Nothing writes them any more; the history projection still drops rows
/// that carry them, because a journal written before the change keeps its
/// rows and a tally the console never drew should not start appearing as a
/// teammate now.
#[must_use]
pub fn is_legacy_report_author(agent_id: &str) -> bool {
    matches!(agent_id, "hive-report" | "hive-failure")
}

/// The pair conversation two agents share (`dm:<a>+<b>`, ids sorted), the
/// thread the Session tab lists for each teammate pair.
#[must_use]
pub fn pair_conversation(one: &str, two: &str) -> String {
    let (first, second) = if one <= two { (one, two) } else { (two, one) };
    format!("dm:{first}+{second}")
}

/// The conversation an `ask` opens: the asker and every seat it asked, in one
/// channel (`dm:<a>+<b>[+<c>…]`, ids sorted and de-duplicated).
///
/// An ask may name a group since tinyhivemind's "ask a group" change, and one
/// conversation then holds all of them. With a single askee this is exactly
/// [`pair_conversation`], so every pair channel written before keeps its key.
#[must_use]
pub fn conversation_channel(asker: &str, askees: &[String]) -> String {
    let mut members: Vec<&str> = std::iter::once(asker)
        .chain(askees.iter().map(String::as_str))
        .collect();
    members.sort_unstable();
    members.dedup();
    format!("dm:{}", members.join("+"))
}

/// Every seat a conversation channel names — two for a pair, more for a
/// group ask — or `None` for any other key.
///
/// The inverse of [`conversation_channel`]. The same caution as
/// [`pair_seats`] applies: the key alone is not authority to read it.
#[must_use]
pub fn conversation_seats(chat: &str) -> Option<Vec<&str>> {
    let rest = chat.strip_prefix("dm:")?;
    let seats: Vec<&str> = rest.split('+').collect();
    (seats.len() >= 2 && seats.iter().all(|seat| !seat.is_empty())).then_some(seats)
}

/// The two seats a pair channel names, or `None` for any other key.
///
/// The inverse of [`pair_conversation`]. A caller deciding whether a stored
/// channel belongs to it must check *both* ids against its own roster: the
/// key is minted from two roster ids and says nothing about where the two
/// were talking, so its name alone is not authority to read it.
#[must_use]
pub fn pair_seats(chat: &str) -> Option<(&str, &str)> {
    chat.strip_prefix("dm:")?.split_once('+')
}

/// The head of a seeded question: `@<asker> on #<desk> asks: <question>`.
const ASKS: &str = " asks: ";

/// The question a seeded row carries, without the attribution head.
#[must_use]
pub fn asked_message(text: &str) -> String {
    match text.split_once(ASKS) {
        Some((head, rest)) if head.starts_with('@') => rest.trim().to_string(),
        _ => text.trim().to_string(),
    }
}

/// The row an answer comes home as: attributed to the seat that closed the
/// far episode, on the desk it closed on.
#[must_use]
pub fn returned_note(target: &str, desk_name: &str, answer: &str) -> String {
    format!("@{target} on #{desk_name} answered: {}", answer.trim())
}

/// An answer row with [`returned_note`]'s attribution removed — the fold
/// that carries it already says who answered.
#[must_use]
pub fn unattributed(target: &str, desk_name: &str, text: &str) -> String {
    let head = returned_note(target, desk_name, "");
    text.strip_prefix(head.trim_end())
        .map_or_else(|| text.to_string(), |rest| rest.trim_start().to_string())
}

/// Where an answering episode sends its answer home.
///
/// Kept on the far episode's checkpoint (`EpisodeStateSaved.origin`) so a
/// resumed episode still knows, and on nothing else: the asking desk is not
/// told an answer is coming, it is told the answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReturnAddress {
    /// The asking desk.
    pub desk: String,
    /// The thread on that desk the episode runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_root: Option<EventSeq>,
    /// The asking episode.
    pub episode_id: String,
    /// The seat that asked — reassigned when the answer lands.
    pub asker: String,
    /// The journal sequence of the forward marker the return answers.
    pub forward_seq: u64,
}

#[cfg(test)]
#[path = "referral_tests.rs"]
mod tests;
