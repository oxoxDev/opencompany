//! The resolve path's fork for approvals a hive episode seat parked.
//!
//! An episode seat is not a brain turn, so its decisions are never continued
//! as one: no pooled turn redeems its grant and no chat cycle answers its
//! request. Once the last decision the seat waits on lands, every decision is
//! handed to the episode through [`EpisodeReleases`], and the seat, released,
//! redeems an approved call itself under the single-use grant the approve
//! minted. An episode no longer running in this process is resumed from its
//! checkpoint by the brain.

use crate::error::Result;
use crate::ports::types::{ApprovalId, CompanyEvent, Verdict};
use crate::runtime::cycle::CycleRunner;
use crate::runtime::episode_resume::{EpisodeSeat, SeatAsk, SeatDecision, SeatVerdict};
use crate::runtime::types::CycleReport;

use super::CompanyRuntime;

/// The system actor an expiry is recorded under.
pub(crate) const EXPIRY_ACTOR: &str = "expiry";

impl CompanyRuntime {
    /// The episode seat that parked `id`, if a seat did.
    #[cfg(feature = "openhuman")]
    pub(crate) fn episode_seat_of(&self, id: &ApprovalId) -> Option<EpisodeSeat> {
        self.journal
            .approval_cycle(id)
            .flatten()
            .as_deref()
            .and_then(crate::runtime::episode_resume::parse)
    }

    /// Holds an escalation's answer for the seat's decision and retires it
    /// from the blocker queue, so a boot does not replay it into a blocker
    /// resume.
    #[cfg(feature = "openhuman")]
    pub(crate) async fn hold_episode_answer(
        &self,
        id: &ApprovalId,
        resolution: &crate::ports::blockers::BlockerResolution,
    ) {
        use crate::ports::blockers::BlockerVerdict;
        let verdict = match resolution.verdict {
            BlockerVerdict::Retry | BlockerVerdict::Amend => SeatVerdict::Approved,
            BlockerVerdict::Skip => SeatVerdict::Skipped,
            BlockerVerdict::Cancel => SeatVerdict::Denied,
        };
        self.grants
            .episode_releases()
            .answer(id, verdict, resolution.answer.clone());
        if let Err(error) = self.journal.record_blocker_resumed(id).await {
            tracing::warn!(
                company = %self.id,
                approval = %id,
                %error,
                "[hive] an escalation answered for an episode seat could not be retired"
            );
        }
    }

    /// Hands a seat's released batch to its episode, resuming the episode
    /// when nothing running here takes it.
    pub(crate) async fn resume_episode_seat(
        &self,
        seat: &EpisodeSeat,
        batch: Vec<CompanyEvent>,
    ) -> Result<CycleReport> {
        let decisions: Vec<SeatDecision> = batch
            .iter()
            .filter_map(|event| match event {
                CompanyEvent::ApprovalResolved {
                    approval_id,
                    verdict,
                    ..
                } => Some(self.seat_decision(approval_id, *verdict)),
                _ => None,
            })
            .collect();
        let unjournaled: Vec<CompanyEvent> = batch
            .into_iter()
            .filter(|event| !self.journaled_by_the_sweep(event))
            .collect();
        self.retire_seat_continuations(&decisions).await;
        for event in unjournaled {
            if let Err(error) = self.events.append(&self.id, event).await {
                tracing::warn!(
                    company = %self.id,
                    %error,
                    "[hive] an episode seat's resolution could not be appended to the event log"
                );
            }
        }
        let count = decisions.len();
        let taken = self
            .grants
            .episode_releases()
            .deliver(&seat.episode_id, &seat.seat, decisions);
        tracing::info!(
            company = %self.id,
            episode = %seat.episode_id,
            seat = %seat.seat,
            decisions = count,
            running = taken,
            "[hive] an episode seat's decisions are in"
        );
        if !taken && !self.brain.resume_episode(&seat.episode_id).await {
            tracing::error!(
                company = %self.id,
                episode = %seat.episode_id,
                seat = %seat.seat,
                "[hive] an episode seat was decided but its episode is not running and could \
                 not be resumed"
            );
            self.announce_to_operator(
                "A teammate you just answered was part of a room conversation that is no \
                 longer running, so your decision could not be handed back to it. Ask the \
                 room again to pick the work back up.",
            )
            .await;
        }
        Ok(CycleRunner::new(self).already_resolved_report())
    }

    /// What the operator decided on one of a seat's approvals, in the terms
    /// the seat asked in.
    fn seat_decision(&self, id: &ApprovalId, verdict: Verdict) -> SeatDecision {
        let effect = self.journal.approval_effect(id);
        let kind = effect.as_ref().map(|e| e.kind.clone()).unwrap_or_default();
        let ask = if kind == crate::ports::types::REQUEST_APPROVAL_EFFECT_KIND {
            SeatAsk::Request {
                title: self
                    .grants
                    .peek_continuation(id)
                    .and_then(|continuation| {
                        continuation
                            .call
                            .args
                            .get("title")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| "your request".to_owned()),
            }
        } else if effect
            .as_ref()
            .is_some_and(crate::ports::blockers::is_blocker_effect)
        {
            SeatAsk::Question {
                needed: effect
                    .as_ref()
                    .and_then(|effect| {
                        serde_json::from_value::<crate::ports::blockers::BlockerPayload>(
                            effect.payload.clone(),
                        )
                        .ok()
                    })
                    .map(|payload| payload.needed)
                    .filter(|needed| !needed.trim().is_empty())
                    .unwrap_or_else(|| "what you asked".to_owned()),
            }
        } else {
            SeatAsk::Call {
                args: self
                    .grants
                    .peek(id)
                    .map(|grant| grant.args)
                    .unwrap_or_else(|| serde_json::json!({})),
                tool: kind,
            }
        };
        let (verdict, answer) = self
            .grants
            .episode_releases()
            .take_answer(id)
            .unwrap_or_else(|| {
                let verdict = match verdict {
                    Verdict::Approve => SeatVerdict::Approved,
                    Verdict::Deny => SeatVerdict::Denied,
                };
                (verdict, String::new())
            });
        SeatDecision {
            approval_id: id.clone(),
            ask,
            verdict,
            answer,
        }
    }

    /// Retires the explicit-request continuations a seat's decisions carry,
    /// as a delivered pooled continuation would, so no boot replays them into
    /// a brain turn.
    async fn retire_seat_continuations(&self, decisions: &[SeatDecision]) {
        for decision in decisions {
            let id = &decision.approval_id;
            if self.grants.consume_continuation(id).is_some()
                && let Err(error) = self.journal.record_approval_continuation_consumed(id).await
            {
                tracing::warn!(
                    company = %self.id,
                    approval = %id,
                    %error,
                    "[hive] a seat's decision continuation could not be retired; a restart may \
                     hand the decision to the episode again"
                );
            }
        }
    }

    /// Whether the expiry sweep already journaled `event`'s resolution: a
    /// system expiry of a gated call. An expired explicit request is
    /// continued rather than journaled by the sweep, so it is not.
    fn journaled_by_the_sweep(&self, event: &CompanyEvent) -> bool {
        matches!(
            event,
            CompanyEvent::ApprovalResolved { by, .. }
                if by.kind == crate::ports::types::ActorKind::System && by.id == EXPIRY_ACTOR
        ) && match event {
            CompanyEvent::ApprovalResolved { approval_id, .. } => {
                self.grants.peek_continuation(approval_id).is_none()
            }
            _ => false,
        }
    }
}
