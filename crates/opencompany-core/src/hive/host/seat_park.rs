//! What one seat turn claims, what it leaves waiting on the operator, and how
//! the operator's decision reaches the seat again.
//!
//! A seat turn runs on the episode's own task, outside every chat cycle, so it
//! takes its own claims: an approval bucket keyed by its episode-seat turn key,
//! the explicit-request guard, a publish claim and an output claim. `after_turn`
//! drains the approval bucket and parks it through the runtime's shared
//! [`ApprovalParker`], under that same turn key, so the resolve path can find
//! its way back to this episode. `park_seat` files what the turn published on
//! a card minted for the room, and hands the outputs it produced, along with
//! that card, to `delivery` to ride the seat's next row on the desk.

use std::sync::PoisonError;

use tinyhivemind::Sequence;

use super::{DESK_AUTHOR, DeskHost, SeatParking};
use crate::harness::built_in::policy::{
    ApprovalClaim, ApprovalRequest, ApprovalRequestQueue, ApprovalScope, DrainedRequests,
    MAX_APPROVAL_REQUESTS_PER_TURN,
};
use crate::harness::built_in::publish::{
    PendingPublish, PendingPublishQueue, PublishClaim, PublishDestination,
};
use crate::harness::built_in::turn_outputs::TurnOutputClaim;
use crate::ports::types::{ApprovalId, ChatOutput, CompanyEvent, CompanyId, EventSeq, StoredEvent};
use crate::runtime::approval_park::{ApprovalParker, ParkSite};
use crate::runtime::episode_resume::{EpisodeReleases, SeatDecision, turn_key};
use crate::runtime::journal::{ApprovalConversation, TaskLink};

/// The shared queues a seat turn claims its buckets on.
#[derive(Clone)]
pub(crate) struct SeatQueues {
    pub(crate) approvals: ApprovalRequestQueue,
    pub(crate) publishes: PendingPublishQueue,
}

impl SeatQueues {
    /// Opens one seat turn's claims.
    ///
    /// The publish bucket names the **episode** it belongs to (#2464). It was
    /// `Unclaimed` while nothing filed what a seat published, which refused
    /// the call in-turn rather than staging into a queue nobody drained --
    /// the right fail-safe, and the reason a seat that had spent a whole turn
    /// producing a report could not hand it over. `settle` drains this bucket
    /// and `park_seat` files it, so the destination is now a promise the host
    /// keeps.
    pub(crate) fn claim(
        &self,
        episode_id: &str,
        seat: &str,
        desk_id: &str,
        thread_root: Option<EventSeq>,
    ) -> SeatClaims {
        SeatClaims {
            approvals: self
                .approvals
                .claim(ApprovalScope::Seat(turn_key(episode_id, seat))),
            queue: self.approvals.clone(),
            publish: self.publishes.claim(PublishDestination::Episode {
                desk_id: desk_id.to_owned(),
                episode_id: episode_id.to_owned(),
                thread_root,
            }),
            outputs: self.publishes.output_collector().claim(),
        }
    }
}

/// One seat turn's claims, held from the turn's start until `after_turn`.
pub(crate) struct SeatClaims {
    approvals: ApprovalClaim,
    queue: ApprovalRequestQueue,
    publish: PublishClaim,
    outputs: TurnOutputClaim,
}

/// What a seat turn left behind once its claims are released.
pub(crate) struct SettledTurn {
    requests: DrainedRequests,
    /// What the seat published, to be filed by `park_seat`. The files
    /// themselves rather than a count: the count could only be apologised
    /// for.
    pub(super) publishes: Vec<PendingPublish>,
    /// Still open, so filing (which records an artifact output) lands in the
    /// same bucket as whatever the turn itself produced, before either is
    /// read.
    outputs: TurnOutputClaim,
}

/// What a seat turn hands over: the outputs it produced, and the card its
/// published files were filed on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Delivery {
    pub(crate) outputs: Vec<ChatOutput>,
    pub(crate) task_id: Option<String>,
}

impl Delivery {
    /// Whether there is nothing to hand over.
    pub(crate) fn is_empty(&self) -> bool {
        self.outputs.is_empty() && self.task_id.is_none()
    }
}

impl SeatClaims {
    /// Runs `turn` inside every claim, and keeps what it produced.
    pub(crate) async fn run<F, T>(&mut self, turn: F) -> T
    where
        F: std::future::Future<Output = T> + Send,
    {
        Box::pin(
            self.approvals.scoped(
                self.queue
                    .turn_scoped(self.publish.scoped(self.outputs.scoped(turn))),
            ),
        )
        .await
    }

