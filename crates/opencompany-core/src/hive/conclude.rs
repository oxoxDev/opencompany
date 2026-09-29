//! The closing turn a settled episode routes to one seat.
//!
//! # The hole this fills
//!
//! An episode returns from `run_episode` when every assigned seat has called
//! `complete_episode`, and [`crate::hive::conducted`] then writes
//! `EpisodeCompleted`. Until this module existed that row carried
//! `completed_by: None` and `summary_seq: None` -- "every seat completed, so no
//! one seat closed it" -- and both fields have been plumbed through
//! [`crate::hive::episode_store`] and out of the operator API as `completedBy`
//! and `summarySeq` since long before anything populated them.
//!
//! The gap that left is measurable. Six live runs of one brief through an
//! eight-seat desk and through an operator's direct line: all three DM runs
//! ended with one seat handing the operator an integrated plan across every
//! lane the brief named, and **none** of the three desk runs did. What the
//! operator got instead was whichever seat happened to finish last -- "external
//! copy is final", "paid ad copy is done", four rulings on a copy deck. One
//! desk run wrote eight files and no plan at all.
//!
//! The asymmetry is not a prompt difference. A DM owner is blocked until every
//! `ask` it opened comes back, and the answers come back *to it*, so assembly
//! is the only way for it to finish. On a desk `broadcast` hands work sideways
//! and the episode simply stops when nobody broadcasts again. No seat owns the
//! close, so nobody performs it.
//!
//! # What this does
//!
//! At the settle point -- after the last seat completes, before the closing row
//! is journaled -- ask Jev which seat should assemble the result, seat that one
//! seat for a single turn with `broadcast` withheld, and let its
//! `complete_episode` message be the episode's summary. Its sequence becomes
//! `summary_seq` and its id becomes `completed_by`.
//!
//! Withholding `broadcast` is the load-bearing part. It is the same constraint
//! that makes a DM synthesise every time, and without it the closing seat can
//! route the assembly to somebody else and reopen the room -- which is exactly
//! the branching that cost one live desk run eight waves for four hand-offs.
//! With it, `complete_episode` is the only exit.
//!
//! # Why the host and not the driver
//!
//! `tinyhivemind-driver` has no hosts, and which seats a desk may route to,
//! which verbs this company withholds and what an operator's line means are all
//! OpenCompany's decisions. The driver is right that *it* cannot name a seat
//! that closed the episode. Naming one is a host's job, the way
//! [`crate::hive::takeover`] and [`crate::hive::dispatch`] are.

use std::collections::BTreeMap;

use serde_json::json;
use tinyhivemind_embed::Router;
use tinyhivemind_typesafe::{
    NoulCriteria, Question, SystemOneAnswer, SystemOneRequest, SystemOneResponse,
    SystemOneTransport,
};

use crate::error::{OpenCompanyError, Result};
use crate::hive::graph::DeskHive;
use crate::hive::routing::{EffectiveRouting, RoutingPlanDto};
use crate::ports::types::{EpisodeReason, EventSeq};

/// What the closing seat is told the turn is for.
///
/// Paired with [`crate::hive::host::broadcast_absent_note`] at the call site,
/// for the same reason a DM owner gets both: the driver's brief ends "Hand what
/// is another seat's on with `broadcast`", and that verb is not on this belt.
/// A seat told to hand off, holding no verb that hands off, is the defect
/// `takeover::guest_persona_note` records the cost of from the other side.
///
/// The wording leads with the work rather than the format. A live desk run that
/// was asked for "the plan" produced a plan document written *first*, by the
/// strategist, before any other lane existed -- task ids are monotonic and put
/// it ahead of the copy deck, the landing page and the email plan. Assembly has
/// to be told it comes last, or it gets done first and never revisited.
pub const PERSONA_NOTE: &str = "\n\n## This turn\n\nEvery teammate on this desk has \
     finished and recorded what they found. Nothing is left open, and nobody is waiting on \
     you for a decision.\n\nYour one job is to say what it all adds up to, for the person who \
     asked. Read what the others established, and write the answer to their original \
     request as one piece: the decisions that were actually made, the parts that lock \
     together, what is dated and owned, and anything still genuinely open. Where two \
     teammates assumed different things, say which one holds.\n\nThis is not a status \
     report. Do not list who did what, and do not simply point at the files -- the person \
     reading has the same request they started with and wants it answered. Publish an \
     artifact alongside it when the answer is long enough to be worth opening later.\n\nOne \
     thing you must be straight about. Not every part of this was necessarily written by the \
     teammate who owns it -- on a thin round one teammate sometimes drafts several lanes to \
     get the work moving. Where that happened, say so in a line: which parts their owner \
     produced, and which are a stand-in draft still waiting on that owner. The person reading \
     cannot tell from a well-written answer whether a lane was specialist work or somebody \
     else's guess, and presenting the second as the first is the one way this message can \
     mislead them.\n\nYou cannot hand this on. `complete_episode` is how you finish, and its message is the \
     answer.";

