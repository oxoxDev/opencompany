//! Runtime tests: approval extension, retirement failures, and thread-root resolution.

use super::CompanyEvent;

/// A runtime with a live event log, for the thread-root tests. Returns the
/// tempdir too: dropping it deletes the log the runtime is reading.
pub(super) async fn runtime_with_events()
-> (crate::company::runtime::CompanyRuntime, tempfile::TempDir) {
    let home_dir = tempfile::Builder::new()
        .prefix("opencompany-parent-")
        .tempdir()
        .expect("tempdir");
    let manifest: crate::company::types::CompanyManifest = toml::from_str(
        r#"
        [company]
        name = "Acme"

        [[agent]]
        id = "ceo"
        role = "Chief"

        [policy]
        mode = "supervised"
        "#,
    )
    .expect("manifest");
    let rt = crate::runtime::RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest)
        .build()
        .await
        .expect("runtime");
    (rt, home_dir)
}

/// A helper effect and a manifest for the extend tests.
fn extend_test_effect() -> crate::ports::types::Effect {
    crate::ports::types::Effect {
        kind: "payment.send".into(),
        group: crate::ports::types::EffectGroup::Spend,
        amount_usd: Some(1_200.0),
        established_thread: false,
        first_time_counterparty: false,
        payload: serde_json::json!({ "to": "vendor@example.test" }),
        agent: Some("ceo".into()),
        run_id: None,
    }
}

/// A **gated tool call** parked by a workflow agent node, in the shape
/// production actually creates (Codex on the B-012 PR, second round).
///
/// This is the path that leaves an attempt reading `WaitingApproval`:
/// `caps::park_gated_calls` journals `ApprovalPolicy::effect_for`'s effect —
/// `kind` is the **tool name**, `run_id` is `None` — under the node's
/// `workflow-node:{run}:{node}` cycle, and the node then settles its attempt
/// `WaitingApproval`. The earlier fixture here paired a `gate_effect` with a
/// hand-made attempt row, a combination no parking path produces, and so
/// reported a fix that could not fire in production as working.
///
/// Returns the attempt row's id — the row the expiry has to find.
async fn park_gated_node_call(
    rt: &std::sync::Arc<crate::company::runtime::CompanyRuntime>,
    approval: &crate::ports::types::ApprovalId,
    lineage: &str,
    node: &str,
    at_millis: u64,
    arm_continuation: bool,
) -> String {
    use crate::ports::runs::RunStatus;
    use crate::ports::types::{Effect, EffectGroup, EventSeq};
    use crate::runtime::journal::{ApprovalConversation, TaskLink};

    let attempt = crate::ports::generate_id();
    rt.runs()
        .create_run(
            rt.id(),
            crate::ports::NewRun::for_workflow_node(attempt.clone(), lineage, node, "ceo"),
        )
        .await
        .unwrap();
    rt.runs()
        .begin_run(rt.id(), &attempt, EventSeq::new(1))
        .await
        .unwrap();
    rt.runs()
        .finish_run(
            rt.id(),
            &attempt,
            crate::ports::runs::RunOutcome::new(RunStatus::WaitingApproval),
        )
        .await
        .unwrap();

    // `effect_for`'s shape, field for field: the tool's own name as the
    // kind, the agent stamped, and **no** `run_id`.
    let effect = Effect {
        kind: "workspace.write".into(),
        group: EffectGroup::Other,
        amount_usd: None,
        established_thread: false,
        first_time_counterparty: false,
        payload: serde_json::json!({ "path": "README.md" }),
        agent: Some("ceo".into()),
        run_id: None,
    };
    let node_turn = crate::runtime::workflow_resume::workflow_node_turn_key(lineage, node);
    if arm_continuation {
        rt.continuations.arm(&node_turn);
    }
    rt.approval_gate
        .rehydrate(approval.clone(), effect.clone(), at_millis);
    rt.journal
        .record_parked(
            approval,
            &effect,
            at_millis,
            TaskLink::Unlinked,
            ApprovalConversation::default(),
            Some(node_turn),
        )
        .await
        .unwrap();
    attempt
}

