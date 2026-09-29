use super::*;

/// The in-turn brake arms **only** when the teammate declares a daily cap — and
/// a malformed manifest value is ignored rather than forwarded.
///
/// This is deliberate and matches upstream: openhuman constructs
/// `BudgetStopHook` nowhere and applies only an opt-in token-based goal hook, so
/// this crate, like upstream, refuses to invent a blanket per-turn number no
/// operator can see or change. A teammate with no declared budget is not
/// hard-stopped mid-turn. Forwarding a malformed value would be worse than
/// ignoring it: the vendored hook fails closed on a non-finite or non-positive
/// cap, so a zero would silently halt every turn that teammate ever ran at its
/// first iteration.
#[tokio::test]
async fn the_budget_brake_arms_only_when_a_daily_cap_is_declared() {
    let (model_url, _script) = spawn_script(vec![Turn::Say("hi")], 12).await;
    let dir = tempfile::tempdir().unwrap();
    let mut agent = company_agent(model_url, dir.path(), None, 0).await;

    // No declared budget → no hook armed.
    assert_eq!(agent.turn_spend_cap_usd(), None);

    // A declared daily cap arms the brake at exactly that value — one cap bounds
    // the worst-case overshoot rather than "one turn, of unknown size".
    agent.budget_usd_daily = Some(2.0);
    assert_eq!(agent.turn_spend_cap_usd(), Some(2.0));

    agent.budget_usd_daily = Some(500.0);
    assert_eq!(agent.turn_spend_cap_usd(), Some(500.0));

    // Malformed values arm nothing rather than a fail-closed hook: such a
    // teammate is already refused pre-dispatch (`spent >= cap` holds at zero
    // spend), so no hook is the safe and honest choice.
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        agent.budget_usd_daily = Some(bad);
        assert_eq!(
            agent.turn_spend_cap_usd(),
            None,
            "a manifest cap of {bad} must not reach the hook"
        );
    }
}

/// The headline proof, and the one that fails without the fix: a turn whose work
/// takes more than the inherited ten iterations — read the standards, read the
/// checklist, read the prior spec, … — now **delivers its answer** instead of
/// pausing at a checkpoint the operator has to resume.
///
/// Twelve reads is deliberately just past the old ceiling and well inside the
/// new one, so this measures the ceiling rather than the size of the script.
#[tokio::test]
async fn a_turn_past_the_old_ten_iteration_ceiling_now_finishes() {
    let reads = INHERITED_CAP + 2;
    let (model_url, script) = spawn_script(read_then_answer(reads, "Spec published."), 12).await;

    let dir = tempfile::tempdir().unwrap();
    let agent = company_agent(model_url, dir.path(), None, reads).await;

    let (outcome, _usages) = agent.run("Draft and publish the pricing spec.").await;
    let outcome = outcome.expect("the turn runs");

    assert!(
        outcome.reply.contains("Spec published."),
        "the turn did not deliver its answer — it paused instead: {}",
        outcome.reply
    );
    // The script's reads really happened: one model call per tool round plus the
    // final answer. Without this the assertion above could pass on a turn that
    // never looped at all.
    assert_eq!(
        model_calls(&script),
        reads + 1,
        "expected {reads} tool rounds plus a final answer"
    );
    assert!(
        !hit_cap(&outcome),
        "a turn that finished must not report an iteration-cap pause"
    );
}

