//! `take_over`: a guest seat claims the work and says so in its own line.
//!
//! # Why this is one tool and not two
//!
//! Because the episode half is not optional. A seat that announces without
//! concluding leaves the asker waiting forever: `Ledger::answered` is reached
//! only by the conductor's `Dm`, which it mints when a conversation ends with
//! `complete_episode` (`conduct/child.rs`), and while an ask is outstanding
//! the asker's own `complete_episode` is refused outright
//! (`driver::Error::AwaitingReply`). So an announcement that stands *instead
//! of* an answer is a stall with a receipt — the shape a live run already
//! spent an episode on, a copywriter nudged round and round because the
//! analyst it asked never concluded.
//!
//! Handing the seat a bare `announce` would make that stall reachable in one
//! call. This wraps the room's own `complete_episode` instead: the
//! conversation concludes first, and the announcement is what happens after
//! it does. A refusal from the room is returned untouched and nothing is
//! announced.
//!
//! # Why the announcement is not an utterance
//!
//! It lands outside the episode on purpose — `announce_takeover` writes
//! `episode: None`, because the operator can come back to that line tomorrow
//! and the episode will be long closed. A row built to outlive the episode
//! cannot be the row that advances it, so the driver never sees this one and
//! tinyhivemind learns nothing about `dm:` or operators. The library's own
//! `Ask` doc draws the same line: "this is a question, not a handoff".

use tinytools::{Tool, ToolResult};

use crate::hive::seating::TakeoverLoan;

/// One guest seat's claim, staged for the episode to act on once it ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TakeoverClaim {
    /// The episode this was staged inside.
    ///
    /// The queue is one per company runtime -- `HarnessDeps` is built once and
    /// its `Arc` is shared by every `HarnessDeps::clone` a message makes -- so
    /// "everything staged" is not this episode's claims, it is every open
    /// episode's. Draining without this key let an episode finishing on one
    /// desk carry on a claim made on another, opening the claimer's line while
    /// that teammate was still a live seat in the episode it claimed in: the
    /// one thing [`TakeoverQueue`] exists to prevent, reintroduced by the
    /// drain.
    pub episode: String,
    /// The teammate that took the work on. Its own operator line is where the
    /// work carries on.
    pub seat: String,
    /// The chat the announcement landed in -- `dm:<seat>`.
    pub chat: String,
    /// The announcement's sequence, which the follow-on episode opens from so
    /// its first row is the claim the operator can already see.
    pub at: u64,
    /// What it said it was taking on, in its own words -- the opening the
    /// claimer's line carries on from.
    pub saying: String,
}

/// Claims staged by the seats of one episode.
///
/// # Why a queue and not the tool doing it
///
/// Because a seat cannot start an episode and must not start this one. The
/// claimer is a *live seat inside the asker's episode* when it calls the verb:
/// opening its own line there would put the same teammate in two turns at
/// once. And the tool holds `events`, `company` and `agent` -- no hive map, no
/// pool -- so it could not dispatch even if that were safe.
///
/// So the claim is staged, the asker's episode finishes (which is correct: the
/// takeover *concluded* that conversation), and the dispatcher -- which does
/// hold the hives and the pool -- opens the claimer's line afterwards. One
/// conversation ends, another begins, in that order.
#[derive(Clone, Debug, Default)]
pub struct TakeoverQueue {
    claims: std::sync::Arc<std::sync::Mutex<Vec<TakeoverClaim>>>,
}

impl TakeoverQueue {
    /// Stages one claim.
    pub fn stage(&self, claim: TakeoverClaim) {
        self.claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(claim);
    }

    /// Takes what `episode` staged, leaving every other episode's claims
    /// where they are.
    ///
    /// Draining rather than reading: a claim acted on twice would open the
    /// same line twice, and this queue lives as long as the company runtime.
    ///
    /// Keyed because it lives that long. It is reached through `HarnessDeps`,
    /// built once per runtime and shared by `Arc` through every clone, so
    /// every episode in the company stages into this one `Vec`. An unkeyed
    /// drain therefore hands one episode's ending the claims of episodes that
    /// are still running -- and opening a claimer's line while it is still a
    /// seat elsewhere is exactly what staging exists to avoid.
    #[must_use]
    pub fn drain(&self, episode: &str) -> Vec<TakeoverClaim> {
        let mut claims = self
            .claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mine, theirs) = std::mem::take(&mut *claims)
            .into_iter()
            .partition(|claim| claim.episode == episode);
        *claims = theirs;
        mine
    }
}

