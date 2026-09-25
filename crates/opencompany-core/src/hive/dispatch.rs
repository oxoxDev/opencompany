//! Where a chat message goes: the surface it lands on, and the episode it
//! opens when that surface is a desk with a room.
//!
//! This is the chat body of the harness brain's cycle (plan hive-desks,
//! Phase 5). A message on a **desk of two or more bound seats** opens (or
//! joins) an episode on that desk's hive and is answered by the rounds the
//! driver runs; the brain pushes no bubble for it, because every seat's
//! utterance is already a journaled `AgentReply` and the console sees each one
//! land live. Every other surface — a DM, `#general`, a workflow thread, a
//! desk of one — is one ordinary turn on the responder the host's own rules
//! pick: the teammate the message named, else the desk's default responder,
//! else the orchestrator. No router and no driver touch those.
//!
//! The dispatcher for a company is built from the harness pool's live agents
//! per message rather than cached: a hive is a validation and a handful of
//! `Arc` clones, and a roster or desk change is then in force on the next
//! message with nothing to invalidate.

use std::collections::HashMap;
use std::sync::Arc;

use tinyhivemind_embed::Router;

use crate::hive::conducted::{EpisodeReport, HiveDispatcher, Trigger};
use crate::hive::graph::desk_hives;
use crate::ports::events::EventLog;
use crate::ports::types::{CompanyEvent, CompanyRecord, EventSeq, Mention};

/// The surface a chat message landed on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Surface {
    /// A desk (or a thread in one) whose hive runs episodes.
    Room {
        /// The canonical desk id.
        desk_id: String,
    },
    /// Everything else: one turn on one responder.
    Single,
}

/// Which surface `chat` is, given the hives this company runs.
#[must_use]
pub fn surface_of(
    record: &CompanyRecord,
    hives: &HashMap<String, Arc<crate::hive::graph::DeskHive>>,
    chat: Option<&str>,
) -> Surface {
    let Some(chat) = chat else {
        return Surface::Single;
    };
    if crate::server::chat_history::is_general_chat(Some(chat)) {
        return Surface::Single;
    }
    // An operator DM, when one runs a hive.
    //
    // `resolve_desk_id` cannot answer for these: a DM is not a desk in the
    // manifest, so it would fall through to `Single` and take the pooled
    // path. The hive map is the authority -- `dm_hives` only builds one when
    // DM episodes are on, so an absent entry is the flag being off and the
    // pooled turn is the right answer.
    if chat.starts_with(crate::runtime::assignee::DM_PREFIX) {
        return if hives.contains_key(chat) {
            Surface::Room {
                desk_id: chat.to_owned(),
            }
        } else {
            Surface::Single
        };
    }
    // A declared desk owns its key outright, hive or no hive.
    //
    // The `else` is not dead: `desk_hives` builds nothing for a desk with
    // nobody to deliberate with, and skips one whose members would not bind.
    // Falling through on that would hand a DESK's message to the DM arm below
    // and, where a teammate shares the id (issue #1743), run it as that
    // teammate's private episode. `Single` is what this answered before the DM
    // arm existed, and it stays the answer (tinysweeper on #2484).
    if let Some(desk_id) = record.resolve_desk_id(chat) {
        return if hives.contains_key(&desk_id) {
            Surface::Room { desk_id }
        } else {
            Surface::Single
        };
    }
    // The same DM, addressed the way the console addresses it.
    //
    // `dmThreadId` (`views/room/channels.ts`) posts an ordinary teammate's DM
    // under the **bare** teammate id; only a teammate whose id is a General
    // spelling is addressed `dm:<id>`. The arm above is keyed on the prefixed
    // form alone, so every DM the console sends fell through to `Single` and
    // took the pooled path -- DM episodes were unreachable from the console
    // whatever `OPENCOMPANY_DM_EPISODES` said, and a seat that never ran never
    // had `ask`, so a teammate asked to consult somebody wrote the consultation
    // into the operator's own line instead of holding one.
    //
    // Resolved through the roster exactly as `chat_responder` resolves the two
    // spellings (`runtime::delegation_tools`), and **after** `resolve_desk_id`,
    // so a declared desk still wins the key it shares with a teammate (issue
    // #1743) and only a non-desk key can reach a DM hive.
    if let Some(agent) = record.resolve_roster_agent_id(chat) {
        let key = format!("{}{agent}", crate::runtime::assignee::DM_PREFIX);
        if hives.contains_key(&key) {
            return Surface::Room { desk_id: key };
        }
    }
    Surface::Single
}

