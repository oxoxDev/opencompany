use super::*;
use crate::harness::built_in::policy::ApprovalRequestQueue;
use crate::harness::built_in::policy::policy_test_helpers_tests::in_cycle;
use crate::ports::blockers::{BlockerKind, BlockerPayload, BlockerSource};
use tinytools::Tool;

fn tool(queue: &ApprovalRequestQueue) -> EscalateToHumanTool {
    EscalateToHumanTool::new(
        queue.clone(),
        "engineer".to_string(),
        "engineer".to_string(),
    )
}

#[tokio::test]
async fn a_question_parks_as_an_information_blocker() {
    in_cycle(async {
        let queue = ApprovalRequestQueue::default();
        let result = tool(&queue)
            .execute(serde_json::json!({ "question": "staging or prod?" }))
            .await
            .expect("the tool runs");
        assert!(
            !result.is_error,
            "asking is not a failure: {}",
            result.text()
        );

        let drained = queue.drain(8);
        assert_eq!(drained.requests.len(), 1);
        let request = &drained.requests[0];
        assert_eq!(request.tool, ESCALATE_TO_HUMAN_TOOL);
        assert_eq!(request.effect.kind, "blocker.information");

        let payload: BlockerPayload =
            serde_json::from_value(request.effect.payload.clone()).expect("payload round-trips");
        assert_eq!(payload.kind, BlockerKind::Information);
        assert_eq!(payload.source, BlockerSource::AgentQuestion);
        assert_eq!(
            payload.step, None,
            "a question asked mid-turn names no step; the approval's task link does"
        );
        assert!(payload.reason.contains("staging or prod?"));
    })
    .await;
}

/// The context the agent already gathered rides along, so the operator can
/// answer without re-deriving it — and it is joined into the reason rather
/// than dropped into a field nothing renders yet.
#[tokio::test]
async fn gathered_context_reaches_the_question() {
    in_cycle(async {
        let queue = ApprovalRequestQueue::default();
        tool(&queue)
            .execute(serde_json::json!({
                "question": "which brief is current?",
                "context": "the Jan and Mar briefs contradict on pricing"
            }))
            .await
            .expect("the tool runs");

        let drained = queue.drain(8);
        let payload: BlockerPayload =
            serde_json::from_value(drained.requests[0].effect.payload.clone()).expect("payload");
        assert!(payload.reason.contains("which brief is current?"));
        assert!(payload.reason.contains("contradict on pricing"));
        assert!(
            payload.reason.contains("engineer"),
            "the context is attributed to the agent that gathered it"
        );
    })
    .await;
}

/// A blank question is refused rather than parked: an empty card reaches a
/// person with nothing to answer and still costs them the interruption.
#[tokio::test]
async fn an_empty_question_is_refused_and_parks_nothing() {
    let queue = ApprovalRequestQueue::default();
    assert!(
        tool(&queue)
            .execute(serde_json::json!({ "question": "   " }))
            .await
            .is_err()
    );
    assert!(queue.drain(8).requests.is_empty());
}

/// Approving an escalation must not re-dispatch the agent into calling the
/// same tool again — see the `agent` field's note.
#[tokio::test]
async fn an_escalation_mints_no_grant() {
    in_cycle(async {
        let queue = ApprovalRequestQueue::default();
        tool(&queue)
            .execute(serde_json::json!({ "question": "staging or prod?" }))
            .await
            .expect("runs");
        assert!(queue.drain(8).requests[0].effect.agent.is_none());
    })
    .await;
}

/// The card a person reads names the teammate by the label they know, not
/// the roster id — and still carries the id separately so the console can
/// resolve "Asked by" for itself.
#[tokio::test]
async fn the_reason_names_the_display_label_never_the_roster_id() {
    in_cycle(async {
        let queue = ApprovalRequestQueue::default();
        let tool = EscalateToHumanTool::new(
            queue.clone(),
            "backend_engineer_7f3a".to_string(),
            "Alex".to_string(),
        );
        tool.execute(serde_json::json!({
            "question": "which environment?",
            "context": "staging is already deployed"
        }))
        .await
        .expect("runs");

        let effect = queue.drain(8).requests[0].effect.clone();
        let payload: BlockerPayload =
            serde_json::from_value(effect.payload.clone()).expect("payload round-trips");
        assert!(
            payload.reason.contains("Alex"),
            "the operator reads the teammate's name: {}",
            payload.reason
        );
        assert!(
            !payload.reason.contains("backend_engineer_7f3a"),
            "the raw roster id must not leak into operator-visible text: {}",
            payload.reason
        );
        assert_eq!(
            crate::ports::blockers::asked_by(&effect).as_deref(),
            Some("backend_engineer_7f3a"),
            "the card still carries the asking agent so the console can render \"Asked by\""
        );
    })
    .await;
}