/// One seat's closing turn, once it has run.
#[derive(Clone, Debug)]
pub struct Conclusion {
    /// The seat that assembled the result: `EpisodeCompleted.completed_by`.
    pub seat: String,
    /// Where its message landed: `EpisodeCompleted.summary_seq`.
    ///
    /// `None` when the seat ran but recorded nothing, which a live run shows
    /// happens. The episode still closes, and still names the seat, because a
    /// closing row that pointed at a sequence holding no message would be
    /// worse than one that points nowhere.
    pub summary_seq: Option<u64>,
    /// Turns the closing round ran, to be added to the episode's own.
    ///
    /// # Why these have to be carried back
    ///
    /// The closing turn cannot go through `resume_episode`: that function takes
    /// no starters and derives who is due from the snapshot alone, and a
    /// settled snapshot has nobody due -- it would return having run nothing.
    /// So the round is its own `run`, with its own conductor, and the episode's
    /// `Report` knows nothing about it. Left unadded, every concluded episode
    /// journals a `rounds` and logs a `turns` one short of what happened, and
    /// the console's episode list disagrees with its own transcript.
    pub turns: u64,
    /// Waves the closing round proposed, added the same way and for the same
    /// reason.
    pub waves: u64,
}

/// Whether a settled episode gets a closing turn at all.
///
/// Four reasons it does not, each one a way the turn would be wrong rather
/// than merely redundant:
///
/// * **An operator's direct line already synthesises.** The owner is blocked
///   on its own asks and holds the answers, and
///   `host::dm_persona_note` tells it so; three live runs assembled 3/3
///   without any help from here. A closing turn there would be a second
///   synthesis of a synthesis.
/// * **One seat is already its own conclusion.** With nothing to assemble
///   from anyone else, the seat would be asked to summarise itself.
/// * **Only a clean settle has anything to assemble.** A `RoundCap`,
///   `Timeout`, `Failed` or `MembershipChanged` episode ended with lanes
///   unfinished; asking a seat to say what it adds up to would invite it to
///   present a partial run as a complete answer.
/// * **A closing turn does not get its own closing turn.** Guarded by the
///   caller passing `spoke` from the episode proper.
#[must_use]
pub fn eligible(desk_id: &str, reason: EpisodeReason, spoke: usize) -> bool {
    !desk_id.starts_with(crate::runtime::assignee::DM_PREFIX)
        && spoke > 1
        && matches!(reason, EpisodeReason::CompleteEpisode)
}

/// What Jev is asked, when it is asked who should close.
///
/// Phrased as the desk's own next message rather than as a question about
/// seats, because that is what `route_desk` scores: it picks the seat best
/// placed to *take* a message, and the message here is the assembly job.
#[must_use]
pub fn routing_text(request: &str) -> String {
    format!(
        "Every lane of this request is finished and recorded. What remains is to assemble \
         it: read what each teammate established and answer the original request as one \
         piece, for the person who asked. The request was: {request}"
    )
}

/// One line per seat that finished, for the router to choose against.
///
/// `desk_request`'s `thread_context` is the only way a routing call learns what
/// has already happened, and at **opening** time there is nothing to put in it
/// -- which is why `conducted::opening` passes an empty vector and why this
/// function must not. Choosing who should assemble an episode from seat
/// descriptions alone is choosing blind: a live run where the strategist wrote
/// five lanes and the copywriter one looks, to a router given no context,
/// exactly like a run where the work was spread evenly.
///
/// # The budget, and why it is not per finding
///
/// `jev-1.13` documents 32k tokens for the state plus the longest question, and
/// that is the binding constraint. Against it, thirteen measured live runs put
/// 88 findings at a median of 1,024 characters and a worst whole-episode total
/// of 16,731 -- roughly 4,200 tokens, about an eighth of the window.
///
/// This used to cap each finding at 600 characters, which truncated 78% of them
/// and dropped 48.5% of every character the episode produced, to stay inside a
/// budget nothing was close to spending. Worse, it cut the **last** finding, and
/// the `needed` question asks whether that last message already answers the
/// request as one whole piece -- so the decision was judged on a message cut in
/// half.
///
/// So the cap is on the whole state, sized from the documented window, and the
/// last finding is never touched. `usage.input_tokens` on the response is the
/// authoritative number and `decide` logs it: if these characters convert worse
/// than assumed, that log says so rather than a guess here.
/// `jev-1.13`'s context window: the tokens a request may spend on its state
/// plus its longest question, which is the limit that binds this call.
///
/// (There is a second, looser 64k limit covering the state and *every* question
/// together; it only binds when the questions themselves are large, and ours are
/// two short paragraphs.)
const JEV_CONTEXT_TOKENS: usize = 32_000;

