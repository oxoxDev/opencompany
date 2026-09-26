//! Coordination metrics over a company's journal — the store-reading twin of
//! `scripts/measure-coordination.mjs` (plan hive-desks, Phase 8).
//!
//! The question the plan asks of the hive desks is one number and two facts:
//! do seats run **at once**, do they **talk to each other** (broadcast, dm,
//! referral), and does every episode **complete**. The Node script answers it
//! from the live `/events` stream a console sees; this answers it from the
//! rows themselves, with no host running — `opencompany measure --company
//! <id>` after a run, or a test over an in-memory journal — so the two can be
//! compared and neither has to be trusted alone.
//!
//! The fold mirrors `scripts/lib/coordination-metrics.mjs` frame for frame:
//! turn brackets keyed by turn id (`TurnStarted` → `TurnSettled` /
//! `TurnFailed`), episodes from `EpisodeOpened` / `EpisodeCompleted`, rounds
//! from the revisions the turn rows carry, contacts from `BroadcastRouted` /
//! `DmDelivered` /
//! `ReferralEnqueued`, and the utterance-kind histogram from
//! `AgentReply.episode`. So do the thresholds ([`Thresholds`]), so a
//! measurement passes or fails the same way on both paths.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;

use crate::error::Result;
use crate::ports::events::EventLog;
use crate::ports::types::{CompanyEvent, CompanyId, EventSeq, StoredEvent};

/// Rows read per page while walking the journal forward.
const PAGE: usize = 512;

/// The thresholds a run must clear — the plan's, and
/// `DEFAULT_THRESHOLDS` in `scripts/lib/coordination-metrics.mjs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Thresholds {
    /// Seat turns open at once, at the peak.
    pub max_concurrent_turns: usize,
    /// Referrals that crossed from one desk to another.
    pub cross_desk_referrals: usize,
    /// Agent-to-agent broadcasts plus dms.
    pub agent_contacts: usize,
    /// Distinct `from→to` pairs over every contact kind.
    pub distinct_pairs: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            max_concurrent_turns: 2,
            cross_desk_referrals: 1,
            agent_contacts: 1,
            distinct_pairs: 2,
        }
    }
}

/// One episode as the fold saw it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeMeasure {
    /// The desk.
    pub chat_id: String,
    /// Rounds run: the distinct wave revisions this episode's turn rows
    /// carry, or the count the completion reported, whichever is larger.
    pub rounds: u32,
    /// Whether an `EpisodeCompleted` row closed it.
    pub completed: bool,
    /// Why it closed, in the journal's own word.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Opening to completion, in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to_complete_millis: Option<u64>,
}

/// The numbers a measurement prints. Field names match `summarize()` in
/// `scripts/lib/coordination-metrics.mjs` where the two report the same
/// thing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// The company measured.
    pub company: String,
    /// The first sequence considered.
    pub since_seq: u64,
    /// Journal rows folded.
    pub rows: usize,
    /// Seat turns open at once, at the peak.
    pub max_concurrent_turns: usize,
    /// Turns that started while another was still open.
    pub overlaps: usize,
    /// Turns that started while a turn of the **same agent** was still open.
    /// Must be zero: one agent runs at most one turn at a time.
    pub same_agent_overlaps: usize,
    /// Turns still open at the end of the journal.
    pub open_turns: usize,
    /// Episodes opened.
    pub episodes_opened: usize,
    /// Episodes completed.
    pub episodes_completed: usize,
    /// Each episode, keyed by id.
    pub episodes: BTreeMap<String, EpisodeMeasure>,
    /// `broadcast` rows routed onward.
    pub broadcasts: usize,
    /// `dm` rows delivered.
    pub dms: usize,
    /// Referrals that crossed desks (non-returning, `from_desk != to_desk`).
    pub cross_desk_referrals: usize,
    /// Those referrals as `from→to` desk pairs, in journal order.
    pub referral_pairs: Vec<String>,
    /// Every agent→agent pair over broadcasts, dms and referrals.
    pub distinct_pairs: BTreeSet<String>,
    /// Utterance kinds over `AgentReply.episode.kind`.
    pub utterance_kinds: BTreeMap<String, usize>,
    /// Routing-plan kinds over `EpisodeOpened` and `BroadcastRouted`.
    pub plan_kinds: BTreeMap<String, usize>,
    /// Routers over `BroadcastRouted`.
    pub routers: BTreeMap<String, usize>,
}

