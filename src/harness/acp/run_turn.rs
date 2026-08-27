//! Running a company's turn on an **ACP agent** instead of the embedded
//! OpenHuman harness.
//!
//! ## What this unlocks
//!
//! [`RunTurn`] is the seam between "the company cycle" and "an agent runs a
//! turn". It had exactly one implementation, `HarnessRunTurn`, which drives an
//! in-process OpenHuman agent and therefore needs an inference credential and
//! the whole vendored runtime. A second implementation over ACP serves three
//! things at once:
//!
//! - **A desktop company with no key.** The embedded host runs a turn on the
//!   operator's own `claude-code-acp`, against their existing subscription.
//!   Nothing to configure on first run, which is a materially different product
//!   from one that opens on a credential form.
//! - **Reverse dispatch.** A cloud host hands a task to a runner on someone's
//!   machine; the runner is an ACP agent as far as this is concerned.
//! - **Any other harness.** Codex, and anything else that speaks ACP.
//!
//! ## Why a port rather than an ACP client in here
//!
//! The transport differs per caller — a subprocess over stdio for the desktop,
//! a WebSocket for a runner — and neither belongs in the host crate. The port
//! itself ([`AcpAgent`], [`AcpAgentFactory`], `AcpTurn`, `AcpUpdate`) lives at
//! [`crate::ports::acp`], ungated, because the desktop shell that supplies the
//! stdio implementation deliberately does not enable the `openhuman` feature
//! this module lives behind — see that module's own docs for why. What
//! belongs here is [`AcpRunTurn`]: the adapter that folds whatever an
//! `AcpAgent` reports into this crate's own [`TurnStep`] shape, a genuine
//! `openhuman` dependency the port itself has none of.
//!
//! ## The mapping, and where it is lossy
//!
//! ACP's `session/update` variants and OpenCompany's [`TurnStep`] were designed
//! for different things, and the join is not total:
//!
//! | `sessionUpdate` | becomes |
//! |---|---|
//! | `agent_message_chunk` | appended to the reply |
//! | `agent_thought_chunk` | one coalesced `Thinking` step |
//! | `tool_call` | a `ToolCall` step, `Running` |
//! | `tool_call_update` | that step's status and result |
//! | `plan`, `available_commands_update`, … | dropped |
//!
//! Dropped rather than approximated: a `plan` is a task board, and inventing
//! `TurnStep`s for its entries would put rows on the operator's timeline that
//! no tool call produced.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::Result;
use crate::error::OpenCompanyError;
use crate::harness::TurnOutcome;
pub use crate::ports::acp::{AcpAgent, AcpAgentFactory, AcpTurn, AcpUpdate};
use crate::ports::types::{CompanyId, TurnStep, TurnStepKind, TurnStepStatus};
use crate::runtime::delegation::RunTurn;

/// [`RunTurn`] over an [`AcpAgent`].
pub struct AcpRunTurn {
    agent: Arc<dyn AcpAgent>,
}

impl AcpRunTurn {
    pub fn new(agent: Arc<dyn AcpAgent>) -> Self {
        Self { agent }
    }

    /// The session an agent's turns share.
    ///
    /// Per (company, agent) so two desks do not share a conversation, and
    /// stable across turns so the second question in a thread does not arrive
    /// with no memory of the first.
    fn session_key(company: &CompanyId, agent_id: &str) -> String {
        format!("{}::{agent_id}", company.as_ref())
    }

    async fn run_once(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
    ) -> Result<TurnOutcome> {
        let key = Self::session_key(company, agent_id);
        let turn = self.agent.prompt(company, &key, message).await?;
        Ok(fold(turn))
    }
}

/// How a turn ended, coarsened from ACP's raw `stopReason` string into the
/// shapes this fold treats differently.
///
/// `EndTurn` is the only one that means "the agent said everything it meant
/// to say"; every other value means the reply in hand — if any — is partial,
/// and the fold must say so rather than let it pass for an ordinary answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopKind {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    Other,
}

/// Classifies ACP's raw `stopReason` into [`StopKind`].
///
/// `max_tokens` and `max_turn_requests` stay distinct variants (PR #1880
/// review) even though the note either produces reads similarly: only
/// `max_turn_requests` is this protocol's analog of openhuman's
/// tool-iteration cap — the number of agent/tool round trips in the turn hit
/// a limit — and only that one may set
/// [`TurnOutcome::hit_iteration_cap`](crate::harness::TurnOutcome::hit_iteration_cap).
/// `max_tokens` is a token-generation budget on a single response, unrelated
/// to how many tool calls ran; folding it into the same flag would make a
/// workflow node's `LimitStop { limit: "max_tool_iterations" }` misreport
/// which cap actually stopped the turn.
fn classify_stop_reason(raw: &str) -> StopKind {
    match raw {
        "end_turn" => StopKind::EndTurn,
        "max_tokens" => StopKind::MaxTokens,
        "max_turn_requests" => StopKind::MaxTurnRequests,
        "refusal" => StopKind::Refusal,
        "cancelled" => StopKind::Cancelled,
        _ => StopKind::Other,
    }
}