/// The bare name, before the episode's tool prefix.
pub const TAKE_OVER_TOOL: &str = "take_over";

/// Wraps one seat's `complete_episode` so claiming work also reaches the
/// operator.
pub struct TakeOverTool {
    /// The room's own `complete_episode`, taken from a spare belt.
    complete: Box<dyn Tool>,
    /// Where the announcement goes, and who it is from.
    loan: TakeoverLoan,
    /// This tool's prefixed name, owned because [`Tool::name`] returns a
    /// borrow.
    name: String,
    /// What the seat is told `chat`/`parent` must be, kept in the schema so
    /// the room's own refusal is not the first time it finds out.
    describe: String,
}

impl std::fmt::Debug for TakeOverTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TakeOverTool")
            .field("name", &self.name)
            .field("loan", &self.loan)
            .finish_non_exhaustive()
    }
}

impl TakeOverTool {
    /// Builds the tool over `complete` — the seat's own `complete_episode`,
    /// which must come from a belt of its own: a `Box<dyn Tool>` moves, and
    /// the seat still needs the copy on its belt.
    #[must_use]
    pub fn new(complete: Box<dyn Tool>, loan: TakeoverLoan, prefix: &str) -> Self {
        let name = format!("{prefix}{TAKE_OVER_TOOL}");
        let describe =
            format!("Same `chat` and `parent` as `{prefix}complete_episode` — this turn's own.");
        Self {
            complete,
            loan,
            name,
            describe,
        }
    }
}

#[async_trait::async_trait]
impl Tool for TakeOverTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Take this work on yourself. Concludes your conversation with whoever asked you — so \
         they stop waiting and can finish — and tells the operator, in your own line with them, \
         that you have it. Use it whenever you are becoming the owner, INCLUDING when 'I will \
         own it' is itself the answer you were asked for: this is the only way the operator \
         hears it. Saying so in your reply does not reach them, and neither does asking the \
         teammate who asked you to pass it on. Not for an answer you are handing back — a \
         teammate that asked for an estimate wants the estimate, and `complete_episode` is how \
         it gets one. After this, the operator discusses the work with you, not with them. Say \
         in `message` what you are taking on, in plain words — it is read by a person, and it \
         is the last thing the teammate that asked you hears."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "What you are taking on and what happens next, in one or two \
                                    plain sentences. A person reads this."
                },
                "chat": {
                    "type": "string",
                    "description": self.describe
                },
                "parent": {
                    "type": ["string", "null"],
                    "description": self.describe
                }
            },
            "required": ["message", "chat", "parent"],
            "additionalProperties": false
        })
    }

    fn permission_level(&self) -> tinytools::PermissionLevel {
        tinytools::PermissionLevel::Write
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let message = args
            .get("message")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .ok_or_else(|| anyhow::anyhow!("`message` is required"))?
            .to_string();

        // **The room decides first.**
        //
        // Its refusals are the ones worth reading -- a wrong `chat`, a second
        // action in one turn, a conversation that is not this seat's to end --
        // and each is already worded for the seat. Passing the arguments
        // through unchanged keeps them that way, and keeps this tool from
        // owning a copy of a contract it does not define.
        let concluded = self.complete.execute(args).await?;
        if concluded.is_error {
            return Ok(concluded);
        }

        let chat = crate::hive::dispatch::announce_takeover(
            self.loan.events.as_ref(),
            &self.loan.company,
            &self.loan.agent,
            &message,
        )
        .await;

        // **A failed announcement is not a failed takeover.**
        //
        // The conclusion is already committed and the asker is already
        // released; reporting an error here would tell the seat to try again,
        // and the retry would be refused as a second action on a settled
        // conversation. So this reports what is true: the handover happened,
        // the operator was not told, and saying so is now the seat's problem
        // rather than something it can fix with another call.
        Ok(match chat {
            Ok((chat, seq)) => {
                // **And the work carries on in that line.**
                //
                // Announcing alone left the claim a dead end: the operator was
                // told a teammate owned the campaign, and nothing ran. A live
                // run watched exactly that -- "I'm owning the pricing launch
                // campaign end to end" -- followed by silence, no episode, and
                // a line the operator had to prod to restart.
                //
                // Staged rather than opened here: this seat is still mid-turn
                // inside the asker's episode, and opening its own line now
                // would run the same teammate twice at once. The episode acts
                // on it once it ends.
                self.loan.queue.stage(TakeoverClaim {
                    episode: self.loan.episode.clone(),
                    seat: self.loan.agent.clone(),
                    chat: chat.clone(),
                    at: seq.value(),
                    saying: message.clone(),
                });
                ToolResult::success(format!(
                    "recorded: you have this work, and the operator has been told in `{chat}`. \
                     Carry on there -- this conversation is closed. Your turn is complete; say \
                     nothing else."
                ))
            }
            Err(error) => ToolResult::success(format!(
                "recorded: you have this work and whoever asked you is no longer waiting. The \
                 operator could NOT be told ({error}) — do not retry, and do not assume they \
                 know. Your turn is complete; say nothing else."
            )),
        })
    }
}

