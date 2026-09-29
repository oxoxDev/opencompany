use crate::ports::TaskRecord;
use crate::ports::tasks::{
    COLUMN_DONE, COLUMN_IN_PROGRESS, COLUMN_IN_REVIEW, TaskStore, TaskTitle,
};
use crate::ports::types::{CompanyEvent, CompanyId, EventSeq};
use std::sync::Arc;
use tempfile::TempDir;

type Runtime = crate::company::runtime::CompanyRuntime;

async fn runtime() -> (Arc<Runtime>, TempDir) {
    runtime_with_tasks(None).await
}

/// A [`TaskStore`] whose `list` always fails, so a review lookup can be
/// driven through the task-store-error arm rather than the "no such
/// card" one.
struct FailingTasks;

#[async_trait::async_trait]
impl TaskStore for FailingTasks {
    async fn list(&self, _company: &CompanyId) -> crate::Result<Vec<TaskRecord>> {
        Err(crate::error::OpenCompanyError::Harness(
            "the board is unavailable".to_string(),
        ))
    }
    async fn upsert(&self, _company: &CompanyId, _task: &TaskRecord) -> crate::Result<()> {
        Ok(())
    }
    async fn update_if_column(
        &self,
        _company: &CompanyId,
        _task: &TaskRecord,
        _observed: &TaskRecord,
        _expected_column: &str,
    ) -> crate::Result<bool> {
        Ok(false)
    }
    async fn delete(&self, _company: &CompanyId, _id: &str) -> crate::Result<bool> {
        Ok(false)
    }
}

async fn runtime_with_tasks(tasks: Option<Arc<dyn TaskStore>>) -> (Arc<Runtime>, TempDir) {
    runtime_with(tasks, None).await
}

/// An [`EventLog`](crate::ports::events::EventLog) decorator whose
/// reads can be switched to fail after setup, so a test can seed real
/// events through a working log and then drive the review-anchor
/// lookup through the read-failure arm. `append`/`subscribe` always
/// delegate to a real [`FsEventLog`](crate::store::fs::FsEventLog) so
/// seeding never observes the failure and behaves exactly as
/// production does.
struct FailingReadsEventLog {
    inner: crate::store::fs::FsEventLog,
    fail_reads: std::sync::atomic::AtomicBool,
}

impl FailingReadsEventLog {
    fn new(inner: crate::store::fs::FsEventLog) -> Self {
        Self {
            inner,
            fail_reads: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn fail_reads_from_now_on(&self) {
        self.fail_reads
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl crate::ports::events::EventLog for FailingReadsEventLog {
    async fn append(&self, id: &CompanyId, event: CompanyEvent) -> crate::Result<EventSeq> {
        self.inner.append(id, event).await
    }

    async fn read_from(
        &self,
        id: &CompanyId,
        seq: EventSeq,
        limit: usize,
    ) -> crate::Result<Vec<crate::ports::types::StoredEvent>> {
        if self.fail_reads.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::error::OpenCompanyError::Harness(
                "the event log is unavailable".to_string(),
            ));
        }
        self.inner.read_from(id, seq, limit).await
    }

    async fn read_before(
        &self,
        id: &CompanyId,
        before: Option<EventSeq>,
        limit: usize,
    ) -> crate::Result<Vec<crate::ports::types::StoredEvent>> {
        if self.fail_reads.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::error::OpenCompanyError::Harness(
                "the event log is unavailable".to_string(),
            ));
        }
        self.inner.read_before(id, before, limit).await
    }

    fn subscribe(
        &self,
        id: &CompanyId,
    ) -> futures::stream::BoxStream<'static, crate::ports::events::EventStreamItem> {
        self.inner.subscribe(id)
    }
}

async fn runtime_with(
    tasks: Option<Arc<dyn TaskStore>>,
    events: Option<Arc<dyn crate::ports::events::EventLog>>,
) -> (Arc<Runtime>, TempDir) {
    let home = tempfile::Builder::new()
        .prefix("opencompany-review-")
        .tempdir()
        .expect("tempdir");
    let manifest: crate::company::CompanyManifest = toml::from_str(
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n\
         [[group_chat]]\nid = \"strategy\"\nname = \"Strategy\"\nmembers = [\"ceo\"]\n",
    )
    .expect("manifest");
    let mut builder = crate::runtime::RuntimeBuilder::new(home.path().to_path_buf(), manifest)
        .with_id(CompanyId::new("acme"));
    if let Some(tasks) = tasks {
        builder = builder.with_tasks(tasks);
    }
    if let Some(events) = events {
        builder = builder.with_events(events);
    }
    let runtime = Arc::new(builder.build().await.expect("runtime"));
    (runtime, home)
}

