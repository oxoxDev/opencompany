//! Runtime tests: blocker claim release on journal/settle failure, and banked blockers driven on boot.

use super::tests_approval::{RefusingExtendStore, runtime_with_events, seed_parked};
#[cfg(feature = "openhuman")]
use super::tests_core::RefusingJournalStore;
use super::{CompanyEvent, continuation_failure_notice};

/// `extend_approval` moves the gate's live deadline **before**
/// it journals the extension. When the journal append then fails, the
/// caller sees the error, but the live view already reflects the later
/// deadline — and nothing durable backs that, so a restart from the same
/// journal comes back believing the approval was never extended at all.
/// This pins that sequence exactly, as the real, current consequence: a
/// caller told the extend failed still sees the live queue disagree with
/// it until the next restart quietly settles the disagreement in the
/// caller's favor.
#[tokio::test]
async fn a_failed_extend_append_leaves_a_live_extension_that_reverts_on_restart() {
    use crate::ports::types::{Actor, ActorKind};

    let manifest: crate::company::types::CompanyManifest =
        toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"supervised\"\n")
            .expect("manifest");
    let store = std::sync::Arc::new(RefusingExtendStore {
        inner: crate::ports::journal::MemoryJournalStore::default(),
    });
    let home_dir = tempfile::tempdir().expect("tempdir");

    let rt1 = crate::runtime::RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest.clone())
        .with_journal_store(store.clone())
        .build()
        .await
        .expect("runtime");
    let id = seed_parked(&rt1, "appr-extend-fail", 1_000).await;
    let ttl = rt1.approval_gate.ttl_millis();
    let original_deadline = 1_000 + ttl;

    let extend = rt1
        .extend_approval(
            &id,
            Actor {
                kind: ActorKind::User,
                id: "operator".into(),
            },
        )
        .await;
    assert!(extend.is_err(), "the forced append failure must surface");
    assert!(
        rt1.pending_approvals()[0].expires_at_millis.unwrap() > original_deadline,
        "the live gate already moved the deadline even though nothing durable recorded it"
    );
    drop(rt1);

    let rt2 = crate::runtime::RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest)
        .with_journal_store(store)
        .build()
        .await
        .expect("runtime");
    let replayed = rt2.pending_approvals();
    assert_eq!(replayed.len(), 1, "the approval is still parked");
    assert_eq!(
        replayed[0].expires_at_millis,
        Some(original_deadline),
        "the extension a caller was told failed must not silently revert on restart"
    );
}

/// A [`JournalStore`](crate::ports::journal::JournalStore) that refuses
/// every `ApprovalExpired` line and passes everything else through.
struct RefusingExpiredStore {
    inner: crate::ports::journal::MemoryJournalStore,
}

