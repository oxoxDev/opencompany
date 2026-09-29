use super::tests_core2::*;

/// The responder's own pause survives the relay turn replacing its text —
/// the budget-pause analogue of
/// [`a_responders_own_halt_survives_the_relay_replacing_the_reply`].
#[tokio::test]
async fn a_responders_own_budget_pause_survives_the_relay_replacing_the_reply() {
    let fx = Fixture::nested();
    let turns = ScriptedTurns::new(
        &fx,
        vec![
            // The orchestrator runs out of credits AND still manages to hand
            // off — the pause is on the turn that queued the delegation.
            Turn {
                reply: "handing it to engineering".to_string(),
                tool_pushes: vec![handoff("ship the API")],
                budget_paused: Some(crate::harness::BudgetPause {
                    agent: "chief".to_string(),
                    summary: "Paused — chief's turn ran out of inference budget/credits."
                        .to_string(),
                }),
                ..Turn::default()
            },
            Turn::reply("shipped"),
            Turn::reply("All shipped."),
        ],
    );

    let out = fx
        .runner(&turns)
        .handle_operator_message("chief", "ship the API", Some("general"))
        .await
        .expect("operator message handled");

    let pause = out
        .budget_paused
        .expect("the responder's pause must survive the relay overwriting the reply");
    assert_eq!(pause.agent, "chief");
    assert_eq!(out.reply, "All shipped.", "the relay did replace the text");
}

/// Issue #1846 review (Codex #3865395868, the chat-created-hand-off half):
/// the hand-off's own card — opened by `open_hand_off_work_card`, tracked
/// separately from any card this delegation is nested inside — must also
/// settle `Paused` when the delegate's turn ran out of credits, not
/// `Completed` — the terminal-state asymmetry `HarnessBrain::run_task`
/// already closed for the top-level orchestrator's own dispatched turn.
#[tokio::test]
async fn a_hand_offs_own_card_settles_paused_when_the_delegate_ran_out_of_credits() {
    let fx = Fixture::new();
    let turns = ScriptedTurns::new(
        &fx,
        vec![Turn::budget_paused(
            "Paused — engineer's turn ran out of inference budget/credits.",
            "engineer",
            "Paused — engineer's turn ran out of inference budget/credits, so it \
             stopped instead of failing silently.",
        )],
    );
    let outcome = fx
        .runner(&turns)
        .run_delegation(
            handoff("draft the launch plan"),
            None,
            MessageContext::default(),
        )
        .await
        .expect("delegation runs");

    let desk_reply = outcome
        .desk_reply
        .expect("the delegate's turn produced a reply, paused or not");
    assert!(
        desk_reply.budget_paused.is_some(),
        "the pause must reach the caller through `DeskReply` — it is what \
         `handle_task_delegations` later carries into `TaskHandoff`"
    );

    let cards = fx.cards().await;
    assert_eq!(cards.len(), 1, "{cards:?}");
    assert_eq!(
        cards[0].column, COLUMN_PAUSED,
        "a pause must not read as a completed answer: {:?}",
        cards[0]
    );
}

