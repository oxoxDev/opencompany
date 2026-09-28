use super::*;

/// An errored hand-off must not STRAND the card (issue #213 review).
///
/// `hand_card_over` has already persisted the card as `in_progress`
/// reassigned to the delegate before their turn starts. If that turn then
/// errors and the failure is propagated out of `run_task`, the settle and
/// the final `upsert` are both skipped and the card sits in `in_progress`
/// under the delegate with no result — and nothing re-dispatches it, because
/// `task_enters_in_progress` only edge-fires on the *transition* into that
/// column and the card is already there. Exactly the state issue #204 exists
/// to eliminate, reintroduced through the error path.
///
/// So an errored hand-off takes the same arm an errored turn does: settle
/// `Failed` → `todo`, with the reason on the note.
#[tokio::test]
async fn a_hand_off_whose_delegate_errors_lands_in_todo_not_stranded_in_progress() {
    let dir = tempfile::tempdir().unwrap();
    // Invoke 1 is the dispatched orchestrator turn (queues the hand-off);
    // invoke 2 is the delegate's own turn, which errors.
    let (brain, provider) = brain_that_delegates_with(
        dir.path(),
        vec![vec![Delegation::DelegateToDesk {
            desk: "eng_desk".to_string(),
            instruction: "fetch my activity".to_string(),
        }]],
        TurnFaults {
            fail_from: Some(2),
            ..TurnFaults::default()
        },
    );
    dispatch_card(&brain, &provider.tasks.clone(), "t-boom").await;

    let after = only_card(&provider.tasks).await;
    assert_eq!(
        after.column, COLUMN_TODO,
        "an errored hand-off must return the card to To-do, never leave it stranded in \
         progress where nothing will re-dispatch it"
    );
    let note = after.note.expect("note");
    // "dispatch failed", not "hand-off failed": since the async hand-off the
    // delegate errors inside its OWN attempt, so the reason is recorded by
    // that attempt's settle rather than by the hand-off that queued it. The
    // property this test is named for — To-do, never stranded in progress —
    // is asserted above and is unchanged.
    assert!(
        note.contains("dispatch failed:"),
        "the failure reason lands on the note: {note}"
    );
    assert!(
        note.contains("delegated to engineer: fetch my activity"),
        "the hand-off that was attempted is still recorded: {note}"
    );
    // The failure is the assignee's, not the operator's — a cancellation is
    // the only ending attributed to `operator`.
    assert!(
        !note.contains("[operator]"),
        "an errored run is not an operator cancellation: {note}"
    );
}

/// A hand-off an operator cancels mid-flight produced nothing, so the card
/// returns to `todo` reported as the cancellation it actually was.
///
/// The claim is only made because `run_delegation` reports the cancellation
/// as a fact (`DelegationOutcome::cancelled`); a hand-off that ends empty
/// for any other reason no longer reaches this arm at all (issue #213
/// review finding 2).
#[tokio::test]
async fn a_cancelled_hand_off_returns_the_card_to_todo_as_a_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    // Invoke 1 is the dispatched orchestrator turn (queues the hand-off);
    // invoke 2 is the delegate's turn, which cancels itself mid-run.
    let (brain, provider) = brain_that_delegates_with(
        dir.path(),
        vec![vec![Delegation::DelegateToDesk {
            desk: "eng_desk".to_string(),
            instruction: "fetch my activity".to_string(),
        }]],
        TurnFaults {
            cancel_on: vec![2],
            ..TurnFaults::default()
        },
    );
    dispatch_card(&brain, &provider.tasks.clone(), "t-cancel").await;

    let after = only_card(&provider.tasks).await;
    assert_eq!(
        after.column, COLUMN_TODO,
        "a cancelled hand-off must not read as finished, and must not strand in progress"
    );
    let note = after.note.expect("note");
    // The wording moved with the async hand-off. A cancelled delegate is now
    // cancelled inside ITS OWN dispatch, so the reason is `run_task`'s
    // steer-cancel wording rather than the sync hand-off's report of a
    // delegate that never produced anything. The property this test is named
    // for — To-do, attributed to the operator — is unchanged.
    assert!(
        note.contains("cancelled while in flight"),
        "the cancellation is reported as the cause: {note}"
    );
    // A cancellation is the operator's act, so the block is theirs — not the
    // delegate's, who never said it.
    assert!(
        note.contains("[operator] cancelled while in flight"),
        "a cancellation is attributed to the operator: {note}"
    );
}