/// The short, fixed note surfaced when a turn stopped for a reason other than
/// `end_turn`. Landed as its own [`TurnStep`] of kind
/// [`TurnStepKind::Note`], never concatenated into
/// [`TurnOutcome::reply`](crate::harness::TurnOutcome::reply) (PR #1880
/// review) — the reply is the agent's own words, and folding a
/// platform-generated notice into it would leave the operator unable to tell
/// how much of the text the agent actually said. `EndTurn` returns `None`;
/// callers only invoke this for a non-`EndTurn` [`StopKind`].
///
/// Every arm is a **fixed** string — none of them interpolate
/// `raw_stop_reason`, even the `Other` arm, which used to (PR #1880 review).
/// A `stopReason` this fold does not recognise is unvalidated, unbounded text
/// straight off the wire from an external ACP agent — the same class of risk
/// the module doc already calls out for a tool call's `title` — and this
/// `Note` step is not a private log line: it becomes an engine transcript
/// entry (`workflows/caps::transcript_from_steps` maps `Note` to
/// `"agent_message"`), which can be replayed as prior context for later
/// engine reasoning. Interpolating the raw value there would hand an
/// external agent a channel to inject diagnostic text, newlines, or an
/// oversized payload into durable, operator- and agent-visible history. The
/// raw value is still worth knowing for debugging — see `fold`'s bounded
/// `tracing::warn!` right before this is called for `Other`.
fn stop_reason_note(kind: StopKind) -> Option<String> {
    match kind {
        StopKind::EndTurn => None,
        StopKind::MaxTokens => Some("[stopped: hit the token limit before finishing]".to_string()),
        StopKind::MaxTurnRequests => {
            Some("[stopped: hit the tool-call limit before finishing]".to_string())
        }
        StopKind::Refusal => Some("[stopped: the agent declined to continue]".to_string()),
        StopKind::Cancelled => Some("[stopped: cancelled before finishing]".to_string()),
        StopKind::Other => Some("[stopped: unrecognized stop reason]".to_string()),
    }
}

/// Bound on the raw `stopReason` logged for `StopKind::Other` (PR #1880
/// review). A log line is a reasonable place for the diagnostic value —
/// unlike a `TurnStep` or an engine error message, it is not replayed as
/// context and not returned to any client — but it is still unvalidated wire
/// text, so it gets the same UTF-8-safe char-count bound the rest of the crate
/// applies before logging or persisting external content, sized for "enough
/// to recognise the reason, not enough to flood the log".
const UNKNOWN_STOP_REASON_LOG_CHARS: usize = 120;

/// Builds the reply for a turn that produced no `MessageChunk` text.
///
/// Never returns an empty string: a blank reply from a tool-only turn, or one
/// cut short before the agent said anything, would read on the operator's
/// timeline as "the agent had nothing to say" rather than what actually
/// happened. Says only **that** tools ran, never **what** they were (PR #1880
/// review) — a tool call's `title` comes verbatim off the wire from the
/// external ACP agent, with no host-side bounding or redaction (unlike the
/// built-in harness's server-computed step label), so it can carry arbitrary
/// upstream content. The titles themselves are already on the operator's
/// timeline as this turn's [`TurnStep`]s; restating them in a field meant to
/// read as the agent's own words would only duplicate that exposure for no
/// new information.
fn synthesize_empty_reply(steps: &[TurnStep]) -> &'static str {
    let ran_tools = steps.iter().any(|step| step.kind == TurnStepKind::ToolCall);
    if ran_tools {
        "[no reply text — see steps]"
    } else {
        // A clean end with no text and no tool calls. Still never blank.
        "[no reply]"
    }
}

