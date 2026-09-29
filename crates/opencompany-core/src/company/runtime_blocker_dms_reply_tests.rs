//! Runtime blocker DMs: which teammate a blocker surfaces to, cause grouping, and per-conversation reply attribution.

use crate::company::blocker_sender::BlockerSenderSignals;
use crate::company::runtime::{BlockerReplyPlan, CompanyRuntime};
use crate::company::task_intent::BlockerReplyIntent;
use crate::ports::blockers::{BlockerKind, BlockerPayload, BlockerSource, BlockerStep};
use crate::ports::types::CompanyId;
use std::sync::Arc;
use tempfile::TempDir;

async fn runtime() -> (Arc<CompanyRuntime>, TempDir) {
    let home = tempfile::Builder::new()
        .prefix("opencompany-blocker-dms-")
        .tempdir()
        .expect("tempdir");
    let manifest: crate::company::CompanyManifest = toml::from_str(
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n\
         [[agent]]\nid = \"eng\"\nrole = \"Engineer\"\n",
    )
    .expect("manifest");
    let runtime = Arc::new(
        crate::runtime::RuntimeBuilder::new(home.path().to_path_buf(), manifest)
            .with_id(CompanyId::new("acme"))
            .build()
            .await
            .expect("runtime"),
    );
    (runtime, home)
}

fn blocker(task_id: &str, group_key: Option<&str>) -> BlockerPayload {
    BlockerPayload {
        kind: BlockerKind::Infrastructure,
        source: BlockerSource::Tool,
        step: Some(BlockerStep::Task {
            task_id: task_id.to_string(),
        }),
        reason: format!("could not connect to mcp server for {task_id}"),
        needed: "the integration reconnected from Apps".to_string(),
        group_key: group_key.map(str::to_string),
    }
}

fn assignee(id: &str) -> BlockerSenderSignals {
    BlockerSenderSignals {
        started_by: None,
        owner_desk: None,
        assignee: Some(id.to_string()),
    }
}

/// A blocker parks into its teammate's DM: the approval's thread is that
/// DM, and a `blocker_parked` notification is filed pointing at it — with
/// no payload beyond the one-line title.
#[tokio::test]
async fn a_blocker_surfaces_in_the_responsible_teammates_dm() {
    let (runtime, _home) = runtime().await;
    runtime
        .park_blocker(&blocker("t-1", None), "t-1", assignee("eng"))
        .await
        .expect("parks");

    let pending = runtime.pending_approvals();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].thread.as_deref(),
        Some("dm:eng"),
        "the card routes into the DM with the teammate it is attributed to"
    );

    let notes = runtime
        .notifications()
        .list(runtime.id(), "eng")
        .await
        .expect("notifications");
    let parked = notes
        .iter()
        .find(|n| n.notification.kind == "blocker_parked")
        .expect("a blocker-parked notification is filed");
    assert_eq!(parked.notification.context.as_deref(), Some("dm:eng"));
    assert!(
        parked.notification.title.contains("eng"),
        "the title names who is blocked: {}",
        parked.notification.title
    );
}

/// The projection names which kind of step a parked blocker stopped
/// (issue #2028) — the console needs this to word `skip`/`cancel`
/// honestly, since neither does the same thing to a board card that it
/// does to a workflow node.
#[tokio::test]
async fn pending_approvals_names_the_stopped_steps_kind() {
    let (runtime, _home) = runtime().await;
    runtime
        .park_blocker(&blocker("t-1", None), "t-1", assignee("eng"))
        .await
        .expect("parks a task-step blocker");
    let node_payload = BlockerPayload {
        kind: BlockerKind::Information,
        source: BlockerSource::Tool,
        step: Some(BlockerStep::Node {
            run_id: "run-1".to_string(),
            node_id: "draft".to_string(),
        }),
        reason: "needs a model choice".to_string(),
        needed: "which model to use".to_string(),
        group_key: None,
    };
    runtime
        .park_blocker(&node_payload, "t-2", assignee("eng"))
        .await
        .expect("parks a node-step blocker");

    let pending = runtime.pending_approvals();
    assert_eq!(pending.len(), 2);
    let kinds: std::collections::HashSet<_> = pending
        .iter()
        .map(|a| a.blocker_step_kind.clone())
        .collect();
    assert_eq!(
        kinds,
        std::collections::HashSet::from([Some("task".to_string()), Some("node".to_string())]),
        "a task-step and a node-step blocker must project distinct step kinds, not the \
         same value: {pending:?}"
    );
}