/// A later hand-off that ANSWERS must not be discarded by an earlier one
/// that produced nothing (issue #213 review finding 3).
///
/// Before this, the first hand-off owned the card unconditionally. A first
/// hand-off cancelled mid-flight still produced a `TaskHandoff`, so a second
/// hand-off in the same drain took the "does not own the card" arm: its
/// answer was appended to the note, but the card still settled `Cancelled`
/// -> `todo` off the first. Work that ran and produced output ended up
/// filed under a card marked cancelled.
#[tokio::test]
async fn a_later_answering_hand_off_takes_the_card_over_from_an_earlier_empty_one() {
    let dir = tempfile::tempdir().unwrap();
    // Invoke 1 is the dispatched orchestrator turn, queuing BOTH hand-offs.
    // Invoke 2 is the first delegate's run, cancelled mid-flight; invoke 3
    // is the second delegate's run, which answers.
    let (brain, provider) = brain_that_delegates_with(
        dir.path(),
        vec![vec![
            Delegation::DelegateToDesk {
                desk: "eng_desk".to_string(),
                instruction: "first attempt".to_string(),
            },
            Delegation::DelegateToDesk {
                desk: "eng_desk".to_string(),
                instruction: "second attempt".to_string(),
            },
        ]],
        TurnFaults {
            cancel_on: vec![2],
            ..TurnFaults::default()
        },
    );
    dispatch_card(&brain, &provider.tasks.clone(), "t-two-handoffs").await;

    let after = only_card(&provider.tasks).await;
    // The card settles from the ONE hand-off that owns it — cancelled here,
    // so To-do. A second hand-off in the same turn never ran.
    assert_eq!(
        after.column, COLUMN_TODO,
        "the card settles from the hand-off that owns it"
    );
    assert_eq!(
        after.assignee, "engineer",
        "the owner is an agent: the lead of the desk the card was handed to"
    );
    let note = after.note.expect("note");
    // The protection #213 added, reached a stronger way. It used to be
    // "a later hand-off that ANSWERS takes the card from an empty one", so
    // work that ran could never be filed under a cancelled card. Async
    // hand-off removes the race instead of resolving it: the first hand-off
    // owns the card and its delegate is dispatched, so a second one is
    // recorded and NOT started. There is no output to misfile because no
    // second run happened.
    assert!(
        note.contains("also asked eng_desk: second attempt") && note.contains("not started"),
        "the second hand-off is recorded, and visibly did not run: {note}"
    );
    assert!(
        !note.contains("[eng_desk] second attempt") && !note.contains("[engineer] second attempt"),
        "a second hand-off must not produce work under a card owned by the first: {note}"
    );
}

/// The compatibility half: a dispatched turn that delegates nothing still
/// runs exactly one turn and settles under the agent that ran it.
///
/// (Deliberately no assertion on `assignee` — linking the *non-delegating*
/// working agent to the card is issue #205's fix, and this test must not
/// pin the blank it leaves behind today.)
#[tokio::test]
async fn a_dispatched_turn_that_delegates_nothing_settles_exactly_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, provider) = brain_that_delegates(dir.path(), Vec::new());
    dispatch_card(&brain, &provider.tasks.clone(), "t-plain").await;

    assert_eq!(
        provider.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "no delegation → one turn, no hand-off"
    );
    let after = only_card(&provider.tasks).await;
    assert_eq!(after.column, "in_review");
    assert!(
        after.note.expect("note").contains("[chief]"),
        "the agent that ran it owns the result"
    );
}

/// A hand-off to a desk nobody leads has nowhere to go: rather than
/// stranding the card in `in_progress` waiting on a delegate that will
/// never run, the delegator's own reply settles it exactly as before.
#[tokio::test]
async fn a_hand_off_to_an_unknown_desk_settles_under_the_delegator() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, provider) = brain_that_delegates(
        dir.path(),
        vec![Some(Delegation::DelegateToDesk {
            desk: "ghost".to_string(),
            instruction: "look into it".to_string(),
        })],
    );
    dispatch_card(&brain, &provider.tasks.clone(), "t-ghost").await;

    assert_eq!(
        provider.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "an unknown desk has no lead to run a second turn"
    );
    let after = only_card(&provider.tasks).await;
    assert_eq!(
        after.column, "in_review",
        "the card must not strand in progress waiting on a delegate that cannot run"
    );
    assert_ne!(after.assignee, "ghost");
}