    /// Releases the approval and publish claims, keeping the output claim
    /// open for `park_seat` to file publishes and drain outputs inside one
    /// bucket.
    pub(crate) fn settle(self) -> SettledTurn {
        SettledTurn {
            requests: self.approvals.drain(MAX_APPROVAL_REQUESTS_PER_TURN),
            publishes: self.publish.drain(),
            outputs: self.outputs,
        }
    }
}

/// What parking one seat's requests came to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeatParked {
    /// The approvals now on the operator's queue.
    pub parked: Vec<ApprovalId>,
    /// Why each request that did not park failed, in words for the seat.
    pub refused: Vec<String>,
}

/// Parks a seat's requests on this company's approval gate, under the seat's
/// episode turn key and the desk's conversation.
pub struct EpisodeSeatParking {
    parker: ApprovalParker,
    company: CompanyId,
    desk_id: String,
    thread_root: Option<EventSeq>,
    episode_id: String,
}

impl EpisodeSeatParking {
    /// Parking for one episode on one desk.
    #[must_use]
    pub fn new(
        parker: ApprovalParker,
        company: CompanyId,
        desk_id: String,
        thread_root: Option<EventSeq>,
        episode_id: String,
    ) -> Self {
        Self {
            parker,
            company,
            desk_id,
            thread_root,
            episode_id,
        }
    }
}

#[async_trait::async_trait]
impl SeatParking for EpisodeSeatParking {
    async fn park(&self, seat: &str, requests: Vec<ApprovalRequest>) -> SeatParked {
        let mut outcome = SeatParked::default();
        for request in requests {
            let site = ParkSite {
                task: TaskLink::Unlinked,
                conversation: ApprovalConversation {
                    thread: Some(self.desk_id.clone()),
                    parent: self.thread_root,
                },
                turn: Some(turn_key(&self.episode_id, seat)),
            };
            match self.parker.park(&self.company, request.effect, site).await {
                Ok(id) => outcome.parked.push(id),
                Err(error) => {
                    tracing::error!(
                        company = %self.company,
                        episode = %self.episode_id,
                        %seat,
                        tool = %request.tool,
                        %error,
                        "[hive] a seat's approval request could not be parked"
                    );
                    outcome.refused.push(format!(
                        "Your `{}` request could not be put in front of the operator ({error}), \
                         so nobody was asked. Do not say it is waiting on them.",
                        request.tool
                    ));
                }
            }
        }
        outcome
    }
}

impl DeskHost {
    /// The queues seat turns claim on: this host's own, else the roster's.
    pub(super) fn seat_queues(&self) -> Option<SeatQueues> {
        self.queues.clone().or_else(|| {
            self.roster.as_ref().map(|(_, deps)| SeatQueues {
                approvals: deps.approval_requests.clone(),
                publishes: deps.pending_publishes.clone(),
            })
        })
    }

    /// Where decisions for parked seats arrive: this host's own registry,
    /// else the company's.
    pub(crate) fn seat_releases(&self) -> Option<EpisodeReleases> {
        self.releases.clone().or_else(|| {
            self.roster
                .as_ref()
                .map(|(_, deps)| deps.approval_requests.grants().episode_releases())
        })
    }

    /// Reads back what a resumed episode had open: its conversations, the
    /// conversation each still-parked seat is waiting in, and the wave.
    pub(crate) fn recall(&self, rows: &[StoredEvent], revision: u64) {
        let mut conversations = self
            .conversations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut lanes = self
            .parked_lanes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for stored in rows {
            match &stored.event {
                CompanyEvent::ConversationOpened {
                    episode_id,
                    conversation_id,
                    root,
                    ..
                } if *episode_id == self.episode_id => {
                    conversations.insert(*root, conversation_id.clone());
                }
                CompanyEvent::EpisodeSeatParked {
                    episode_id,
                    seat,
                    thread,
                    ..
                } if *episode_id == self.episode_id => {
                    lanes.insert(seat.clone(), thread.map(Sequence));
                }
                CompanyEvent::EpisodeSeatResumed {
                    episode_id, seat, ..
                } if *episode_id == self.episode_id => {
                    lanes.remove(seat);
                }
                _ => {}
            }
        }
        self.wave.store(
            revision.saturating_add(1),
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    /// Claims seat turns on `approvals` rather than on the roster's.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn claiming(mut self, approvals: ApprovalRequestQueue) -> Self {
        self.queues = Some(SeatQueues {
            approvals,
            publishes: PendingPublishQueue::default(),
        });
        self
    }

    /// Claims seat turns' publishes on `publishes` rather than the roster's.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn publishing(mut self, publishes: PendingPublishQueue) -> Self {
        let approvals = self
            .queues
            .as_ref()
            .map(|queues| queues.approvals.clone())
            .unwrap_or_default();
        self.queues = Some(SeatQueues {
            approvals,
            publishes,
        });
        self
    }