/// The sender is resolved, not passed through: a park with no attribution
/// still lands in a real DM — the orchestrator's.
#[tokio::test]
async fn an_unattributed_blocker_falls_to_the_orchestrator_dm() {
    let (runtime, _home) = runtime().await;
    runtime
        .park_blocker(
            &blocker("t-1", None),
            "t-1",
            BlockerSenderSignals::default(),
        )
        .await
        .expect("parks");
    assert_eq!(
        runtime.pending_approvals()[0].thread.as_deref(),
        Some("dm:ceo"),
        "with nothing named, the first (orchestrator) agent answers"
    );
}

/// Blockers sharing a root cause project as one group and are named by
/// the projection's `group_key`.
#[tokio::test]
async fn blockers_sharing_a_cause_group_together() {
    let (runtime, _home) = runtime().await;
    for task in ["t-1", "t-2", "t-3"] {
        runtime
            .park_blocker(
                &blocker(task, Some("connection:slack")),
                task,
                assignee("eng"),
            )
            .await
            .expect("parks");
    }
    let members = runtime.blocker_group_members("connection:slack", Some("task"));
    assert_eq!(
        members.len(),
        3,
        "every card on the broken connection is one group"
    );
    for summary in runtime.pending_approvals() {
        assert_eq!(summary.group_key.as_deref(), Some("connection:slack"));
    }
}

/// **P1 review finding on PR #2038.** A connection failure can stop
/// both a board card and a workflow node, and both park with the same
/// `connection:<name>` group key — but Skip means "produces nothing"
/// to a node and "redispatch, run it again" to a task. Fanning one
/// verdict across the two step kinds silently applies the wrong
/// consequence to whichever wasn't addressed, so the fan-out group
/// must split by step kind even when the root cause is shared.
#[tokio::test]
async fn a_shared_cause_never_fans_a_verdict_across_step_kinds() {
    let (runtime, _home) = runtime().await;
    let task_id = runtime
        .park_blocker(
            &blocker("t-1", Some("connection:slack")),
            "t-1",
            assignee("eng"),
        )
        .await
        .expect("parks a task-step blocker");
    let node_payload = BlockerPayload {
        kind: BlockerKind::Infrastructure,
        source: BlockerSource::Tool,
        step: Some(BlockerStep::Node {
            run_id: "run-1".to_string(),
            node_id: "draft".to_string(),
        }),
        reason: "could not connect to mcp server for run-1".to_string(),
        needed: "the integration reconnected from Apps".to_string(),
        group_key: Some("connection:slack".to_string()),
    };
    let node_id = runtime
        .park_blocker(&node_payload, "t-2", assignee("eng"))
        .await
        .expect("parks a node-step blocker on the same connection");

    let fanned = runtime
        .parked_blocker_group(&task_id)
        .expect("the task blocker is still parked");
    assert_eq!(
        fanned,
        vec![task_id.clone()],
        "the task blocker's fan-out group must not include the node-step sibling \
         just because they share a connection: {fanned:?}"
    );

    let (_, follow_up) = runtime
        .apply_blocker_reply_spawned(
            &fanned,
            &task_id,
            crate::ports::blockers::BlockerVerdict::Skip,
            "",
            None,
        )
        .await
        .expect("resolves the task blocker alone");
    crate::company::runtime::join_follow_up(follow_up)
        .await
        .expect("follow-up runs");

    assert!(
        runtime.pending_approvals().iter().any(|p| p.id == node_id),
        "skipping the task card must not have also skipped the workflow node — \
         it is still stalled on the same connection and still needs its own answer"
    );
}