/// A company whose **durable record** declares an agent literally
/// called `system` — the grandfathered shape
/// [`CompanyRuntime::roster_declares_system_author`] exists for.
///
/// Built by saving the roster over an ordinary company rather than by
/// booting one from that manifest, because `RuntimeBuilder::build`
/// validates with the reservation *enforced* and would refuse it. That
/// is the point: the only way a live company carries this id is the
/// reload path, which grandfathers it
/// (`CompanyManifest::from_path_for_reload` passes
/// `enforce_reserved_agent_ids: false`) and hands the runtime a record
/// exactly like the one written here. The record is what
/// `roster_declares_system_author` reads, so this reproduces the state
/// under test without pretending the builder would mint it.
async fn runtime_with_a_system_teammate() -> (Arc<Runtime>, TempDir) {
    let (runtime, home) = runtime().await;
    let mut record = runtime
        .store
        .load(runtime.id())
        .await
        .expect("load")
        .expect("record");
    record.manifest.agents[0].id = crate::ports::SYSTEM_AUTHOR.to_string();
    runtime.store.save(&record).await.expect("save");
    (runtime, home)
}

fn card(id: &str, origin: &str, column: &str) -> TaskRecord {
    TaskRecord {
        id: id.to_string(),
        title: TaskTitle::authored("Ship it"),
        note: None,
        column: column.to_string(),
        priority: "medium".to_string(),
        assignee: "ceo".to_string(),
        updated_at_millis: 1,
        origin: crate::ports::TaskOrigin::new(Some(origin.to_string()), None),
        parent_task_id: None,
        output: None,
        plan: None,
        planning_attempts: Vec::new(),
        deliverable: crate::ports::tasks::TaskDeliverable::Once,
        workflow_proposal: None,
        origin_run_id: None,
        origin_workflow_id: None,
        origin_message_seq: None,
        bounced: None,
    }
}

fn settle_pill(task_id: &str, origin: &str) -> CompanyEvent {
    CompanyEvent::DeskTaskCompleted {
        task_id: task_id.to_string(),
        desk: "ceo".to_string(),
        output: "done".to_string(),
        column: COLUMN_IN_REVIEW.to_string(),
        artifact_ids: Vec::new(),
        origin_chat_id: Some(origin.to_string()),
        origin_parent: None,
    }
}

fn relay_bubble(origin: &str) -> CompanyEvent {
    CompanyEvent::AgentReply {
        audience: Vec::new(),
        chat_id: origin.to_string(),
        agent_id: "ceo".to_string(),
        text: "Here is the draft.".to_string(),
        steps: Vec::new(),
        task_id: None,
        outputs: Vec::new(),
        parent: None,
        mentions: Vec::new(),
        mention_depth: 0,
        episode: None,
    }
}

/// The B-101 mention-ambiguity advisory
/// ([`CompanyRuntime::post_mention_ambiguity_note`]) — an `AgentReply`
/// with the identical `task_id: None` shape a relay bubble has, but
/// authored by [`crate::ports::SYSTEM_AUTHOR`] rather than a roster
/// agent. Used to seed the interleaving `is_relay_bubble_for` must not
/// be fooled by (codex P2, PR #2052 fresh review round).
fn advisory_bubble(origin: &str) -> CompanyEvent {
    CompanyEvent::AgentReply {
        audience: Vec::new(),
        chat_id: origin.to_string(),
        agent_id: crate::ports::SYSTEM_AUTHOR.to_string(),
        text: "@sam matches two people here, so it pinged nobody.".to_string(),
        steps: Vec::new(),
        task_id: None,
        outputs: Vec::new(),
        parent: None,
        mentions: Vec::new(),
        mention_depth: 0,
        episode: None,
    }
}

async fn seed(runtime: &Arc<Runtime>, c: &TaskRecord) {
    runtime.tasks().upsert(runtime.id(), c).await.expect("seed");
}

async fn append(runtime: &Arc<Runtime>, event: CompanyEvent) -> EventSeq {
    runtime
        .events
        .append(runtime.id(), event)
        .await
        .expect("append")
}