impl Report {
    /// The failures against `thresholds`; an empty list is a pass. The same
    /// rules, in the same words, as `evaluate()` in the Node twin.
    #[must_use]
    pub fn failures(&self, thresholds: &Thresholds) -> Vec<String> {
        let mut failures = Vec::new();
        if self.max_concurrent_turns < thresholds.max_concurrent_turns {
            failures.push(format!(
                "max concurrent turns {} < {}",
                self.max_concurrent_turns, thresholds.max_concurrent_turns
            ));
        }
        if self.same_agent_overlaps > 0 {
            failures.push(format!(
                "same-agent overlaps {} (must be 0)",
                self.same_agent_overlaps
            ));
        }
        if self.cross_desk_referrals < thresholds.cross_desk_referrals {
            failures.push(format!(
                "cross-desk referrals {} < {}",
                self.cross_desk_referrals, thresholds.cross_desk_referrals
            ));
        }
        if self.broadcasts + self.dms < thresholds.agent_contacts {
            failures.push(format!(
                "agent→agent dm/broadcast {} < {}",
                self.broadcasts + self.dms,
                thresholds.agent_contacts
            ));
        }
        if self.distinct_pairs.len() < thresholds.distinct_pairs {
            failures.push(format!(
                "distinct pairs {} < {}",
                self.distinct_pairs.len(),
                thresholds.distinct_pairs
            ));
        }
        if self.episodes_opened == 0 {
            failures.push("no episode opened".to_string());
        } else {
            let open: Vec<String> = self
                .episodes
                .iter()
                .filter(|(_, episode)| !episode.completed)
                .map(|(id, episode)| format!("{}/{id}", episode.chat_id))
                .collect();
            if !open.is_empty() {
                failures.push(format!(
                    "{} episode(s) never completed: {}",
                    open.len(),
                    open.join(", ")
                ));
            }
        }
        failures
    }

