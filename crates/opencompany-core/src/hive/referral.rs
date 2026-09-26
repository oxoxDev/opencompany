//! Cross-desk referral: the one mechanism that leaves a room.
//!
//! One hive is one desk, and `CompletionDriver::resume` refuses an episode of
//! another desk — so a seat that names `@#other_desk` (or a teammate whose
//! home is elsewhere) cannot pull that desk into its own round. What it can do
//! is **ask**: `tinyhivemind::referral::referral` decides, purely, whether a
//! committed utterance's first pinging mention crosses; this module is the
//! host half of that decision. It journals the crossing under the trigger's
//! identity so a replay cannot ask twice (`JournalReferralQueue`), it carries
//! everything the far desk needs to open an episode of its own
//! (`DeskReferral`), and it carries the address the answer comes home to
//! (`ReturnAddress`), where the driver appends it under [`HIVE_REFERRAL_AUTHOR`]
//! and reassigns the seat that asked.
//!
//! What crosses is the question and, later, the answer — never a vote, never
//! a seat. The far desk deliberates on its own terms with its own members; the
//! asking desk reads the answer as one more line on its own transcript.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tinyhivemind::referral::{
    Referral, ReferralDecision, ReferralFuture, ReferralInput, ReferralKind, ReferralPolicy,
    ReferralQueue,
};
use tinyhivemind::{EnqueueOutcome, EnqueueRefusal};
use tinyhivemind_core::desk::DeskSet;
use tinyhivemind_core::roster::Roster;

use crate::ports::events::EventLog;
use crate::ports::types::{CompanyEvent, CompanyId, CompanyRecord, EventSeq};

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

/// The head of a seeded question, as [`DeskReferral::seed_text`] writes it.
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

/// One crossing the host is about to make, in the journal's own words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeskReferral {
    /// The asking desk.
    pub from_desk: String,
    /// Its display name, captured now.
    pub from_desk_name: String,
    /// The seat that asked.
    pub asker: String,
    /// Its display label, captured now.
    pub asker_label: String,
    /// The committed row that raised the crossing — with `from_desk`, the
    /// idempotency key.
    pub trigger_sequence: u64,
    /// The desk the question goes to.
    pub to_desk: String,
    /// The first eligible seat there, as `tinyhivemind` resolved it.
    pub target: String,
    /// The question, verbatim.
    pub content: String,
    /// The hop the far episode runs at.
    pub hop: u32,
    /// The asking episode.
    pub episode_id: String,
    /// The thread on the asking desk.
    pub thread_root: Option<EventSeq>,
    /// Whether the answer comes home.
    pub returns: bool,
}

impl DeskReferral {
    /// Builds the crossing from a pure referral decision.
    #[must_use]
    pub fn from_referral(
        referral: &Referral,
        record: &CompanyRecord,
        episode_id: &str,
        thread_root: Option<EventSeq>,
        returns: bool,
    ) -> Self {
        Self {
            from_desk: referral.from.desk_id.clone(),
            from_desk_name: crate::server::chat_history::desk_display_name(
                record,
                &referral.from.desk_id,
            ),
            asker: referral.source_id.clone(),
            asker_label: agent_label(record, &referral.source_id),
            trigger_sequence: referral.key.trigger_sequence,
            to_desk: referral.to.desk_id.clone(),
            target: referral.target_id.clone(),
            content: referral.content.clone(),
            hop: referral.child_hop,
            episode_id: episode_id.to_string(),
            thread_root,
            returns,
        }
    }

    /// The forward marker.
    #[must_use]
    pub fn forward_event(&self, to_episode_id: Option<&str>) -> CompanyEvent {
        CompanyEvent::ReferralEnqueued {
            from_desk: self.from_desk.clone(),
            trigger_sequence: self.trigger_sequence,
            from_desk_name: self.from_desk_name.clone(),
            returning: false,
            answers: None,
            conversation: None,
            rows: None,
            asker: self.asker.clone(),
            asker_label: self.asker_label.clone(),
            to_desk: self.to_desk.clone(),
            target: self.target.clone(),
            episode_id: Some(self.episode_id.clone()),
            to_episode_id: to_episode_id.map(str::to_string),
            hop: self.hop,
        }
    }

    /// The row that seeds the far desk's episode: the question, attributed.
    #[must_use]
    pub fn seed_text(&self) -> String {
        format!(
            "@{} on #{} asks: {}",
            self.asker, self.from_desk_name, self.content
        )
    }

    /// Where the answer goes home, once the forward marker is journaled.
    #[must_use]
    pub fn return_address(&self, forward_seq: EventSeq) -> Option<ReturnAddress> {
        self.returns.then(|| ReturnAddress {
            desk: self.from_desk.clone(),
            thread_root: self.thread_root,
            episode_id: self.episode_id.clone(),
            asker: self.asker.clone(),
            forward_seq: forward_seq.value(),
        })
    }
}