/// A reply in a DM with a single pending blocker resolves it, and a
/// grouped reply fans the verdict to every card in the group.
#[tokio::test]
async fn a_reply_resolves_the_whole_group_and_fans_the_verdict() {
    let (runtime, _home) = runtime().await;
    for task in ["t-1", "t-2"] {
        runtime
            .park_blocker(
                &blocker(task, Some("connection:slack")),
                task,
                assignee("eng"),
            )
            .await
            .expect("parks");
    }
    let plan = runtime
        .plan_blocker_reply("dm:eng", None, "go ahead and retry")
        .await
        .expect("plan");
    let ids = match plan {
        BlockerReplyPlan::Resolve { ids, intent } => {
            assert_eq!(intent, BlockerReplyIntent::Retry);
            assert_eq!(ids.len(), 2, "one card, both parks");
            ids
        }
        _ => panic!("a single group in the DM resolves"),
    };
    runtime
        .apply_blocker_reply(&ids, BlockerReplyIntent::Retry, "go ahead and retry", None)
        .await
        .expect("applies");
    assert!(
        runtime.pending_approvals().is_empty(),
        "the verdict fanned to every card in the group"
    );
}

/// Parks a blocker the way a cycle that came from **no** conversation
/// does: `cycle_conversation` answers with a default
/// `ApprovalConversation`, so the journal row carries `thread: None`.
/// Every planning-pass park written before commit `26d558c92` has the
/// same shape, and those rows survive journal replay.
async fn park_thread_less_blocker(runtime: &Arc<CompanyRuntime>, task_id: &str) {
    use crate::ports::types::{Effect, EffectGroup};
    use crate::runtime::journal::{ApprovalConversation, TaskLink};

    let payload = blocker(task_id, None);
    let effect = Effect {
        kind: payload.effect_kind(),
        group: EffectGroup::Other,
        amount_usd: None,
        established_thread: false,
        first_time_counterparty: false,
        payload: serde_json::to_value(&payload).expect("payload"),
        agent: None,
        run_id: None,
    };
    let id = runtime
        .approvals
        .park(runtime.id(), effect.clone())
        .await
        .expect("parks");
    runtime
        .journal
        .record_parked(
            &id,
            &effect,
            super::now_millis(),
            TaskLink::from_task_id(Some(task_id)),
            ApprovalConversation::default(),
            None,
        )
        .await
        .expect("journals");
}

/// A blocker that names no conversation is pending in **no**
/// conversation — `#general` least of all.
#[tokio::test]
async fn a_thread_less_blocker_is_pending_in_no_conversation() {
    let (runtime, _home) = runtime().await;
    park_thread_less_blocker(&runtime, "t-1").await;
    assert_eq!(
        runtime.pending_approvals()[0].thread,
        None,
        "the park under test is the thread-less shape"
    );

    for desk in ["main", "general", "General", ""] {
        let plan = runtime
            .plan_blocker_reply(desk, None, "please retry the nightly import")
            .await
            .expect("plan");
        assert!(
            matches!(plan, BlockerReplyPlan::NotBlocker),
            "a top-level message in {desk:?} must run as an ordinary turn, not settle a \
             blocker no conversation raised: {plan:?}"
        );
    }
    assert_eq!(
        runtime.pending_approvals().len(),
        1,
        "nothing was consumed, so the blocker still pends for whoever can actually answer it"
    );
}