/// Characters per token, for turning that window into something countable
/// without a tokenizer.
///
/// Three rather than the usual four for English prose, so the conversion
/// under-estimates how much text fits rather than over-estimates it.
const CHARS_PER_TOKEN: usize = 3;

/// What the whole findings list may spend: half the window.
///
/// Half rather than all of it, because the findings are not the only thing in the
/// request -- the operator's own words, both questions and the JSON around them
/// share the same budget -- and because `CHARS_PER_TOKEN` is an estimate rather
/// than a count. Thirteen measured live runs never came close either way: the
/// worst whole episode was 16,731 characters, an eighth of this.
const STATE_CHAR_BUDGET: usize = (JEV_CONTEXT_TOKENS / 2) * CHARS_PER_TOKEN;

#[must_use]
pub(crate) fn findings(rows: &[crate::ports::types::StoredEvent], desk_id: &str) -> Vec<String> {
    let mut lines: Vec<String> = rows
        .iter()
        .filter_map(|stored| match &stored.event {
            crate::ports::types::CompanyEvent::AgentReply {
                chat_id,
                agent_id,
                text,
                ..
            } if chat_id == desk_id => Some(format!("{agent_id}: {}", text.trim())),
            _ => None,
        })
        .collect();
    // Oldest first when the budget is short, because the newest findings are
    // what both questions turn on -- and the last one especially, which is the
    // message `needed` judges. A roster large enough to reach this has bigger
    // problems than a lost early line.
    let mut total: usize = lines.iter().map(|line| line.chars().count()).sum();
    while total > STATE_CHAR_BUDGET && lines.len() > 1 {
        total -= lines.remove(0).chars().count();
    }
    lines
}

/// The sequence of the closing turn's own message, or `None` when it wrote
/// none.
///
/// # Why a watermark and not simply "its last reply"
///
/// Because the concluding seat is usually one that already spoke -- the lead,
/// most often -- so its last desk reply is an ordinary deliberation row until
/// the closing turn adds one. `above` is the highest sequence the settled
/// episode already held, so a row only qualifies by being newer than the whole
/// episode that preceded the turn. A turn that recorded nothing then leaves
/// `None`, which is what [`Conclusion::summary_seq`] documents, instead of
/// naming a mid-episode message as the episode's answer.
///
/// Separated from the round it follows so it can be tested on rows rather than
/// on a journal: the rule is the part that was wrong, and the caller only has
/// to read the episode back and hand it over.
#[must_use]
pub fn closing_summary_seq(
    rows: &[crate::ports::types::StoredEvent],
    desk_id: &str,
    seat: &str,
    above: u64,
) -> Option<u64> {
    rows.iter()
        .rev()
        .take_while(|stored| stored.seq.value() > above)
        .find(|stored| match &stored.event {
            crate::ports::types::CompanyEvent::AgentReply {
                chat_id, agent_id, ..
            } => chat_id == desk_id && agent_id == seat,
            _ => false,
        })
        .map(|stored| stored.seq.value())
}