/// Issue #1846 review (Codex #3870516681) — **the regression.** A desk
/// that paused for lack of credits has not ANSWERED, so there is nothing
/// for the CEO relay to hand back.
///
/// Before this fix the fold pushed the delegate's pause placeholder into
/// `desk_replies`, whose non-empty check launched the relay anyway: a
/// second inference call at the same exhausted provider, which paused too
/// and parked a SECOND marker — this one for the RESPONDER, with no notice
/// anywhere pointing at it. Being newer, that orphan supersedes the live
/// CTA on the delegate's own notice, disabling the one button that would
/// have worked.
///
/// The script deliberately supplies only TWO turns: the responder's
/// hand-off and the delegate's pause. A relay would need a third, so if
/// the gate ever regresses, `ScriptedTurns` runs dry and this fails loudly
/// rather than silently parking an extra marker.
#[tokio::test]
async fn a_delegates_budget_pause_does_not_launch_the_ceo_relay() {
    let fx = Fixture::nested();
    let turns = ScriptedTurns::new(
        &fx,
        vec![
            Turn::tooling("handing it to engineering", vec![handoff("ship the API")]),
            Turn::budget_paused(
                "Paused — engineer's turn ran out of inference budget/credits.",
                "engineer",
                "Paused — engineer's turn ran out of inference budget/credits, so it \
                 stopped instead of failing silently.",
            ),
        ],
    );

    let out = fx
        .runner(&turns)
        .handle_operator_message("chief", "ship the API", Some("general"))
        .await
        .expect("operator message handled");

    assert!(
        out.budget_paused.is_some(),
        "sanity: the delegate's pause still folds through to the operator"
    );
    assert_eq!(
        out.budget_paused.as_ref().map(|p| p.agent.as_str()),
        Some("engineer"),
        "the pause named must be the delegate's, not a relay's: {:?}",
        out.budget_paused
    );

    // Asserted on the CALLS, not on the parked markers: the marker
    // registry is process-global and keyed by company id, and every
    // `Fixture::nested()` shares the manifest's one id — so a sibling test
    // parking for "chief" would make a marker assertion here pass or fail
    // on test-execution order rather than on this behaviour. The relay
    // launching at all is the defect; the orphan marker is its downstream
    // consequence.
    let calls = turns.calls();
    assert_eq!(
        calls.len(),
        2,
        "exactly two turns: the responder's hand-off and the delegate's paused turn. A \
         third is the CEO relay firing into the same exhausted provider — the pre-fix \
         defect, which parks a second, unreachable marker for the responder: {calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|(_, prompt)| prompt.contains("You delegated this to your team")),
        "no call may carry a relay prompt — that sentence is `build_relay_prompt`'s and \
         nothing else's: {calls:?}"
    );

    // Issue #1906: this used to assert `out.reply.contains("engineer")`,
    // which passes for the wrong reason — the responder's own hand-off
    // sentence happens to name the desk — and was read as proving the fold
    // reached the operator. It does not reach them: the caller overwrites
    // the reply on any pause. What the skip owes the operator is the pause
    // itself, asserted above; what it owes the reader is that the bubble is
    // left exactly as the responder wrote it.
    assert_eq!(
        out.reply, "handing it to engineering",
        "the skip must leave the responder's own reply untouched: {}",
        out.reply
    );
}

/// Issue #1906: a hand-off the responder's tool REFUSED — a desk this
/// company does not have — must still be logged when the relay is skipped
/// for a budget pause.
///
/// `drain_refusals` + its `tracing::warn!` lived only inside the relay
/// branch, so on the skip path the refusal was swept away by
/// `DelegationClaim`'s drop with nothing anywhere recording it. Nothing
/// leaked — the scope clears either way — but the log line IS the record on
/// this path: there is no card in scope to note the refusal on (see the
/// relay branch's own comment, issue #272).
///
/// Captured with a thread-local subscriber rather than a global one, and
/// on `#[tokio::test]`'s current-thread runtime, so the whole turn runs on
/// the thread the sink is installed for and no sibling test in this binary
/// races for the process-wide slot.
#[tokio::test]
async fn a_refused_hand_off_is_still_logged_when_the_relay_is_skipped() {
    use std::io::Write;

    #[derive(Clone, Default)]
    struct Sink(Arc<std::sync::Mutex<Vec<u8>>>);
    struct Writer(Arc<std::sync::Mutex<Vec<u8>>>);
    impl Write for Writer {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log sink").extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
        type Writer = Writer;
        fn make_writer(&'a self) -> Self::Writer {
            Writer(self.0.clone())
        }
    }

    let fx = Fixture::nested();
    let turns = ScriptedTurns::new(
        &fx,
        vec![
            // One hand-off that lands, and one the tool refuses outright
            // because no such desk is on the roster.
            Turn {
                reply: "handing it to engineering".to_string(),
                tool_pushes: vec![handoff("ship the API")],
                refuses: vec!["legal_desk".to_string()],
                ..Turn::default()
            },
            Turn::budget_paused(
                "Paused — engineer's turn ran out of inference budget/credits.",
                "engineer",
                "Paused — engineer's turn ran out of inference budget/credits, so it \
                 stopped instead of failing silently.",
            ),
        ],
    );

    let sink = Sink::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(sink.clone())
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let out = fx
        .runner(&turns)
        .handle_operator_message("chief", "ship the API", Some("general"))
        .await
        .expect("operator message handled");
    drop(guard);

    assert!(
        out.budget_paused.is_some(),
        "sanity: this is the relay-skip path, not the relay one"
    );
    let logs = String::from_utf8_lossy(&sink.0.lock().expect("log sink").clone()).to_string();
    assert!(
        logs.contains("hand-offs to desks this company does not have"),
        "a refused hand-off on the skip path has no card to land on, so the log is its only \
         record: {logs:?}"
    );
    assert!(
        logs.contains("refused=1"),
        "the count of refusals is what makes the line actionable: {logs:?}"
    );
}

