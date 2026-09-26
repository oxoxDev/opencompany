use super::*;

// Issue #470: the `composio_execute` fixtures are built here, from the same
// key the classifier reads, so a call in a test reaches the same catalogue
// lookup a call in production does.
use super::policy_test_helpers_tests::*;
use crate::policy::test_support::{
    COMPOSIO_OTHER_SEND_SLUG, COMPOSIO_READ_SLUG, COMPOSIO_SEND_SLUG, composio_args,
    composio_read_args, composio_send_args, composio_unclassified_args,
    composio_unclassified_args_numbered,
};

// --- The park queue (issue #172) ----------------------------------------

/// The core of #172: a `RequireApproval` decision no longer evaporates into
/// the model's transcript — it is recorded, with the call projected onto the
/// effect the operator will see, so the runtime can park it.
#[tokio::test]
async fn require_approval_records_the_request_to_park() {
    in_cycle(async {
        let (p, queue) = queued_policy("supervised", &[]);
        let args = composio_send_args();
        assert!(matches!(
            p.check(&request("composio_execute", args.clone())).await,
            ToolPolicyDecision::RequireApproval { .. }
        ));

        let queued = queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN).requests;
        assert_eq!(queued.len(), 1, "the gated call was recorded");
        assert_eq!(queued[0].tool, "composio_execute");
        assert_eq!(queued[0].effect.kind, "composio_execute");
        assert_eq!(queued[0].effect.group, EffectGroup::Send);
        assert_eq!(queued[0].effect.payload, args);
        assert!(
            queued[0].reason.contains("supervised"),
            "the operator-facing reason rides along: {}",
            queued[0].reason
        );
    })
    .await;
}

/// Issue #470, at the layer that projects a blocked call onto the effect
/// the operator's card is built from.
///
/// The fixtures above used to name their action under a key nothing reads,
/// so every Composio test in this module classified through the
/// unknown-is-a-send fallback and none of them ever reached the catalogue.
/// Read and send came out identical, and a regression in the split would
/// have failed nothing here. This asserts they come out different, and
/// asserts the classification rather than the parking decision — so it
/// stays honest across issue #559, which changes whether a read parks but
/// not what it is.
#[tokio::test]
async fn a_composio_read_and_a_composio_send_are_classified_differently() {
    in_cycle(async {
        let read = composio_read_args();
        let send = composio_send_args();

        assert_eq!(
            classify_group("composio_execute", &read),
            EffectGroup::Other,
            "`{COMPOSIO_READ_SLUG}` is tagged `Read` in the vendored catalogue; \
         if this fails the lookup is not being reached"
        );
        assert!(
            grantable("composio_execute", &read),
            "a read scoped to one connected account is what a standing grant \
         can honestly describe"
        );

        assert_eq!(
            classify_group("composio_execute", &send),
            EffectGroup::Send,
            "`{COMPOSIO_SEND_SLUG}` is tagged `Write`"
        );
        assert!(!grantable("composio_execute", &send));

        // The cautious fallback still has its own coverage, and still says
        // send — but now because the catalogue was asked and had no answer,
        // not because the classifier never saw an action at all.
        let unknown = composio_unclassified_args();
        assert_eq!(
            classify_group("composio_execute", &unknown),
            EffectGroup::Send
        );
        assert!(!grantable("composio_execute", &unknown));

        // And the split survives the round trip through the park queue: the
        // group asserted above is the one the operator's card is built from.
        let (p, queue) = queued_policy("supervised", &[]);
        let _ = p.check(&request("composio_execute", send.clone())).await;
        let queued = queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN).requests;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].effect.group, EffectGroup::Send);
        assert_eq!(
            queued[0].effect.payload, send,
            "the card shows the arguments the agent actually sent, action key \
         included"
        );
    })
    .await;
}