/// Which seat closes the episode.
///
/// Jev picks, and the desk lead is the fallback -- the same fallback
/// `conducted::opening` uses, so a routing outage costs the episode its choice
/// of concluder and never its conclusion. A plan that names nobody, or names
/// several, resolves to its first seat and then to the lead, because a closing
/// turn is one seat's by definition: two seats assembling in parallel would
/// produce two answers and settle nothing.
///
/// `rows` are the episode's own, and they are what makes the choice informed
/// rather than nominal -- see [`findings`].
pub async fn pick_concluder(
    desk: &DeskHive,
    routing: &EffectiveRouting,
    router: Option<&(dyn Router + '_)>,
    request: &str,
    thread_root: Option<EventSeq>,
    rows: &[crate::ports::types::StoredEvent],
) -> Result<String> {
    let lead = desk.lead().ok_or_else(|| {
        OpenCompanyError::Harness(format!("desk `{}` has no seats", desk.desk_id))
    })?;
    // Bounded through `fit_state`, the same way the decision's own state is: this
    // path reaches the same routing model against the same window, so clipping
    // only the request here would have left the findings to overrun it instead.
    let (request, findings) = fit_state(request, &findings(rows, &desk.desk_id));
    let ask = desk.hive.desk_request(
        routing_text(&request),
        findings,
        thread_root.map(|root| tinyhivemind::Sequence(root.value())),
        desk.roster_version,
        routing.policy(),
    );
    let plan = desk
        .hive
        .route_desk(router, None, &ask, None, &lead)
        .await
        .map_err(|error| OpenCompanyError::Harness(error.to_string()))?;
    let picked = RoutingPlanDto::from(&plan)
        .agent_ids()
        .into_iter()
        .next()
        .unwrap_or(lead);
    tracing::debug!(
        desk = %desk.desk_id,
        seat = %picked,
        "[hive] routed the closing turn"
    );
    Ok(picked)
}

/// How confident System One must be that the work is already assembled before
/// the closing turn is skipped.
///
/// # Why the threshold is high and the default is to conclude
///
/// The two mistakes are not the same size. A closing turn that was not needed
/// costs one turn -- roughly twenty-five seconds, measured on the thinnest live
/// run -- and a message that repeats what the last seat said. A closing turn
/// wrongly skipped costs the answer: one live run's final desk row, after
/// fourteen rows of real work, was the system's own "the operator approved your
/// request" echo. Without the closing turn that episode ends telling the
/// operator nothing.
///
/// So skipping needs a positive, confident "already assembled". Six of six live
/// runs needed one, which is also why this gate exists to avoid a case that has
/// not yet been observed rather than one that has.
pub const ALREADY_ASSEMBLED: f64 = 0.8;

/// What the closing decision came back as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// One seat should assemble, and this is it.
    Conclude(String),
    /// The last seat already answered the operator's request as one piece.
    NotNeeded,
}

/// The one System One call behind the closing decision.
///
/// Two questions, evaluated independently in a single request, because
/// `SystemOneRequest.questions` is a map rather than one question: `needed`
/// judges whether assembly is still outstanding, and `who` picks the seat. That
/// is why this reaches the transport directly instead of going through
/// `route_desk`, which asks only the second (see
/// [`crate::hive::jev::jev_transport`]).
/// What the operator's own request may spend of the state budget.
///
/// A quarter, because the findings are the part the questions are actually
/// judged on: `needed` reads the last one and `who` reads them all. A request
/// long enough to reach this is one nobody would have read either.
const REQUEST_CHAR_BUDGET: usize = STATE_CHAR_BUDGET / 4;

/// What a cut says about itself, so the model is not handed a truncated request
/// as though it were the whole one.
const CUT_MARKER: &str = "... (cut to fit the routing call)";

/// Cut `text` to `budget` characters **including** the marker, saying so when it
/// cuts.
///
/// The marker's own length is reserved rather than added afterwards: appending it
/// to a full budget's worth of text overruns the budget by the marker, which is
/// the bug this counts out.
fn clipped(text: &str, budget: usize) -> String {
    if text.chars().count() <= budget {
        return text.to_owned();
    }
    let keep = budget.saturating_sub(CUT_MARKER.chars().count());
    text.chars().take(keep).collect::<String>() + CUT_MARKER
}

/// The request and findings that will fit in one call's state.
///
/// # Why the trimming in `findings` is not enough
///
/// Two ways the state could still overrun the window. `findings` stops trimming
/// at one line, so a single reply longer than the whole budget was returned
/// untouched -- and a seat that pastes a file into its finding is not a strange
/// thing to happen. And `request` was never counted at all, though it sits in the
/// same state.
///
/// So the last resort is a cut rather than a refusal: a decision made on a
/// clipped state still concludes the episode, while a request the transport
/// rejects loses the closing turn altogether. The cut says it happened, so the
/// model is not told a truncated request is the whole one.
#[must_use]
fn fit_state(request: &str, findings: &[String]) -> (String, Vec<String>) {
    let request = clipped(request, REQUEST_CHAR_BUDGET);
    let room = STATE_CHAR_BUDGET.saturating_sub(request.chars().count());
    let mut lines = findings.to_vec();
    let mut total: usize = lines.iter().map(|line| line.chars().count()).sum();
    while total > room && lines.len() > 1 {
        total -= lines.remove(0).chars().count();
    }
    // One finding, still too long: the only thing left to cut is the finding
    // itself.
    if let Some(last) = lines.last_mut()
        && last.chars().count() > room
    {
        *last = clipped(last, room);
    }
    (request, lines)
}