async fn stored(runtime: &Arc<Runtime>, id: &str) -> TaskRecord {
    runtime
        .tasks()
        .list(runtime.id())
        .await
        .expect("list")
        .into_iter()
        .find(|t| t.id == id)
        .expect("card survives")
}

#[tokio::test]
async fn a_settle_pill_resolves_its_in_review_card() {
    let (rt, _home) = runtime().await;
    seed(&rt, &card("t-1", "strategy", COLUMN_IN_REVIEW)).await;
    let pill = append(&rt, settle_pill("t-1", "strategy")).await;

    let target = rt.review_feedback_target("strategy", pill).await.unwrap();
    assert_eq!(target.map(|c| c.id), Some("t-1".to_string()));
}

#[tokio::test]
async fn a_relay_bubble_resolves_via_its_settle_pill() {
    let (rt, _home) = runtime().await;
    seed(&rt, &card("t-1", "strategy", COLUMN_IN_REVIEW)).await;
    append(&rt, settle_pill("t-1", "strategy")).await;
    let bubble = append(&rt, relay_bubble("strategy")).await;

    let target = rt.review_feedback_target("strategy", bubble).await.unwrap();
    assert_eq!(
        target.map(|c| c.id),
        Some("t-1".to_string()),
        "the relay bubble carries no card link, so it anchors on the settle pill \
         immediately before it"
    );
}

#[tokio::test]
async fn the_resolver_declines_a_card_that_left_review() {
    let (rt, _home) = runtime().await;
    seed(&rt, &card("t-1", "strategy", COLUMN_DONE)).await;
    let pill = append(&rt, settle_pill("t-1", "strategy")).await;

    assert!(
        rt.review_feedback_target("strategy", pill)
            .await
            .unwrap()
            .is_none(),
        "a card already approved is not open for review"
    );
}

#[tokio::test]
async fn the_resolver_declines_a_pill_from_another_conversation() {
    let (rt, _home) = runtime().await;
    seed(&rt, &card("t-1", "strategy", COLUMN_IN_REVIEW)).await;
    let pill = append(&rt, settle_pill("t-1", "strategy")).await;

    assert!(
        rt.review_feedback_target("marketing", pill)
            .await
            .unwrap()
            .is_none(),
        "a reply in another desk must not review this desk's card"
    );
}

/// Codex #3903031192: a settle pill's relay bubble is the only reply
/// target that anchors to its card. A later, unrelated `AgentReply` in
/// the same desk — an ordinary chat turn — carries the identical
/// `task_id: None` shape, so a reply to *that* message must not be
/// mistaken for review feedback on the earlier card just because the
/// pill is still the nearest one before it.
#[tokio::test]
async fn the_resolver_declines_a_later_ordinary_reply_that_is_not_the_relay() {
    let (rt, _home) = runtime().await;
    seed(&rt, &card("t-1", "strategy", COLUMN_IN_REVIEW)).await;
    append(&rt, settle_pill("t-1", "strategy")).await;
    let true_relay = append(&rt, relay_bubble("strategy")).await;
    let later_ordinary_turn = append(&rt, relay_bubble("strategy")).await;

    assert_eq!(
        rt.review_feedback_target("strategy", true_relay)
            .await
            .unwrap()
            .map(|c| c.id),
        Some("t-1".to_string()),
        "the pill's own relay bubble still anchors to its card"
    );
    assert!(
        rt.review_feedback_target("strategy", later_ordinary_turn)
            .await
            .unwrap()
            .is_none(),
        "replying to a later ordinary turn must run a normal turn, not \
         re-open the earlier card just because the pill is still the \
         nearest one before it"
    );
}

/// PR #2052 fresh review round, codex P2: a dispatch that has appended
/// its `DeskTaskCompleted` but has not yet run
/// `journal_dispatch_replies` leaves a window in which another
/// accepted chat's ambiguous `@name` can interleave a same-desk B-101
/// advisory before the genuine relay lands. The advisory carries
/// `task_id: None` exactly like a relay bubble, so it must not be
/// mistaken for "the first `AgentReply` after the pill" — that would
/// make the real relay's own reply fail `seq == parent` and silently
/// run as an ordinary chat turn instead of review feedback.
#[tokio::test]
async fn the_resolver_skips_an_interleaved_ambiguity_advisory_to_find_the_real_relay() {
    let (rt, _home) = runtime().await;
    seed(&rt, &card("t-1", "strategy", COLUMN_IN_REVIEW)).await;
    append(&rt, settle_pill("t-1", "strategy")).await;
    // The advisory lands between the pill and the relay: exactly the
    // interleaving window the finding describes.
    append(&rt, advisory_bubble("strategy")).await;
    let true_relay = append(&rt, relay_bubble("strategy")).await;

    assert_eq!(
        rt.review_feedback_target("strategy", true_relay)
            .await
            .unwrap()
            .map(|c| c.id),
        Some("t-1".to_string()),
        "the real relay must still anchor to its card past an \
         interleaved system advisory"
    );
}