/// A turn that outruns its money is halted **inside** the turn — and the halt is
/// not an iteration-cap pause.
///
/// Both halves matter. Before #988 nothing in this crate could stop a running
/// turn except the iteration ceiling itself: the plan-level token ceiling and
/// the teammate's `budget_usd_daily` are both pre-dispatch, so a turn that
/// started under a cap could finish arbitrarily far over it. Raising the ceiling
/// without this hook would have removed the last brake rather than added one.
///
/// And the two stops must stay distinguishable. openhuman only reports
/// `hit_cap` when the run actually reached `max_tool_iterations` with no final
/// response, so a hook-driven halt that stops on the first iteration — nowhere
/// near the ceiling — reads as `false`. That is what Part 1 of #926 needs: it
/// renders the cap pause to the operator and must not label a budget halt as
/// one.
#[tokio::test]
async fn a_budget_halt_stops_the_turn_and_is_not_an_iteration_cap_pause() {
    // One model call reports a million prompt tokens. On the `chat-v1` tier
    // that estimates to ~$0.14, so the very first iteration crosses a $0.05 cap
    // and the hook halts a turn the script was willing to run for twelve more
    // rounds.
    let reads = INHERITED_CAP + 2;
    let (model_url, script) =
        spawn_script(read_then_answer(reads, "Spec published."), 1_000_000).await;

    let dir = tempfile::tempdir().unwrap();
    let agent = company_agent(model_url, dir.path(), Some(0.05), reads).await;

    let (outcome, _usages) = agent.run("Draft and publish the pricing spec.").await;
    let outcome = outcome.expect("the turn runs");

    assert!(
        !outcome.reply.contains("Spec published."),
        "the budget hook did not stop the turn — it ran to the script's answer: {}",
        outcome.reply
    );
    // Halted early, not merely slowed: the script offered `reads + 1` rounds and
    // the turn spent a small handful. (The exact count is left loose because the
    // vendored turn adds a closing wrap-up call after a partial run, and that
    // call is not the thing under test.)
    let calls = model_calls(&script);
    assert!(
        calls < reads,
        "expected the turn to halt well short of its {reads} scripted rounds, got {calls}"
    );
    assert!(
        !hit_cap(&outcome),
        "a budget halt must NOT be reported as an iteration-cap pause — Part 1 of #926 \
         renders that pause to the operator and the two are different outcomes"
    );
}

/// The negative case the budget-halt test above needs to be meaningful: a
/// teammate who has declared **no** `budget_usd_daily` gets no in-turn brake at
/// all, so a turn that would have blown past any invented blanket figure still
/// finishes.
///
/// Same script as the budget-halt test — a million reported prompt tokens per
/// call, which would trip even a generous fixed ceiling on the very first
/// iteration — with the manifest budget omitted instead of set. If this test
/// fails, either a hook is being armed for a teammate who declared nothing, or
/// some other default crept back in.
#[tokio::test]
async fn a_turn_with_no_declared_budget_gets_no_in_turn_brake_at_any_cost() {
    let reads = INHERITED_CAP + 2;
    let (model_url, script) =
        spawn_script(read_then_answer(reads, "Spec published."), 1_000_000).await;

    let dir = tempfile::tempdir().unwrap();
    let agent = company_agent(model_url, dir.path(), None, reads).await;
    assert_eq!(
        agent.turn_spend_cap_usd(),
        None,
        "the fixture must actually be undeclared for this test to prove anything"
    );

    let (outcome, _usages) = agent.run("Draft and publish the pricing spec.").await;
    let outcome = outcome.expect("the turn runs");

    assert!(
        outcome.reply.contains("Spec published."),
        "a turn with no declared budget was halted anyway — a brake armed \
         without one being declared: {}",
        outcome.reply
    );
    assert_eq!(
        model_calls(&script),
        reads + 1,
        "expected every scripted round to run — nothing should have cut it short"
    );
    assert!(
        !hit_cap(&outcome),
        "the script stayed well under the iteration ceiling; a cap pause here \
         would mean something other than the intended reply mechanism stopped \
         the turn"
    );
}

/// The contrast case, so the assertion above is a distinction rather than a
/// constant: a turn that really does exhaust [`MAX_TOOL_ITERATIONS`] **does**
/// report the cap.
///
/// Without this test `!hit_cap` in the budget case could hold because nothing
/// ever sets it.
#[tokio::test]
async fn exhausting_the_raised_cap_still_reports_an_iteration_cap_pause() {
    let reads = MAX_TOOL_ITERATIONS + 5;
    let (model_url, script) = spawn_script(read_then_answer(reads, "Spec published."), 12).await;

    let dir = tempfile::tempdir().unwrap();
    let agent = company_agent(model_url, dir.path(), None, reads).await;

    let (outcome, _usages) = agent.run("Draft and publish the pricing spec.").await;
    let outcome = outcome.expect("the turn runs");

    assert!(
        hit_cap(&outcome),
        "a turn that never stopped calling tools must report the cap: {}",
        outcome.reply
    );
    // It got all the way to the raised ceiling before pausing — the cap moved
    // with the constant rather than staying at the inherited ten. `>=` rather
    // than `==` because the vendored turn adds a wrap-up call on top of the
    // loop's own rounds to compose the checkpoint.
    let calls = model_calls(&script);
    assert!(
        calls >= MAX_TOOL_ITERATIONS,
        "the turn paused after {calls} model calls, short of the stated \
         {MAX_TOOL_ITERATIONS} — the raised cap is not in effect"
    );
}