#[async_trait::async_trait]
impl crate::ports::journal::JournalStore for RefusingExpiredStore {
    async fn append_journal(
        &self,
        id: &crate::ports::types::CompanyId,
        line: &str,
        durability: crate::ports::journal::Durability,
    ) -> crate::Result<()> {
        if line.contains("ApprovalExpired") {
            return Err(crate::error::OpenCompanyError::Store(
                "RefusingExpiredStore: the volume is full".to_string(),
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

/// `sweep_expired_capped` removes every id in the batch from the
/// live `parked` map up front, stashing each one's effect in
/// `expired_effects` for [`CompanyRuntime::retire_approval`] to collect.
/// `sweep_expired_approvals` then walks that batch and returns on the
/// **first** `retire_approval` failure (`?`), so a durable-write failure
/// partway through strands every id after it: already gone from `parked`,
/// still sitting in `expired_effects`, and never revisited because the
/// next sweep's scan is over `parked`, which no longer names them.
#[tokio::test]
async fn a_failed_retirement_mid_batch_strands_the_rest_of_the_batch() {
    use crate::ports::types::{Actor, ActorKind, Verdict};

    let manifest: crate::company::types::CompanyManifest =
        toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"supervised\"\n")
            .expect("manifest");
    let store = std::sync::Arc::new(RefusingExpiredStore {
        inner: crate::ports::journal::MemoryJournalStore::default(),
    });
    let home_dir = tempfile::tempdir().expect("tempdir");
    let rt = std::sync::Arc::new(
        crate::runtime::RuntimeBuilder::new(home_dir.path().to_path_buf(), manifest)
            .with_journal_store(store)
            .build()
            .await
            .expect("runtime"),
    );

    let ttl = rt.approval_gate.ttl_millis();
    let long_expired = crate::ports::now_millis().saturating_sub(ttl + 60_000);
    // Alphabetical order matches park-time order here, so the cap sorts
    // "appr-a" first — the one whose retirement is attempted (and fails)
    // — and "appr-b" is the untouched survivor stranded behind it.
    seed_parked(&rt, "appr-a", long_expired).await;
    seed_parked(&rt, "appr-b", long_expired).await;
    assert_eq!(
        rt.pending_approvals().len(),
        2,
        "both are parked and expired"
    );

    let swept = rt.sweep_expired_approvals().await;
    assert!(
        swept.is_err(),
        "the forced ApprovalExpired failure must surface"
    );

    // `record_expired` moves its in-memory `parked` entry out **before**
    // journaling the expiry — the same optimistic-then-persist order
    // The extend case pins the same shape — so "appr-a"'s failed attempt still drops
    // it from the journal's own pending view in-memory, with nothing
    // durable behind that removal. Only "appr-b", whose retirement was
    // never even attempted, is left on the console's pending list.
    let pending = rt.pending_approvals();
    assert_eq!(
        pending.len(),
        1,
        "only the untried survivor is left on the console's pending list: {pending:?}"
    );
    assert_eq!(
        pending[0].id,
        crate::ports::types::ApprovalId::new("appr-b")
    );

    // The gate's own live `parked` map already dropped both — that is
    // what `sweep_expired_capped` did before the failing retirement ever
    // ran — so a decision on the survivor is not a decision on anything:
    // it comes back as a safe no-op, never as the operator's verdict.
    let (receipt, _handle) = rt
        .resolve_approval_spawned(
            &crate::ports::types::ApprovalId::new("appr-b"),
            Verdict::Approve,
            Actor {
                kind: ActorKind::User,
                id: "operator".into(),
            },
            crate::runtime::grants::GrantScope::Once,
        )
        .await
        .expect("a losing resolve is a receipt, not an error");
    assert!(
        matches!(
            receipt,
            crate::runtime::cycle::ResolveReceipt::AlreadyResolved
        ),
        "the survivor is gone from the gate's live map, so even the operator's own \
         decision on it silently no-ops instead of settling it: {receipt:?}"
    );

    // A later sweep never even reaches the still-refusing store: its scan
    // is over `parked`, which no longer names either id, so it succeeds
    // trivially with nothing to report — the stranded survivor is retired
    // by nothing and never seen again, while the console goes on listing it.
    let second_sweep = rt
        .sweep_expired_approvals()
        .await
        .expect("nothing left in `parked` to retire");
    assert!(
        second_sweep.is_empty(),
        "the stranded survivor is never retried by a later sweep: {second_sweep:?}"
    );
}

#[tokio::test]
async fn an_unresolvable_thread_root_degrades_to_the_channel() {
    use crate::ports::types::{Actor, ActorKind, CompanyEvent, EventSeq};

    let (rt, _home_dir) = runtime_with_events().await;

    // A real root in `desk-finance`, and a second message elsewhere.
    let root = rt
        .events
        .append(
            &rt.id,
            CompanyEvent::OperatorMessage {
                mentions: Vec::new(),
                text: "pay the invoice".into(),
                by: None,
                chat: Some("desk-finance".into()),
                parent: None,
                deliverable: None,
                attachments: Vec::new(),
            },
        )
        .await
        .expect("append");
    let elsewhere = rt
        .events
        .append(
            &rt.id,
            CompanyEvent::OperatorMessage {
                mentions: Vec::new(),
                text: "unrelated".into(),
                by: None,
                chat: Some("desk-ops".into()),
                parent: None,
                deliverable: None,
                attachments: Vec::new(),
            },
        )
        .await
        .expect("append");

    // The good case: a root that exists, in the channel being answered.
    assert_eq!(
        rt.resolvable_parent(Some(root), "desk-finance").await,
        Some(root),
    );

    // No root recorded at all — the overwhelmingly common case, and the
    // pre-#435 behaviour.
    assert_eq!(rt.resolvable_parent(None, "desk-finance").await, None);

    // A root that resolves but lives in another channel. Renderable
    // nowhere, and proof the recorded pair was already inconsistent.
    assert_eq!(
        rt.resolvable_parent(Some(elsewhere), "desk-finance").await,
        None,
        "a root in another channel must not follow the answer across",
    );

    // A root that is simply GONE, with a live message after it.
    //
    // This is the case the exact-sequence check exists for, and it has to
    // be built deliberately. `read_from` returns events with sequence >=
    // the one asked for, so a vanished root comes back as its *successor*.
    // Asking past the end of the log proves nothing — that read is empty
    // and every implementation returns `None`. A genuine gap is what
    // separates "found it" from "found the next one", and the only thing
    // that makes gaps is pruning, which the events module documents as
    // leaving them by design.
    //
    // So: a prunable frame, then a real message in the channel, then a
    // pass that removes the first. Without the sequence check the message
    // answers for the hole underneath it — and it is in the right channel,
    // so the channel check waves it through.
    let doomed = rt
        .events
        .append(
            &rt.id,
            CompanyEvent::WorkspaceChanged {
                node_id: "n-1".into(),
                change: "updated".into(),
            },
        )
        .await
        .expect("append");
    let after_the_hole = rt
        .events
        .append(
            &rt.id,
            CompanyEvent::OperatorMessage {
                mentions: Vec::new(),
                text: "and another thing".into(),
                by: None,
                chat: Some("desk-finance".into()),
                parent: None,
                deliverable: None,
                attachments: Vec::new(),
            },
        )
        .await
        .expect("append");
    rt.events
        .prune(
            &rt.id,
            &crate::ports::events::RetentionPolicy {
                max_entries_per_kind: Some(0),
                ..Default::default()
            },
        )
        .await
        .expect("prune");
    // The hole is real, and the next event is a same-channel message.
    let successor = rt
        .events
        .read_from(&rt.id, doomed, 1)
        .await
        .expect("read")
        .into_iter()
        .next()
        .expect("the message after the hole answers the read");
    assert_eq!(
        successor.seq, after_the_hole,
        "the pruned sequence must genuinely be absent, answered by its successor",
    );
    assert_eq!(
        rt.resolvable_parent(Some(doomed), "desk-finance").await,
        None,
        "a vanished root must not be answered by the message that follows it",
    );

    // And past the end of the log, where the read is simply empty.
    let beyond = EventSeq::new(after_the_hole.value() + 500);
    assert_eq!(
        rt.resolvable_parent(Some(beyond), "desk-finance").await,
        None
    );

    // A sequence that resolves to something that is not a chat message at
    // all cannot root a thread either.
    let not_a_message = rt
        .events
        .append(
            &rt.id,
            CompanyEvent::LifecycleChanged {
                from: "idle".into(),
                to: "running".into(),
                by: Actor {
                    kind: ActorKind::Operator,
                    id: "owner".into(),
                },
            },
        )
        .await
        .expect("append");
    assert_eq!(
        rt.resolvable_parent(Some(not_a_message), "desk-finance")
            .await,
        None,
    );
}

/// A thread rooted under any legacy spelling of #general keeps its parent
/// when answered into #general.
#[tokio::test]
async fn a_root_in_any_spelling_of_the_general_desk_still_resolves() {
    use crate::ports::types::CompanyEvent;

    let (rt, _home_dir) = runtime_with_events().await;

    // Three roots, one desk: the unaddressed post, the console's own
    // thread id, and the desk named outright.
    let mut roots = Vec::new();
    for chat in [None, Some("main"), Some("General")] {
        roots.push(
            rt.events
                .append(
                    &rt.id,
                    CompanyEvent::OperatorMessage {
                        mentions: Vec::new(),
                        text: "ship it".into(),
                        by: None,
                        chat: chat.map(str::to_string),
                        parent: None,
                        deliverable: None,
                        attachments: Vec::new(),
                    },
                )
                .await
                .expect("append"),
        );
    }

    for root in &roots {
        assert_eq!(
            rt.resolvable_parent(Some(*root), "general").await,
            Some(*root),
            "root {root} must resolve when answered into #general",
        );
    }

    // …and the folding stops there. A real desk is still compared
    // verbatim, so this widening cannot pull an unrelated thread in.
    let elsewhere = rt
        .events
        .append(
            &rt.id,
            CompanyEvent::OperatorMessage {
                mentions: Vec::new(),
                text: "unrelated".into(),
                by: None,
                chat: Some("desk-ops".into()),
                parent: None,
                deliverable: None,
                attachments: Vec::new(),
            },
        )
        .await
        .expect("append");
    assert_eq!(
        rt.resolvable_parent(Some(elsewhere), "General").await,
        None,
        "a named desk is not the General desk",
    );
    assert_eq!(
        rt.resolvable_parent(Some(roots[0]), "desk-ops").await,
        None,
        "and the General desk is not a named one",
    );
}

/// Issue #966: the failed-continuation report is authored by the runtime.
///
/// This site appends the `AgentReply` itself, so it never sees
/// `OutboundMessage::agent` or its `channel` fallback — it has to name the
/// author, and it used to name `OPERATOR_CHANNEL`. That made a correct
/// system row byte-identical on disk to a reply the pre-#885 defect had
/// damaged, which is the finding recorded on #966.
#[test]
fn a_failed_continuation_report_is_authored_by_the_runtime_not_the_operator() {
    let event = continuation_failure_notice("desk-general".to_string(), None);
    let CompanyEvent::AgentReply {
        agent_id, chat_id, ..
    } = event
    else {
        panic!("the notice must stay an AgentReply — the console renders it from that arm");
    };
    assert_eq!(agent_id, crate::ports::SYSTEM_AUTHOR);
    assert_ne!(
        agent_id,
        crate::runtime::channel::OPERATOR_CHANNEL,
        "a notice must not store the author a destination-overwrite produces"
    );
    assert_eq!(
        chat_id, "desk-general",
        "it still lands in the thread it answers"
    );
}

/// Issue #1861 (found by Codex on #1905): a gate park that lands and then
/// fails to journal must not leave the approval decidable.
///
/// # The window
///
/// `park_blocker` parks on the gate first and journals second. A `?` on the
/// journal write reported the park as failed — so `settle_blocked` returned
/// the card to To-do — while the gate still held a live, decidable entry
/// against it. The operator is then shown a question for a card nobody
/// paused, which is the exact inconsistency `unpark_blocker` exists to
/// prevent on the other side of this pair.
///
/// `record_parked` also populates the projection *before* its append, so
/// the same failure left a pending approval that no journal line would ever
/// replay: visible until the process exits, gone after a boot.
///
/// Both are asserted here, because clearing one without the other just
/// moves the disagreement.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn a_blocker_that_cannot_be_journaled_leaves_no_decidable_approval() {
    let home = tempfile::tempdir().expect("home");
    let manifest: crate::company::CompanyManifest = toml::from_str(
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
    let journal = std::sync::Arc::new(RefusingJournalStore::default());
    let runtime = crate::runtime::RuntimeBuilder::new(home.path().to_path_buf(), manifest)
        .with_id(crate::ports::types::CompanyId::new("acme"))
        .with_journal_store(journal.clone())
        .build()
        .await
        .expect("runtime");

    // The volume goes away *after* boot, so this is an ordinary runtime.
    journal.arm();

    let payload = crate::ports::blockers::BlockerPayload {
        kind: crate::ports::blockers::BlockerKind::Infrastructure,
        source: crate::ports::blockers::BlockerSource::Provider,
        step: Some(crate::ports::blockers::BlockerStep::Task {
            task_id: "t-1".to_string(),
        }),
        reason: "the model `gpt-nonexistent` was rejected".to_string(),
        needed: "a model id this provider serves".to_string(),
        group_key: None,
    };

    let parked = runtime
        .park_blocker(
            &payload,
            "t-1",
            crate::company::blocker_sender::BlockerSenderSignals::default(),
        )
        .await;
    assert!(
        parked.is_err(),
        "an unjournaled park is reported as a failed park, so the caller returns the card"
    );

    assert!(
        runtime.approval_gate.parked_ids().is_empty(),
        "the gate entry must be rolled back — otherwise the operator can decide a blocker \
         for a card that was handed straight back to To-do"
    );
    assert!(
        runtime.pending_approvals().is_empty(),
        "and the projection row `record_parked` inserted before its append must go with it"
    );
}

/// **P1 review finding on PR #2038.** `claim_and_settle_blocker` claims
/// the blocker's resolution slot before banking it durably. If the bank
/// then fails (a transient journal write error), the claim used to stay
/// taken with nothing behind it — so a retry lost the race against its
/// own earlier attempt and answered `AlreadyResolved` forever, and the
/// blocker became unanswerable for the rest of the process's life. The
/// claim must be released on that failure so a retry can actually settle.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn a_transient_journal_failure_releases_the_blocker_claim_for_retry() {
    let home = tempfile::tempdir().expect("home");
    let manifest: crate::company::CompanyManifest = toml::from_str(
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
    let journal = std::sync::Arc::new(RefusingJournalStore::default());
    let runtime = std::sync::Arc::new(
        crate::runtime::RuntimeBuilder::new(home.path().to_path_buf(), manifest)
            .with_id(crate::ports::types::CompanyId::new("acme"))
            .with_journal_store(journal.clone())
            .build()
            .await
            .expect("runtime"),
    );

    let payload = crate::ports::blockers::BlockerPayload {
        kind: crate::ports::blockers::BlockerKind::Infrastructure,
        source: crate::ports::blockers::BlockerSource::Provider,
        step: Some(crate::ports::blockers::BlockerStep::Task {
            task_id: "t-1".to_string(),
        }),
        reason: "the model `gpt-nonexistent` was rejected".to_string(),
        needed: "a model id this provider serves".to_string(),
        group_key: None,
    };
    let id = runtime
        .park_blocker(
            &payload,
            "t-1",
            crate::company::blocker_sender::BlockerSenderSignals::default(),
        )
        .await
        .expect("parks before the volume goes away");

    // The volume goes away *after* the park, so the claim/bank/settle
    // path is what fails, not the park itself.
    journal.arm();
    let failed = runtime
        .apply_blocker_reply_spawned(
            std::slice::from_ref(&id),
            &id,
            crate::ports::blockers::BlockerVerdict::Retry,
            "",
            None,
        )
        .await;
    assert!(
        failed.is_err(),
        "the armed journal store must fail the bank and surface the error: {failed:?}"
    );

    // The volume is back. If the earlier failure left the claim taken,
    // this retry loses the race against itself and reports
    // `AlreadyResolved` without ever settling — the bug this test is for.
    journal.disarm();
    let (receipt, follow_up) = runtime
        .apply_blocker_reply_spawned(
            std::slice::from_ref(&id),
            &id,
            crate::ports::blockers::BlockerVerdict::Retry,
            "",
            None,
        )
        .await
        .expect("the retry must be accepted once the volume is back");
    crate::company::runtime::join_follow_up(follow_up)
        .await
        .expect("follow-up runs");

    assert!(
        matches!(receipt, crate::runtime::cycle::ResolveReceipt::Settled(_)),
        "a transient journal failure must not permanently strand the claim — the \
         retry must actually settle the blocker, not report AlreadyResolved forever: \
         {receipt:?}"
    );
}