/// Issue #1846 review (Codex #3865395857): when the CEO-relay call ITSELF
/// pauses — not the responder's own turn, and not a delegate's, both
/// already covered above — `run_inner`'s default park (see `mod.rs`)
/// parks whatever text the relay call was actually made with:
/// `relay_prompt`, an internally-generated prompt, not the operator's own
/// words. This proves the relay fold re-parks with `message` — the same
/// discipline `run_hand_off` already applies via `self.reissue_message`
/// on the hand-off path.
#[tokio::test]
async fn the_ceo_relays_own_pause_reparks_with_the_original_operator_message() {
    let fx = Fixture::nested();
    let turns = ScriptedTurns::new(
        &fx,
        vec![
            Turn::tooling("handing it to engineering", vec![handoff("ship the API")]),
            Turn::reply("shipped"),
            Turn::budget_paused(
                "Paused — chief's turn ran out of inference budget/credits.",
                "chief",
                "Paused — chief's turn ran out of inference budget/credits, so it \
                 stopped instead of failing silently.",
            ),
        ],
    );

    let out = fx
        .runner(&turns)
        .handle_operator_message("chief", "ship the API", Some("general"))
        .await
        .expect("operator message handled");

    assert!(
        out.budget_paused.is_some(),
        "sanity: the relay's own pause still folds through"
    );

    let marker = crate::runtime::grants::budget_pauses_for(&fx.record.id)
        .peek("chief")
        .expect("a marker was parked for the paused relay call");
    assert_eq!(
        marker.message, "ship the API",
        "the marker must carry the OPERATOR's original words — a redeem re-dispatches \
         `marker.message` verbatim as a fresh operator message, so parking the internal \
         relay prompt here would silently run a different request"
    );
}

/// A spend halt and a budget pause on the SAME chain are both reported —
/// they are different terminal states with different operator actions
/// (raise a cap / narrow the ask vs. add credits), so one must not mask
/// the other.
#[tokio::test]
async fn a_spend_halt_and_a_budget_pause_in_the_same_chain_both_survive() {
    let fx = Fixture::nested();
    let turns = ScriptedTurns::new(
        &fx,
        vec![
            Turn::tooling("handing it to engineering", vec![handoff("ship the API")]),
            Turn::tooling(
                "I built it; asking research about the rate limits",
                vec![nested_handoff("what rate limits do competitors use?")],
            ),
            Turn::spend_halted("I got as far as two competitors", "researcher", 4.02, 4.0),
            Turn {
                reply: "Built. Research is partial, and I'm out of credits too.".to_string(),
                budget_paused: Some(crate::harness::BudgetPause {
                    agent: "engineer".to_string(),
                    summary: "Paused — engineer's turn ran out of inference budget/credits."
                        .to_string(),
                }),
                ..Turn::default()
            },
        ],
    );

    let out = fx
        .runner(&turns)
        .handle_operator_message("chief", "ship the API", Some("general"))
        .await
        .expect("operator message handled");

    assert_eq!(
        out.halted_for_spend.map(|h| h.agent),
        Some("researcher".to_string()),
        "the spend halt must still surface"
    );
    assert_eq!(
        out.budget_paused.map(|p| p.agent),
        Some("engineer".to_string()),
        "and the budget pause, on a DIFFERENT teammate, must not be masked by it"
    );
}