    /// Takes decisions for parked seats from `releases`.
    #[must_use]
    pub fn releasing(mut self, releases: EpisodeReleases) -> Self {
        self.releases = Some(releases);
        self
    }

    pub(super) fn keep_seat_claims(&self, seat: &str, claims: SeatClaims) {
        self.seat_claims
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(seat.to_owned(), claims);
    }

    pub(super) fn take_seat_claims(&self, seat: &str) -> Option<SeatClaims> {
        self.seat_claims
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(seat)
    }

    /// Files what one seat published, on a card minted for this episode.
    ///
    /// The card's origin is the desk and the thread the episode answers in,
    /// so the operator opens the deliverable from the room that produced it
    /// rather than from a card floating free of any conversation.
    ///
    /// # Errors
    ///
    /// Whatever stops the board or the artifact store recording it.
    async fn file_seat_publishes(
        &self,
        seat: &str,
        publishes: Vec<PendingPublish>,
    ) -> crate::Result<String> {
        let Some((_, deps)) = self.roster.as_ref() else {
            return Err(crate::OpenCompanyError::Harness(
                "a seat published a file but this host has no roster to file it with".to_string(),
            ));
        };
        let (publish_chat, publish_root) = self.publish_chat(seat);
        let card = crate::harness::publish::filing::PublishFiling {
            company: &self.company,
            deps,
        }
        .record_conversation_publishes(
            seat,
            crate::runtime::delegation::ChatTarget::in_thread(Some(&publish_chat), publish_root),
            publishes,
        )
        .await?;
        tracing::info!(
            company = %self.company,
            episode = %self.episode_id,
            %seat,
            task_id = %card,
            "[hive] a seat published; minted a card to carry it"
        );
        Ok(card)
    }

    /// Where one seat's publish is filed, and under which thread.
    ///
    /// The chat is [`DeskHost::row_chat`]'s answer -- one rule for every row
    /// a seat produces, whether it is speech, a delivery or a publish, so the
    /// three cannot drift into filing the same seat's work in two places.
    /// That doc carries the reasoning; what is added here is the thread.
    ///
    /// A pair channel gets `None`. The episode's thread root is a position in
    /// the desk's transcript and means nothing in the pair conversation, so
    /// carrying it there would parent the row to an unrelated row or to
    /// nothing at all. On the desk itself the root still applies.
    pub(super) fn publish_chat(&self, seat: &str) -> (String, Option<EventSeq>) {
        let chat = self.row_chat(seat);
        let thread = (chat == self.desk_id).then_some(self.thread_root).flatten();
        (chat, thread)
    }