#[must_use]
pub fn closing_questions(request: &str, findings: &[String], seats: &[String]) -> SystemOneRequest {
    let mut questions = BTreeMap::new();
    questions.insert(
        "needed".to_owned(),
        Question::Noul {
            instructions: json!(
                "Every teammate on this desk has finished. Judge whether the operator's \
                 original request has ALREADY been answered as one whole piece by the last \
                 message below -- not whether the work is done, but whether somebody has \
                 drawn it together into an answer the operator can act on. A message that \
                 reports one teammate's own lane, or that says work is finished without \
                 saying what it adds up to, is NOT an answer to the request."
            ),
            criteria: Some(NoulCriteria {
                true_description:
                    "The last message already answers the whole request as one piece.".to_owned(),
                false_description:
                    "The last message covers one lane, reports status, or leaves the request \
                     unanswered as a whole."
                        .to_owned(),
            }),
        },
    );
    questions.insert(
        "who".to_owned(),
        Question::Choice {
            instructions: json!(
                "If this episode still needs drawing together, which one teammate should do \
                 it? Pick the seat best placed to say what the whole thing adds up to for the \
                 operator -- the one whose remit spans the lanes, not simply whoever produced \
                 the most."
            ),
            criteria: seats
                .iter()
                .map(|seat| (seat.clone(), None))
                .collect::<BTreeMap<String, Option<serde_json::Value>>>(),
        },
    );
    let (request, findings) = fit_state(request, findings);
    SystemOneRequest {
        state: json!({ "request": request, "findings": findings }),
        model: JEV_MODEL.to_owned(),
        questions,
    }
}

/// The model alias the closing decision asks for.
///
/// The same alias the router asks for: the proxy resolves it to a concrete
/// `typesafe/jev-*` id and reports that id back, so pinning a version here
/// would age worse than the alias does.
pub const JEV_MODEL: &str = "jev-latest";

/// Read one closing decision out of a System One response.
///
/// `lead` is the answer whenever the response does not clearly say otherwise: a
/// missing `who`, a choice naming a seat that is not on this desk, or an
/// undecodable answer all resolve to the lead rather than to no conclusion,
/// because [`ALREADY_ASSEMBLED`] explains why absence must never mean skip.
#[must_use]
pub fn read_decision(response: &SystemOneResponse, seats: &[String], lead: &str) -> Decision {
    if let Some(SystemOneAnswer::Noul(answer)) = response.answers.get("needed")
        && answer.noul >= ALREADY_ASSEMBLED
    {
        return Decision::NotNeeded;
    }
    let picked = match response.answers.get("who") {
        Some(SystemOneAnswer::Choice(answer))
            if seats.iter().any(|seat| seat == &answer.choice) =>
        {
            answer.choice.clone()
        }
        _ => lead.to_owned(),
    };
    Decision::Conclude(picked)
}

/// Ask System One, in one call, whether this episode needs a closing turn and
/// which seat should take it.
///
/// Every failure path resolves to concluding with `lead`: a transport error, a
/// timeout, an undecodable body. The reason is in [`ALREADY_ASSEMBLED`] -- an
/// unnecessary closing turn costs a turn, a missing one costs the answer -- so
/// the only thing that skips is System One positively and confidently saying the
/// request is already answered whole.
pub async fn decide(
    transport: &dyn SystemOneTransport,
    request: &str,
    findings: &[String],
    seats: &[String],
    lead: &str,
) -> Decision {
    let ask = closing_questions(request, findings, seats);
    match transport.evaluate(&ask).await {
        Ok(response) => {
            let decision = read_decision(&response, seats, lead);
            // `input_tokens` is the only authoritative measure of what this
            // call actually costs against `jev-1.13`'s 32k window; the character
            // budget in `findings` is a proxy for it. Logged so the proxy can be
            // checked against the real number instead of trusted.
            tracing::debug!(
                model = %response.model,
                answers = response.answers.len(),
                input_tokens = response.usage.input_tokens,
                ?decision,
                "[hive] the closing decision came back"
            );
            decision
        }
        Err(error) => {
            // Not `%error`, for the reason `jev::evaluate` does not print it
            // either: `Error::Transport` displays as `status: message`, and that
            // message is the proxy's response body -- which echoes this request,
            // and this request carries the desk's findings.
            tracing::warn!(
                status = ?crate::hive::jev::transport_status(&error),
                kind = crate::hive::jev::failure_kind(&error),
                "[hive] the closing decision failed; this episode concludes with its lead"
            );
            Decision::Conclude(lead.to_owned())
        }
    }
}

#[cfg(test)]
#[path = "conclude_tests.rs"]
mod tests;