/// The responder's own halt survives the relay turn replacing its text.
///
/// This is the sibling of the sticky OR beside it, and the same trap: the
/// relay overwrites `operator_reply` wholesale, so a halt tracked as "the
/// last turn's value" would be erased by a relay turn that itself ran fine.
#[tokio::test]
async fn a_responders_own_halt_survives_the_relay_replacing_the_reply() {
    let fx = Fixture::nested();
    let turns = ScriptedTurns::new(
        &fx,
        vec![
            // The orchestrator runs out of money AND still manages to hand
            // off — the halt is on the turn that queued the delegation.
            Turn {
                reply: "handing it to engineering".to_string(),
                tool_pushes: vec![handoff("ship the API")],
                spend_halt: Some(crate::harness::SpendHalt {
                    agent: "chief".to_string(),
                    spent_usd: 2.5,
                    cap_usd: 2.0,
                }),
                ..Turn::default()
            },
            Turn::reply("shipped"),
            Turn::reply("All shipped."),
        ],
    );

    let out = fx
        .runner(&turns)
        .handle_operator_message("chief", "ship the API", Some("general"))
        .await
        .expect("operator message handled");

    let halt = out
        .halted_for_spend
        .expect("the responder's halt must survive the relay overwriting the reply");
    assert_eq!(halt.agent, "chief");
    assert_eq!(out.reply, "All shipped.", "the relay did replace the text");
}

/// Two halts in one chain report the **first**, not the last.
///
/// One operator message, one bubble, one cap it can name. First-wins keeps
/// the claim incomplete but never wrong — and keeps it anchored to the
/// teammate nearest the answer the operator reads, rather than to whichever
/// turn happened to run last.
#[tokio::test]
async fn two_halts_in_one_chain_report_the_first() {
    let fx = Fixture::nested();
    let turns = ScriptedTurns::new(
        &fx,
        vec![
            Turn {
                reply: "handing it to engineering".to_string(),
                tool_pushes: vec![handoff("ship the API")],
                spend_halt: Some(crate::harness::SpendHalt {
                    agent: "chief".to_string(),
                    spent_usd: 2.5,
                    cap_usd: 2.0,
                }),
                ..Turn::default()
            },
            Turn::spend_halted("partly done", "engineer", 9.1, 9.0),
            Turn::reply("Partly shipped."),
        ],
    );

    let out = fx
        .runner(&turns)
        .handle_operator_message("chief", "ship the API", Some("general"))
        .await
        .expect("operator message handled");

    let halt = out.halted_for_spend.expect("a halt is reported");
    assert_eq!(
        halt.agent, "chief",
        "the first halt in the chain is the one named"
    );
}

/// The bound bites in the MEMBER'S OWN TURN, and the third lead never runs.
///
/// Under `max_delegation_depth = 1` — the "recursion off" setting, and the
/// pre-#176 behaviour exactly — the engineering lead's hand-off is refused
/// at the tool boundary with the new reason, so no second desk turn happens
/// at all.
#[tokio::test]
async fn a_hand_off_past_the_depth_bound_is_refused_and_never_runs() {
    let fx = Fixture::nested();
    let turns = ScriptedTurns::new(
        &fx,
        vec![
            Turn::tooling("handing it to engineering", vec![handoff("ship the API")]),
            Turn::tooling(
                "asking research",
                vec![nested_handoff("what rate limits do competitors use?")],
            ),
            Turn::reply("Done, though I could not consult research."),
        ],
    )
    .with_max_depth(1);

    fx.runner(&turns)
        .handle_operator_message("chief", "ship the API", Some("general"))
        .await
        .expect("operator message handled");

    assert_eq!(
        turns.staged(),
        vec![
            orchestrator::Staged::Queued,
            orchestrator::Staged::NoDrain(orchestrator::NoDrainReason::Depth),
        ],
        "the member's hand-off must be refused in its own turn, as depth-capped"
    );
    let calls = turns.calls();
    assert_eq!(
        calls.len(),
        3,
        "chief, engineer, relay — the researcher must never run: {calls:?}"
    );
    assert!(
        calls.iter().all(|(agent, _)| agent != "researcher"),
        "{calls:?}"
    );
}