/// An agent with no display name still gets a readable card: a blank label
/// falls back to a generic word rather than ever printing the id.
#[tokio::test]
async fn a_blank_display_label_falls_back_to_a_teammate() {
    in_cycle(async {
        let queue = ApprovalRequestQueue::default();
        let tool = EscalateToHumanTool::new(
            queue.clone(),
            "backend_engineer_7f3a".to_string(),
            String::new(),
        );
        tool.execute(serde_json::json!({
            "question": "which environment?",
            "context": "staging is already deployed"
        }))
        .await
        .expect("runs");

        let effect = queue.drain(8).requests[0].effect.clone();
        let payload: BlockerPayload =
            serde_json::from_value(effect.payload.clone()).expect("payload round-trips");
        assert!(payload.reason.contains("a teammate"), "{}", payload.reason);
        assert!(!payload.reason.contains("backend_engineer_7f3a"));
    })
    .await;
}

/// Two agents asking distinct questions in the same turn race through
/// `execute` concurrently — nothing upstream of this tool serialises the
/// calls — so both must still land their own card rather than one
/// silently losing to the other on the shared queue's `Mutex`.
///
/// Driven from two worker threads through a [`Barrier`], not from
/// `tokio::join!`: `execute` has no suspension point around its
/// synchronous `push`, so joined futures are polled to completion one
/// after the other on a single task. That arrangement exercises two serial
/// inserts and would pass unchanged if simultaneous calls could lose a
/// card — which is the only thing this test exists to rule out.
///
/// Repeated, because the barrier releases both workers before either
/// reaches `push` rather than at `push` itself: a single round can
/// interleave benignly. Making the window certain would mean a test hook
/// inside the queue every caller pays for, so the rounds buy it instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_questions_from_different_agents_both_park() {
    in_cycle(async {
        use std::sync::{Arc, Barrier};

        for round in 0..20 {
            let queue = ApprovalRequestQueue::default();
            let finance = EscalateToHumanTool::new(
                queue.clone(),
                "finance".to_string(),
                "finance".to_string(),
            );
            let legal =
                EscalateToHumanTool::new(queue.clone(), "legal".to_string(), "legal".to_string());
            let gate = Arc::new(Barrier::new(2));

            let ask = |tool: EscalateToHumanTool, question: &'static str, gate: Arc<Barrier>| {
                tokio::task::spawn_blocking(move || {
                    gate.wait();
                    tokio::runtime::Handle::current().block_on(in_cycle(
                        tool.execute(serde_json::json!({ "question": question })),
                    ))
                })
            };
            let a = ask(finance, "approve the Q3 budget?", gate.clone());
            let b = ask(legal, "sign the NDA as-is?", gate.clone());
            assert!(!a.await.expect("joins").expect("runs").is_error);
            assert!(!b.await.expect("joins").expect("runs").is_error);

            let drained = queue.drain(8);
            let reasons: Vec<&String> = drained.requests.iter().map(|r| &r.reason).collect();
            assert_eq!(
                drained.requests.len(),
                2,
                "round {round}: both concurrent questions must reach the queue, not just \
             whichever wins the race: {reasons:?}"
            );
            for question in ["approve the Q3 budget?", "sign the NDA as-is?"] {
                assert!(
                    reasons.iter().any(|reason| reason.contains(question)),
                    "round {round}: the queue must hold each agent's own question, not one of \
                 them twice: {reasons:?}"
                );
            }
        }
    })
    .await;
}