/// Builds the company's hives over the agents `bind` resolves.
#[must_use]
pub fn hives_for(
    record: &CompanyRecord,
    bind: &dyn Fn(&str) -> Option<openhuman_embed::Agent>,
) -> HashMap<String, Arc<crate::hive::graph::DeskHive>> {
    // Echoed by a Jev evaluation and compared within one request; a
    // per-build counter would be no more meaningful than the roster size.
    let roster_version = record.effective_agents().len() as u64;
    let (mut hives, errors) = desk_hives(record, roster_version, bind);
    for error in errors {
        tracing::warn!(company = %record.id, %error, "[hive] a desk got no hive");
    }
    // Operator DMs, when the flag is on. Keyed by the chat id itself, which
    // is what `surface_of` looks up -- and absent when it is off, which is
    // how a DM keeps taking the pooled path.
    if crate::hive::graph::dm_episodes_enabled(&crate::app::config::ProcessEnv) {
        let (dms, errors) = crate::hive::graph::dm_hives(record, roster_version, bind);
        for error in errors {
            tracing::warn!(company = %record.id, %error, "[hive] a DM got no hive");
        }
        let count = dms.len();
        hives.extend(dms);
        tracing::info!(company = %record.id, count, "[hive] operator DMs run as episodes");
    }
    hives
}

/// The Jev router this host routes with, if a TinyHumans key resolves.
#[must_use]
pub fn host_router() -> Option<Arc<dyn Router>> {
    match crate::hive::jev::jev_router(&crate::app::config::ProcessEnv, None) {
        Ok(Some(router)) => Some(Arc::new(router)),
        Ok(None) => {
            tracing::info!("[hive] no TinyHumans key: desks route by lead and mention");
            None
        }
        Err(error) => {
            tracing::warn!(%error, "[hive] the Jev router is misconfigured; routing by lead and mention");
            None
        }
    }
}

/// The message that opens or joins an episode, from a journaled operator
/// message.
#[must_use]
pub fn trigger_for(
    seq: Option<EventSeq>,
    text: &str,
    parent: Option<EventSeq>,
    mentions: &[Mention],
) -> Trigger {
    Trigger {
        seq: seq.unwrap_or(EventSeq::new(0)),
        text: text.to_string(),
        parent,
        mentions: mentions.to_vec(),
    }
}

/// One host mention in the library's shape.
///
/// The host's `User` is the library's `Person`; everything else is the same
/// target under another name.
#[must_use]
pub fn tinyhivemind_mention(
    mention: &crate::ports::types::Mention,
) -> tinyhivemind_core::mention::Mention {
    use crate::ports::types::MentionTarget as HostTarget;
    use tinyhivemind_core::mention::MentionTarget;

    let target = match &mention.target {
        HostTarget::Agent { id } => MentionTarget::Agent { id: id.clone() },
        HostTarget::User { id } => MentionTarget::Person { id: id.clone() },
        HostTarget::Desk { id } => MentionTarget::Desk { id: id.clone() },
        HostTarget::Everyone => MentionTarget::Everyone,
    };
    tinyhivemind_core::mention::Mention {
        target,
        text: mention.text.clone(),
        offset: mention.offset,
        quiet: mention.quiet,
    }
}

/// Assembles a dispatcher for one company.
#[must_use]
pub fn dispatcher(
    record: Arc<CompanyRecord>,
    events: Arc<dyn EventLog>,
    hives: HashMap<String, Arc<crate::hive::graph::DeskHive>>,
    deps: Arc<crate::harness::built_in::HarnessDeps>,
    pool: Arc<crate::harness::built_in::HarnessPool>,
    mentions: Option<crate::runtime::mention_seam::MentionSeam>,
) -> Arc<HiveDispatcher> {
    Arc::new(HiveDispatcher {
        record,
        events,
        hives,
        router: host_router(),
        deps,
        pool,
        mentions,
    })
}

