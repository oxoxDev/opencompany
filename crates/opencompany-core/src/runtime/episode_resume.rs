//! Handing an operator's decision back to the hive episode seat that asked.
//!
//! A seat turn parks its approvals under a turn key of its own,
//! `episode-seat:{episode}:{seat}`, so the resolve path can tell an episode's
//! approval from a chat cycle's or a workflow run's by the key alone. When
//! the last decision a seat waits on lands, the runtime hands every decision
//! to [`EpisodeReleases`]; the running episode takes them in its `released`
//! wait, tells the seat what was decided, and lets it take its turn again.
//!
//! A decision for an episode that is not running in this process is banked
//! here and the episode is resumed from its checkpoint, whose `released` wait
//! then finds it at once.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::Notify;

use crate::ports::types::ApprovalId;

/// The prefix of every hive episode seat's approval turn key.
pub const EPISODE_SEAT_TURN_PREFIX: &str = "episode-seat:";

/// One seat of one episode, as its turn key names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpisodeSeat {
    /// The episode.
    pub episode_id: String,
    /// The seat.
    pub seat: String,
}

/// The approval turn key a seat's parks are recorded under.
#[must_use]
pub fn turn_key(episode_id: &str, seat: &str) -> String {
    format!("{EPISODE_SEAT_TURN_PREFIX}{episode_id}:{seat}")
}

/// The episode and seat a turn key names, or `None` for any other key.
#[must_use]
pub fn parse(turn: &str) -> Option<EpisodeSeat> {
    let rest = turn.strip_prefix(EPISODE_SEAT_TURN_PREFIX)?;
    let (episode_id, seat) = rest.split_once(':')?;
    if episode_id.is_empty() || seat.is_empty() {
        return None;
    }
    Some(EpisodeSeat {
        episode_id: episode_id.to_owned(),
        seat: seat.to_owned(),
    })
}

/// What a seat asked the operator for.
#[derive(Clone, Debug, PartialEq)]
pub enum SeatAsk {
    /// A gated tool call, re-issued under its single-use grant when approved.
    Call {
        /// The tool.
        tool: String,
        /// The exact arguments the grant admits.
        args: serde_json::Value,
    },
    /// An explicit `request_approval`.
    Request {
        /// What the seat asked to do.
        title: String,
    },
    /// An `escalate_to_human` question.
    Question {
        /// What the seat needed.
        needed: String,
    },
}

/// How the operator answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeatVerdict {
    /// Approved, or told to go ahead.
    Approved,
    /// Denied, cancelled, or left to expire.
    Denied,
    /// Told to skip the step and carry on without it.
    Skipped,
}

/// One decision on one of a seat's parked approvals.
#[derive(Clone, Debug, PartialEq)]
pub struct SeatDecision {
    /// The approval decided.
    pub approval_id: ApprovalId,
    /// What had been asked.
    pub ask: SeatAsk,
    /// What the operator decided.
    pub verdict: SeatVerdict,
    /// The operator's own words, when they wrote any.
    pub answer: String,
}

impl SeatDecision {
    /// The decision in plain words, addressed to the seat that asked.
    #[must_use]
    pub fn note(&self) -> String {
        let answer = self.answer.trim();
        let words = if answer.is_empty() {
            String::new()
        } else {
            format!(" The operator wrote: \"{answer}\"")
        };
        match (&self.ask, self.verdict) {
            (SeatAsk::Call { tool, args }, SeatVerdict::Approved) => {
                let args = serde_json::to_string(args).unwrap_or_else(|_| "{}".to_owned());
                format!(
                    "The operator approved your `{tool}` call. Make it again now with exactly \
                     these arguments: {args}.{words}"
                )
            }
            (SeatAsk::Call { tool, .. }, _) => format!(
                "The operator did not approve your `{tool}` call, so it did not run. Do not \
                 retry it; carry on without it.{words}"
            ),
            (SeatAsk::Request { title }, SeatVerdict::Approved) => format!(
                "The operator approved your request: {title}. Go ahead. Do not ask again for \
                 the same thing.{words}"
            ),
            (SeatAsk::Request { title }, _) => format!(
                "The operator denied your request: {title}. Do not do it; carry on without it \
                 or stop that part of the work.{words}"
            ),
            (SeatAsk::Question { needed }, SeatVerdict::Approved) if !answer.is_empty() => {
                format!("The operator answered your question ({needed}): \"{answer}\"")
            }
            (SeatAsk::Question { needed }, SeatVerdict::Approved) => {
                format!("The operator told you to go ahead with what you asked about ({needed}).")
            }
            (SeatAsk::Question { needed }, SeatVerdict::Skipped) => format!(
                "The operator said to skip what you asked about ({needed}) and carry on \
                 without it.{words}"
            ),
            (SeatAsk::Question { needed }, SeatVerdict::Denied) => format!(
                "The operator declined what you asked about ({needed}). Stop that part of the \
                 work.{words}"
            ),
        }
    }
}