/// Issue #559, at the gate an agent actually hits: a Composio read runs
/// under `supervised` instead of parking, and nothing is queued for a
/// human — while a send on the same tool still parks.
///
/// This is the behaviour the issue is about. `a_composio_read_and_a_
/// composio_send_are_classified_differently` above pins what the two calls
/// *are*; this pins what the desk *does* with them, which is the part an
/// operator notices when every page of a mailbox raises a card.
#[tokio::test]
async fn a_composio_read_runs_under_supervision_without_parking() {
    in_cycle(async {
        let (p, queue) = queued_policy("supervised", &[]);

        assert_eq!(
            p.check(&request("composio_execute", composio_read_args()))
                .await,
            ToolPolicyDecision::Allow,
            "reading a connected account changes nothing and costs nothing"
        );
        assert_eq!(
            queue.queued(),
            0,
            "no card was raised, so no human was interrupted"
        );

        // Paging the same list is not a second decision, because there was
        // never a first one. This is the symptom the issue opens with.
        for _ in 0..5 {
            let _ = p
                .check(&request("composio_execute", composio_read_args()))
                .await;
        }
        assert_eq!(queue.queued(), 0);

        // The send half is untouched: same tool, same desk, still parks.
        assert!(matches!(
            p.check(&request("composio_execute", composio_send_args()))
                .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
        assert_eq!(queue.queued(), 1);
    })
    .await;
}

/// A `readonly` desk still denies the read. That tier's contract is that
/// nothing outside the company is reached at all — #559 moves the read out
/// of the parking bucket, not out of the reaching-outward one.
#[tokio::test]
async fn a_readonly_desk_still_denies_a_composio_read() {
    let p = policy("readonly", &[], None);
    assert!(matches!(
        p.check(&request("composio_execute", composio_read_args()))
            .await,
        ToolPolicyDecision::Deny { .. }
    ));
    assert!(matches!(
        p.check(&request("composio_execute", composio_send_args()))
            .await,
        ToolPolicyDecision::Deny { .. }
    ));
}

/// `always_approve` parks regardless of tier — including under `full` — so
/// that arm has to record its request too.
#[tokio::test]
async fn always_approve_records_the_request_even_under_full_autonomy() {
    in_cycle(async {
        let (p, queue) = queued_policy("full", &["payment"]);
        assert!(matches!(
            p.check(&request(
                "payment.send",
                serde_json::json!({ "amount_usd": 40.0 })
            ))
            .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
        let queued = queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN).requests;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].effect.kind, "payment.send");
        assert_eq!(queued[0].effect.amount_usd, Some(40.0));
    })
    .await;
}

/// Allowed and denied calls leave the queue alone: only a call actually
/// waiting on the operator may reach the Approvals page.
#[tokio::test]
async fn allow_and_deny_record_nothing() {
    let (supervised, allow_queue) = queued_policy("supervised", &[]);
    assert_eq!(
        supervised
            .check(&request("read_file", serde_json::json!({})))
            .await,
        ToolPolicyDecision::Allow
    );
    assert_eq!(allow_queue.queued(), 0, "an allowed call parks nothing");

    let (readonly, deny_queue) = queued_policy("readonly", &[]);
    assert!(matches!(
        readonly
            .check(&request("publish_post", serde_json::json!({})))
            .await,
        ToolPolicyDecision::Deny { .. }
    ));
    assert_eq!(
        deny_queue.queued(),
        0,
        "a denied call is refused outright, never parked"
    );
}

/// openhuman blocks a gated call but lets the turn continue, so a model that
/// keeps re-trying the same tool must not stack up duplicate approvals.
#[tokio::test]
async fn a_retried_call_is_recorded_once() {
    in_cycle(async {
        let (p, queue) = queued_policy("supervised", &[]);
        let args = composio_send_args();
        for _ in 0..3 {
            let _ = p.check(&request("composio_execute", args.clone())).await;
        }
        assert_eq!(queue.queued(), 1, "the same call parks once");

        // A different call to the same tool is a distinct request. Another
        // catalogued send, so the second call is classified rather than merely
        // unrecognised.
        let _ = p
            .check(&request(
                "composio_execute",
                composio_args(COMPOSIO_OTHER_SEND_SLUG),
            ))
            .await;
        assert_eq!(queue.queued(), 2);
    })
    .await;
}

/// The drain is capped, so a runaway turn can't flood the operator's queue.
///
/// The slugs here are deliberately uncatalogued (issue #470): this test
/// wants many calls the queue treats as distinct and is indifferent to what
/// any of them classify as, so naming real actions would only invite a
/// reader to think the classification mattered. They do still land under
/// the real action key, so each one reaches the catalogue lookup and misses
/// it — the honest fallback, rather than a call carrying no action at all.
#[tokio::test]
async fn the_drain_is_capped_and_empties_the_queue() {
    in_cycle(async {
        let (p, queue) = queued_policy("supervised", &[]);
        for i in 0..(MAX_APPROVAL_REQUESTS_PER_TURN + 4) {
            let _ = p
                .check(&request(
                    "composio_execute",
                    composio_unclassified_args_numbered(i),
                ))
                .await;
        }
        let drained = queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN);
        assert_eq!(drained.requests.len(), MAX_APPROVAL_REQUESTS_PER_TURN);
        assert_eq!(queue.queued(), 0, "the overflow is discarded, not carried");

        // Issue #561: and the drain says how many it threw away, rather than
        // handing back a `Vec` indistinguishable from a complete one.
        assert_eq!(
            drained.discarded, 4,
            "12 gated calls, a cap of 8, so 4 were dropped"
        );
        let notice = drained
            .overflow_notice()
            .expect("an overflowing drain has something to tell the operator");
        assert!(
            notice.contains('4'),
            "the count is in the sentence: {notice}"
        );
        assert!(
            notice.contains("not** run") || notice.contains("not run"),
            "the operator must not read this as 'the calls happened, the records \
         were lost': {notice}"
        );
    })
    .await;
}