    /// The report as the aligned table `opencompany measure` prints.
    #[must_use]
    pub fn to_table(&self, thresholds: &Thresholds) -> String {
        let rounds = self
            .episodes
            .iter()
            .map(|(id, episode)| format!("{id}={}", episode.rounds))
            .collect::<Vec<_>>()
            .join(" ");
        let times = self
            .episodes
            .iter()
            .filter_map(|(id, episode)| {
                episode
                    .time_to_complete_millis
                    .map(|millis| format!("{id}={millis}"))
            })
            .collect::<Vec<_>>()
            .join(" ");
        let reasons = self
            .episodes
            .iter()
            .filter_map(|(id, episode)| {
                episode
                    .reason
                    .as_ref()
                    .map(|reason| format!("{id}={reason}"))
            })
            .collect::<Vec<_>>()
            .join(" ");
        let dash = |value: String| {
            if value.is_empty() {
                "-".to_string()
            } else {
                value
            }
        };
        let histogram = |map: &BTreeMap<String, usize>| {
            dash(
                map.iter()
                    .map(|(key, count)| format!("{key}={count}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        };
        let failures = self.failures(thresholds);
        let verdict = if failures.is_empty() {
            "PASS: every threshold met".to_string()
        } else {
            format!(
                "FAIL ({}):\n  - {}",
                failures.len(),
                failures.join("\n  - ")
            )
        };
        let mut out = String::new();
        let mut line = |label: &str, value: String| {
            out.push_str(&format!("{label:<26}{value}\n"));
        };
        line(
            "company",
            format!(
                "{} (since seq {}, {} rows)",
                self.company, self.since_seq, self.rows
            ),
        );
        line(
            "max concurrent turns",
            self.max_concurrent_turns.to_string(),
        );
        line("turn overlaps", self.overlaps.to_string());
        line("same-agent overlaps", self.same_agent_overlaps.to_string());
        line("open turns", self.open_turns.to_string());
        line(
            "episodes",
            format!(
                "{}/{} completed",
                self.episodes_completed, self.episodes_opened
            ),
        );
        line("rounds per episode", dash(rounds));
        line(
            "broadcasts / dms",
            format!("{} / {}", self.broadcasts, self.dms),
        );
        line(
            "cross-desk referrals",
            format!(
                "{} {}",
                self.cross_desk_referrals,
                self.referral_pairs.join(" ")
            ),
        );
        line(
            "distinct pairs",
            format!(
                "{} {}",
                self.distinct_pairs.len(),
                self.distinct_pairs
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        );
        line("plan kinds", histogram(&self.plan_kinds));
        line("routers", histogram(&self.routers));
        line("utterance kinds", histogram(&self.utterance_kinds));
        line("time to complete (ms)", dash(times));
        line("reasons", dash(reasons));
        out.push('\n');
        out.push_str(&verdict);
        out.push('\n');
        out
    }
}

/// An open turn bracket.
struct OpenTurn {
    agent_id: Option<String>,
}

/// The running fold.
#[derive(Default)]
struct Fold {
    report: Report,
    open: HashMap<String, OpenTurn>,
    opened_at: HashMap<String, u64>,
    /// The wave revisions each episode's turns ran in.
    ///
    /// A round is counted from the turn rows rather than from a row of its
    /// own, because the conductor announces no round: a wave is whoever is
    /// due, and nobody decides its membership in advance. The turn rows are
    /// what actually happened, and they carry the revision, so the distinct
    /// revisions *are* the rounds — with the bonus that a wave which only
    /// turned seats inside private conversations is still counted, which a
    /// desk-shaped round row never could.
    ///
    /// A `RoundStarted` row feeds the same set. Journals written before the
    /// loop moved to the library carry them, and an episode must measure the
    /// same however it was run.
    revisions: HashMap<String, BTreeSet<u64>>,
}

impl Fold {
    fn count(map: &mut BTreeMap<String, usize>, key: impl Into<String>) {
        *map.entry(key.into()).or_insert(0) += 1;
    }

    fn pair(&mut self, from: &str, to: &str) {
        if from != to {
            self.report.distinct_pairs.insert(format!("{from}→{to}"));
        }
    }

    fn episode(&mut self, id: &str, chat_id: &str) -> &mut EpisodeMeasure {
        let episode = self.report.episodes.entry(id.to_string()).or_default();
        if episode.chat_id.is_empty() {
            episode.chat_id = chat_id.to_string();
        }
        episode
    }

    fn fold(&mut self, stored: &StoredEvent) {
        self.report.rows += 1;
        match &stored.event {
            CompanyEvent::TurnStarted {
                turn_id,
                agent_id,
                chat_id,
                episode_id,
                round_revision,
                ..
            } => {
                if let (Some(episode_id), Some(revision)) = (episode_id, round_revision) {
                    self.episode(episode_id, chat_id);
                    self.revisions
                        .entry(episode_id.clone())
                        .or_default()
                        .insert(*revision);
                }
                if !self.open.is_empty() {
                    self.report.overlaps += 1;
                }
                if agent_id.is_some()
                    && self
                        .open
                        .values()
                        .any(|turn| turn.agent_id.as_deref() == agent_id.as_deref())
                {
                    self.report.same_agent_overlaps += 1;
                }
                self.open.insert(
                    turn_id.clone(),
                    OpenTurn {
                        agent_id: agent_id.clone(),
                    },
                );
                self.report.max_concurrent_turns =
                    self.report.max_concurrent_turns.max(self.open.len());
            }
            CompanyEvent::TurnSettled { turn_id, .. }
            | CompanyEvent::TurnFailed { turn_id, .. } => {
                self.open.remove(turn_id);
            }
            CompanyEvent::EpisodeOpened {
                chat_id,
                episode_id,
                plan,
                ..
            } => {
                self.report.episodes_opened += 1;
                self.episode(episode_id, chat_id);
                self.opened_at.insert(episode_id.clone(), stored.at_millis);
                Self::count(&mut self.report.plan_kinds, plan_kind(plan));
            }
            // Legacy: the hand-written round loop announced its own rounds.
            // Feeding the same set keeps an old journal measuring the same as
            // a new one, and keeps the two from double-counting an episode
            // that somehow carries both.
            CompanyEvent::RoundStarted {
                chat_id,
                episode_id,
                revision,
                ..
            } => {
                self.episode(episode_id, chat_id);
                self.revisions
                    .entry(episode_id.clone())
                    .or_default()
                    .insert(*revision);
            }
            CompanyEvent::BroadcastRouted {
                chat_id,
                episode_id,
                agent_id,
                plan,
                router,
                ..
            } => {
                self.episode(episode_id, chat_id);
                self.report.broadcasts += 1;
                Self::count(&mut self.report.plan_kinds, plan_kind(plan));
                Self::count(&mut self.report.routers, router_word(*router));
                for target in plan.agent_ids() {
                    self.pair(agent_id, &target);
                }
            }
            CompanyEvent::DmDelivered {
                chat_id,
                episode_id,
                from,
                to,
                ..
            } => {
                self.episode(episode_id, chat_id);
                self.report.dms += 1;
                for target in to {
                    self.pair(from, target);
                }
            }
            CompanyEvent::ReferralEnqueued {
                from_desk,
                to_desk,
                asker,
                target,
                returning,
                ..
            } => {
                if *returning {
                    return;
                }
                self.pair(asker, target);
                if from_desk != to_desk {
                    self.report.cross_desk_referrals += 1;
                    self.report
                        .referral_pairs
                        .push(format!("{from_desk}→{to_desk}"));
                }
            }
            CompanyEvent::EpisodeCompleted {
                chat_id,
                episode_id,
                rounds,
                reason,
                ..
            } => {
                let opened_at = self.opened_at.get(episode_id).copied();
                let episode = self.episode(episode_id, chat_id);
                let first_completion = !episode.completed;
                episode.completed = true;
                episode.rounds = episode.rounds.max(*rounds);
                episode.reason = Some(reason_word(*reason).to_string());
                episode.time_to_complete_millis =
                    opened_at.map(|opened| stored.at_millis.saturating_sub(opened));
                if first_completion {
                    self.report.episodes_completed += 1;
                }
            }
            CompanyEvent::AgentReply {
                episode: Some(episode),
                ..
            } => {
                let kind = serde_json::to_value(episode.kind)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| format!("{:?}", episode.kind));
                Self::count(&mut self.report.utterance_kinds, kind);
            }
            _ => {}
        }
    }

    fn finish(mut self, company: &CompanyId, since: EventSeq) -> Report {
        self.report.company = company.to_string();
        self.report.since_seq = since.value();
        self.report.open_turns = self.open.len();
        // An episode's rounds are the distinct revisions its turns ran in,
        // never fewer than the completion reported: a fold that starts
        // mid-episode (`since`) sees only the revisions after its cut, and
        // the completion's own count is the whole story.
        for (episode_id, revisions) in &self.revisions {
            if let Some(episode) = self.report.episodes.get_mut(episode_id) {
                let counted = u32::try_from(revisions.len()).unwrap_or(u32::MAX);
                episode.rounds = episode.rounds.max(counted);
            }
        }
        self.report
    }
}

/// The `kind` tag of a plan, as the wire spells it.
fn plan_kind(plan: &crate::hive::routing::RoutingPlanDto) -> &'static str {
    use crate::hive::routing::RoutingPlanDto;
    match plan {
        RoutingPlanDto::One { .. } => "one",
        RoutingPlanDto::Hive { .. } => "hive",
        RoutingPlanDto::Clarify { .. } => "clarify",
        RoutingPlanDto::Fallback { .. } => "fallback",
    }
}

fn router_word(router: crate::hive::routing::Router) -> &'static str {
    use crate::hive::routing::Router;
    match router {
        Router::Jev => "jev",
        Router::Fallback => "fallback",
        Router::Explicit => "explicit",
    }
}

fn reason_word(reason: crate::ports::types::EpisodeReason) -> &'static str {
    use crate::ports::types::EpisodeReason;
    match reason {
        EpisodeReason::CompleteEpisode => "complete_episode",
        EpisodeReason::RoundCap => "round_cap",
        EpisodeReason::Timeout => "timeout",
        EpisodeReason::Failed => "failed",
        EpisodeReason::MembershipChanged => "membership_changed",
    }
}

/// Folds every row of `company` from `since` (inclusive) to the tail.
pub async fn measure(
    events: &dyn EventLog,
    company: &CompanyId,
    since: EventSeq,
) -> Result<Report> {
    let mut fold = Fold::default();
    let mut cursor = since;
    loop {
        let page = events.read_from(company, cursor, PAGE).await?;
        let Some(last) = page.last() else { break };
        let next = EventSeq::new(last.seq.value() + 1);
        for stored in &page {
            fold.fold(stored);
        }
        if page.len() < PAGE {
            break;
        }
        cursor = next;
    }
    Ok(fold.finish(company, since))
}

/// Folds rows already in hand — a test's, or a journal read some other way.
#[must_use]
pub fn measure_rows(company: &CompanyId, since: EventSeq, rows: &[StoredEvent]) -> Report {
    let mut fold = Fold::default();
    for stored in rows.iter().filter(|stored| stored.seq >= since) {
        fold.fold(stored);
    }
    fold.finish(company, since)
}

#[cfg(test)]
#[path = "measure_tests.rs"]
mod tests;