    /// Parks what a seat's turn raised, telling the seat about anything that
    /// did not reach the operator. `true` when something parked.
    pub(super) async fn park_seat(&self, seat: &str, settled: SettledTurn) -> bool {
        let SettledTurn {
            requests,
            publishes,
            outputs,
        } = settled;
        let mut problems = Vec::new();
        // **What the seat published, on a card that carries it (#2464).**
        //
        // This used to be an apology -- "not filed anywhere, tell the
        // operator they were not delivered" -- because the bucket had no
        // destination and nothing drained it. Now the claim names the
        // episode and this is the drain, so the file becomes a card in the
        // room it came out of. A failure here still gets the apology: the
        // work ran, and a seat that is told nothing would report a delivery
        // that did not happen.
        let mut task_id = None;
        if !publishes.is_empty() {
            // **Named, not counted.**
            //
            // Nothing can durably retain these: the bucket belongs to a claim
            // that `settle` has already released, and no later drain reaches
            // an episode -- `push_refusal` files into the bucket the workflow
            // runner drains, which is the "queue nobody empties" shape this
            // whole area exists to avoid. What is recoverable is the file
            // itself, still in the seat's sandbox under this path. So the
            // seat is told which paths did not land, and can say so precisely
            // rather than reporting a number the operator cannot act on.
            let sources: Vec<String> = publishes
                .iter()
                .map(|staged| staged.source.clone())
                .collect();
            // Scoped by the turn's own output claim: `record_published_artifacts`
            // registers the artifact through the same ambient collector a
            // tool call writes to, and that registration is a no-op outside a
            // claimed scope.
            match outputs
                .scoped(self.file_seat_publishes(seat, publishes))
                .await
            {
                Ok(card) => task_id = Some(card),
                Err(error) => {
                    tracing::error!(
                        company = %self.company,
                        episode = %self.episode_id,
                        %seat,
                        %error,
                        sources = sources.join(", "),
                        "[hive] a seat published files that could not be recorded"
                    );
                    problems.push(format!(
                        "These file(s) you published in this room could not be filed: {}. They \
                         are still in your sandbox at those paths. Tell the operator plainly \
                         that they were not delivered, and name them.",
                        sources.join(", ")
                    ));
                }
            }
        }
        // What the turn produced rides its next row on the desk, or a row of
        // its own once the wave has committed everything it will
        // (`delivery.rs`).
        let outputs = outputs.drain();
        self.hold_delivery(seat, Delivery { outputs, task_id });
        if let Some(notice) = requests.overflow_notice() {
            problems.push(notice);
        }
        let mut parked = Vec::new();
        if !requests.requests.is_empty() {
            match self.parking.as_ref() {
                Some(parking) => {
                    let outcome = parking.park(seat, requests.requests).await;
                    parked = outcome.parked;
                    problems.extend(outcome.refused);
                }
                None => {
                    tracing::error!(
                        company = %self.company,
                        episode = %self.episode_id,
                        %seat,
                        count = requests.requests.len(),
                        "[hive] a seat raised approvals on a desk that cannot park them"
                    );
                    problems.push(
                        "What you asked the operator for could not be recorded here, so nobody \
                         was asked. Do not say it is waiting on them."
                            .to_owned(),
                    );
                }
            }
        }
        if !problems.is_empty() {
            let body = problems.join("\n\n");
            if let Err(error) = self.append_note(seat, None, body).await {
                tracing::error!(
                    company = %self.company,
                    %seat,
                    %error,
                    "[hive] could not tell a seat its request did not reach the operator"
                );
            }
        }
        if parked.is_empty() {
            return false;
        }
        tracing::info!(
            company = %self.company,
            episode = %self.episode_id,
            %seat,
            approvals = parked.len(),
            "[hive] a seat parked on the operator"
        );
        self.parked_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(seat.to_owned(), parked);
        true
    }

    /// The row saying `seat` parked, in the conversation it parked in.
    pub(super) fn seat_parked_row(&self, seat: &str, thread: Option<Sequence>) -> CompanyEvent {
        self.parked_lanes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(seat.to_owned(), thread);
        let approval_ids = self
            .parked_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(seat)
            .unwrap_or_default();
        CompanyEvent::EpisodeSeatParked {
            chat_id: self.desk_id.clone(),
            episode_id: self.episode_id.clone(),
            seat: seat.to_owned(),
            thread: thread.map(|root| root.0),
            approval_ids,
        }
    }

    /// Tells `seat` what the operator decided, where it parked.
    pub(super) async fn tell_decisions(
        &self,
        seat: &str,
        decisions: &[SeatDecision],
    ) -> crate::Result<EventSeq> {
        let lane = self
            .parked_lanes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(seat)
            .flatten();
        let body = decisions
            .iter()
            .map(SeatDecision::note)
            .collect::<Vec<_>>()
            .join("\n\n");
        tracing::info!(
            company = %self.company,
            episode = %self.episode_id,
            %seat,
            decisions = decisions.len(),
            "[hive] a parked seat was released by the operator"
        );
        self.append_note(seat, lane, body).await
    }

    /// A note to one seat, in the conversation `lane` roots, else on the desk.
    async fn append_note(
        &self,
        seat: &str,
        lane: Option<Sequence>,
        body: String,
    ) -> crate::Result<EventSeq> {
        let chat = lane.and_then(|root| {
            self.conversations
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&root.0)
                .cloned()
        });
        let (chat, thread) = match chat {
            Some(chat) => (chat, lane),
            None => (self.desk_id.clone(), None),
        };
        let event = self.reply(&chat, DESK_AUTHOR, body, thread, Some(seat));
        self.events.append(&self.company, event).await
    }
}

#[cfg(test)]
#[path = "seat_park_tests.rs"]
mod tests;