/// Drives the episode on its own task and returns at once.
///
/// The cycle that accepted the message must not wait on the room: rounds
/// run for as long as the seats take, the operator's request is already
/// journaled, and the console follows the episode frames live. The task is
/// type-erased for the reason `driver::spawn_desk_message` is.
pub fn spawn_episode(
    dispatcher: Arc<HiveDispatcher>,
    desk_id: String,
    trigger: Trigger,
) -> tokio::task::JoinHandle<Option<EpisodeReport>> {
    let task: std::pin::Pin<Box<dyn std::future::Future<Output = Option<EpisodeReport>> + Send>> =
        Box::pin(async move {
            match dispatcher.run_desk_message(&desk_id, trigger).await {
                Ok(report) => Some(report),
                Err(error) => {
                    tracing::warn!(desk = %desk_id, %error, "[hive] the episode failed");
                    None
                }
            }
        });
    tokio::spawn(task)
}

/// Carries on a parked episode from its checkpoint on its own task, as
/// [`spawn_episode`] runs a new one.
pub fn spawn_resume(
    dispatcher: Arc<HiveDispatcher>,
    episode_id: String,
) -> tokio::task::JoinHandle<Option<EpisodeReport>> {
    let task: std::pin::Pin<Box<dyn std::future::Future<Output = Option<EpisodeReport>> + Send>> =
        Box::pin(async move {
            match dispatcher.resume_desk_message(&episode_id).await {
                Ok(report) => report,
                Err(error) => {
                    tracing::error!(episode = %episode_id, %error, "[hive] the episode could not be resumed");
                    None
                }
            }
        });
    tokio::spawn(task)
}
/// A teammate tells the operator it is taking something on, in its own line.
///
/// # Why the askee speaks, and not the asker
///
/// The obvious shape is the other way round: the teammate holding the
/// conversation pushes the question into the other's line and steps back. It
/// does not work, and the reason is structural rather than incidental. An
/// episode opens when a message arrives *through the cycle* -- that is the one
/// call site of `spawn_episode`. A row appended straight to the journal is the
/// record of a message, not the delivery of one: nobody reads for it, and the
/// teammate it was addressed to never wakes.
///
/// Inverting it removes the problem instead of working around it. The askee is
/// **already running** -- it was asked, so it has a turn. It does not need one
/// started for it; it needs somewhere to say so. And the operator's reply is an
/// ordinary message on an ordinary chat, so it comes through the cycle like any
/// other and opens that teammate's episode by the normal door.
///
/// It also makes the transfer consensual. A hand-off pushed at someone is work
/// they have not agreed to; this is a teammate saying it has the thing, which
/// is the only version an operator can rely on.
///
/// # What the operator sees
///
/// A row in `dm:{agent}` -- the same console channel a parked blocker stamps
/// (`blocker_sender::dm_thread`). From then on that line is where the work is
/// discussed, and a reply there reaches this teammate rather than whoever the
/// operator first wrote to.
///
/// # Errors
///
/// Whatever stops the journal accepting the row.
pub async fn announce_takeover(
    events: &dyn EventLog,
    company: &crate::ports::types::CompanyId,
    agent: &str,
    saying: &str,
) -> crate::Result<(String, EventSeq)> {
    let chat = crate::company::blocker_sender::dm_thread(agent);
    let seq = events
        .append(
            company,
            CompanyEvent::AgentReply {
                chat_id: chat.clone(),
                agent_id: agent.to_owned(),
                text: saying.to_owned(),
                steps: Vec::new(),
                outputs: Vec::new(),
                task_id: None,
                parent: None,
                mentions: Vec::new(),
                mention_depth: 0,
                audience: Default::default(),
                // Not an episode's row. The announcement outlives whatever
                // episode prompted it -- the operator can come back to this
                // line tomorrow, and the episode will be long closed.
                episode: None,
            },
        )
        .await?;
    Ok((chat, seq))
}

/// What the teammate that handed work on tells the operator, if it says
/// anything at all.
///
/// Fixed wording. The tool that handed work over used to return a sentence for
/// the agent to paraphrase, and it paraphrased it into a promise -- "they will
/// answer this turn" -- that nothing could keep. What is true is that somebody
/// else has it and where they will be reached; that is what this says.
#[must_use]
pub fn hand_off_notice(to: &str, chat: &str) -> String {
    format!(
        "@{to} has picked this up. Their line with you ({chat}) is where they \
         will reply."
    )
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