/// CONC-axis (TOOL-021): the cap in `the_drain_is_capped_and_empties_the_queue`
/// above is proven with sequential pushes — each `check` is awaited before
/// the next fires. This drives the same overflow from genuinely concurrent
/// pushes, via real worker threads and a barrier (not `tokio::join!`, which
/// has no suspension point around `push`'s synchronous body and would just
/// serialise the two futures on one task — the exact false confidence this
/// lane's brief warns about). `push`'s per-scope `Mutex` must make every
/// racing call land exactly once: no card lost to a race, none double
/// counted, and `requests.len() + discarded` must equal the number of
/// calls that actually raced, every round.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_pushes_past_the_cap_are_never_lost_or_double_counted() {
    use std::sync::{Arc, Barrier};

    const RACERS: usize = MAX_APPROVAL_REQUESTS_PER_TURN + 5;

    for round in 0..20 {
        let queue = ApprovalRequestQueue::default();
        let gate = Arc::new(Barrier::new(RACERS));

        let mut handles = Vec::with_capacity(RACERS);
        for i in 0..RACERS {
            let queue = queue.clone();
            let gate = gate.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                gate.wait();
                let request = ApprovalRequest {
                    tool: "composio_execute".to_string(),
                    reason: format!("racer {i}"),
                    effect: Effect {
                        kind: format!("composio.call.{i}"),
                        group: EffectGroup::Other,
                        amount_usd: None,
                        established_thread: false,
                        first_time_counterparty: false,
                        payload: serde_json::json!({ "racer": i }),
                        agent: None,
                        run_id: None,
                    },
                };
                CURRENT_SCOPE.sync_scope(ApprovalScope::Cycle, || queue.push(request));
            }));
        }
        for handle in handles {
            handle.await.expect("racer joins");
        }

        let drained = queue.drain_scope(&ApprovalScope::Cycle, MAX_APPROVAL_REQUESTS_PER_TURN);
        assert_eq!(
            drained.requests.len(),
            MAX_APPROVAL_REQUESTS_PER_TURN,
            "round {round}: the drain must be exactly full, not short a card a race lost"
        );
        assert_eq!(
            drained.discarded,
            RACERS - MAX_APPROVAL_REQUESTS_PER_TURN,
            "round {round}: every racer that did not fit must be counted, not silently \
             dropped from the tally"
        );
        let reasons: std::collections::HashSet<&String> =
            drained.requests.iter().map(|r| &r.reason).collect();
        assert_eq!(
            reasons.len(),
            MAX_APPROVAL_REQUESTS_PER_TURN,
            "round {round}: no racer's card duplicated another's under the race: {reasons:?}"
        );
    }
}