/// The carve-out is not a blanket refusal: a blocker stamped with a real
/// thread still answers to it. Guards the fix from being "skip every
/// blocker", which would pass the test above and break #1862 outright.
#[tokio::test]
async fn a_threaded_blocker_still_answers_in_its_own_dm() {
    let (runtime, _home) = runtime().await;
    park_thread_less_blocker(&runtime, "t-1").await;
    runtime
        .park_blocker(&blocker("t-2", None), "t-2", assignee("eng"))
        .await
        .expect("parks");

    let plan = runtime
        .plan_blocker_reply("dm:eng", None, "retry it")
        .await
        .expect("plan");
    match plan {
        BlockerReplyPlan::Resolve { ids, .. } => assert_eq!(
            ids.len(),
            1,
            "only the blocker stamped with this DM is in scope; the thread-less one is in \
             no conversation and must not be fanned in"
        ),
        other => panic!("the DM's own blocker still resolves: {other:?}"),
    }
}

/// An unrelated reply is not a verdict — it falls through to an ordinary
/// turn rather than settling the blocker.
#[tokio::test]
async fn an_unrelated_reply_is_not_a_verdict() {
    let (runtime, _home) = runtime().await;
    runtime
        .park_blocker(&blocker("t-1", None), "t-1", assignee("eng"))
        .await
        .expect("parks");
    let plan = runtime
        .plan_blocker_reply("dm:eng", None, "hey, how's it going?")
        .await
        .expect("plan");
    assert!(
        matches!(plan, BlockerReplyPlan::NotBlocker),
        "a greeting runs as a normal turn and settles nothing"
    );
    assert_eq!(
        runtime.pending_approvals().len(),
        1,
        "the blocker still pends"
    );
}

/// Two distinct blocked things in one DM: a bare verdict asks which; a
/// verdict naming one resolves only that one.
#[tokio::test]
async fn several_blockers_disambiguate_by_name() {
    let (runtime, _home) = runtime().await;
    runtime
        .park_blocker(
            &blocker("t-1", Some("connection:slack")),
            "t-1",
            assignee("eng"),
        )
        .await
        .expect("parks");
    runtime
        .park_blocker(
            &blocker("t-2", Some("connection:notion")),
            "t-2",
            assignee("eng"),
        )
        .await
        .expect("parks");

    let ambiguous = runtime
        .plan_blocker_reply("dm:eng", None, "retry it")
        .await
        .expect("plan");
    assert!(
        matches!(ambiguous, BlockerReplyPlan::AskWhich { .. }),
        "a bare verdict over two blocked things asks which"
    );

    let named = runtime
        .plan_blocker_reply("dm:eng", None, "retry slack")
        .await
        .expect("plan");
    match named {
        BlockerReplyPlan::Resolve { ids, .. } => {
            assert_eq!(
                ids,
                runtime.blocker_group_members("connection:slack", Some("task"))
            );
        }
        _ => panic!("naming the connection resolves only its group"),
    }
}

/// An explicit reply settles only a blocker parked in the same
/// conversation: a verdict threaded to another DM's blocker card, sent
/// from a desk with no blocker of its own, runs as an ordinary turn.
#[tokio::test]
async fn an_explicit_reply_stays_within_its_conversation() {
    let (runtime, _home) = runtime().await;
    runtime
        .park_blocker(
            &blocker("t-1", Some("connection:slack")),
            "t-1",
            assignee("eng"),
        )
        .await
        .expect("parks");
    let parent = runtime
        .events
        .read_from(
            runtime.id(),
            crate::ports::types::EventSeq::new(0),
            usize::MAX,
        )
        .await
        .expect("read")
        .into_iter()
        .find(|stored| {
            matches!(
                stored.event,
                crate::ports::types::CompanyEvent::ApprovalParked { .. }
            )
        })
        .expect("the park is on the log")
        .seq;

    let same = runtime
        .plan_blocker_reply("dm:eng", Some(parent), "retry")
        .await
        .expect("plan");
    assert!(
        matches!(same, BlockerReplyPlan::Resolve { .. }),
        "a reply in the blocker's own DM resolves it"
    );

    let cross = runtime
        .plan_blocker_reply("dm:ops", Some(parent), "retry")
        .await
        .expect("plan");
    assert!(
        matches!(cross, BlockerReplyPlan::NotBlocker),
        "the same verdict from another conversation settles nothing"
    );
}
