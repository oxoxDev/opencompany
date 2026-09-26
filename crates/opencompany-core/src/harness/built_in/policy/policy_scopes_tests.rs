use super::*;

// Issue #470: the `composio_execute` fixtures are built here, from the same
// key the classifier reads, so a call in a test reaches the same catalogue
// lookup a call in production does.
use super::policy_test_helpers_tests::*;

// --- scopes: a turn's entries are its own, and nobody else's (#439) ------

/// The regression #395 narrowed and #439 removes: a workflow node parks its
/// gated calls while a chat cycle is part-way through its own turn.
///
/// #395 did this with a boundary index, which worked only because the queue
/// was append-only — it encoded a *guess* about who wrote what. The scope
/// encodes the fact, so the node cannot see the cycle's entry at all rather
/// than merely declining to take it.
#[tokio::test]
async fn a_run_drains_its_own_entries_and_cannot_see_another_turns() {
    let queue = ApprovalRequestQueue::default();

    // A chat cycle's own turn parked this one and has not drained yet.
    let cycle = queue.claim(ApprovalScope::Cycle);
    cycle
        .scoped(async { queue.push(gated("chat.thing")) })
        .await;

    // The workflow node's turn parks two, concurrently.
    let run = queue.claim(ApprovalScope::Run("run-1".into()));
    let taken = run
        .scoped(async {
            queue.push(gated("node.thing"));
            queue.push(gated("node.other"));
            assert_eq!(queue.queued(), 2, "the run sees only its own");
            queue.drain(10)
        })
        .await;
    assert_eq!(
        taken
            .requests
            .iter()
            .map(|r| r.tool.as_str())
            .collect::<Vec<_>>(),
        vec!["node.thing", "node.other"],
        "only the node's own entries come back"
    );

    // The cycle's entry is untouched and still drains as its own.
    let drained = cycle.scoped(async { queue.drain(10) }).await;
    assert_eq!(
        drained
            .requests
            .iter()
            .map(|r| r.tool.as_str())
            .collect::<Vec<_>>(),
        vec!["chat.thing"],
        "the chat cycle's entry survived the run's drain, unmoved"
    );
}

/// The race a boundary index could never fix: **two concurrent workflow
/// runs**. Both took a boundary against one shared vector, so the later
/// `split_off` swallowed the earlier run's tail. Scopes make them disjoint.
#[tokio::test]
async fn two_concurrent_runs_cannot_take_each_others_entries() {
    let queue = ApprovalRequestQueue::default();
    let one = queue.claim(ApprovalScope::Run("run-1".into()));
    let two = queue.claim(ApprovalScope::Run("run-2".into()));

    // Interleaved exactly as two spawned runs would be.
    one.scoped(async { queue.push(gated("one.a")) }).await;
    two.scoped(async { queue.push(gated("two.a")) }).await;
    one.scoped(async { queue.push(gated("one.b")) }).await;

    let two_got = two.scoped(async { queue.drain(10) }).await;
    assert_eq!(
        two_got
            .requests
            .iter()
            .map(|r| r.tool.as_str())
            .collect::<Vec<_>>(),
        vec!["two.a"],
        "run-2 must not swallow run-1's tail",
    );
    let one_got = one.scoped(async { queue.drain(10) }).await;
    assert_eq!(
        one_got
            .requests
            .iter()
            .map(|r| r.tool.as_str())
            .collect::<Vec<_>>(),
        vec!["one.a", "one.b"],
        "and run-1 keeps both of its own, in order",
    );
}

/// A push outside every claim is refused, and a chat cycle that claims
/// afterwards must not find it: nothing unclaimed can be filed under an
/// unrelated cycle's task or thread.
#[tokio::test]
async fn an_unclaimed_push_is_refused_and_never_reaches_a_later_cycle() {
    let queue = ApprovalRequestQueue::default();
    assert_eq!(queue.push(gated("orphan")), ApprovalPush::Unclaimed);

    let cycle = queue.claim(ApprovalScope::Cycle);
    let drained = cycle.scoped(async { queue.drain(10) }).await;
    assert!(
        drained.requests.is_empty(),
        "an unrelated cycle must not adopt an orphaned request: {:?}",
        drained.requests
    );
}

