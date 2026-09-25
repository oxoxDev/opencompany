//! What a seat's turn hands over, and the row that carries it.
//!
//! A seat's publishes are filed when its turn ends (`seat_park::park_seat`),
//! on a board card minted for the room the same way a conversation's are, and
//! every output the turn produced rides the seat's next row on the desk. A
//! turn that produced outputs and said nothing on the desk gets a row of its
//! own once the wave has committed everything it will.

use std::sync::PoisonError;
use std::sync::atomic::Ordering;

use super::DeskHost;
use super::seat_park::Delivery;
use crate::ports::types::{CompanyEvent, ReplyEpisode, UtteranceKind};

impl DeskHost {
    /// Keeps what `seat`'s turn handed over for its next desk row.
    pub(super) fn hold_delivery(&self, seat: &str, delivery: Delivery) {
        if delivery.is_empty() {
            return;
        }
        let mut held = self
            .deliveries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let entry = held.entry(seat.to_owned()).or_default();
        entry.outputs.extend(delivery.outputs);
        if delivery.task_id.is_some() {
            entry.task_id = delivery.task_id;
        }
    }

    /// Puts what the author handed over on `event`, when it is a desk row.
    pub(super) fn attach_delivery(&self, chat: &str, event: &mut CompanyEvent) {
        self.committed.store(true, Ordering::SeqCst);
        if chat != self.desk_id {
            return;
        }
        let CompanyEvent::AgentReply {
            agent_id,
            outputs,
            task_id,
            ..
        } = event
        else {
            return;
        };
        let Some(delivery) = self
            .deliveries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(agent_id.as_str())
        else {
            return;
        };
        outputs.extend(delivery.outputs);
        if task_id.is_none() {
            *task_id = delivery.task_id;
        }
    }

    /// At a checkpoint: once a wave has committed everything it will, gives
    /// each delivery no row carried a row of its own.
    ///
    /// A checkpoint follows every commit and closes every wave, so the one
    /// with no commit before it is the wave's end.
    pub(super) fn settle_deliveries(&self) {
        if !self.committed.swap(false, Ordering::SeqCst) {
            self.flush_deliveries();
        }
    }

    /// Writes every delivery still held as a row of its own on the desk.
    pub(crate) fn flush_deliveries(&self) {
        let held = std::mem::take(
            &mut *self
                .deliveries
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for (seat, delivery) in held {
            let mut event = self.reply(&self.desk_id.clone(), &seat, String::new(), None, None);
            if let CompanyEvent::AgentReply {
                outputs,
                task_id,
                episode,
                ..
            } = &mut event
            {
                *outputs = delivery.outputs.clone();
                *task_id = delivery.task_id.clone();
                *episode = Some(ReplyEpisode {
                    id: self.episode_id.clone(),
                    revision: self.wave_of(&seat),
                    kind: UtteranceKind::Post,
                    to: Vec::new(),
                    routed_by: None,
                });
            }
            match self.append(event) {
                Ok(seq) => tracing::info!(
                    company = %self.company,
                    episode = %self.episode_id,
                    %seat,
                    seq = seq.value(),
                    "[hive] a seat's outputs got a row of their own"
                ),
                Err(error) => {
                    tracing::error!(
                        company = %self.company,
                        episode = %self.episode_id,
                        %seat,
                        %error,
                        "[hive] could not journal a seat's outputs; holding them for the next flush"
                    );
                    self.hold_delivery(&seat, delivery);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;