/// Lifts `complete_episode` out of a belt built for exactly this.
///
/// `None` when the belt carries no such tool, which is what a host that
/// renamed or withheld it means; the caller then offers no `take_over`
/// rather than one that wraps nothing.
#[must_use]
pub fn take_complete_episode(belt: &mut Vec<Box<dyn Tool>>, prefix: &str) -> Option<Box<dyn Tool>> {
    let wanted = format!("{prefix}complete_episode");
    let at = belt.iter().position(|tool| tool.name() == wanted)?;
    Some(belt.remove(at))
}

/// Builds the seat's `take_over`, when the episode lent it one.
///
/// Returns `None` for a seat with nothing to announce — every desk seat, and
/// the teammate whose DM the episode is running in, which answers the
/// operator directly.
#[must_use]
pub fn tool_for(loan: &crate::hive::seating::SeatLoan, prefix: &str) -> Option<Box<dyn Tool>> {
    let takeover = loan.takeover.clone()?;
    // A belt of its own: `Box<dyn Tool>` does not clone, and the seat still
    // needs the `complete_episode` on the belt it is handed.
    let mut spare = loan.source.belt();
    let complete = take_complete_episode(&mut spare.tools, prefix)?;
    Some(Box::new(TakeOverTool::new(complete, takeover, prefix)) as Box<dyn Tool>)
}

/// Every seat whose loan carries a takeover is also told about it.
///
/// The persona is where a seat learns what it may do (`dm_persona_note`), and
/// a verb it is handed but never told about is one live runs show it will not
/// reach for.
///
/// # Why this takes the prefix
///
/// Because the seat is handed `desk_take_over` and this used to name
/// `take_over`. A live run watched a teammate agree in words to own the work
/// -- "Yes, I'll take ownership ... end to end" -- and then reach for
/// `complete_episode`, the only verb it had been told about *by a name it
/// could see on its belt*. Naming a tool a seat does not have is the same
/// defect as handing it one nobody mentioned, from the other side.
#[must_use]
pub fn guest_persona_note(prefix: &str) -> String {
    format!(
        "\n\nIf you are taking this work on -- if the answer is that you will own it -- say so \
         with `{prefix}{TAKE_OVER_TOOL}`: it ends this conversation for the teammate that asked \
         you and tells the operator, in your own line with them, that you have it.\n\nThat tool \
         is the only way to reach the operator from here. Answering that you will own it does \
         not tell them, and neither does asking whoever asked you to pass it on -- they are \
         finishing their own conversation, not yours, and the operator never hears it."
    )
}

#[cfg(test)]
#[path = "takeover_tests.rs"]
mod tests;

/// Whether this desk and seat make a guest — the pairing `take_over` exists
/// for.
///
/// A guest is a teammate bound into somebody else's operator DM so it can be
/// asked things. The owner of the DM is not one: it is already the operator's
/// correspondent there, so it has nobody to announce to and no work to claim
/// that it does not already hold.
#[must_use]
pub fn is_guest_seat(desk_id: &str, seat: &str) -> bool {
    desk_id
        .strip_prefix(crate::runtime::assignee::DM_PREFIX)
        .is_some_and(|owner| owner != seat)
}