/// A seat's claim drains its own bucket and nothing else, even while a cycle
/// holds entries of its own.
#[tokio::test]
async fn a_seat_claim_drains_only_its_own_entries() {
    let queue = ApprovalRequestQueue::default();
    let cycle = queue.claim(ApprovalScope::Cycle);
    let seat = queue.claim(ApprovalScope::Seat("desk/episode/writer".into()));

    cycle
        .scoped(async { queue.push(gated("chat.thing")) })
        .await;
    assert_eq!(
        seat.scoped(async { queue.push(gated("seat.thing")) }).await,
        ApprovalPush::Queued
    );

    let seat_got = seat.drain(10);
    assert_eq!(
        seat_got
            .requests
            .iter()
            .map(|r| r.tool.as_str())
            .collect::<Vec<_>>(),
        vec!["seat.thing"],
    );
    assert_eq!(
        queue.len_in(&ApprovalScope::Cycle),
        1,
        "the cycle's entry stays"
    );
}

/// A claim's own drain still caps and counts what it discards.
#[tokio::test]
async fn a_claim_drain_caps_and_counts_overflow() {
    let queue = ApprovalRequestQueue::default();
    let seat = queue.claim(ApprovalScope::Seat("desk/episode/writer".into()));
    seat.scoped(async {
        for i in 0..(MAX_APPROVAL_REQUESTS_PER_TURN + 2) {
            queue.push(gated(&format!("seat.{i}")));
        }
    })
    .await;

    let drained = seat.drain(MAX_APPROVAL_REQUESTS_PER_TURN);
    assert_eq!(drained.requests.len(), MAX_APPROVAL_REQUESTS_PER_TURN);
    assert_eq!(drained.discarded, 2);
    assert!(drained.overflow_notice().is_some());
}

/// A gated call raised where no approval can be recorded is refused outright,
/// and the refusal says why, rather than claiming the operator was asked.
#[tokio::test]
async fn an_unclaimed_gated_call_is_denied_and_says_why() {
    let (p, queue) = queued_policy("supervised", &[]);
    let decision = p.check(&request("send_email", serde_json::json!({}))).await;
    let ToolPolicyDecision::Deny { reason } = decision else {
        panic!("an unclaimed gated call must be denied: {decision:?}");
    };
    assert!(
        reason.contains("cannot record an approval request"),
        "{reason}"
    );
    assert!(reason.contains("Do not tell anyone you asked"), "{reason}");
    assert_eq!(queue.len_in(&ApprovalScope::Cycle), 0);
}

/// The claim's exit half. A turn that returns early — an error, a steer, an
/// `?` — must not leave its entries for whoever claims that scope next.
/// `clear()` at the top of a cycle never gave this; `Drop` does.
#[tokio::test]
async fn dropping_a_claim_discards_that_scopes_entries() {
    let queue = ApprovalRequestQueue::default();
    {
        let cycle = queue.claim(ApprovalScope::Cycle);
        cycle.scoped(async { queue.push(gated("abandoned")) }).await;
        assert_eq!(queue.len_in(&ApprovalScope::Cycle), 1, "parked mid-turn");
    }
    // Observed WITHOUT claiming. Asserting through a fresh claim would pass
    // even with `Drop` removed, because `claim` clears on entry too — this
    // assertion was vacuous until it read the bucket directly.
    assert_eq!(
        queue.len_in(&ApprovalScope::Cycle),
        0,
        "the abandoned entries must go when the claim does, not when the \
         next claim happens to clear them",
    );
}

