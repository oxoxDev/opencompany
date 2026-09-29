//! The closing turn a settled episode routes to one seat.
//!
//! Split out of `conducted.rs` to keep that file under the 750-line cap: this
//! is one coherent step of the episode lifecycle -- everything between the last
//! seat settling and the closing row being journaled. What the step is *for*,
//! and the six live runs that measured the gap it fills, is documented on
//! [`crate::hive::conclude`]; this module is only the part that needs
//! `HiveDispatcher`'s own `run`, parking and event log.

use std::sync::Arc;

use super::{Episode, HiveDispatcher, run};
use crate::hive::episode_store;
use crate::hive::graph::DeskHive;
use crate::hive::routing::EffectiveRouting;
use crate::ports::types::EventSeq;

impl HiveDispatcher {
    /// Run a settled episode's closing turn, and say where its message landed.
    ///
    /// Reached only through [`crate::hive::conclude::eligible`]. Failure here
    /// is deliberately not fatal: the episode *has* settled, every seat's work
    /// is already journaled, and refusing to write the closing row because the
    /// summary turn stalled would lose a finished episode over an extra. A
    /// warning names the seat and the episode instead, and the row goes down
    /// with `completed_by: None` exactly as it did before this existed.
    pub(super) async fn conclusion(
        &self,
        desk: &DeskHive,
        routing: &EffectiveRouting,
        episode_id: &str,
        thread_root: Option<EventSeq>,
        opened_at: EventSeq,
        request: &str,
    ) -> Option<crate::hive::conclude::Conclusion> {
        // **A read that failed is not an episode with nothing in it.**
        //
        // These rows are both the router's evidence and the watermark below, and
        // an empty vector is a legitimate value for each -- so swallowing the
        // error let a storage failure look like a settled episode that produced
        // nothing. The oracle would then judge `needed` against no findings at
        // all, and "already assembled" is exactly what no findings reads like:
        // a failed read could talk the desk out of concluding.
        //
        // So a failure keeps the round but takes the decision away from it: the
        // lead concludes, which is the fallback every other failure here uses.
        let settled_rows = match episode_store::episode_rows(
            self.events.as_ref(),
            &self.record.id,
            episode_id,
        )
        .await
        {
            Ok(rows) => Some(rows),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "[hive] could not read the episode back; its lead concludes without a decision"
                );
                None
            }
        };
        let lead = match desk.lead() {
            Some(lead) => lead,
            None => {
                tracing::warn!(desk = %desk.desk_id, "[hive] no lead, so no closing turn");
                return None;
            }
        };
        // One call, two questions -- whether this still needs assembling and who
        // should do it -- when an oracle resolves. Without one, `route_desk`
        // answers the second and the episode always concludes, which is the
        // behaviour before the decision existed.
        // A read that failed leaves no evidence, so nothing here routes on it:
        // not the oracle, whose `needed` would read "no findings" as "already
        // assembled", and not `route_desk` either, which would spend a call to
        // score an empty context and answer with the lead anyway. The lead
        // concludes directly, which is what the warning above promises.
        let Some(settled_rows) = settled_rows else {
            return self
                .closing_round(
                    desk,
                    routing,
                    episode_id,
                    thread_root,
                    opened_at,
                    &lead,
                    u64::MAX,
                )
                .await;
        };
        let seat = match self.oracle.as_deref() {
            Some(oracle) => {
                let seats: Vec<String> = desk.hive.members().map(str::to_owned).collect();
                let findings = crate::hive::conclude::findings(&settled_rows, &desk.desk_id);
                match crate::hive::conclude::decide(oracle, request, &findings, &seats, &lead).await
                {
                    crate::hive::conclude::Decision::Conclude(seat) => seat,
                    crate::hive::conclude::Decision::NotNeeded => {
                        tracing::info!(
                            desk = %desk.desk_id,
                            episode = %episode_id,
                            "[hive] the desk already answered the request, so no closing turn"
                        );
                        return None;
                    }
                }
            }
            None => match crate::hive::conclude::pick_concluder(
                desk,
                routing,
                self.router.as_deref(),
                request,
                thread_root,
                &settled_rows,
            )
            .await
            {
                Ok(seat) => seat,
                // The lead, not nothing. Every other failure on this path
                // concludes with the lead, for the reason `ALREADY_ASSEMBLED`
                // gives: an unneeded closing turn costs a turn, a missing one
                // costs the answer. A routing failure is no different, and
                // returning here made it the one exception.
                Err(error) => {
                    tracing::warn!(
                        %error,
                        desk = %desk.desk_id,
                        "[hive] the closing turn routed nowhere; its lead concludes"
                    );
                    lead.clone()
                }
            },
        };
        // **The line the closing turn must write above.**
        //
        // Taken before the round runs, because the read-back below cannot
        // otherwise tell the closing message from anything this seat said during
        // the episode proper. The concluder is usually a seat that already spoke
        // -- the lead, most often -- so "its last reply on the desk" is an
        // ordinary deliberation row until the closing turn adds one. Without
        // this, a closing turn that recorded nothing would hand
        // `EpisodeCompleted.summary_seq` a mid-episode message and the console
        // would label it the episode's summary.
        let before = settled_rows
            .iter()
            .map(|stored| stored.seq.value())
            .max()
            .unwrap_or(0);
        self.closing_round(
            desk,
            routing,
            episode_id,
            thread_root,
            opened_at,
            &seat,
            before,
        )
        .await
    }

    /// Run the closing turn for one named seat and read its summary back.
    ///
    /// `above` is the sequence a qualifying reply must exceed: the settled
    /// episode's last row normally, and `u64::MAX` when the episode could not be
    /// read — there the closing row cannot be told from a deliberation row, so
    /// nothing qualifies and `summary_seq` stays unset rather than naming the
    /// wrong message.
    #[allow(clippy::too_many_arguments)]
    async fn closing_round(
        &self,
        desk: &DeskHive,
        routing: &EffectiveRouting,
        episode_id: &str,
        thread_root: Option<EventSeq>,
        opened_at: EventSeq,
        seat: &str,
        above: u64,
    ) -> Option<crate::hive::conclude::Conclusion> {
        let seat = seat.to_owned();
        let before = above;
        let outcome = run(Episode {
            record: Arc::clone(&self.record),
            deps: Arc::clone(&self.deps),
            pool: Arc::clone(&self.pool),
            events: Arc::clone(&self.events),
            desk,
            routing,
            // Nothing to route: the seat is already chosen, and it holds no
            // `broadcast` for a router to govern.
            router: None,
            episode_id: episode_id.to_owned(),
            thread_root,
            opened_at,
            starters: vec![seat.clone()],
            concluding: true,
            parking: self.seat_parking(&desk.desk_id, thread_root, episode_id),
            mentions: self.mentions.clone(),
        })
        .await;
        let closing = match outcome {
            Ok(closing) => closing,
            Err(error) => {
                tracing::warn!(%error, seat = %seat, episode = %episode_id, "[hive] the closing turn failed");
                return None;
            }
        };
        // Where the message landed, read back rather than tracked: the seat
        // journals through the host like any other turn, so its answer is a row
        // the closing turn added -- which is what `before` distinguishes. A turn
        // that recorded nothing leaves `None`, as `Conclusion::summary_seq`
        // documents, rather than pointing at something the seat said earlier.
        let summary_seq =
            match episode_store::episode_rows(self.events.as_ref(), &self.record.id, episode_id)
                .await
            {
                Ok(rows) => {
                    crate::hive::conclude::closing_summary_seq(&rows, &desk.desk_id, &seat, before)
                }
                Err(error) => {
                    tracing::warn!(%error, "[hive] could not read back the closing message");
                    None
                }
            };
        tracing::info!(
            desk = %desk.desk_id,
            episode = %episode_id,
            seat = %seat,
            summary_seq = ?summary_seq,
            "[hive] the episode was concluded"
        );
        Some(crate::hive::conclude::Conclusion {
            seat,
            summary_seq,
            turns: closing.turns,
            waves: closing.waves,
        })
    }
}