/// The negative half: a reply to the advisory itself is not a relay
/// bubble and must not anchor to the card either — only the genuine
/// relay does.
#[tokio::test]
async fn a_reply_to_the_advisory_itself_is_not_review_feedback() {
    let (rt, _home) = runtime().await;
    seed(&rt, &card("t-1", "strategy", COLUMN_IN_REVIEW)).await;
    append(&rt, settle_pill("t-1", "strategy")).await;
    let advisory = append(&rt, advisory_bubble("strategy")).await;
    append(&rt, relay_bubble("strategy")).await;

    assert!(
        rt.review_feedback_target("strategy", advisory)
            .await
            .unwrap()
            .is_none(),
        "the advisory is not itself a relay bubble, so replying to it \
         must run an ordinary chat turn"
    );
}

/// codex P2, 2026-09-04: the advisory filter above must not be a
/// blanket ban on the *string* `system`.
///
/// `SYSTEM_AUTHOR` is a reserved agent id, but the reservation is
/// grandfathered on reload, so a company declared before it can carry
/// a roster teammate literally called `system`. Its replies are
/// ordinary teammate replies; skipping them would lose that company's
/// review anchor entirely — trading the bug the filter fixes for a
/// worse one on the companies it does not apply to.
#[tokio::test]
async fn a_grandfathered_system_teammate_still_anchors_its_own_relay() {
    let (rt, _home) = runtime_with_a_system_teammate().await;
    seed(&rt, &card("t-1", "strategy", COLUMN_IN_REVIEW)).await;
    append(&rt, settle_pill("t-1", "strategy")).await;
    // Authored by the roster agent `system`: on this company that is a
    // teammate speaking, not the runtime reporting on itself.
    let relay = append(
        &rt,
        CompanyEvent::AgentReply {
            audience: Vec::new(),
            chat_id: "strategy".to_string(),
            agent_id: crate::ports::SYSTEM_AUTHOR.to_string(),
            text: "Here is the draft.".to_string(),
            steps: Vec::new(),
            task_id: None,
            outputs: Vec::new(),
            parent: None,
            mentions: Vec::new(),
            mention_depth: 0,
            episode: None,
        },
    )
    .await;

    assert_eq!(
        rt.review_feedback_target("strategy", relay)
            .await
            .unwrap()
            .map(|c| c.id),
        Some("t-1".to_string()),
        "a roster teammate whose id happens to be `system` keeps its \
         relay bubble; the filter is for the runtime's own advisories, \
         which this company has none of"
    );
}

/// Codex #3905031260: the event log is company-wide, so unrelated
/// activity on another desk can put more events between a pill and its
/// relay than a single scan page holds. Both `settle_pill_before`
/// (backward, from the reply to the pill) and `is_relay_bubble_for`
/// (forward, from the pill to the reply) must page past that, not give
/// up at the first page and silently fall through to an ordinary turn.
#[tokio::test]
async fn the_relay_resolves_past_a_flood_of_another_desks_events() {
    let (rt, _home) = runtime().await;
    seed(&rt, &card("t-1", "strategy", COLUMN_IN_REVIEW)).await;
    let pill = append(&rt, settle_pill("t-1", "strategy")).await;
    for _ in 0..300 {
        append(&rt, relay_bubble("marketing")).await;
    }
    let true_relay = append(&rt, relay_bubble("strategy")).await;

    assert_eq!(
        rt.review_feedback_target("strategy", true_relay)
            .await
            .unwrap()
            .map(|c| c.id),
        Some("t-1".to_string()),
        "300 unrelated marketing-desk events between the pill (seq {pill}) and its \
         own relay must not hide either end of the scan behind one page"
    );
}