/// Seeds one parked approval into BOTH the live gate and the durable journal
/// under a fixed id at `at_millis`, exactly as a real park leaves them — the
/// gate answers "is this live?" for extend/sweep, the journal projects the
/// deadline and replays on boot.
pub(super) async fn seed_parked(
    rt: &crate::company::runtime::CompanyRuntime,
    id: &str,
    at_millis: u64,
) -> crate::ports::types::ApprovalId {
    use crate::ports::types::ApprovalId;
    use crate::runtime::journal::{ApprovalConversation, TaskLink};
    let approval = ApprovalId::new(id);
    let effect = extend_test_effect();
    rt.approval_gate
        .rehydrate(approval.clone(), effect.clone(), at_millis);
    rt.journal
        .record_parked(
            &approval,
            &effect,
            at_millis,
            TaskLink::Unlinked,
            ApprovalConversation::default(),
            None,
        )
        .await
        .unwrap();
    approval
}

/// **B-012.** A workflow run parked on a gate stops claiming it is waiting
/// once that approval expires.
///
/// A parked run is recorded `WaitingApproval` — settled, nothing executing,
/// and the status is the row's account of *why* it stopped. Expiry retired
/// the approval and dropped it from the pending set, but nothing revisited
/// the row, so it went on naming an approval no sweep would see again: the
/// Observatory showed a run awaiting a decision while the approvals list
/// showed nothing to decide, and neither screen was wrong about its own
/// data.
#[tokio::test]
async fn an_expired_approval_settles_the_workflow_run_that_was_waiting_on_it() {
    use crate::ports::runs::RunStatus;
    use crate::ports::types::ApprovalId;
    use std::sync::Arc;

    let (rt, _home) = runtime_with_events().await;
    let rt = Arc::new(rt);

    // A workflow node parked on a gate: an attempt row settled
    // `WaitingApproval` and linked to the lineage, plus the gate itself
    // carrying that lineage. Parked at epoch 0 — past any TTL.
    let approval = ApprovalId::new("appr-b012");
    let attempt = park_gated_node_call(&rt, &approval, "wr-b012", "solve", 0, false).await;

    let expired = rt.sweep_expired_approvals().await.unwrap();
    assert!(
        expired.contains(&approval),
        "the sweep must find the epoch-0 park: {expired:?}"
    );

    let row = rt
        .runs()
        .get_run(rt.id(), &attempt)
        .await
        .unwrap()
        .expect("the attempt row survives the sweep");
    assert_eq!(
        row.status,
        RunStatus::Cancelled,
        "a default-denied gate leaves the attempt cancelled, not still waiting"
    );
}

/// **The narrowing** the settle above is scoped by. One expiry must not
/// cancel a *sibling* node still waiting on a live decision.
///
/// A graph can park two nodes on two gates, and `RunFilter` can only ask
/// for the lineage — so "every `WaitingApproval` attempt of this run" is
/// the obvious query and the wrong one. The node is read off the gate's own
/// payload (`gate_node_id`) to close that gap.
#[tokio::test]
async fn an_expiry_leaves_a_sibling_node_still_waiting_on_a_live_gate() {
    use crate::ports::runs::RunStatus;
    use crate::ports::types::ApprovalId;
    use std::sync::Arc;

    let (rt, _home) = runtime_with_events().await;
    let rt = Arc::new(rt);

    // Same lineage, two nodes: one parked at epoch 0 (past any TTL), one
    // parked now (nowhere near it).
    let expiring = ApprovalId::new("appr-expiring");
    let attempt_expiring =
        park_gated_node_call(&rt, &expiring, "wr-two-gates", "solve", 0, false).await;
    let live = ApprovalId::new("appr-live");
    let attempt_live = park_gated_node_call(
        &rt,
        &live,
        "wr-two-gates",
        "review",
        crate::ports::now_millis(),
        false,
    )
    .await;

    let expired = rt.sweep_expired_approvals().await.unwrap();
    assert!(
        expired.contains(&expiring) && !expired.contains(&live),
        "only the epoch-0 park expires: {expired:?}"
    );

    let settled = rt
        .runs()
        .get_run(rt.id(), &attempt_expiring)
        .await
        .unwrap()
        .expect("the expired node's attempt survives");
    assert_eq!(settled.status, RunStatus::Cancelled);

    let sibling = rt
        .runs()
        .get_run(rt.id(), &attempt_live)
        .await
        .unwrap()
        .expect("the sibling's attempt survives");
    assert_eq!(
        sibling.status,
        RunStatus::WaitingApproval,
        "the sibling node is still waiting on a decision nobody has made"
    );
}