/// De-duplication is per scope. Two turns asking for the same tool are two
/// asks; collapsing them would hide one turn's request behind another's.
#[tokio::test]
async fn duplicate_suppression_is_per_scope_not_global() {
    let queue = ApprovalRequestQueue::default();
    let cycle = queue.claim(ApprovalScope::Cycle);
    let run = queue.claim(ApprovalScope::Run("run-1".into()));

    cycle
        .scoped(async {
            queue.push(gated("same"));
            queue.push(gated("same"));
            assert_eq!(queue.queued(), 1, "a retry within one turn is one ask");
        })
        .await;
    run.scoped(async {
        queue.push(gated("same"));
        assert_eq!(queue.queued(), 1, "the other turn's ask is its own");
    })
    .await;
}

/// Issue #439's half of the grant-lifetime guarantee, alongside
/// `grants_survive_a_queue_clear`.
///
/// A grant is minted by one turn's decision and redeemed by a different,
/// later one, so it belongs to the company and never to a scope. If it had
/// been folded into the per-scope map, dropping the claim would take it —
/// and approvals would fail in exactly their own happy path.
#[tokio::test]
async fn grants_outlive_a_scope() {
    let grants = GrantSet::default();
    let queue = ApprovalRequestQueue::with_grants(grants.clone());
    let args = serde_json::json!({ "to": "a@b.test" });
    grants.grant(GrantedCall {
        approval_id: crate::ports::types::ApprovalId::new("a1"),
        agent: "finance".to_string(),
        tool: "composio_execute".to_string(),
        args: args.clone(),
        at_millis: 1_000,
        origin_thread: None,
        origin_parent: None,
        origin_task: None,
    });

    {
        let cycle = queue.claim(ApprovalScope::Cycle);
        cycle.scoped(async { queue.push(gated("whatever")) }).await;
    }

    assert!(
        queue
            .grants()
            .consume("finance", "composio_execute", &args)
            .is_some(),
        "the scope went; the grant did not",
    );
}

/// The trap the derived `Default` used to hide: a queue built with
/// `default()` has its **own** grant set, so a grant minted elsewhere can
/// never be redeemed through it and every approval re-parks forever.
///
/// Production uses `with_grants` and is safe; this pins the difference so
/// the hazard is a stated property rather than a footgun — and so a future
/// per-scope refactor cannot reach for `default()` and silently scope
/// grants along with the requests.
#[test]
fn grants_are_not_shared_by_default() {
    let shared = GrantSet::default();
    let args = serde_json::json!({ "to": "a@b.test" });
    let call = GrantedCall {
        approval_id: crate::ports::types::ApprovalId::new("a1"),
        agent: "finance".to_string(),
        tool: "composio_execute".to_string(),
        args: args.clone(),
        at_millis: 1_000,
        origin_thread: None,
        origin_parent: None,
        origin_task: None,
    };
    shared.grant(call.clone());

    assert!(
        ApprovalRequestQueue::default()
            .grants()
            .consume("finance", "composio_execute", &args)
            .is_none(),
        "a default queue cannot see a grant minted anywhere else",
    );
    assert!(
        ApprovalRequestQueue::with_grants(shared)
            .grants()
            .consume("finance", "composio_execute", &args)
            .is_some(),
        "…and `with_grants` is the constructor that can — the one production uses",
    );
}