/// The ordinary path says nothing. A notice on every turn would train the
/// operator to ignore the one that matters.
#[tokio::test]
async fn a_drain_under_the_cap_reports_no_overflow() {
    in_cycle(async {
        let (p, queue) = queued_policy("supervised", &[]);
        for i in 0..(MAX_APPROVAL_REQUESTS_PER_TURN - 1) {
            let _ = p
                .check(&request(
                    "composio_execute",
                    composio_unclassified_args_numbered(i),
                ))
                .await;
        }
        let drained = queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN);
        assert_eq!(drained.requests.len(), MAX_APPROVAL_REQUESTS_PER_TURN - 1);
        assert_eq!(drained.discarded, 0);
        assert!(drained.overflow_notice().is_none());
    })
    .await;
}

/// Exactly at the cap is not an overflow. An off-by-one here would cry wolf
/// on the commonest boundary case.
#[tokio::test]
async fn a_drain_exactly_at_the_cap_reports_no_overflow() {
    in_cycle(async {
        let (p, queue) = queued_policy("supervised", &[]);
        for i in 0..MAX_APPROVAL_REQUESTS_PER_TURN {
            let _ = p
                .check(&request(
                    "composio_execute",
                    composio_unclassified_args_numbered(i),
                ))
                .await;
        }
        let drained = queue.drain(MAX_APPROVAL_REQUESTS_PER_TURN);
        assert_eq!(drained.requests.len(), MAX_APPROVAL_REQUESTS_PER_TURN);
        assert_eq!(drained.discarded, 0);
        assert!(drained.overflow_notice().is_none());
    })
    .await;
}

/// One dropped request reads as one, not as "1 calls".
///
/// The nouns agreed from the start; the verbs and pronouns did not, so a
/// single discard read "1 further gated tool call **were** not raised …
/// **They were** not run and **they are** not on the Approvals page". The
/// whole sentence has to agree, not the countable nouns in it — an operator
/// reading a confidently-worded, ungrammatical notice has cause to wonder
/// what else about it is stale.
#[test]
fn the_overflow_notice_is_singular_for_a_single_dropped_request() {
    let drained = DrainedRequests::new(Vec::new(), 1, 8);
    let notice = drained.overflow_notice().expect("one is still an overflow");
    assert!(notice.contains("1 further gated tool call "), "{notice}");
    assert!(!notice.contains("calls"), "{notice}");
    assert!(notice.contains("call was not raised"), "{notice}");
    assert!(notice.contains("It was **not** run"), "{notice}");
    assert!(notice.contains("it is **not** on the"), "{notice}");
    assert!(!notice.contains("were"), "{notice}");
    assert!(!notice.contains("they"), "{notice}");
    assert!(!notice.contains("They"), "{notice}");
}

/// …and the plural is untouched: the agreement fix must not singularise the
/// case that was already right.
#[test]
fn the_overflow_notice_stays_plural_for_several_dropped_requests() {
    let drained = DrainedRequests::new(Vec::new(), 3, 8);
    let notice = drained.overflow_notice().expect("three is an overflow");
    assert!(
        notice.contains("3 further gated tool calls were not"),
        "{notice}"
    );
    assert!(notice.contains("They were **not** run"), "{notice}");
    assert!(notice.contains("they are **not** on the"), "{notice}");
    assert!(!notice.contains(" was "), "{notice}");
}

/// The notice names the cap the drain was actually taken against, not one a
/// caller supplied later.
///
/// `discarded` was always captured at drain time while `cap` arrived at the
/// sentence, so `drain(8)` followed by `overflow_notice(20)` produced a
/// confidently-worded, wrong number for the operator — the same class of
/// defect as the invisible discard #561 fixes. Storing it makes that
/// unrepresentable, and this pins that the stored value is the one used.
#[tokio::test]
async fn the_notice_quotes_the_cap_the_drain_was_taken_against() {
    in_cycle(async {
        let (p, queue) = queued_policy("supervised", &[]);
        for i in 0..5 {
            let _ = p
                .check(&request(
                    "composio_execute",
                    composio_unclassified_args_numbered(i),
                ))
                .await;
        }
        let drained = queue.drain(3);
        assert_eq!(drained.cap(), 3);
        assert_eq!(drained.discarded, 2);
        let notice = drained.overflow_notice().expect("2 were dropped");
        assert!(
            notice.contains("at most 3"),
            "the sentence must quote the cap that did the discarding: {notice}"
        );
        assert!(
            !notice.contains(&MAX_APPROVAL_REQUESTS_PER_TURN.to_string()),
            "and not the constant the call site happened to have in scope: {notice}"
        );
    })
    .await;
}