/// **An expiry settles its attempt even when it releases a continuation**
/// (Codex on the B-012 PR, third round).
///
/// The tempting reading is that a released node is "still going" and must
/// not be settled. It is not: a continuation runs as a **new** attempt —
/// `RunAttempts` is rebuilt per run and `caps` mints every attempt under
/// `generate_id()` — so nothing ever writes this row again. Skipping it left
/// exactly the stale `WaitingApproval` this issue exists to remove, and the
/// earlier version of this test could not see that, because it asserted only
/// that the cancellation *error* was absent and never looked at the status.
///
/// The scenario is the one that makes a node's batch non-empty, since an
/// expiry alone never does (`ContinuationQueue::decide` banks no event for
/// one): two gated calls on one node, one answered and one expired.
#[tokio::test]
async fn an_expiry_settles_its_attempt_even_when_it_releases_a_continuation() {
    use crate::ports::runs::RunStatus;
    use crate::ports::types::{Actor, ActorKind, ApprovalId, Verdict};
    use std::sync::Arc;

    let (rt, _home) = runtime_with_events().await;
    let rt = Arc::new(rt);

    let approval = ApprovalId::new("appr-released");
    let attempt = park_gated_node_call(&rt, &approval, "wr-released", "solve", 0, true).await;

    // The node's *second* gated call, answered by the operator before the
    // first expires. Its banked event is what makes the released batch
    // non-empty, and so what makes this node continue at all.
    let node_turn = crate::runtime::workflow_resume::workflow_node_turn_key("wr-released", "solve");
    rt.continuations.arm(&node_turn);
    assert!(
        rt.continuations
            .decide(
                &node_turn,
                Some(CompanyEvent::ApprovalResolved {
                    approval_id: ApprovalId::new("appr-answered"),
                    verdict: Verdict::Approve,
                    by: Actor {
                        kind: ActorKind::Operator,
                        id: "operator".into(),
                    },
                }),
            )
            .is_none(),
        "the node is still blocked on the gate that has not expired yet"
    );

    let expired = rt.sweep_expired_approvals().await.unwrap();
    assert!(
        expired.contains(&approval),
        "the sweep must find the epoch-0 park: {expired:?}"
    );

    let row = rt
        .runs()
        .get_run(rt.id(), &attempt)
        .await
        .unwrap()
        .expect("the attempt row survives the sweep");
    assert_eq!(
        row.status,
        RunStatus::Cancelled,
        "the released continuation runs as a NEW attempt, so this row is nobody else's \
         to settle and must not be left reading `WaitingApproval`"
    );
}

