//! Where a chat message goes: the surface it lands on, and the episode it
//! opens when that surface is a desk with a room.
//!
//! This is the chat body of the harness brain's cycle (plan hive-desks,
//! Phase 5). A message on a **room of two or more bound seats** — a desk,
//! `#general`, or an operator DM — opens (or joins) an episode on that room's
//! hive and is answered by the rounds the driver runs; the brain pushes no
//! bubble for it, because every seat's utterance is already a journaled
//! `AgentReply` and the console sees each one land live. Every other surface —
//! a message with no chat id, a workflow thread, a desk of one — is one
//! ordinary turn on the responder the host's own rules pick: the teammate the
//! message named, else the desk's default responder, else the orchestrator.
//! No router and no driver touch those.
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
    if chat == crate::ports::general_channel::GENERAL_CHANNEL_ID {
        return if hives.contains_key(chat) {
            Surface::Room {
                desk_id: chat.to_owned(),
            }
        } else {
            Surface::Single
        };
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
    hives_for_in(record, bind, &crate::app::config::ProcessEnv)
}

/// [`hives_for`] with its switches read from `env`.
pub(crate) fn hives_for_in(
    record: &CompanyRecord,
    bind: &dyn Fn(&str) -> Option<openhuman_embed::Agent>,
    env: &dyn crate::app::config::EnvSource,
) -> HashMap<String, Arc<crate::hive::graph::DeskHive>> {
    // Echoed by a Jev evaluation and compared within one request; a
    // per-build counter would be no more meaningful than the roster size.
    let roster_version = record.effective_agents().len() as u64;
    let (mut hives, errors) = desk_hives(record, roster_version, bind);
    for error in errors {
        tracing::warn!(company = %record.id, %error, "[hive] a desk got no hive");
    }
    if crate::hive::graph::general_episodes_enabled(env) {
        match crate::hive::graph::general_hive(record, roster_version, bind) {
            Ok(Some(general)) => {
                tracing::info!(
                    company = %record.id,
                    seats = general.members().len(),
                    "[hive] #general runs as episodes"
                );
                hives.insert(general.desk_id.clone(), general);
            }
            Ok(None) => {
                tracing::debug!(company = %record.id, "[hive] #general has fewer than two bound seats");
            }
            Err(error) => {
                tracing::warn!(company = %record.id, %error, "[hive] #general got no hive");
            }
        }
    }
    // Operator DMs, when the flag is on. Keyed by the chat id itself, which
    // is what `surface_of` looks up -- and absent when it is off, which is
    // how a DM keeps taking the pooled path.
    if crate::hive::graph::dm_episodes_enabled(env) {
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

/// Tiers 1 and 2 of the routing credential, shared by everything that reaches
/// System One on a company's behalf.
///
/// A configured `Credential` is that identity. An unconfigured one means
/// "nothing here -- walk the inherited environment ladder", which is tier 3 and
/// lives in `jev.rs`. `None` is different and stronger: the secret store could
/// not be read, so this company's identity is *unknown*.
/// [`company_key::load`](crate::company::company_key::load) propagates that
/// error deliberately and its own doc says why -- mapping it to "no credential"
/// would route a company that has a key under the host's own
/// `OPENCOMPANY_INFERENCE_KEY` for as long as the store stayed unreadable,
/// borrowing an identity rather than using the one this company pays under.
///
/// # Why this is one function
///
/// Because it was two. Routing read the company key and the closing oracle did
/// not, so a company that signed in through the console routed as itself and
/// concluded as the host -- silently, because a host with no oracle concludes
/// every settled episode and picks the desk lead, which reads as a policy
/// rather than as a credential nobody looked for. Anything that asks System One
/// a question on a company's behalf resolves its identity here.
///
/// Called **once** per dispatcher, and its answer handed to both consumers.
/// Calling it per consumer made them share the logic but not the read: two
/// store reads a moment apart can disagree -- one failing, or a rotation
/// landing between them -- and the pair would split again for a reason no
/// amount of shared code prevents. One read cannot disagree with itself, and a
/// [`Credential`](crate::company::Credential) is already the seam that carries
/// a rotating token rather than a value frozen at build time, so sharing one is
/// what keeps both callers on the same identity as it rotates.
async fn routing_credential(
    company: &crate::ports::types::CompanyId,
    secrets: Option<&Arc<dyn crate::ports::SecretStore>>,
) -> Option<crate::company::Credential> {
    let env = &crate::app::config::ProcessEnv;
    let credential = crate::app::config::EnvSource::get(env, crate::hive::jev::JEV_KEY_ENV)
        .map(crate::company::Credential::from_value)
        .unwrap_or_default();
    if credential.configured() {
        return Some(credential);
    }
    let Some(secrets) = secrets else {
        return Some(credential);
    };
    match crate::company::company_key::load(company, secrets.as_ref()).await {
        Ok(company_key) => Some(company_key),
        Err(error) => {
            tracing::warn!(
                %error,
                "[hive] the company's account key could not be read; routing by lead and mention"
            );
            None
        }
    }
}

/// The Jev router this host routes with, built from an already-resolved
/// credential -- `None` when [`routing_credential`] could not read one.
///
/// # Which key, in what order
///
/// 1. `OPENCOMPANY_JEV_KEY`, because routing and inference are not always the
///    same vendor (see [`jev::JEV_KEY_ENV`]) and an operator who names a
///    routing vendor explicitly means it.
/// 2. The company's own account key -- `tinyhumans/key`, what a console sign-in
///    stores and [`company_key::load`](crate::company::company_key::load)
///    reads. This tier is why the function takes a company at all. Without it a
///    host whose only credential arrived through the console routed by lead and
///    mention while inference, reading that same key by that same seam, worked:
///    the key material was present and routing was the one resolver not looking
///    at it.
/// 3. The inherited environment ladder inside [`jev::jev_router`]
///    (`OPENCOMPANY_INFERENCE_KEY`, the TinyHumans token file,
///    `TINYHUMANS_API_KEY`), which is the right answer when routing and
///    inference *are* the same vendor.
///
/// Read live on every dispatch rather than cached at boot, for the reason
/// `resolve_effective` re-reads the secret store on every call: a company that
/// signs in while the host is up should route through Jev on its next message,
/// not its next restart.
///
/// `secrets` is `None` wherever no store is wired -- tests, stub runtimes --
/// which skips tier 2 and leaves the resolution exactly as it was.
#[must_use]
pub fn host_router(credential: Option<crate::company::Credential>) -> Option<Arc<dyn Router>> {
    let env = &crate::app::config::ProcessEnv;
    let credential = credential?;
    // Unconfigured here is not "no key anywhere": tier 3 lives inside
    // `jev_router`, which is what `None` asks it to walk.
    let resolved = if credential.configured() {
        crate::hive::jev::jev_router_from(env, credential)
    } else {
        crate::hive::jev::jev_router(env, None)
    };
    match resolved {
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

/// The System One transport the closing decision asks, or `None`.
///
/// Built from the *same* [`routing_credential`] result [`host_router`] is given,
/// and silent in the same way: an instance with no credential concludes every
/// settled episode and picks the desk lead, which is the behaviour before
/// [`crate::hive::conclude::decide`] existed.
#[must_use]
pub fn host_oracle(
    credential: Option<crate::company::Credential>,
) -> Option<Arc<crate::hive::jev::TinyHumansSystemOne>> {
    let env = &crate::app::config::ProcessEnv;
    let credential = credential?;
    // Tier 3 lives inside `jev_transport`, which is what `None` asks it to walk.
    let resolved = if credential.configured() {
        crate::hive::jev::jev_transport_from(env, credential)
    } else {
        crate::hive::jev::jev_transport(env, None)
    };
    match resolved {
        Ok(Some(transport)) => Some(Arc::new(transport)),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%error, "[hive] no closing oracle; every settled desk episode concludes");
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
///
/// `async` because the routing credential is resolved here and the
/// [`SecretStore`](crate::ports::SecretStore) is async. Every caller is already
/// in an async context. The alternative -- a credential pre-resolved onto
/// [`HarnessDeps`](crate::harness::built_in::HarnessDeps) -- would go stale the
/// way the inference copy of the account key did before issue #2266 removed it.
pub async fn dispatcher(
    record: Arc<CompanyRecord>,
    events: Arc<dyn EventLog>,
    hives: HashMap<String, Arc<crate::hive::graph::DeskHive>>,
    deps: Arc<crate::harness::built_in::HarnessDeps>,
    pool: Arc<crate::harness::built_in::HarnessPool>,
    mentions: Option<crate::runtime::mention_seam::MentionSeam>,
) -> Arc<HiveDispatcher> {
    // Before the record is moved into the dispatcher, and before the struct, so
    // the await is not held across a partially-built value.
    let credential = routing_credential(&record.id, deps.secrets.as_ref()).await;
    let router = host_router(credential.clone());
    let oracle = host_oracle(credential);
    Arc::new(HiveDispatcher {
        record,
        events,
        hives,
        router,
        oracle,
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
            // Minted here, not inside, so the id survives a failure: a claim
            // staged by an episode that then errored is still a claim the
            // operator holds a durable row about, and a queue that lives as
            // long as the runtime would strand it.
            let episode_id = uuid::Uuid::new_v4().simple().to_string();
            let outcome = match dispatcher
                .run_desk_message_as(&desk_id, trigger, episode_id.clone())
                .await
            {
                Ok(report) => Some(report),
                Err(error) => {
                    tracing::warn!(desk = %desk_id, %error, "[hive] the episode failed");
                    None
                }
            };
            // **A claimed handover carries on in the claimer's own line.**
            //
            // Drained here, after the episode this claim came out of has
            // ended, and deliberately not before: the claimer was a seat
            // *inside* that episode, so opening its line any earlier would run
            // the same teammate in two turns at once. `take_over` already
            // concluded the asker's conversation -- that episode finishing is
            // the handover's first half, and this is its second.
            //
            // Failures are logged and dropped rather than failing the episode
            // that just succeeded: the conversation really did transfer and
            // the operator really was told, so the recoverable state is a line
            // that needs one more message, not a run to unwind.
            carry_on_takeovers(&dispatcher, &episode_id).await;
            outcome
        });
    tokio::spawn(task)
}

/// The job a takeover's claimer is set, in the words it used to claim it.
///
/// Its own function so the call site's wording is reachable from a test. The
/// claim is quoted rather than referred to: the brief window is the seat's
/// unread rows, not a slice this code controls, so "the words above" is a
/// pointer at something that may not be there.
#[must_use]
pub fn carry_on_job(saying: &str) -> String {
    format!(
        "You have taken this work on. In your own words: \"{}\"\n\nIt is yours now, and this \
         is your line with the operator. Carry it out, and tell them where it stands when you \
         have something worth reading.",
        saying.trim()
    )
}

/// How a carried-on episode is opened: **at** the job, **rooted on** the claim.
///
/// Extracted because the pairing is the thing that regressed, and asserting
/// `trigger_for` echoes its arguments does not pin it -- the defect was this
/// call site passing `None` for `parent`, which `trigger_for` accepted
/// happily. `run_desk_message` then took the root from `seq`, every utterance
/// hung off a `SYSTEM_AUTHOR` row, and the console draws one as a pill with
/// no "N replies": the claimer's whole report to the operator was journaled
/// and unreachable.
///
/// `seq` is what the seat is briefed from and what its assignment is;
/// `parent` is where a reader finds the conversation. The job is the first,
/// the claim is the second.
#[must_use]
fn carry_on_trigger(claim: &crate::hive::takeover::TakeoverClaim, opened_at: EventSeq) -> Trigger {
    trigger_for(
        Some(opened_at),
        &carry_on_job(&claim.saying),
        Some(EventSeq::new(claim.at)),
        &[],
    )
}

/// Opens the claimer's own line for every takeover staged inside `episode`.
///
/// One episode ends and another begins, which is what a handover is: the
/// asker's conversation was concluded by the verb itself, and the work now
/// belongs to a teammate the operator talks to directly.
async fn carry_on_takeovers(dispatcher: &Arc<HiveDispatcher>, episode: &str) {
    for claim in dispatcher.deps.takeovers.drain(episode) {
        let desk_id = format!("{}{}", crate::runtime::assignee::DM_PREFIX, claim.seat);
        if !dispatcher.hives.contains_key(&desk_id) {
            // DM episodes are off, or this teammate has no hive. The operator
            // still has the announcement; what they do not get is a room.
            tracing::warn!(
                seat = %claim.seat,
                chat = %claim.chat,
                "[hive] a takeover was announced but its line runs no episodes, so nothing \
                 carries it on"
            );
            continue;
        }
        // **The opening has to be a row, because a row is all the seat reads.**
        //
        // Seeding the episode at the claim's own sequence left the seat
        // answering itself: it opened its line, saw its own sentence and the
        // word "concluded", and was told by the brief that *that* was its
        // assignment. A live run closed in two turns having asked nobody --
        // it had not been given anything to do.
        //
        // Handing the instruction to `trigger_for` does not fix it. A
        // trigger's text only picks who speaks first (`opening`), and in a DM
        // `dm_opening` pins the owner and returns before even reading it, so
        // the sentence is built and dropped. What a seat is briefed from is
        // the journal from `trigger.seq` on -- so the job has to be written
        // there, and the episode opened at the row that carries it.
        //
        // Authored by `SYSTEM_AUTHOR` and addressed to the claimer, so the
        // brief reads it the way it reads a nudge.
        //
        // The address is *not* what keeps it off the operator's screen --
        // `aside_audience` is "a coordination device between agents and never
        // access control; an operator reads the row regardless"
        // (`server::chat_history`). They see it, as the system pill that
        // opens the claimer's line, which is honest about why that line
        // suddenly has work in it. What the address does is tell the driver
        // whose brief it belongs in, so the other seats of the episode are
        // not handed an instruction written to somebody else.
        let opening = carry_on_job(&claim.saying);
        // Falls back to the claim's sequence, which is where this opened
        // before the row existed. A journal that will not take the row is not
        // a reason to strand work a teammate has already accepted in public;
        // the degraded episode is the old behaviour, and the warning says so.
        let opened_at = match brief_takeover(
            dispatcher.events.as_ref(),
            &dispatcher.record.id,
            &claim.seat,
            &opening,
        )
        .await
        {
            Ok(seq) => seq,
            Err(error) => {
                tracing::warn!(
                    seat = %claim.seat,
                    %error,
                    "[hive] a takeover's opening could not be journaled, so its episode opens \
                     on the claim itself and the seat is briefed with no job"
                );
                crate::ports::types::EventSeq::new(claim.at)
            }
        };
        // **Opened at the job, rooted on the claim.**
        //
        // `run_desk_message` takes the thread root from `parent`, falling back
        // to `seq` -- so opening at the job row alone made that row the root,
        // and every utterance of the episode hung off it. The console renders
        // a `SYSTEM_AUTHOR` row as a pill rather than a message, and a pill
        // carries no "N replies", so the claimer's own report to the operator
        // -- the `complete_episode` row, the point of the whole episode --
        // was journaled and unreachable. A live run ended with the operator
        // seeing a claim, a grey line, and "Episode complete - 3 rounds".
        //
        // The two answers are genuinely different questions. `seq` is what
        // the seat is briefed from and what its assignment is; `parent` is
        // where the conversation hangs for a reader. The job is the first and
        // the claim is the second.
        let trigger = carry_on_trigger(&claim, opened_at);
        tracing::info!(
            seat = %claim.seat,
            chat = %claim.chat,
            "[hive] a takeover carries on in the claimer's own line"
        );
        spawn_episode(Arc::clone(dispatcher), desk_id, trigger);
    }
}

/// Carries on a parked episode from its checkpoint on its own task, as
/// [`spawn_episode`] runs a new one.
pub fn spawn_resume(
    dispatcher: Arc<HiveDispatcher>,
    episode_id: String,
) -> tokio::task::JoinHandle<Option<EpisodeReport>> {
    let task: std::pin::Pin<Box<dyn std::future::Future<Output = Option<EpisodeReport>> + Send>> =
        Box::pin(async move {
            let outcome = match dispatcher.resume_desk_message(&episode_id).await {
                Ok(report) => report,
                Err(error) => {
                    tracing::error!(episode = %episode_id, %error, "[hive] the episode could not be resumed");
                    None
                }
            };
            // A resumed episode ends like any other, and a seat can claim
            // work on either side of the park. Without this the claims of an
            // episode that parked once were staged and never acted on: the
            // operator keeps a durable "I own this" row and nothing runs,
            // which is the dead end the carry-on exists to close.
            if let Some(report) = &outcome {
                carry_on_takeovers(&dispatcher, &report.episode_id).await;
            }
            outcome
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
/// episode opens when a message arrives *through the cycle*, which was the one
/// call site of `spawn_episode`. A row appended straight to the journal is the
/// record of a message, not the delivery of one: nobody reads for it, and the
/// teammate it was addressed to never wakes.
///
/// `carry_on_takeovers` has since added a second call site, which opens the
/// claimer's line directly and hand-builds the trigger the chat path derives
/// from a real message. That is where several defects came from, and routing
/// it back through the cycle is the open design question -- `chat_and_emit`
/// lives in the HTTP layer and there is no hive-level seam for it today. The
/// reasoning below is unchanged by that: it is why the *askee* announces, and
/// nothing about the second call site makes pushing at the asker work.
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

/// Writes the job into the claimer's line and says where the episode opens.
///
/// # Why this is a row and not a prompt
///
/// Because a seat is briefed from the journal, not from the trigger. A
/// `Trigger`'s text reaches exactly one place -- [`HiveDispatcher::opening`],
/// which uses it to route who speaks first -- and in a DM [`dm_opening`] pins
/// the owner and returns before that read happens. Everything the seat
/// actually sees is the rows from `trigger.seq` on. So an instruction that is
/// not a row is an instruction nobody receives, which is what a live run
/// found: the episode opened at the claim's sequence, and the brief told the
/// claimer its assignment *was* the claim it had already made.
///
/// # Why `SYSTEM_AUTHOR`, and addressed
///
/// The same author the driver's own notes carry, so the brief renders this
/// the way it renders a nudge -- as the room telling a seat something, which
/// is what it is.
///
/// The audience is the claimer, and that is not a privacy claim: an
/// `aside_audience` is a coordination device between agents, never access
/// control, and an operator reads the row regardless (`server::chat_history`).
/// They see it as the pill that opens the line, which is the honest account
/// of why that line suddenly has work in it. What the address buys is that
/// the episode's other seats are not briefed with an instruction addressed to
/// somebody else.
///
/// # Errors
///
/// Whatever stops the journal accepting the row.
pub async fn brief_takeover(
    events: &dyn EventLog,
    company: &crate::ports::types::CompanyId,
    agent: &str,
    job: &str,
) -> crate::Result<EventSeq> {
    events
        .append(
            company,
            CompanyEvent::AgentReply {
                chat_id: crate::company::blocker_sender::dm_thread(agent),
                agent_id: crate::ports::SYSTEM_AUTHOR.to_owned(),
                text: job.to_owned(),
                steps: Vec::new(),
                outputs: Vec::new(),
                task_id: None,
                parent: None,
                mentions: Vec::new(),
                mention_depth: 0,
                audience: vec![agent.to_owned()],
                // The row opens an episode; it is not one of its utterances.
                // Stamping it would make the episode its own cause, and the
                // console's band reads the first stamped row as the opening
                // turn.
                episode: None,
            },
        )
        .await
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