/// The per-turn fan-out cap applies at **every** level, and needs no new
/// code to do so: the outer drain moves its items into a local vector
/// before running any of them, so a member's turn starts against an empty
/// queue and gets the whole cap to itself — and its fourth push is refused.
///
/// Pinned because "the cap is per turn" is an emergent property of how the
/// drain is written, not something stated anywhere. A refactor that drained
/// lazily would silently make the cap per *message* and this is what would
/// catch it.
#[tokio::test]
async fn the_fan_out_cap_applies_at_every_level() {
    let fx = Fixture::nested();
    let card = |n: u32| Delegation::SpawnTask {
        title: format!("follow-up {n}"),
        note: None,
        assignee: None,
    };
    let turns = ScriptedTurns::new(
        &fx,
        vec![
            Turn::tooling("handing it to engineering", vec![handoff("ship the API")]),
            // The member gets the FULL cap of its own, and one more is
            // refused.
            Turn::tooling(
                "built it, opening follow-ups",
                vec![card(1), card(2), card(3), card(4)],
            ),
            Turn::reply("Shipped."),
        ],
    );

    fx.runner(&turns)
        .handle_operator_message("chief", "ship the API", Some("general"))
        .await
        .expect("operator message handled");

    assert_eq!(
        turns.staged(),
        vec![
            orchestrator::Staged::Queued,
            orchestrator::Staged::Queued,
            orchestrator::Staged::Queued,
            orchestrator::Staged::Queued,
            orchestrator::Staged::OverCap,
        ],
        "the member's own turn gets the full per-turn cap, and no more"
    );
    // Three follow-up cards from the member, plus the hand-off's own card.
    let mut titles: Vec<String> = fx
        .cards()
        .await
        .into_iter()
        .map(|c| c.title.to_string())
        .collect();
    titles.sort();
    assert_eq!(titles.len(), 4, "{titles:?}");
    assert!(
        titles.contains(&"follow-up 3".to_string()) && !titles.contains(&"follow-up 4".to_string()),
        "{titles:?}"
    );
}

/// A cancelled NESTED run folds in as a cancellation, never as a reply.
///
/// The member said it was handing that slice on; an answer that silently
/// omits the branch is the confident falsehood the whole delegation stack
/// exists to prevent.
#[tokio::test]
async fn a_cancelled_nested_run_folds_in_as_a_cancellation() {
    let fx = Fixture::nested();
    let turns = ScriptedTurns::new(
        &fx,
        vec![
            Turn::tooling("handing it to engineering", vec![handoff("ship the API")]),
            Turn::tooling(
                "asking research",
                vec![nested_handoff("what rate limits do competitors use?")],
            ),
            Turn::cancelled("(discarded)"),
            Turn::reply("Built; the research question was stopped."),
        ],
    );

    fx.runner(&turns)
        .handle_operator_message("chief", "ship the API", Some("general"))
        .await
        .expect("operator message handled");

    let calls = turns.calls();
    assert_eq!(calls.len(), 4, "{calls:?}");
    let relay_prompt = &calls[3].1;
    assert!(
        relay_prompt.contains("was cancelled before it replied"),
        "a cancelled branch must be named, not omitted: {relay_prompt}"
    );
    assert!(
        !relay_prompt.contains("(discarded)"),
        "a cancelled run's text must NEVER be folded in as a reply: {relay_prompt}"
    );
}
