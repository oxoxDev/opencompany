//! Issue #983: settle chat turns a previous host process left open.
//!
//! The tenant-side half of "a turn that was accepted must be readable". A chat
//! turn journals a [`CompanyEvent::TurnStarted`] the moment the request is
//! accepted and a [`CompanyEvent::TurnFailed`] if it errors, so the pair is a
//! bracket the way `WorkflowRunStarted` / `WorkflowRunFinished` is — and a start
//! with no terminal at boot is a turn that died with the last host.
//!
//! Kept beside the workflow sweep it mirrors rather than folded into it, because
//! the two read different brackets and neither wants the other's shape.

use std::collections::HashMap;
use std::sync::Arc;

use crate::ports::EventLog;
use crate::ports::types::{CompanyEvent, CompanyId, EventSeq};

/// The reason stamped on a turn the host never got to finish.
///
/// Phrased as a host fact, like [`INTERRUPTED_BY_RESTART`][super::INTERRUPTED_BY_RESTART]:
/// nothing about the question went wrong, the process holding the answer went
/// away. Without this line the transcript holds the operator's question, no
/// answer after it, and no explanation — which is indistinguishable from a
/// message that never warranted a reply.
pub const TURN_INTERRUPTED_BY_RESTART: &str = concat!(
    "this turn was interrupted by a host restart and never answered; ",
    "ask again — nothing it did before stopping is lost, but no reply was produced"
);

/// Terminates chat turns a previous host process left open.
///
/// # Why an unterminated start is provably dead
///
/// The same three invariants
/// [`reap_orphaned_runs`](crate::ports::runs::reap_orphaned_runs) rests on, and
/// they hold for a chat turn verbatim: a turn is a process-local `tokio::spawn`
/// so it cannot outlive the process that spawned it; exactly one process owns a
/// company's journal (it is a documented single-writer log); and every turn
/// serialises on the per-company cycle mutex, so no turn from *this* process is
/// in flight before boot completes. So at boot an unmatched `TurnStarted` cannot
/// belong to a live turn: there are no live turns. No timeout heuristic is
/// needed, for exactly the reason none is needed there.
///
/// # It must NOT run on a rebuild
///
/// The argument above holds at boot and is false the moment a company has been
/// serving — and here the consequence is worse than it is for a workflow run. A
/// chat turn survives a live runtime swap ([`rebuild_company`](super::rebuild_company)):
/// `rebuild_company` quiesces and drains the *cycle* lock, but the spawned turn
/// task owns the reply journaling and the row settle **after** its cycle
/// returns, and the successor adopts the same mutex. Sweeping mid-life would
/// therefore stamp "interrupted by a host restart" on a turn that is still
/// working, and its real answer would land afterwards — leaving the operator a
/// transcript that says the turn failed and then answers it. The caller gates on
/// the handover being absent; see the call site in the runtime builder. Same
/// lesson as #290.
///
/// Best-effort throughout: a read or append failure is logged and swallowed,
/// because record-keeping must never stop a company from booting.
pub async fn sweep_interrupted_turns(events: &Arc<dyn EventLog>, company: &CompanyId) {
    let stored = match events
        .read_from(company, EventSeq::new(0), usize::MAX)
        .await
    {
        Ok(stored) => stored,
        Err(err) => {
            tracing::warn!(
                %company,
                %err,
                "could not read the journal to sweep interrupted turns"
            );
            return;
        }
    };

    // One pass keyed on turn id: a start inserts, a settlement — either way it
    // ended — removes. Whatever is left was accepted and never settled.
    // `HashMap` rather than a set because the log line names the desk, which
    // lives only on the start. `TurnSettled` closes a bracket exactly as
    // `TurnFailed` does (plan hive-desks, Phase 2): a seat turn that answered
    // is not one the host died under.
    let mut open: HashMap<String, String> = HashMap::new();
    for stored in stored {
        match stored.event {
            CompanyEvent::TurnStarted {
                turn_id, chat_id, ..
            } => {
                open.insert(turn_id, chat_id);
            }
            CompanyEvent::TurnFailed { turn_id, .. }
            | CompanyEvent::TurnSettled { turn_id, .. } => {
                open.remove(&turn_id);
            }
            _ => {}
        }
    }

    if open.is_empty() {
        return;
    }

    // Sorted so the appended order is deterministic — a `HashMap` iteration
    // order would make the journal's tail differ run to run for no reason.
    let mut interrupted: Vec<(String, String)> = open.into_iter().collect();
    interrupted.sort_by(|a, b| a.0.cmp(&b.0));

    for (turn_id, chat_id) in interrupted {
        tracing::info!(
            %company,
            turn = %turn_id,
            chat = %chat_id,
            "settling a chat turn left open by a previous host process"
        );
        if let Err(err) = events
            .append(
                company,
                CompanyEvent::TurnFailed {
                    turn_id: turn_id.clone(),
                    error: TURN_INTERRUPTED_BY_RESTART.to_string(),
                    agent_id: None,
                    chat_id: Some(chat_id.clone()),
                    episode_id: None,
                    round_revision: None,
                    outcome: None,
                },
            )
            .await
        {
            tracing::warn!(
                %company,
                turn = %turn_id,
                %err,
                "could not settle an interrupted turn; the next boot sweeps it again"
            );
        }
    }
}

#[cfg(test)]
#[path = "turn_sweep_tests.rs"]
mod tests;