/// Folds a turn's updates into the outcome the company cycle expects.
///
/// Separate from the trait impl so it is testable without an agent, and because
/// this — not the plumbing — is where the semantics live.
pub fn fold(turn: AcpTurn) -> TurnOutcome {
    let mut reply = String::new();
    let mut steps: Vec<TurnStep> = Vec::new();
    // Where each tool call's step landed, so a later update finds it. A tool
    // call that never completes keeps the `Running` status it was created with,
    // which is exactly what that status means.
    let mut positions: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut thinking = false;

    for update in turn.updates {
        match update {
            AcpUpdate::MessageChunk(text) => reply.push_str(&text),
            AcpUpdate::ThoughtChunk => {
                // One step for a run of thoughts, not one per chunk: a model
                // emits these by the hundred, and a timeline of them is noise.
                if !thinking {
                    thinking = true;
                    steps.push(TurnStep {
                        kind: TurnStepKind::Thinking,
                        status: TurnStepStatus::Ok,
                        label: "Thinking".to_string(),
                        ..TurnStep::default()
                    });
                }
            }
            AcpUpdate::ToolCall { id, title } => {
                thinking = false;
                positions.insert(id, steps.len());
                steps.push(TurnStep {
                    kind: TurnStepKind::ToolCall,
                    status: TurnStepStatus::Running,
                    label: title,
                    ..TurnStep::default()
                });
            }
            AcpUpdate::ToolCallUpdate { id, status, result } => {
                thinking = false;
                let Some(&index) = positions.get(&id) else {
                    // An update for a call we never saw start. Dropped rather
                    // than synthesised: a step with no label is worse on a
                    // timeline than no step.
                    continue;
                };
                let step = &mut steps[index];
                step.status = match status.as_str() {
                    "completed" => TurnStepStatus::Ok,
                    "failed" => TurnStepStatus::Error,
                    // `pending` and `in_progress` both mean "not done".
                    _ => TurnStepStatus::Running,
                };
                if result.is_some() {
                    step.result = result;
                }
            }
        }
    }

    // Issue #1853: `stop_reason` is ACP's own signal for how the turn ended,
    // and the old fold never read it — a tool-only turn folded to `reply ==
    // ""`, and a max_tokens/refusal/cancelled turn folded identically to a
    // clean `end_turn`, indistinguishable to the operator from an ordinary
    // answer.
    let kind = classify_stop_reason(&turn.stop_reason);

    if kind == StopKind::Other {
        // Diagnostic only (PR #1880 review) — never the source for anything
        // durable. `stop_reason_note`'s `Other` arm and `abnormal_stop` below
        // both deliberately drop the raw value; this bounded copy is the only
        // place it survives, and only in a log line, char-capped so a
        // malformed/oversized `stopReason` cannot flood it either.
        let bounded: String = turn
            .stop_reason
            .chars()
            .take(UNKNOWN_STOP_REASON_LOG_CHARS)
            .collect();
        tracing::warn!(
            stop_reason = %bounded,
            "[harness::acp] unrecognized ACP stop reason"
        );
    }

    if reply.trim().is_empty() {
        reply = synthesize_empty_reply(&steps).to_string();
    }

    // PR #1880 review: the stop-reason notice is platform-generated, not
    // agent-authored, so it lands as its own step rather than blurring into
    // `reply` above.
    let note = stop_reason_note(kind);
    if let Some(note) = &note {
        steps.push(TurnStep {
            kind: TurnStepKind::Note,
            status: TurnStepStatus::Ok,
            label: note.clone(),
            ..TurnStep::default()
        });
    }

    TurnOutcome {
        reply,
        steps,
        // A max_turn_requests stop is exactly the shape issue #926 describes:
        // the tool loop was cut off by a budget rather than the model
        // choosing to stop. `max_tokens` is a different budget — a single
        // response's token limit — and is deliberately excluded (PR #1880
        // review): downstream (`workflows/caps`) reports this flag as
        // "stopped at the max_tool_iterations cap", which would misdescribe a
        // token-limited stop. Every other `StopKind` is not a cap —
        // `Refusal`/`Cancelled`/`Other` are surfaced as a step note instead,
        // and `EndTurn` needs no flag at all.
        hit_iteration_cap: matches!(kind, StopKind::MaxTurnRequests),
        // PR #1880 review (second round): `max_tokens` is excluded from
        // `hit_iteration_cap` above for a real reason (a different budget,
        // a different downstream message), but that exclusion used to leave
        // it with NO cap signal at all — only the step note, which
        // `HarnessAgentRunner` never read. Same shape as `hit_iteration_cap`,
        // a distinct field rather than another reading of it, because the
        // two need different `limit` names downstream.
        token_limited: matches!(kind, StopKind::MaxTokens),
        // PR #1880 review: `Refusal`/`Cancelled`/`Other` are not a resumable
        // cap either — there is no checkpoint to continue from, unlike
        // `hit_iteration_cap` above — so `HarnessAgentRunner` must not settle
        // these as a plain `Succeeded`/`StopReason::Finished` the way it used
        // to when `hit_iteration_cap == false` was the only signal it read.
        // Reuses `note`'s text: both sinks want the same short, fixed,
        // non-wire-derived notice, and `stop_reason_note`'s `Other` arm is
        // already the one place that keeps the raw `stopReason` out of it.
        abnormal_stop: matches!(
            kind,
            StopKind::Refusal | StopKind::Cancelled | StopKind::Other
        )
        .then(|| note.clone().unwrap_or_default()),
        // Issue #1032: nor is there a spend halt to report. The stop hooks are
        // installed around THIS crate's `agent.turn`, and an ACP turn does not
        // run through it — the external process bills and stops on its own
        // terms, which this side neither arms nor observes.
        halted_for_spend: None,
    }
}

#[async_trait]
impl RunTurn for AcpRunTurn {
    async fn run(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
        _chat_id: Option<&str>,
    ) -> Result<TurnOutcome> {
        self.run_once(company, agent_id, message).await
    }

    async fn run_steered(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
        control: &crate::company::steer::SteerControl,
        _chat_id: Option<&str>,
        _run_sink: Option<Arc<crate::harness::run_trace::RunTraceSink>>,
    ) -> Result<TurnOutcome> {
        self.steered(company, agent_id, message, control).await
    }

    async fn run_steered_background(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
        control: &crate::company::steer::SteerControl,
        _run_sink: Option<Arc<crate::harness::run_trace::RunTraceSink>>,
    ) -> Result<TurnOutcome> {
        self.steered(company, agent_id, message, control).await
    }
}

impl AcpRunTurn {
    /// How long a cancelled turn may keep running before the waiter gives up.
    ///
    /// Cancellation in ACP is cooperative: `session/cancel` is a notification,
    /// and a harness inside a long tool call only notices when that call
    /// returns. So the post-cancel wait stays, but it is bounded — a cancelled
    /// turn that has not drained its output within this window is abandoned,
    /// not waited on forever. The window is generous enough for a slow tool
    /// call to finish and its updates to flush.
    const CANCEL_GRACE: Duration = Duration::from_secs(30);

    /// Bound on a single `session/cancel` round trip. A cancel that never
    /// answers — a wedged host, a dead subprocess — must not pin the steered
    /// turn forever; the grace wait is what actually reaps a turn that ignores
    /// the cancel, and this bound just keeps the attempt to tell it from
    /// blocking that.
    const CANCEL_RPC_TIMEOUT: Duration = Duration::from_secs(5);