/// A queue nobody installed stays inert — the default policy behaves exactly
/// as it did before #172 for every non-harness construction site.
#[tokio::test]
async fn a_policy_without_a_shared_queue_still_decides_normally() {
    in_cycle(async {
        let p = policy("supervised", &[], None);
        assert!(matches!(
            p.check(&request("send_email", serde_json::json!({}))).await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
    })
    .await;
}

/// Issue #1458: a standing denial is only **enforced** on the agent turn
/// path, where openhuman treats a `Deny` verdict as fail-closed. The
/// workflow gate deliberately does not honour `Deny`
/// (`src/workflows/gate.rs`), so this policy must not advertise one for a
/// workflow subject — even if a stale denial is sitting in the set — or the
/// gate would be handed a verdict it is documented to ignore and the
/// operator's "don't ask again" would be silently dropped on the next run.
#[tokio::test]
async fn a_standing_deny_is_not_advertised_for_a_workflow_subject() {
    in_cycle(async {
        let grants = GrantSet::default();
        let queue = ApprovalRequestQueue::with_grants(grants.clone());
        grants.grant_standing(crate::runtime::grants::StandingGrant {
            id: crate::runtime::grants::GrantId::new("deny-1"),
            agent: String::new(),
            workflow: Some("sports_digest".to_string()),
            tool: "web_fetch".to_string(),
            verdict: Verdict::Deny,
            granted_by: crate::ports::types::Actor {
                kind: crate::ports::types::ActorKind::User,
                id: "user-1".into(),
            },
            approval_id: crate::ports::types::ApprovalId::new("appr-1"),
            at_millis: 1_000,
            expires_at_millis: crate::ports::now_millis() + 60 * 60 * 1000,
            origin_thread: None,
            origin_parent: None,
            origin_task: None,
            scope: Some("https://docs.rs".to_string()),
        });

        let p = policy("full", &["web_fetch"], None)
            .with_requests(queue)
            .with_workflow("sports_digest");
        let decision = p
            .check(&request(
                "web_fetch",
                serde_json::json!({ "url": "https://docs.rs/x" }),
            ))
            .await;
        assert!(
            !matches!(decision, ToolPolicyDecision::Deny { .. }),
            "a workflow standing denial must not be advertised on the gate path: {decision:?}"
        );
    })
    .await;
}

/// INPUT-axis (TOOL-006): `standing_deny_applies` derives the call's own
/// scope from its arguments (`standing_scope_of`) before matching it
/// against the grant's stored scope. A call with no `url` at all — a
/// malformed shape relative to what `web_fetch` normally carries —
/// resolves to no scope, and a scoped denial requires an EXACT match
/// (`admits_scope`), so it must not apply to a scope-less call rather
/// than being (mis)treated as a wildcard match either way.
#[tokio::test]
async fn a_scoped_standing_deny_does_not_apply_to_a_call_with_no_url_argument() {
    let grants = GrantSet::default();
    let queue = ApprovalRequestQueue::with_grants(grants.clone());
    grants.grant_standing(agent_standing_deny(
        "deny-1",
        "engineer",
        crate::ports::now_millis() + 60 * 60 * 1000,
    ));

    let p = policy("full", &[], None)
        .with_requests(queue)
        .with_agent("engineer");
    let decision = p.check(&request("web_fetch", serde_json::json!({}))).await;
    assert!(
        !matches!(decision, ToolPolicyDecision::Deny { .. }),
        "a scoped denial must not match a call whose scope could not be computed at all: \
         {decision:?}"
    );
}

/// CONC-axis (TOOL-006): unlike a single-use grant, a standing denial is
/// never consumed — two concurrent calls against the SAME live denial
/// must both see it, with no race letting one slip through as if the
/// first call had "used it up". Driven from real worker threads and a
/// barrier, not `tokio::join!` — `check` has no suspension point here to
/// interleave two joined futures on, so they would just run serially and
/// prove nothing about a race.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_calls_against_the_same_standing_deny_are_both_refused() {
    use std::sync::{Arc, Barrier};

    let grants = GrantSet::default();
    let queue = ApprovalRequestQueue::with_grants(grants.clone());
    grants.grant_standing(agent_standing_deny(
        "deny-1",
        "engineer",
        crate::ports::now_millis() + 60 * 60 * 1000,
    ));
    let p = Arc::new(
        policy("full", &[], None)
            .with_requests(queue)
            .with_agent("engineer"),
    );
    let gate = Arc::new(Barrier::new(2));
    let call = |p: Arc<ApprovalPolicy>, gate: Arc<Barrier>| {
        tokio::task::spawn_blocking(move || {
            gate.wait();
            tokio::runtime::Handle::current().block_on(p.check(&request(
                "web_fetch",
                serde_json::json!({ "url": "https://docs.rs/x" }),
            )))
        })
    };
    let a = call(p.clone(), gate.clone());
    let b = call(p, gate);
    let (a, b) = (a.await.expect("joins"), b.await.expect("joins"));
    assert!(matches!(a, ToolPolicyDecision::Deny { .. }), "{a:?}");
    assert!(matches!(b, ToolPolicyDecision::Deny { .. }), "{b:?}");
}

/// FAIL-axis (TOOL-006): a standing denial past its own TTL is stale data
/// — the mint side's sweep may not have gotten to it yet — and must not
/// keep enforcing a refusal the operator's decision no longer covers.
#[tokio::test]
async fn an_expired_standing_deny_no_longer_applies() {
    let grants = GrantSet::default();
    let queue = ApprovalRequestQueue::with_grants(grants.clone());
    grants.grant_standing(agent_standing_deny(
        "deny-1",
        "engineer",
        crate::ports::now_millis().saturating_sub(1_000),
    ));

    let p = policy("full", &[], None)
        .with_requests(queue)
        .with_agent("engineer");
    let decision = p
        .check(&request(
            "web_fetch",
            serde_json::json!({ "url": "https://docs.rs/x" }),
        ))
        .await;
    assert!(
        !matches!(decision, ToolPolicyDecision::Deny { .. }),
        "an expired standing denial must not still be enforced: {decision:?}"
    );
}

/// BOUND-axis (TOOL-006): the expiry boundary is strictly `<`
/// (`StandingGrant::is_live_at`) — live comfortably before its deadline,
/// already expired exactly AT it. Pinned through the policy entry point,
/// not just the grant set directly, so a change to either side of that
/// `<` is caught where it is actually consulted.
///
/// The "live" side uses a generous window rather than the deadline minus
/// one millisecond: `check` calls `now_millis()` again internally, so a
/// one-millisecond margin captured before the call is not guaranteed to
/// survive the dispatch to `standing_deny_applies` and would make this
/// test flaky on nothing but scheduling noise. The "expired" side has no
/// such problem — real time only moves forward, so a deadline equal to a
/// `now` captured strictly before the call is guaranteed to have already
/// passed by the time `check` reads the clock again.
#[tokio::test]
async fn a_standing_deny_expires_exactly_at_its_deadline_not_after() {
    let live_grants = GrantSet::default();
    live_grants.grant_standing(agent_standing_deny(
        "deny-1",
        "engineer",
        crate::ports::now_millis() + 60 * 60 * 1000,
    ));
    let live = policy("full", &[], None)
        .with_requests(ApprovalRequestQueue::with_grants(live_grants))
        .with_agent("engineer");
    assert!(
        matches!(
            live.check(&request(
                "web_fetch",
                serde_json::json!({ "url": "https://docs.rs/x" })
            ))
            .await,
            ToolPolicyDecision::Deny { .. }
        ),
        "comfortably before its deadline the denial must still be live"
    );

    let now = crate::ports::now_millis();
    let expired_grants = GrantSet::default();
    expired_grants.grant_standing(agent_standing_deny("deny-1", "engineer", now));
    let expired = policy("full", &[], None)
        .with_requests(ApprovalRequestQueue::with_grants(expired_grants))
        .with_agent("engineer");
    assert!(
        !matches!(
            expired
                .check(&request(
                    "web_fetch",
                    serde_json::json!({ "url": "https://docs.rs/x" })
                ))
                .await,
            ToolPolicyDecision::Deny { .. }
        ),
        "at the deadline instant itself (now already >= expires_at_millis by the time \
         `check` reads the clock) the denial must already read as expired"
    );
}