/// **A settle that only changes status must not erase what the attempt
/// spent** (Codex on the B-012 PR, third round).
///
/// `RunStore::finish_run` assigns `usage` and `step_count` from the outcome
/// rather than merging, and `RunOutcome::new` zeroes both — so cancelling an
/// expired attempt from a bare outcome silently wipes the tokens and cost it
/// really did spend, on a row the billing surfaces read.
#[tokio::test]
async fn settling_an_expired_attempt_keeps_the_usage_it_recorded() {
    use crate::ports::runs::{RunOutcome, RunStatus};
    use crate::ports::types::{ApprovalId, TokenUsage};
    use std::sync::Arc;

    let (rt, _home) = runtime_with_events().await;
    let rt = Arc::new(rt);

    let approval = ApprovalId::new("appr-usage");
    let attempt = park_gated_node_call(&rt, &approval, "wr-usage", "solve", 0, false).await;

    // What the attempt spent before it parked. Re-settled onto the parked
    // row exactly as a real turn's trace fold would leave it.
    let usage = TokenUsage {
        input: 1_200,
        output: 340,
        cached_input: 0,
        cost_usd: 0.042,
    };
    rt.runs()
        .finish_run(
            rt.id(),
            &attempt,
            RunOutcome::new(RunStatus::WaitingApproval)
                .with_usage(usage)
                .with_step_count(7),
        )
        .await
        .unwrap();

    rt.sweep_expired_approvals().await.unwrap();

    let row = rt
        .runs()
        .get_run(rt.id(), &attempt)
        .await
        .unwrap()
        .expect("the attempt row survives the sweep");
    assert_eq!(row.status, RunStatus::Cancelled);
    assert_eq!(
        row.usage, usage,
        "the expiry changed the status; it must not have erased the spend"
    );
    assert_eq!(row.step_count, 7, "nor the trace it recorded");
}

/// Issue #1865 (Codex review on PR #1883): a late resolve that discovers
/// an approval already past its deadline owes the SAME "expired
/// unanswered" notification the sweep loop files when it discovers the
/// identical deadline first.
///
/// `notify_approval_expired` used to be invoked from nowhere but
/// `sweep_expired_approvals`, so `retire_if_expired` — the path a late
/// `resolve_approval_spawned`/`resolve_approval_amended_spawned` takes
/// when `settle_approval` answers `ResolveReceipt::Expired` — ran the
/// whole four-step `retire_approval` transaction and never told anybody.
/// The exact same expiry notified when the sweeper found it and stayed
/// silent when an operator's late click found it instead.
#[tokio::test]
async fn a_late_resolve_that_discovers_an_expiry_files_the_same_notification_as_the_sweep() {
    use crate::ports::types::{Actor, ActorKind, Verdict};
    use crate::runtime::grants::GrantScope;
    use std::sync::Arc;

    let (rt, _home) = runtime_with_events().await;
    let rt = Arc::new(rt);
    // Parked at epoch 0 — unambiguously past any TTL, the same trick
    // `expired_approval_is_labelled_as_an_expiry_and_carries_its_wait`
    // (src/server/ops/write_test.rs) uses.
    let id = seed_parked(&rt, "appr-late", 0).await;

    let by = Actor {
        kind: ActorKind::Operator,
        id: "owner".into(),
    };
    let (receipt, follow_up) = rt
        .resolve_approval_spawned(&id, Verdict::Approve, by, GrantScope::Once)
        .await
        .unwrap();
    assert!(
        receipt.expired(),
        "an epoch-0 park must read as expired, not approved: {receipt:?}"
    );
    super::join_follow_up(follow_up).await.unwrap();

    let notifications = rt.notifications().list(rt.id(), "owner").await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|n| n.notification.kind == "approval_expired"
                && n.notification.subject.id == id.as_ref()),
        "a late resolve that discovers an expiry must file the same \
         approval_expired notification the sweep files, got {notifications:?}"
    );
}

/// Issue #971 (the projection this issue builds on): a card's deadline is the
/// deadline anchor plus the gate's TTL, resolved once at the single
/// projection point.
#[tokio::test]
async fn pending_approvals_projects_deadline_as_anchor_plus_ttl() {
    let (rt, _home) = runtime_with_events().await;
    seed_parked(&rt, "appr-deadline", 5_000).await;
    let ttl = rt.approval_gate.ttl_millis();
    assert_eq!(
        rt.pending_approvals()[0].expires_at_millis,
        Some(5_000 + ttl),
        "a fresh card's deadline runs from when it was parked"
    );
}