/// Codex #3906873605: a card that settles, is revised, and returns to
/// `in_review` mints a fresh settle pill for the same `task_id` while
/// the old one stays in the log. A reply anchored to that old pill —
/// a stale client, a replayed request, or a direct API call — must be
/// refused rather than re-dispatching the card's latest attempt.
#[tokio::test]
async fn the_resolver_declines_a_superseded_settle_pill() {
    let (rt, _home) = runtime().await;
    seed(&rt, &card("t-1", "strategy", COLUMN_IN_REVIEW)).await;
    let stale_pill = append(&rt, settle_pill("t-1", "strategy")).await;
    let fresh_pill = append(&rt, settle_pill("t-1", "strategy")).await;

    assert!(
        rt.review_feedback_target("strategy", stale_pill)
            .await
            .unwrap()
            .is_none(),
        "a reply anchored to the superseded settle pill must not \
         re-dispatch the card's latest attempt"
    );
    assert_eq!(
        rt.review_feedback_target("strategy", fresh_pill)
            .await
            .unwrap()
            .map(|c| c.id),
        Some("t-1".to_string()),
        "the current settle marker still resolves the card"
    );
}

/// Same gate, reached through a settle pill's relay bubble rather than
/// the pill itself — the relay off a superseded pill must not anchor
/// either.
#[tokio::test]
async fn the_resolver_declines_a_relay_off_a_superseded_settle_pill() {
    let (rt, _home) = runtime().await;
    seed(&rt, &card("t-1", "strategy", COLUMN_IN_REVIEW)).await;
    append(&rt, settle_pill("t-1", "strategy")).await;
    let stale_relay = append(&rt, relay_bubble("strategy")).await;
    append(&rt, settle_pill("t-1", "strategy")).await;
    let fresh_relay = append(&rt, relay_bubble("strategy")).await;

    assert!(
        rt.review_feedback_target("strategy", stale_relay)
            .await
            .unwrap()
            .is_none(),
        "a relay bubble off the superseded pill must not re-dispatch \
         the card's latest attempt"
    );
    assert_eq!(
        rt.review_feedback_target("strategy", fresh_relay)
            .await
            .unwrap()
            .map(|c| c.id),
        Some("t-1".to_string()),
        "the relay off the current settle pill still resolves the card"
    );
}

/// The latest-pill gate is per card, not per desk: one card settling
/// again must not invalidate a different card's still-current anchor
/// in the same desk (guards the interaction with the earlier
/// per-card-actionable fix).
#[tokio::test]
async fn a_superseded_pill_on_one_card_does_not_invalidate_a_sibling_cards_anchor() {
    let (rt, _home) = runtime().await;
    seed(&rt, &card("t-1", "strategy", COLUMN_IN_REVIEW)).await;
    seed(&rt, &card("t-2", "strategy", COLUMN_IN_REVIEW)).await;
    let t1_pill = append(&rt, settle_pill("t-1", "strategy")).await;
    let t2_pill = append(&rt, settle_pill("t-2", "strategy")).await;
    append(&rt, settle_pill("t-1", "strategy")).await;

    assert!(
        rt.review_feedback_target("strategy", t1_pill)
            .await
            .unwrap()
            .is_none(),
        "t-1's original pill is superseded by its own revision"
    );
    assert_eq!(
        rt.review_feedback_target("strategy", t2_pill)
            .await
            .unwrap()
            .map(|c| c.id),
        Some("t-2".to_string()),
        "t-2's pill is untouched by t-1 settling again — the gate is per card"
    );
}

#[tokio::test]
async fn the_resolver_declines_an_ordinary_message() {
    let (rt, _home) = runtime().await;
    seed(&rt, &card("t-1", "strategy", COLUMN_IN_REVIEW)).await;
    let chatter = append(
        &rt,
        CompanyEvent::OperatorMessage {
            text: "unrelated".to_string(),
            by: None,
            chat: Some("strategy".to_string()),
            parent: None,
            deliverable: None,
            mentions: Vec::new(),
            attachments: Vec::new(),
        },
    )
    .await;

    assert!(
        rt.review_feedback_target("strategy", chatter)
            .await
            .unwrap()
            .is_none(),
        "a top-level message names no review surface and starts a normal turn"
    );
}