    /// A turn that can be cancelled while it runs.
    ///
    /// The turn and the steer check race each other. A cancel forwards
    /// `session/cancel` and then **keeps waiting** rather than abandoning the
    /// turn: ACP cancellation is cooperative, the agent still answers with
    /// `stopReason: "cancelled"`, and dropping the future here would leave a
    /// harness mid-tool-call with nothing reading its output. That wait is
    /// bounded by [`Self::CANCEL_GRACE`]: a turn that ignores the cancel past
    /// the grace window is abandoned with an error, not awaited forever.
    async fn steered(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
        control: &crate::company::steer::SteerControl,
    ) -> Result<TurnOutcome> {
        self.steered_with_grace(
            company,
            agent_id,
            message,
            control,
            Self::CANCEL_GRACE,
            Self::CANCEL_RPC_TIMEOUT,
        )
        .await
    }

    /// [`Self::steered`] with both timing bounds made explicit — the post-cancel
    /// grace and the per-cancel-RPC bound — so the tests can expire them in
    /// milliseconds rather than waiting out the real windows.
    async fn steered_with_grace(
        &self,
        company: &CompanyId,
        agent_id: &str,
        message: &str,
        control: &crate::company::steer::SteerControl,
        grace: Duration,
        cancel_rpc: Duration,
    ) -> Result<TurnOutcome> {
        let key = Self::session_key(company, agent_id);
        let turn = self.agent.prompt(company, &key, message);
        tokio::pin!(turn);

        loop {
            tokio::select! {
                outcome = &mut turn => return Ok(fold(outcome?)),
                () = tokio::time::sleep(std::time::Duration::from_millis(250)) => {
                    // `pending`, not `take`: the disposition site after the turn
                    // reads the action to decide what happens to the card, and
                    // consuming it here would leave it with nothing to read.
                    if control.pending().is_some() {
                        // Advisory. Told, then waited for — see above. The RPC
                        // itself is bounded so a cancel that never answers (a
                        // wedged host, a dead subprocess) cannot block the turn;
                        // both outcomes below are logged and the flow continues.
                        match tokio::time::timeout(cancel_rpc, self.agent.cancel(company, &key))
                            .await
                        {
                            Ok(Ok(())) => {}
                            Ok(Err(err)) => {
                                tracing::warn!(%err, "[harness::acp] cancel failed for session {key}");
                            }
                            Err(_elapsed) => {
                                tracing::warn!("[harness::acp] cancel timed out for session {key}");
                            }
                        }
                        match tokio::time::timeout(grace, &mut turn).await {
                            Ok(outcome) => return Ok(fold(outcome?)),
                            Err(_elapsed) => {
                                // The agent ignored the cancel past the grace
                                // window. The port has no abort/reset seam —
                                // `cancel` is all there is — so the best this
                                // side can do is nudge once more and drop the
                                // turn. Dropping the future ends the reader on
                                // this session; the agent's own `session/cancel`
                                // handling (or the host reaping the subprocess)
                                // is the recovery path for the work it still
                                // holds. A later turn on the same key opens a
                                // fresh `session/prompt`, which the agent treats
                                // as a new turn rather than an overlap. The
                                // nudge is bounded the same way: it is best
                                // effort, and the abandonment is the point.
                                let _ = tokio::time::timeout(
                                    cancel_rpc,
                                    self.agent.cancel(company, &key),
                                )
                                .await;
                                return Err(OpenCompanyError::Harness(format!(
                                    "the agent did not stop within {}s of a cancel; \
                                     abandoning the turn",
                                    grace.as_secs()
                                )));
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn turn(updates: Vec<AcpUpdate>) -> AcpTurn {
        AcpTurn {
            updates,
            stop_reason: "end_turn".to_string(),
        }
    }

    fn turn_with_stop_reason(updates: Vec<AcpUpdate>, stop_reason: &str) -> AcpTurn {
        AcpTurn {
            updates,
            stop_reason: stop_reason.to_string(),
        }
    }

    #[test]
    fn a_max_turn_requests_stop_is_the_tool_step_cap() {
        // Issue #1853 established that a stop must not fold identically to a
        // clean end_turn — the operator needs a cap signal. PR #1880 review:
        // `max_turn_requests` is ACP's analog of openhuman's tool-iteration
        // cap, and is the only stop reason that may set `hit_iteration_cap`,
        // because `workflows/caps` reports that flag as "stopped at the
        // max_tool_iterations cap".
        let outcome = fold(turn_with_stop_reason(vec![], "max_turn_requests"));
        assert!(
            outcome.hit_iteration_cap,
            "max_turn_requests is the tool-step cap, not a clean finish"
        );
        assert!(
            !outcome.reply.trim().is_empty(),
            "a capped turn must say so, not fold to a blank reply"
        );
    }

    #[test]
    fn a_max_tokens_stop_is_not_the_tool_step_cap() {
        // A token-generation budget on a single response is a different cap
        // than the tool-iteration one (PR #1880 review) — conflating them
        // would make a workflow node's `LimitStop{"max_tool_iterations"}`
        // misreport which cap actually stopped the turn.
        let outcome = fold(turn_with_stop_reason(vec![], "max_tokens"));
        assert!(
            !outcome.hit_iteration_cap,
            "a max_tokens stop is not the tool-iteration cap"
        );
        assert!(
            !outcome.reply.trim().is_empty(),
            "a capped turn must say so, not fold to a blank reply"
        );
        // PR #1880 review, second round: excluding max_tokens from
        // hit_iteration_cap must not leave it with NO cap signal at all —
        // it gets its own, distinct field instead.
        assert!(
            outcome.token_limited,
            "a max_tokens stop must set token_limited even though hit_iteration_cap stays false"
        );
        assert_eq!(
            outcome.abnormal_stop, None,
            "a token-limited stop is a real, partial checkpoint like the tool cap — not the \
             hard-fail abnormal_stop path"
        );
    }

    #[test]
    fn only_max_tokens_sets_token_limited() {
        // token_limited must not leak onto any other StopKind — in particular
        // not onto max_turn_requests (which has its own hit_iteration_cap
        // signal) or a clean end_turn.
        assert!(fold(turn_with_stop_reason(vec![], "max_tokens")).token_limited);
        for other in [
            "end_turn",
            "max_turn_requests",
            "refusal",
            "cancelled",
            "unknown_reason",
        ] {
            assert!(
                !fold(turn_with_stop_reason(vec![], other)).token_limited,
                "stop_reason {other:?} must not set token_limited"
            );
        }
    }

    #[test]
    fn a_tool_only_turn_gets_a_generic_reply_not_raw_tool_titles() {
        // No MessageChunk at all — the agent's entire turn was tool calls.
        // PR #1880 review: the reply must not copy the tools' raw ACP titles
        // — unlike the built-in harness's step label, a title comes straight
        // off the wire with no host-side bounding, and the timeline (already
        // carrying each ToolCall step's own title) is where that content
        // belongs, not a field meant to read as the agent's own words.
        let outcome = fold(turn(vec![
            AcpUpdate::ToolCall {
                id: "t1".into(),
                title: "Read".into(),
            },
            AcpUpdate::ToolCallUpdate {
                id: "t1".into(),
                status: "completed".into(),
                result: Some("2.4 kB".into()),
            },
            AcpUpdate::ToolCall {
                id: "t2".into(),
                title: "Write".into(),
            },
        ]));
        assert_eq!(outcome.reply, "[no reply text — see steps]");
        assert_eq!(outcome.steps[0].label, "Read");
        assert_eq!(outcome.steps[1].label, "Write");
        // A clean end_turn needs no stop-reason note on top of the synthesis.
        assert!(!outcome.reply.contains("[stopped"));
    }

    #[test]
    fn a_refusal_is_surfaced_as_a_note_step_and_the_cap_stays_false() {
        // The agent had prose to say, then declined to continue. The note
        // must land regardless — a refusal is not a clean finish even when
        // there is a reply to read. PR #1880 review: it lands as a `Note`
        // step, not appended onto the agent's own reply text.
        let outcome = fold(turn_with_stop_reason(
            vec![AcpUpdate::MessageChunk("I can't help with that.".into())],
            "refusal",
        ));
        assert_eq!(
            outcome.reply, "I can't help with that.",
            "the agent's own prose is kept verbatim, with nothing appended"
        );
        assert!(
            outcome.steps.iter().any(|s| s.kind == TurnStepKind::Note
                && s.label == "[stopped: the agent declined to continue]"),
            "the refusal must be surfaced as a step, not silently swallowed: {:?}",
            outcome.steps
        );
        assert!(
            !outcome.hit_iteration_cap,
            "a refusal is not an iteration-cap pause"
        );
        // PR #1880 review: `hit_iteration_cap == false` used to be the only
        // signal `HarnessAgentRunner` read, so a refusal settled a workflow
        // node `Succeeded`/`Finished` — indistinguishable from the agent
        // having actually answered. This is the outcome-level fix, not just
        // the note above: see `workflows::caps::mod::test::an_abnormal_acp_stop_fails_the_workflow_node`
        // for the assertion that it actually stops the graph.
        assert_eq!(
            outcome.abnormal_stop.as_deref(),
            Some("[stopped: the agent declined to continue]"),
            "a refusal must carry a distinct abnormal-stop outcome, not just a note"
        );
    }

    #[test]
    fn a_cancelled_turn_also_carries_an_abnormal_stop() {
        // Same shape as refusal, different trigger: an operator-initiated (or
        // upstream) cancel is just as much "not a resumable cap, not a clean
        // finish" as a refusal is.
        let outcome = fold(turn_with_stop_reason(vec![], "cancelled"));
        assert_eq!(
            outcome.abnormal_stop.as_deref(),
            Some("[stopped: cancelled before finishing]")
        );
        assert!(!outcome.hit_iteration_cap);
    }

    #[test]
    fn an_end_turn_reply_is_left_verbatim() {
        // The ordinary case — and the one the pre-existing seam test already
        // pins — must not gain a note or any other alteration just because
        // this fold now reads `stop_reason`.
        let outcome = fold(turn(vec![AcpUpdate::MessageChunk("all done".into())]));
        assert_eq!(outcome.reply, "all done");
        assert!(!outcome.hit_iteration_cap);
        assert_eq!(
            outcome.abnormal_stop, None,
            "a clean end_turn is not an abnormal stop"
        );
    }

    #[test]
    fn a_max_turn_requests_stop_is_a_cap_not_an_abnormal_stop() {
        // The cap path (issue #926 / #1880's `hit_iteration_cap` split) and
        // the abnormal-stop path (this PR's review) are deliberately
        // disjoint: a capped turn has a real, resumable checkpoint, which is
        // exactly what `abnormal_stop` says there is none of.
        let outcome = fold(turn_with_stop_reason(vec![], "max_turn_requests"));
        assert!(outcome.hit_iteration_cap);
        assert_eq!(
            outcome.abnormal_stop, None,
            "the cap flag already covers this stop; abnormal_stop must stay None"
        );
    }

    #[test]
    fn an_unrecognized_stop_reason_is_surfaced_not_swallowed() {
        // A stop_reason this fold has never heard of must not silently pass
        // for a clean end_turn — it is carried into a note step so the
        // operator (and whoever reads the ticket) can see the turn stopped
        // abnormally.
        //
        // PR #1880 review: the raw string itself must NOT appear — an
        // unrecognized `stopReason` is unvalidated, unbounded text straight
        // off the wire from an external ACP agent, and this note step is not
        // a private log line: `workflows/caps::transcript_from_steps` maps a
        // `Note` step to `"agent_message"` in the engine transcript, which
        // can be replayed as prior context for later engine reasoning. The
        // fixed notice below carries the abnormal-stop signal without
        // reopening that channel.
        let raw = "some_new_reason_acp_added_later__with_diagnostic_junk_🔥";
        let outcome = fold(turn_with_stop_reason(
            vec![AcpUpdate::MessageChunk("partial thought".into())],
            raw,
        ));
        assert_eq!(outcome.reply, "partial thought");
        assert!(
            outcome.steps.iter().any(|s| s.kind == TurnStepKind::Note
                && s.label == "[stopped: unrecognized stop reason]"),
            "an unrecognized stop must still be surfaced as a step: {:?}",
            outcome.steps
        );
        assert!(
            outcome.steps.iter().all(|s| !s.label.contains(raw)),
            "the raw wire value must never appear in a persisted step: {:?}",
            outcome.steps
        );
        assert!(!outcome.hit_iteration_cap);
        assert_eq!(
            outcome.abnormal_stop.as_deref(),
            Some("[stopped: unrecognized stop reason]"),
            "an unrecognized stop must carry a distinct abnormal-stop outcome, not just a note"
        );
        assert!(
            !outcome
                .abnormal_stop
                .as_deref()
                .unwrap_or_default()
                .contains(raw),
            "the raw wire value must never appear in the abnormal-stop message either"
        );
    }

    #[test]
    fn classify_stop_reason_maps_the_known_shapes() {
        assert_eq!(classify_stop_reason("end_turn"), StopKind::EndTurn);
        assert_eq!(classify_stop_reason("max_tokens"), StopKind::MaxTokens);
        assert_eq!(
            classify_stop_reason("max_turn_requests"),
            StopKind::MaxTurnRequests
        );
        assert_eq!(classify_stop_reason("refusal"), StopKind::Refusal);
        assert_eq!(classify_stop_reason("cancelled"), StopKind::Cancelled);
        assert_eq!(classify_stop_reason("anything_else"), StopKind::Other);
        assert_eq!(classify_stop_reason(""), StopKind::Other);
    }

    #[test]
    fn message_chunks_concatenate_in_order() {
        // ACP streams a reply in pieces; the outcome carries one string.
        let outcome = fold(turn(vec![
            AcpUpdate::MessageChunk("Hello".into()),
            AcpUpdate::MessageChunk(", ".into()),
            AcpUpdate::MessageChunk("world".into()),
        ]));
        assert_eq!(outcome.reply, "Hello, world");
        assert!(outcome.steps.is_empty(), "text alone produces no steps");
    }

    #[test]
    fn a_run_of_thoughts_becomes_one_step() {
        // A model emits these by the hundred. One step per chunk would bury the
        // tool calls an operator is actually reading the timeline for.
        let outcome = fold(turn(vec![
            AcpUpdate::ThoughtChunk,
            AcpUpdate::ThoughtChunk,
            AcpUpdate::ThoughtChunk,
        ]));
        assert_eq!(outcome.steps.len(), 1);
        assert_eq!(outcome.steps[0].kind, TurnStepKind::Thinking);
        assert_eq!(outcome.steps[0].label, "Thinking");
    }

    #[test]
    fn thinking_resumes_as_a_new_step_after_a_tool_call() {
        // Two separate bouts of reasoning either side of a call are two steps —
        // coalescing them would put the thinking in the wrong order relative to
        // the work it bracketed.
        let outcome = fold(turn(vec![
            AcpUpdate::ThoughtChunk,
            AcpUpdate::ToolCall {
                id: "t1".into(),
                title: "Read".into(),
            },
            AcpUpdate::ThoughtChunk,
        ]));
        let kinds: Vec<_> = outcome.steps.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TurnStepKind::Thinking,
                TurnStepKind::ToolCall,
                TurnStepKind::Thinking
            ]
        );
    }

    #[test]
    fn a_tool_call_takes_its_final_status_and_result() {
        let outcome = fold(turn(vec![
            AcpUpdate::ToolCall {
                id: "t1".into(),
                title: "Read a file".into(),
            },
            AcpUpdate::ToolCallUpdate {
                id: "t1".into(),
                status: "completed".into(),
                result: Some("2.4 kB".into()),
            },
        ]));
        assert_eq!(outcome.steps.len(), 1, "the update amends, never appends");
        assert_eq!(outcome.steps[0].label, "Read a file");
        assert_eq!(outcome.steps[0].status, TurnStepStatus::Ok);
        assert_eq!(outcome.steps[0].result.as_deref(), Some("2.4 kB"));
    }

    #[test]
    fn a_failed_tool_call_is_an_error_step() {
        let outcome = fold(turn(vec![
            AcpUpdate::ToolCall {
                id: "t1".into(),
                title: "Write".into(),
            },
            AcpUpdate::ToolCallUpdate {
                id: "t1".into(),
                status: "failed".into(),
                result: Some("permission denied".into()),
            },
        ]));
        assert_eq!(outcome.steps[0].status, TurnStepStatus::Error);
        assert!(outcome.steps[0].status.is_failure());
    }

    #[test]
    fn a_tool_call_that_never_completes_stays_running() {
        // Exactly what `Running` means: started, no completion seen by the end
        // of the turn. Marking it `Ok` would report work that never finished as
        // having succeeded.
        let outcome = fold(turn(vec![AcpUpdate::ToolCall {
            id: "t1".into(),
            title: "Long thing".into(),
        }]));
        assert_eq!(outcome.steps[0].status, TurnStepStatus::Running);
    }

    #[test]
    fn several_tool_calls_are_amended_independently() {
        // Interleaved calls are ordinary — an agent starts two and they finish
        // out of order. Each update has to find its own step.
        let outcome = fold(turn(vec![
            AcpUpdate::ToolCall {
                id: "a".into(),
                title: "First".into(),
            },
            AcpUpdate::ToolCall {
                id: "b".into(),
                title: "Second".into(),
            },
            AcpUpdate::ToolCallUpdate {
                id: "b".into(),
                status: "completed".into(),
                result: None,
            },
            AcpUpdate::ToolCallUpdate {
                id: "a".into(),
                status: "failed".into(),
                result: None,
            },
        ]));
        assert_eq!(outcome.steps.len(), 2);
        assert_eq!(outcome.steps[0].label, "First");
        assert_eq!(outcome.steps[0].status, TurnStepStatus::Error);
        assert_eq!(outcome.steps[1].label, "Second");
        assert_eq!(outcome.steps[1].status, TurnStepStatus::Ok);
    }

    #[test]
    fn an_update_for_an_unknown_call_is_dropped_rather_than_invented() {
        // A step with no label is worse on a timeline than no step at all.
        let outcome = fold(turn(vec![AcpUpdate::ToolCallUpdate {
            id: "ghost".into(),
            status: "completed".into(),
            result: Some("x".into()),
        }]));
        assert!(outcome.steps.is_empty());
    }

    /// An agent that answers from a script, so the trait impl can be driven.
    ///
    /// `hang` makes `prompt` never resolve (the grace-expiry path) and
    /// `cancel_fails` makes `cancel` error (the logged-failure path). `cancels`
    /// counts cancel calls so a test can assert the grace path nudged twice.
    ///
    /// `hold_for_cancel` makes `prompt` wait until the first `cancel` arrives —
    /// the shape of a turn that is mid-tool-call when the operator steers, which
    /// is exactly the window the advisory cancel exists for. Without the gate a
    /// prompt that resolves immediately exits the loop before the steer check
    /// ever runs, and the cancel path goes unexercised. `cancel_hangs` makes
    /// `cancel` never answer (the bounded-RPC path).
    struct Scripted {
        turn: AcpTurn,
        hang: bool,
        hold_for_cancel: bool,
        cancel_hangs: bool,
        cancel_fails: bool,
        cancels: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        cancel_started: tokio::sync::Notify,
    }

    impl Scripted {
        fn answering(updates: Vec<AcpUpdate>) -> Self {
            Self {
                turn: AcpTurn {
                    updates,
                    stop_reason: "end_turn".into(),
                },
                hang: false,
                hold_for_cancel: false,
                cancel_hangs: false,
                cancel_fails: false,
                cancels: Default::default(),
                cancel_started: tokio::sync::Notify::new(),
            }
        }
    }

    #[async_trait]
    impl AcpAgent for Scripted {
        async fn prompt(&self, _c: &CompanyId, _k: &str, _m: &str) -> Result<AcpTurn> {
            if self.hang {
                std::future::pending::<()>().await;
            }
            if self.hold_for_cancel {
                self.cancel_started.notified().await;
            }
            Ok(self.turn.clone())
        }
        async fn cancel(&self, _c: &CompanyId, _k: &str) -> Result<()> {
            self.cancels
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.cancel_started.notify_waiters();
            if self.cancel_hangs {
                std::future::pending::<()>().await;
            }
            if self.cancel_fails {
                return Err(OpenCompanyError::Harness("cancel rejected".into()));
            }
            Ok(())
        }
    }

    /// The claim the whole slice rests on: this is usable anywhere the
    /// OpenHuman implementation is.
    ///
    /// Driven through `&dyn RunTurn` rather than through the concrete type,
    /// because that is how the company cycle holds it (`DelegationRunner` takes
    /// `&'a dyn RunTurn`). A type that satisfied the trait but was not
    /// object-safe would compile here and fail at the one site that matters.
    #[tokio::test]
    async fn it_is_usable_through_the_run_turn_seam() {
        let agent = Arc::new(Scripted::answering(vec![
            AcpUpdate::ThoughtChunk,
            AcpUpdate::ToolCall {
                id: "t1".into(),
                title: "Read".into(),
            },
            AcpUpdate::ToolCallUpdate {
                id: "t1".into(),
                status: "completed".into(),
                result: Some("4 items".into()),
            },
            AcpUpdate::MessageChunk("all done".into()),
        ]));
        let run_turn: &dyn RunTurn = &AcpRunTurn::new(agent);

        let outcome = run_turn
            .run(&CompanyId::new("acme"), "ceo", "go", None)
            .await
            .expect("a turn runs");

        assert_eq!(outcome.reply, "all done");
        assert_eq!(outcome.steps.len(), 2);
        assert_eq!(outcome.steps[1].status, TurnStepStatus::Ok);
        assert_eq!(outcome.steps[1].result.as_deref(), Some("4 items"));
    }

    #[tokio::test]
    async fn a_steered_turn_still_returns_an_outcome() {
        // Cancellation in ACP is cooperative: the agent still answers, with
        // `stopReason: "cancelled"`. Abandoning the future on a steer would
        // leave a harness mid-tool-call with nothing reading its output, so the
        // contract is that a steered turn still produces an outcome.
        let agent = Arc::new(Scripted::answering(vec![AcpUpdate::MessageChunk(
            "partial".into(),
        )]));
        let run_turn: &dyn RunTurn = &AcpRunTurn::new(agent);
        let control = crate::company::steer::SteerControl::new();
        control.request(crate::company::steer::SteerAction::Cancel);

        let outcome = run_turn
            .run_steered(&CompanyId::new("acme"), "ceo", "go", &control, None, None)
            .await
            .expect("a steered turn still answers");
        assert_eq!(outcome.reply, "partial");
        // The pending action survives for the disposition site to read, which
        // is what decides where the card lands.
        assert!(
            control.pending().is_some(),
            "the steer must not be consumed here"
        );
    }

    #[tokio::test]
    async fn a_failed_cancel_is_logged_and_the_turn_still_drains() {
        // `session/cancel` can fail (the subprocess is mid-shutdown, say), but
        // that must not turn a cancelled turn into a failure of its own: the
        // cancel is advisory, the error is logged, and the turn still answers.
        // The prompt holds until the cancel arrives so the steer check is
        // actually reached — a prompt that resolves first would exit the loop
        // and leave the cancel path unexercised.
        let mut agent = Scripted::answering(vec![AcpUpdate::MessageChunk("done".into())]);
        agent.cancel_fails = true;
        agent.hold_for_cancel = true;
        let cancels = agent.cancels.clone();
        let agent = Arc::new(agent);
        let run_turn: &dyn RunTurn = &AcpRunTurn::new(agent);
        let control = crate::company::steer::SteerControl::new();
        control.request(crate::company::steer::SteerAction::Cancel);

        let outcome = run_turn
            .run_steered(&CompanyId::new("acme"), "ceo", "go", &control, None, None)
            .await
            .expect("a failed cancel still ends in a turn");
        assert_eq!(outcome.reply, "done");
        assert_eq!(
            cancels.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the failed cancel was still attempted exactly once"
        );
    }

    #[tokio::test]
    async fn a_hung_cancel_rpc_does_not_block_the_turn() {
        // A cancellation RPC that never answers — a wedged host, a dead
        // subprocess — must not pin the steered turn forever. Both cancel calls
        // are bounded, so the turn still settles on the grace schedule.
        let mut agent = Scripted::answering(vec![AcpUpdate::MessageChunk("done".into())]);
        agent.cancel_hangs = true;
        agent.hold_for_cancel = true;
        let agent = Arc::new(agent);
        let run_turn = AcpRunTurn::new(agent);
        let control = crate::company::steer::SteerControl::new();
        control.request(crate::company::steer::SteerAction::Cancel);

        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            run_turn.steered_with_grace(
                &CompanyId::new("acme"),
                "ceo",
                "go",
                &control,
                Duration::from_millis(20), // post-cancel grace
                Duration::from_millis(50), // cancel RPC bound
            ),
        )
        .await
        .expect("the turn settles despite a hung cancel RPC")
        .expect("the release of the prompt lets the turn answer");

        assert_eq!(outcome.reply, "done");
    }

    #[tokio::test]
    async fn a_cancelled_turn_that_ignores_the_cancel_is_abandoned() {
        // A harness inside a tool call that never returns is the one case the
        // cooperative wait must not honour: past the grace window the waiter
        // drops the turn with an error, and nudges `cancel` once more on the
        // way out — the only drain lever the port exposes.
        let agent = Arc::new(Scripted {
            turn: AcpTurn {
                updates: vec![],
                stop_reason: "end_turn".into(),
            },
            hang: true,
            hold_for_cancel: false,
            cancel_hangs: false,
            cancel_fails: false,
            cancels: Default::default(),
            cancel_started: tokio::sync::Notify::new(),
        });
        let cancels = agent.cancels.clone();
        let run_turn = AcpRunTurn::new(agent);
        let control = crate::company::steer::SteerControl::new();
        control.request(crate::company::steer::SteerAction::Cancel);

        let err = run_turn
            .steered_with_grace(
                &CompanyId::new("acme"),
                "ceo",
                "go",
                &control,
                Duration::from_millis(20),
                Duration::from_millis(50),
            )
            .await
            .expect_err("a hung turn is abandoned, not awaited");
        assert!(
            format!("{err}").contains("abandoning the turn"),
            "the error names the abandonment: {err}"
        );
        assert_eq!(
            cancels.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "one cancel on the steer, one best-effort nudge on the way out"
        );
    }

    #[test]
    fn a_session_key_separates_agents_and_companies() {
        // Two desks sharing a session would share a conversation, and one
        // company's turn would carry another's context.
        let acme = CompanyId::new("acme");
        let globex = CompanyId::new("globex");
        assert_ne!(
            AcpRunTurn::session_key(&acme, "ceo"),
            AcpRunTurn::session_key(&acme, "cto")
        );
        assert_ne!(
            AcpRunTurn::session_key(&acme, "ceo"),
            AcpRunTurn::session_key(&globex, "ceo")
        );
        // Stable across turns, or the second question in a thread arrives with
        // no memory of the first.
        assert_eq!(
            AcpRunTurn::session_key(&acme, "ceo"),
            AcpRunTurn::session_key(&acme, "ceo")
        );
    }
}