/// **The load-bearing extend test (issue #1805).** Extending moves the live
/// deadline, and — the half that a redeploy silently reverted before this —
/// the move survives a rebuild of the runtime from the same journal, because
/// the extension is replayed and the gate is rehydrated from the moved anchor.
#[tokio::test]
async fn extend_approval_moves_deadline_and_survives_replay() {
    use crate::ports::types::{Actor, ActorKind};

    let home_dir = tempfile::Builder::new()
        .prefix("opencompany-extend-replay-")
        .tempdir()
        .expect("tempdir");
    let manifest: crate::company::types::CompanyManifest =
        toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"supervised\"\n")
            .expect("manifest");

    // First boot: park an old approval, confirm its original deadline, extend.
    let rt1 = crate::runtime::RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest.clone())
        .build()
        .await
        .expect("runtime");
    let id = seed_parked(&rt1, "appr-replay", 1_000).await;
    let ttl = rt1.approval_gate.ttl_millis();
    assert_eq!(
        rt1.pending_approvals()[0].expires_at_millis,
        Some(1_000 + ttl),
        "the fresh deadline runs from the park instant"
    );

    let new_deadline = rt1
        .extend_approval(
            &id,
            Actor {
                kind: ActorKind::User,
                id: "operator".into(),
            },
        )
        .await
        .expect("extend");
    assert!(
        new_deadline > 1_000 + ttl,
        "the live deadline moved out: {new_deadline} vs {}",
        1_000 + ttl
    );
    assert_eq!(
        rt1.pending_approvals()[0].expires_at_millis,
        Some(new_deadline),
        "the live projection reflects the extension immediately"
    );
    drop(rt1);

    // Second boot from the SAME journal — the redeploy the extension has to
    // survive. Without the replayed `ApprovalExtended` the deadline would
    // revert to `1_000 + ttl`.
    let rt2 = crate::runtime::RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest)
        .build()
        .await
        .expect("runtime");
    let replayed = rt2.pending_approvals();
    assert_eq!(
        replayed.len(),
        1,
        "the approval is still parked after a redeploy"
    );
    assert_eq!(
        replayed[0].expires_at_millis,
        Some(new_deadline),
        "the extended deadline survived the rebuild instead of reverting to the park window"
    );
    // The rehydrated gate enforces the extended window too: a sweep one tick
    // before the new deadline leaves it parked.
    assert!(
        rt2.approval_gate.sweep_expired(new_deadline - 1).is_empty(),
        "the rehydrated gate must enforce the extension, not the original park"
    );
}

/// A [`JournalStore`](crate::ports::journal::JournalStore) that refuses
/// every `ApprovalExtended` line and passes everything else through to an
/// in-memory backend.
#[cfg(feature = "openhuman")]
pub(super) struct RefusingExtendStore {
    pub(super) inner: crate::ports::journal::MemoryJournalStore,
}

#[cfg(feature = "openhuman")]
#[async_trait::async_trait]
impl crate::ports::journal::JournalStore for RefusingExtendStore {
    async fn append_journal(
        &self,
        id: &crate::ports::types::CompanyId,
        line: &str,
        durability: crate::ports::journal::Durability,
    ) -> crate::Result<()> {
        if line.contains("ApprovalExtended") {
            return Err(crate::error::OpenCompanyError::Store(
                "RefusingExtendStore: the volume is full".to_string(),
            ));
        }
        self.inner.append_journal(id, line, durability).await
    }

    async fn read_journal(
        &self,
        id: &crate::ports::types::CompanyId,
    ) -> crate::Result<Vec<String>> {
        self.inner.read_journal(id).await
    }

    async fn journal_imported(&self, id: &crate::ports::types::CompanyId) -> crate::Result<bool> {
        self.inner.journal_imported(id).await
    }

    async fn complete_import(
        &self,
        id: &crate::ports::types::CompanyId,
        lines: Vec<String>,
    ) -> crate::Result<()> {
        self.inner.complete_import(id, lines).await
    }
}