/// Codex #3905031268: a task-store read failure must surface as an
/// error, not collapse into "no review target". Otherwise the explicit
/// review endpoint answers a transient storage error with a misleading
/// 404, and the threaded-feedback path falls through and runs the
/// operator's review note as an ordinary chat turn.
#[tokio::test]
async fn a_task_store_failure_surfaces_as_an_error_not_a_missing_card() {
    let (rt, _home) = runtime_with_tasks(Some(Arc::new(FailingTasks))).await;
    let pill = append(&rt, settle_pill("t-1", "strategy")).await;

    let err = rt
        .review_feedback_target("strategy", pill)
        .await
        .expect_err(
            "a storage failure must not be read as 'no review target' and fall \
             through to an ordinary chat turn",
        );
    assert!(
        matches!(err, crate::error::OpenCompanyError::Harness(_)),
        "unexpected error: {err:?}"
    );
}

/// Codex #3905522633: the same gap `397807637` closed for
/// `TaskStore::list`, one layer over — the `EventLog` reads inside
/// `review_anchor_card`/`settle_pill_before`/`is_relay_bubble_for`
/// must not collapse a transient read failure into "not a review
/// anchor" and let `chat_and_emit` run the operator's review note as
/// an ordinary chat turn.
#[tokio::test]
async fn an_event_log_read_failure_surfaces_as_an_error_not_a_missing_anchor() {
    let home = tempfile::Builder::new()
        .prefix("opencompany-review-events-")
        .tempdir()
        .expect("tempdir");
    let events = Arc::new(FailingReadsEventLog::new(
        crate::store::fs::FsEventLog::new(home.path().to_path_buf()),
    ));
    let manifest: crate::company::CompanyManifest = toml::from_str(
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n\
         [[group_chat]]\nid = \"strategy\"\nname = \"Strategy\"\nmembers = [\"ceo\"]\n",
    )
    .expect("manifest");
    let runtime = Arc::new(
        crate::runtime::RuntimeBuilder::new(home.path().to_path_buf(), manifest)
            .with_id(CompanyId::new("acme"))
            .with_events(events.clone())
            .build()
            .await
            .expect("runtime"),
    );
    seed(&runtime, &card("t-1", "strategy", COLUMN_IN_REVIEW)).await;
    let pill = append(&runtime, settle_pill("t-1", "strategy")).await;

    events.fail_reads_from_now_on();

    let err = runtime
        .review_feedback_target("strategy", pill)
        .await
        .expect_err(
            "an event-log read failure must not be read as 'not a review anchor' \
             and fall through to an ordinary chat turn",
        );
    assert!(
        matches!(err, crate::error::OpenCompanyError::Harness(_)),
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn feedback_appends_a_reviewer_block_and_re_enters_in_progress() {
    let (rt, _home) = runtime().await;
    let mut seeded = card("t-1", "strategy", COLUMN_IN_REVIEW);
    seeded.note = Some("[writer] first draft".to_string());
    seed(&rt, &seeded).await;

    rt.apply_review_feedback(&seeded, "tighten the intro", None)
        .await
        .expect("feedback applies");

    let after = stored(&rt, "t-1").await;
    assert_eq!(
        after.column, COLUMN_IN_PROGRESS,
        "review feedback re-runs the card through the dispatch edge"
    );
    let note = after.note.expect("note");
    assert!(note.contains("[reviewer] tighten the intro"), "{note}");
    assert!(
        note.contains("[writer] first draft"),
        "the prior note is preserved: {note}"
    );
}

#[tokio::test]
async fn empty_feedback_does_not_redispatch() {
    let (rt, _home) = runtime().await;
    let mut seeded = card("t-1", "strategy", COLUMN_IN_REVIEW);
    seeded.note = Some("[writer] first draft".to_string());
    seed(&rt, &seeded).await;

    rt.apply_review_feedback(&seeded, "   ", None)
        .await
        .expect("empty feedback is accepted, not rejected");

    let after = stored(&rt, "t-1").await;
    assert_eq!(
        after.column, COLUMN_IN_REVIEW,
        "a Revise with nothing to say must not re-dispatch the card"
    );
    assert_eq!(
        after.note.as_deref(),
        Some("[writer] first draft"),
        "no reviewer block is appended when there is no feedback"
    );
}

#[tokio::test]
async fn approve_finishes_the_card() {
    use crate::harness::built_in::lifecycle::ReviewDecision;
    let (rt, _home) = runtime().await;
    let seeded = card("t-1", "strategy", COLUMN_IN_REVIEW);
    seed(&rt, &seeded).await;

    rt.apply_review_decision(&seeded, ReviewDecision::Approve, None, None)
        .await
        .expect("approve applies");

    let after = stored(&rt, "t-1").await;
    assert_eq!(after.column, COLUMN_DONE);
}