/// The other half of `an_escalation_mints_no_grant`, and the half that is
/// load-bearing in the opposite direction.
///
/// `agent: None` is what stops an approval re-dispatching the agent into
/// asking the same question again. But `None` is also what
/// `CycleRunner::settle_approval` reads as *a native effect the runtime
/// performs*, and its fall-through hands the effect to
/// `execute_effect_once` — which for a blocker payload ledgers a phantom
/// spend and routes nothing while reporting success. The only thing
/// standing between those two is
/// [`is_blocker_effect`](crate::ports::blockers::is_blocker_effect), which
/// matches on the effect **kind string**. Nothing else couples the kind
/// this tool stamps to the prefix that guard looks for, so a rename on
/// either side reopens the fall-through silently.
#[tokio::test]
async fn an_escalation_is_recognisable_as_a_blocker_so_approval_cannot_execute_it_natively() {
    in_cycle(async {
        let queue = ApprovalRequestQueue::default();
        tool(&queue)
            .execute(serde_json::json!({ "question": "staging or prod?" }))
            .await
            .expect("runs");
        let effect = queue.drain(8).requests[0].effect.clone();
        assert!(
            effect.agent.is_none(),
            "a grant here would re-ask the question"
        );
        assert!(
            crate::ports::blockers::is_blocker_effect(&effect),
            "an agent-None effect that is not recognised as a blocker falls through to native \
         execution on approval: {}",
            effect.kind
        );
        assert!(
            serde_json::from_value::<BlockerPayload>(effect.payload.clone()).is_ok(),
            "the resolve path reads the payload back off the parked effect to carry the step: {:?}",
            effect.payload
        );
        assert!(
            effect.amount_usd.is_none(),
            "a question costs nothing; an amount here is what a phantom spend would be ledgered \
         from"
        );
    })
    .await;
}

/// STATE-axis (REQ-002): `push`'s de-duplication is per [`ApprovalScope`]
/// (issue #439) — two different turns asking the identical question are
/// two requests, not one collapsed into the other. This is the flip side
/// of `a_repeated_identical_escalation_collapses_but_a_distinct_one_survives`
/// (`policy.rs`), which proves the collapse WITHIN one turn; this proves a
/// prior turn's already-drained card does not leave state that suppresses
/// an identical question asked again in a later, separate turn.
#[tokio::test]
async fn escalate_to_human_repeated_across_different_turns_is_not_deduped() {
    let queue = ApprovalRequestQueue::default();
    let tool = tool(&queue);

    let first_turn = queue.claim(crate::harness::built_in::policy::ApprovalScope::Run(
        "run-1".to_string(),
    ));
    let first_drain = first_turn
        .scoped(async {
            tool.execute(serde_json::json!({ "question": "staging or prod?" }))
                .await
                .expect("first turn runs");
            queue.drain(8)
        })
        .await;
    assert_eq!(
        first_drain.requests.len(),
        1,
        "the first turn's own question lands"
    );
    drop(first_turn);

    let second_turn = queue.claim(crate::harness::built_in::policy::ApprovalScope::Run(
        "run-2".to_string(),
    ));
    let second_drain = second_turn
        .scoped(async {
            tool.execute(serde_json::json!({ "question": "staging or prod?" }))
                .await
                .expect("second turn runs");
            queue.drain(8)
        })
        .await;
    assert_eq!(
        second_drain.requests.len(),
        1,
        "a later, separate turn asking the identical question must not read as a duplicate \
         of a card the first turn already drained and lost scope of"
    );
}