/// The return marker: the answer arriving home, paired with its forward.
#[must_use]
pub fn return_event(
    record: &CompanyRecord,
    origin: &ReturnAddress,
    answering_desk: &str,
    answered_by: &str,
    answer_seq: EventSeq,
    to_episode_id: &str,
) -> CompanyEvent {
    CompanyEvent::ReferralEnqueued {
        from_desk: answering_desk.to_string(),
        trigger_sequence: answer_seq.value(),
        from_desk_name: crate::server::chat_history::desk_display_name(record, answering_desk),
        returning: true,
        answers: Some(origin.forward_seq),
        conversation: None,
        rows: None,
        asker: answered_by.to_string(),
        asker_label: agent_label(record, answered_by),
        to_desk: origin.desk.clone(),
        target: origin.asker.clone(),
        episode_id: Some(origin.episode_id.clone()),
        to_episode_id: Some(to_episode_id.to_string()),
        hop: 0,
    }
}

/// The label the console shows for a seat, captured at crossing time.
fn agent_label(record: &CompanyRecord, agent_id: &str) -> String {
    record
        .effective_agents()
        .into_iter()
        .find(|agent| agent.id == agent_id)
        .and_then(|agent| agent.name)
        .unwrap_or_else(|| agent_id.to_string())
}

/// Decides, purely, whether a committed utterance refers across desks.
///
/// A thin wrapper so the driver names one function; every rule is
/// `tinyhivemind`'s. `None` means the utterance stays in its room.
#[must_use]
pub fn decide(
    policy: ReferralPolicy,
    input: &ReferralInput,
    roster: &Roster<'_>,
    desks: &DeskSet<'_>,
) -> Option<Referral> {
    match tinyhivemind::referral::referral(policy, input, roster, desks) {
        Ok(ReferralDecision::One { referral })
            if referral.kind == ReferralKind::Forward && referral.crosses() =>
        {
            Some(*referral)
        }
        Ok(ReferralDecision::One { .. } | ReferralDecision::None { .. }) | Err(_) => None,
    }
}

/// The journal as tinyhivemind's atomic referral queue.
///
/// Idempotent on `(from_desk, trigger_sequence)`: a crossing is raised by a
/// committed row, so anything that reprocesses that row — a restart, a
/// resumed episode replaying its rounds — decides the same crossing again,
/// and the marker already written is what refuses the second ask.
///
/// The queue records the referral it accepted so the driver can dispatch it
/// (open the far episode) after `enqueue_once` returns `Enqueued`; the
/// forward marker is journaled by the driver together with the far episode's
/// id, so the marker can name both ends.
pub struct JournalReferralQueue<'a> {
    events: &'a dyn EventLog,
    company: &'a CompanyId,
    accepted: Mutex<Vec<Referral>>,
}

impl<'a> JournalReferralQueue<'a> {
    /// Opens the queue over one company's journal.
    #[must_use]
    pub fn new(events: &'a dyn EventLog, company: &'a CompanyId) -> Self {
        Self {
            events,
            company,
            accepted: Mutex::new(Vec::new()),
        }
    }

    /// The referrals this queue accepted, oldest first, drained.
    #[must_use]
    pub fn drain(&self) -> Vec<Referral> {
        self.accepted
            .lock()
            .map(|mut held| std::mem::take(&mut *held))
            .unwrap_or_default()
    }

    /// Whether a forward marker for this trigger is already on the journal.
    async fn already_marked(&self, from_desk: &str, trigger_sequence: u64) -> bool {
        // Bounded: a crossing is decided while its trigger is the newest row
        // on the desk, so a marker for it is within a page of the tail.
        const SCAN: usize = 512;
        let Ok(recent) = self.events.read_before(self.company, None, SCAN).await else {
            return false;
        };
        recent.iter().any(|stored| {
            matches!(
                &stored.event,
                CompanyEvent::ReferralEnqueued {
                    from_desk: desk,
                    trigger_sequence: at,
                    returning: false,
                    ..
                } if desk == from_desk && *at == trigger_sequence
            )
        })
    }
}

impl ReferralQueue for JournalReferralQueue<'_> {
    fn enqueue_once(&self, referral: Referral) -> ReferralFuture<'_> {
        Box::pin(async move {
            if referral.kind != ReferralKind::Forward || !referral.crosses() {
                return Ok(EnqueueOutcome::Refused {
                    reason: EnqueueRefusal::TargetUnavailable,
                });
            }
            if self
                .already_marked(&referral.from.desk_id, referral.key.trigger_sequence)
                .await
            {
                return Ok(EnqueueOutcome::Already);
            }
            if let Ok(mut held) = self.accepted.lock() {
                held.push(referral);
            }
            Ok(EnqueueOutcome::Enqueued)
        })
    }
}

#[cfg(test)]
#[path = "referral_tests.rs"]
mod tests;