/// Issue #272: settling under the delegator is the right *behaviour* (#213
/// chose it so a card is never stranded), but it used to be silent — the
/// board showed a card whose note claimed a hand-off and whose owner was the
/// delegator, with nothing connecting the two. The undeliverable hand-off is
/// now recorded on the card, so an operator reads the fact instead of
/// inferring it from an absence.
#[tokio::test]
async fn a_hand_off_that_cannot_be_delivered_says_so_on_the_card() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, provider) = brain_that_delegates(
        dir.path(),
        vec![Some(Delegation::DelegateToDesk {
            desk: "ghost".to_string(),
            instruction: "look into it".to_string(),
        })],
    );
    dispatch_card(&brain, &provider.tasks.clone(), "t-loud").await;

    let note = only_card(&provider.tasks).await.note.expect("note");
    assert!(
        note.contains("hand-off to \"ghost\" was not delivered"),
        "the card must name the hand-off that did not happen: {note}"
    );
    assert!(
        note.contains("this card is still with chief"),
        "the card must say who still owns it: {note}"
    );
}

/// Issue #272, the grounded half: the tool refused the invented target, so
/// no `Delegation` was ever queued. The turn is still free to *say* it
/// handed the work off — that is exactly what happened on the live company
/// — so the board records the refusal independently of the turn's account
/// of it. Without this the card settles under the delegator with a note
/// that claims a hand-off and nothing anywhere contradicting it.
#[tokio::test]
async fn a_refused_hand_off_is_recorded_on_the_card() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, provider) = brain_that_delegates_with(
        dir.path(),
        vec![Vec::new()],
        TurnFaults {
            refused_on_first: vec!["writer".to_string()],
            ..TurnFaults::default()
        },
    );
    dispatch_card(&brain, &provider.tasks.clone(), "t-refused").await;

    let after = only_card(&provider.tasks).await;
    assert_eq!(
        provider.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a refused hand-off runs no delegate"
    );
    assert_eq!(
        after.column, "in_review",
        "the card still settles under the delegator (#213); it is only no longer silent"
    );
    let note = after.note.expect("note");
    assert!(
        note.contains("hand-off to \"writer\" was not delivered"),
        "the refused target must be named on the card: {note}"
    );
    assert!(
        note.contains("not somewhere this company can hand work to"),
        "the cause must be on the card: {note}"
    );
}

/// The other half of #272's note: a delegation that never had a desk target
/// (a `spawn_task`) must not pick up an undeliverable-hand-off line.
#[tokio::test]
async fn a_spawn_task_never_records_an_undeliverable_hand_off() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, provider) = brain_that_delegates(
        dir.path(),
        vec![Some(Delegation::SpawnTask {
            title: "Follow up".to_string(),
            note: None,
            assignee: None,
        })],
    );
    dispatch_card(&brain, &provider.tasks.clone(), "t-quiet").await;

    let cards = provider.tasks.list(&CompanyId::new("acme")).await.unwrap();
    let parent = cards.iter().find(|c| c.id == "t-quiet").expect("parent");
    assert!(
        !parent
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("was not delivered"),
        "a spawn_task has no desk target to fail: {:?}",
        parent.note
    );
}

/// A `spawn_task` queued by a *dispatched* turn now opens its card with the
/// dispatched card as its parent — the lineage `run_delegation` could never
/// stamp while the task path did not drain the queue (issue #185's
/// `parent_task_id`).
#[tokio::test]
async fn a_task_spawned_by_a_dispatched_turn_records_its_parent_card() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, provider) = brain_that_delegates(
        dir.path(),
        vec![Some(Delegation::SpawnTask {
            title: "Follow up next week".to_string(),
            note: None,
            assignee: None,
        })],
    );
    dispatch_card(&brain, &provider.tasks.clone(), "t-parent").await;

    let cards = provider.tasks.list(&CompanyId::new("acme")).await.unwrap();
    let spawned = cards
        .iter()
        .find(|c| c.title == "Follow up next week")
        .expect("the spawned card must actually be opened");
    assert_eq!(spawned.column, COLUMN_TODO);
    assert_eq!(
        spawned.parent_task_id.as_deref(),
        Some("t-parent"),
        "a card spawned inside a dispatch remembers the card it came from"
    );
    // The parent still settles on its own turn's reply — spawning follow-up
    // work is not a hand-off.
    let parent = cards.iter().find(|c| c.id == "t-parent").expect("parent");
    assert_eq!(parent.column, "in_review");
}