#[derive(Default)]
struct Registry {
    running: BTreeSet<String>,
    banked: BTreeMap<String, BTreeMap<String, Vec<SeatDecision>>>,
    answers: HashMap<ApprovalId, (SeatVerdict, String)>,
}

/// The decisions each running or resumable episode's seats are owed.
///
/// One per company, shared by the runtime that resolves approvals and the
/// episode hosts that wait on them. Cloning shares the state.
#[derive(Clone, Default)]
pub struct EpisodeReleases {
    registry: Arc<Mutex<Registry>>,
    changed: Arc<Notify>,
}

impl std::fmt::Debug for EpisodeReleases {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EpisodeReleases")
            .finish_non_exhaustive()
    }
}

impl EpisodeReleases {
    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Banks `decisions` for `seat` of `episode_id`, answering whether that
    /// episode is running in this process and so will take them. `false`
    /// means the caller owes the episode a resume.
    pub fn deliver(&self, episode_id: &str, seat: &str, decisions: Vec<SeatDecision>) -> bool {
        let running = {
            let mut registry = self.lock();
            registry
                .banked
                .entry(episode_id.to_owned())
                .or_default()
                .entry(seat.to_owned())
                .or_default()
                .extend(decisions);
            registry.running.contains(episode_id)
        };
        tracing::debug!(
            episode = %episode_id,
            %seat,
            running,
            "[hive] a parked seat's decisions were delivered"
        );
        self.changed.notify_waiters();
        running
    }

    /// Holds the operator's own answer to an escalation until its decision is
    /// assembled, which is after the answer has left the blocker queue.
    pub fn answer(&self, approval_id: &ApprovalId, verdict: SeatVerdict, answer: String) {
        self.lock()
            .answers
            .insert(approval_id.clone(), (verdict, answer));
    }

    /// Takes the answer [`answer`](Self::answer) held for `approval_id`.
    #[must_use]
    pub fn take_answer(&self, approval_id: &ApprovalId) -> Option<(SeatVerdict, String)> {
        self.lock().answers.remove(approval_id)
    }

    /// Marks `episode_id` as running here. `false` when it already is, so a
    /// second resume of the same episode starts nothing.
    pub fn start(&self, episode_id: &str) -> bool {
        self.lock().running.insert(episode_id.to_owned())
    }

    /// The episode stopped running here, finished or failed.
    pub fn finish(&self, episode_id: &str) {
        let mut registry = self.lock();
        registry.running.remove(episode_id);
        registry.banked.remove(episode_id);
    }

    /// Whether `episode_id` is running in this process.
    #[must_use]
    pub fn is_running(&self, episode_id: &str) -> bool {
        self.lock().running.contains(episode_id)
    }

    /// Takes the decisions banked for any of `parked`, without waiting.
    #[must_use]
    pub fn take(&self, episode_id: &str, parked: &[String]) -> BTreeMap<String, Vec<SeatDecision>> {
        let mut registry = self.lock();
        let Some(seats) = registry.banked.get_mut(episode_id) else {
            return BTreeMap::new();
        };
        let mut released = BTreeMap::new();
        for seat in parked {
            if let Some(decisions) = seats.remove(seat) {
                released.insert(seat.clone(), decisions);
            }
        }
        released
    }

    /// Waits until at least one of `parked` has decisions, and takes every
    /// parked seat's that has.
    pub async fn released(
        &self,
        episode_id: &str,
        parked: &[String],
    ) -> BTreeMap<String, Vec<SeatDecision>> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let released = self.take(episode_id, parked);
            if !released.is_empty() {
                return released;
            }
            changed.await;
        }
    }
}

#[cfg(test)]
#[path = "episode_resume_tests.rs"]
mod tests;