#[tokio::test]
async fn escalate_to_human_exactly_at_the_cap_produces_no_overflow() {
    in_cycle(async {
        use crate::harness::built_in::policy::MAX_APPROVAL_REQUESTS_PER_TURN;

        let queue = ApprovalRequestQueue::default();
        let tool = tool(&queue);
        for i in 0..MAX_APPROVAL_REQUESTS_PER_TURN {
            let outcome = tool
                .execute(serde_json::json!({ "question": format!("question {i}?") }))
                .await
                .expect("runs");
            assert!(!outcome.is_error, "question {i}: {}", outcome.text());
        }

        let drained = queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN);
        assert_eq!(
            drained.requests.len(),
            MAX_APPROVAL_REQUESTS_PER_TURN,
            "exactly the cap's worth of distinct questions must all land"
        );
        assert_eq!(
            drained.discarded, 0,
            "at exactly the cap, nothing overflows"
        );
        assert!(
            drained.overflow_notice().is_none(),
            "no notice is owed when nothing was dropped"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_questions_compete_for_the_final_slot_without_silent_loss() {
    in_cycle(async {
        use crate::harness::built_in::policy::MAX_APPROVAL_REQUESTS_PER_TURN;
        use std::sync::{Arc, Barrier};

        for round in 0..20 {
            let queue = ApprovalRequestQueue::default();
            for i in 0..MAX_APPROVAL_REQUESTS_PER_TURN - 1 {
                let asked = tool(&queue)
                    .execute(serde_json::json!({ "question": format!("existing question {i}") }))
                    .await
                    .expect("the tool runs");
                assert!(!asked.is_error, "{}", asked.text());
            }

            let barrier = Arc::new(Barrier::new(2));
            let ask = |agent: &str, question: &'static str| {
                let tool =
                    EscalateToHumanTool::new(queue.clone(), agent.to_string(), agent.to_string());
                let barrier = barrier.clone();
                tokio::task::spawn_blocking(move || {
                    barrier.wait();
                    tokio::runtime::Handle::current().block_on(in_cycle(
                        tool.execute(serde_json::json!({ "question": question })),
                    ))
                })
            };
            let finance = ask("finance", "approve the final budget?");
            let legal = ask("legal", "approve the final contract?");
            let results = [
                (
                    "approve the final budget?",
                    finance.await.expect("joins").expect("the tool runs"),
                ),
                (
                    "approve the final contract?",
                    legal.await.expect("joins").expect("the tool runs"),
                ),
            ];
            assert_eq!(
                results
                    .iter()
                    .filter(|(_, result)| !result.is_error)
                    .count(),
                1,
                "round {round}: one remaining blocker slot must have exactly one successful caller"
            );

            let drained = queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN);
            assert_eq!(drained.requests.len(), MAX_APPROVAL_REQUESTS_PER_TURN);
            assert_eq!(
                drained.discarded, 0,
                "no accepted question may be discarded"
            );
            for i in 0..MAX_APPROVAL_REQUESTS_PER_TURN - 1 {
                assert!(
                    drained
                        .requests
                        .iter()
                        .any(|r| r.reason == format!("existing question {i}"))
                );
            }
            for (question, result) in results {
                let retained = drained.requests.iter().any(|r| r.reason == question);
                assert_eq!(retained, !result.is_error, "round {round}: {question}");
                if result.is_error {
                    assert!(result.text().contains("not raised"), "{}", result.text());
                }
            }
        }
    })
    .await;
}

#[tokio::test]
async fn a_full_run_accepts_its_duplicate_without_consuming_another_runs_capacity() {
    use crate::harness::built_in::policy::{ApprovalScope, MAX_APPROVAL_REQUESTS_PER_TURN};

    let queue = ApprovalRequestQueue::default();
    let full = queue.claim(ApprovalScope::Run("full".to_string()));
    let other = queue.claim(ApprovalScope::Run("other".to_string()));
    let tool = tool(&queue);
    full.scoped(async {
        for i in 0..MAX_APPROVAL_REQUESTS_PER_TURN {
            let asked = tool
                .execute(serde_json::json!({ "question": format!("question {i}") }))
                .await
                .expect("the tool runs");
            assert!(!asked.is_error, "{}", asked.text());
        }
        let duplicate = tool
            .execute(serde_json::json!({ "question": "question 0" }))
            .await
            .expect("the tool runs");
        assert!(
            !duplicate.is_error,
            "the existing question is already queued"
        );
        let refused = tool
            .execute(serde_json::json!({ "question": "new question" }))
            .await
            .expect("the tool runs");
        assert!(
            refused.is_error,
            "a new question must be refused at the cap"
        );
    })
    .await;

    let independent = other
        .scoped(tool.execute(serde_json::json!({ "question": "question 0" })))
        .await
        .expect("the tool runs");
    assert!(
        !independent.is_error,
        "a different run has its own capacity"
    );
    let full_drain = full
        .scoped(async { queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN) })
        .await;
    assert_eq!(full_drain.requests.len(), MAX_APPROVAL_REQUESTS_PER_TURN);
    assert_eq!(full_drain.discarded, 0);
    let other_drain = other
        .scoped(async { queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN) })
        .await;
    assert_eq!(other_drain.requests.len(), 1);
    assert_eq!(other_drain.requests[0].reason, "question 0");
    assert_eq!(other_drain.discarded, 0);
}

#[tokio::test]
async fn a_question_nothing_can_record_is_an_error_and_parks_nothing() {
    let queue = ApprovalRequestQueue::default();
    let result = tool(&queue)
        .execute(serde_json::json!({ "question": "staging or prod?" }))
        .await
        .expect("the tool runs");

    assert!(
        result.is_error,
        "an unrecorded question must not read as asked"
    );
    assert!(
        result.text().contains("was not recorded"),
        "{}",
        result.text()
    );
    assert!(result.text().contains("Do not tell anyone you asked"));
    assert!(in_cycle(async { queue.drain(8) }).await.requests.is_empty());
}