/// The wiring's whole point: an agent bound to a named harness runs on that
/// harness's engine, and an unbound one stays on the default.
#[tokio::test]
async fn a_bound_agent_runs_on_its_own_lane() {
    let dir = tempfile::tempdir().unwrap();
    let deep = Arc::new(SpyLane {
        label: "deep".to_string(),
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let brain = brain_over_mock_with(dir.path(), two_harness_record()).with_lanes(vec![(
        "deep".to_string(),
        deep.clone() as Arc<dyn crate::runtime::delegation::RunTurn>,
    )]);

    let company = CompanyId::new("acme");
    let out = brain
        .run_turn()
        .run(
            &company,
            "researcher",
            "hi",
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await
        .expect("routes to the deep lane");
    assert_eq!(out.reply, "deep");
    assert_eq!(&*deep.seen.lock().unwrap(), &["researcher".to_string()]);

    // The unbound agent must not reach it — it belongs to the default pool.
    let _ = brain
        .run_turn()
        .run(
            &company,
            "ceo",
            "hi",
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await;
    assert_eq!(
        &*deep.seen.lock().unwrap(),
        &["researcher".to_string()],
        "the default agent must not land on the named lane"
    );
}

/// The named lane is a **real** pool, not a spy: it must build its own
/// roster at boot, or a bound agent's first turn fails `CompanyNotFound`
/// on an empty pool. This is the path the `SpyLane` coverage above cannot
/// reach — a spy forwards every turn, so it would pass whether or not the
/// lane's pool was ever warmed.
#[tokio::test]
async fn a_named_lane_builds_its_roster_at_boot() {
    let dir = tempfile::tempdir().unwrap();
    let brain = brain_over_mock_with(dir.path(), two_harness_record());
    // The lane `lanes::build` produces: its own pool over deps narrowed to
    // the agents it serves.
    let mut deep_deps = (*brain.deps).clone();
    deep_deps.serves = Some(std::collections::HashSet::from(["researcher".to_string()]));
    let deep: Arc<dyn crate::runtime::delegation::RunTurn> = Arc::new(HarnessRunTurn::new(
        Arc::new(HarnessPool::new()),
        Arc::new(deep_deps),
    ));
    let brain = brain.with_lanes(vec![("deep".to_string(), deep.clone())]);

    // Boot warm-up: the router warms every lane's engine, each against its
    // own narrowed deps.
    brain
        .run_turn()
        .ensure(&brain.record())
        .await
        .expect("every lane's roster builds");

    // A bound agent's turn now reaches its lane's engine instead of dying
    // with "company not found".
    let out = brain
        .run_turn()
        .run(
            &CompanyId::new("acme"),
            "researcher",
            "hi",
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await
        .expect("the deep lane's roster is built at boot");
    assert!(out.reply.contains("hi"), "{}", out.reply);
}

/// A declared harness this host cannot run fails the turn, naming the
/// harness and the reason. It must never quietly borrow the default lane:
/// that turn would succeed on a model and a credential nobody chose, and
/// the only evidence would be a billing line.
#[tokio::test]
async fn an_unrunnable_harness_fails_rather_than_falling_back() {
    let dir = tempfile::tempdir().unwrap();
    let brain =
        brain_over_mock_with(dir.path(), two_harness_record()).with_unavailable_lanes(vec![(
            "deep".to_string(),
            "this build has no ACP transport wired".to_string(),
        )]);

    let err = brain
        .run_turn()
        .run(
            &CompanyId::new("acme"),
            "researcher",
            "hi",
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await
        .expect_err("must not fall back to the default lane");
    let msg = err.to_string();
    assert!(msg.contains("researcher"), "{msg}");
    assert!(msg.contains("deep"), "{msg}");
    assert!(msg.contains("ACP transport"), "names the fix: {msg}");
}

/// A company declaring no `[[harness]]` keeps exactly the single-lane path:
/// no lanes, no bindings, nothing to consult.
#[tokio::test]
async fn a_company_with_no_harness_block_is_unrouted() {
    let dir = tempfile::tempdir().unwrap();
    let brain = brain_over_mock(dir.path());
    assert!(brain.lanes.is_empty());
    assert!(brain.unavailable.is_empty());
    assert!(brain.bindings.is_empty());
    assert_eq!(brain.default_harness, "default");
}

/// Issue #1244: a company whose *only* declared harness is `kind = "acp"`
/// must not silently run turns on the embedded engine.
///
/// Before the fix, `lanes::build`'s early return for a single declared
/// harness meant nobody ever asked what *kind* that lone harness was — the
/// caller (here, and identically in `RuntimeBuilder`) unconditionally built
/// a `HarnessRunTurn` from the shared pool regardless. This exercises the
/// real `lanes::build` output rather than a hand-simulated one, so a
/// regression in either `lanes::build` or `HarnessBrain::run_turn` fails it.
#[tokio::test]
async fn a_lone_acp_default_harness_does_not_silently_run_embedded() {
    let dir = tempfile::tempdir().unwrap();
    let manifest: CompanyManifest = toml::from_str(
        r#"
[company]
name = "Acme"

[[agent]]
id = "ceo"
role = "Chief Executive"

[[harness]]
id = "laptop"
kind = "acp"
default = true

[harness.acp]
transport = "local"
agent = "claude"
"#,
    )
    .expect("valid manifest");
    let record = CompanyRecord {
        general_channel: Default::default(),
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: CompanyId::new("acme"),
        manifest,
        ledger: Vec::new(),
        lifecycle: "running".to_string(),
        overlay_agents: Vec::new(),
        overlay_desk_members: Vec::new(),
        overlay_desk_order: Vec::new(),
        overlay_desks: Vec::new(),
        overlay_workflows: Vec::new(),
        overlay_budgets: Vec::new(),
        overlay_policy: None,
        overlay_tool_grants: None,
        overlay_desk_tools: Default::default(),
        disabled_workflows: Vec::new(),
        template_provenance: None,
        setup: None,
        name_confirmed: false,
        activation_completed_at: None,
        created_at_millis: None,
    };

    let brain = brain_over_mock_with(dir.path(), record);
    let secrets: Arc<dyn crate::ports::SecretStore> =
        Arc::new(crate::store::FsSecretStore::new(dir.path()));
    let lanes = crate::harness::lanes::build(
        &brain.record(),
        Arc::new(HarnessPool::new()),
        &brain.deps,
        secrets,
        None,
        None,
    );

    assert!(
        lanes.default_engine.is_none(),
        "an acp default harness has no built-in engine to fall back to"
    );
    assert!(
        lanes.unavailable.iter().any(|(id, _)| id == "laptop"),
        "the default harness's own id must carry the unavailable reason: {:?}",
        lanes.unavailable
    );

    // Wire the brain exactly as `RuntimeBuilder` would, and confirm the
    // turn actually fails instead of quietly answering from the embedded
    // `MockProvider`.
    let brain = brain
        .with_lanes(lanes.lanes)
        .with_unavailable_lanes(lanes.unavailable)
        .with_default_engine(lanes.default_engine);

    let err = brain
        .run_turn()
        .run(
            &CompanyId::new("acme"),
            "ceo",
            "hi",
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await
        .expect_err("must not fall back to the embedded engine");
    let msg = err.to_string();
    assert!(msg.contains("ceo"), "{msg}");
    assert!(msg.contains("laptop"), "{msg}");
    assert!(msg.contains("ACP transport"), "names the fix: {msg}");
}

/// Issue #966: a workflow-copilot reply is authored by the copilot.
///
/// This is the assertion the #885 fix was missing on this branch. The bubble
/// is emitted on the **operator** channel, and before this the author field
/// was left `None` — so the journal writer's `channel` fallback stamped
/// `agent_id: "operator"` on a reply an agent had genuinely produced.
///
/// Asserts the author is *not* the channel, rather than only that it equals
/// the constant: the defect's whole shape is the two being conflated, and a
/// test that checked equality alone would still pass if `CONFINED_AGENT_ID`
/// were ever redefined to `"operator"`.
#[test]
fn a_copilot_turn_is_authored_by_the_copilot_not_the_operator_channel() {
    let bubble = confined_bubble(crate::harness::TurnOutcome {
        reply: "here is what that node does".to_string(),
        steps: Vec::new(),
        hit_iteration_cap: false,
        // Test fixture, not the ACP fold (PR #1880 review).
        abnormal_stop: None,
        halted_for_spend: None,
        budget_paused: None,
    });
    assert_eq!(bubble.channel, "operator", "the destination is unchanged");
    assert_eq!(
        bubble.agent.as_deref(),
        Some(crate::ports::CONFINED_AGENT_ID)
    );
    assert_ne!(
        bubble.agent.as_deref(),
        Some(bubble.channel.as_str()),
        "author and destination must not be the same value — that conflation is issue #885"
    );
}
