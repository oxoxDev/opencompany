use super::*;
use crate::company::CompanyManifest;
use crate::ports::tasks::TaskTitle;
use crate::ports::tasks::{COLUMN_TODO, TaskDeliverable, TaskRecord};
use crate::ports::types::CompanyId;
use crate::runtime::RuntimeBuilder;
use std::sync::Arc;

fn manifest() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n")
        .expect("parse manifest")
}

fn card(id: &str) -> TaskRecord {
    TaskRecord {
        id: id.to_string(),
        title: TaskTitle::authored("Draft the launch note"),
        note: None,
        column: COLUMN_TODO.to_string(),
        priority: "medium".to_string(),
        assignee: String::new(),
        updated_at_millis: 1,
        origin: None,
        parent_task_id: None,
        output: None,
        plan: None,
        planning_attempts: Vec::new(),
        deliverable: TaskDeliverable::Once,
        workflow_proposal: None,
        origin_run_id: None,
        origin_workflow_id: None,
        origin_message_seq: None,
        bounced: None,
    }
}

/// A reply that opened a card, exactly as the dispatch path journals it.
fn reply_naming(task_id: &str) -> CompanyEvent {
    CompanyEvent::AgentReply {
        audience: Vec::new(),
        mentions: Vec::new(),
        mention_depth: 0,
        parent: None,
        task_id: Some(task_id.to_string()),
        outputs: Vec::new(),
        chat_id: GENERAL_CHANNEL_ID.to_string(),
        agent_id: "ceo".to_string(),
        text: "Opened a card for that.".to_string(),
        steps: Vec::new(),
        episode: None,
    }
}

async fn runtime(home: &std::path::Path) -> Arc<CompanyRuntime> {
    Arc::new(
        RuntimeBuilder::new(home.to_path_buf(), manifest())
            .with_id(CompanyId::new("acme"))
            .build()
            .await
            .expect("build a runtime"),
    )
}

/// The chip survives a reload while the card is still on the board — the
/// behaviour issue #246 added and `chat-to-card.spec.ts` pins.
///
/// Asserted first so the test below cannot pass by the projection simply
/// dropping every `task_id` it sees.
#[tokio::test]
async fn a_reply_keeps_its_card_while_the_card_exists() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    runtime
        .tasks()
        .upsert(&id, &card("card-1"))
        .await
        .expect("seed the board");
    runtime
        .events()
        .append(&id, reply_naming("card-1"))
        .await
        .expect("journal the reply");

    let history = history_for_desk(
        &runtime,
        GENERAL_CHANNEL_ID,
        GENERAL_CHANNEL_ID,
        &Viewer::Operator,
        None,
        50,
        true,
    )
    .await
    .expect("history");

    assert_eq!(
        history.iter().filter_map(|m| m.task_id.as_deref()).count(),
        1,
        "the chip is projected while the card is on the board: {history:?}"
    );
}

/// **The reload half of the dismissal (issue #984).**
///
/// The journal still records that the turn opened a card — it did, and that
/// event is not rewritten. What must not happen is the *projection* handing
/// the console an id it can only render as a link to a `404`, which is how a
/// completed delete comes back looking like a failed one.
///
/// Deleting the card is the only difference from the test above.
#[tokio::test]
async fn a_reply_loses_its_card_once_the_card_is_deleted() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    runtime
        .tasks()
        .upsert(&id, &card("card-1"))
        .await
        .expect("seed the board");
    runtime
        .events()
        .append(&id, reply_naming("card-1"))
        .await
        .expect("journal the reply");
    assert!(
        runtime
            .tasks()
            .delete(&id, "card-1")
            .await
            .expect("delete the card"),
        "the card was there to delete"
    );

    let history = history_for_desk(
        &runtime,
        GENERAL_CHANNEL_ID,
        GENERAL_CHANNEL_ID,
        &Viewer::Operator,
        None,
        50,
        true,
    )
    .await
    .expect("history");

    assert!(
        !history.is_empty(),
        "the reply itself still belongs in the transcript — only its card is gone"
    );
    assert!(
        history.iter().all(|m| m.task_id.is_none()),
        "a rehydrated chip for a deleted card is a link to a 404, which reads \
         as the delete having failed: {history:?}"
    );
}

/// The board is read once per history, and not at all when no row carries a
/// card — the cost argument for doing this in the projection.
///
/// Asserted through behaviour rather than a call count: a transcript with no
/// cards comes back unchanged.
#[tokio::test]
async fn a_transcript_with_no_cards_is_untouched() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    runtime
        .events()
        .append(
            &id,
            CompanyEvent::AgentReply {
                audience: Vec::new(),
                mentions: Vec::new(),
                mention_depth: 0,
                parent: None,
                task_id: None,
                outputs: Vec::new(),
                chat_id: GENERAL_CHANNEL_ID.to_string(),
                agent_id: "ceo".to_string(),
                text: "just talking".to_string(),
                steps: Vec::new(),
                episode: None,
            },
        )
        .await
        .expect("journal the reply");

    let history = history_for_desk(
        &runtime,
        GENERAL_CHANNEL_ID,
        GENERAL_CHANNEL_ID,
        &Viewer::Operator,
        None,
        50,
        true,
    )
    .await
    .expect("history");

    assert_eq!(history.len(), 1, "{history:?}");
    assert!(history[0].task_id.is_none(), "{history:?}");
}

/// Issue #1781 review (Codex P1): an `owner`-fallback report — marked via
/// [`crate::runtime::OWNER_FALLBACK_REPORT_AUTHOR`] — must never reach a
/// non-admin viewer, while an ordinary operator-channel report (any other
/// author) is unaffected. Pre-fix, `history_for_desk` had no concept of
/// `admin_only` at all: every signed-in company user, admin or Member, saw
/// every row on a desk they could address, which is exactly the leak this
/// test pins shut.
#[tokio::test]
async fn an_owner_fallback_row_is_hidden_from_a_non_admin_viewer() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    runtime
        .events()
        .append(
            &id,
            CompanyEvent::AgentReply {
                audience: Vec::new(),
                mentions: Vec::new(),
                mention_depth: 0,
                parent: None,
                task_id: None,
                outputs: Vec::new(),
                chat_id: crate::runtime::OPERATOR_CHANNEL.to_string(),
                agent_id: crate::runtime::OWNER_FALLBACK_REPORT_AUTHOR.to_string(),
                text: "admin-only owner report".to_string(),
                steps: Vec::new(),
                episode: None,
            },
        )
        .await
        .expect("journal the owner-fallback report");
    runtime
        .events()
        .append(
            &id,
            CompanyEvent::AgentReply {
                audience: Vec::new(),
                mentions: Vec::new(),
                mention_depth: 0,
                parent: None,
                task_id: None,
                outputs: Vec::new(),
                chat_id: crate::runtime::OPERATOR_CHANNEL.to_string(),
                agent_id: crate::runtime::WORKFLOW_REPLY_AUTHOR.to_string(),
                text: "ordinary workflow report".to_string(),
                steps: Vec::new(),
                episode: None,
            },
        )
        .await
        .expect("journal the ordinary report");

    let as_member = history_for_desk(
        &runtime,
        crate::runtime::OPERATOR_CHANNEL,
        crate::runtime::OPERATOR_CHANNEL,
        &Viewer::Operator,
        None,
        50,
        false,
    )
    .await
    .expect("history");
    assert_eq!(
        as_member.len(),
        1,
        "a non-admin must not see the owner-fallback row: {as_member:?}"
    );
    assert_eq!(as_member[0].text, "ordinary workflow report");
    assert!(!as_member[0].admin_only, "{as_member:?}");

    let as_admin = history_for_desk(
        &runtime,
        crate::runtime::OPERATOR_CHANNEL,
        crate::runtime::OPERATOR_CHANNEL,
        &Viewer::Operator,
        None,
        50,
        true,
    )
    .await
    .expect("history");
    assert_eq!(
        as_admin.len(),
        2,
        "an admin must see both rows: {as_admin:?}"
    );
    assert!(as_admin.iter().any(|m| m.admin_only), "{as_admin:?}");
}

/// The exclusion happens inside the paging loop, before a row counts
/// toward `first` (see `history_for_desk`'s doc) — proven by requesting
/// exactly one row as a non-admin with an admin-only row sorted newest: a
/// post-fetch filter would come back empty here, not with the one visible
/// row underneath it.
#[tokio::test]
async fn a_non_admin_page_fills_past_an_admin_only_row_instead_of_coming_back_short() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    // Oldest first: the visible row, then the admin-only row on top of it.
    runtime
        .events()
        .append(
            &id,
            CompanyEvent::AgentReply {
                audience: Vec::new(),
                mentions: Vec::new(),
                mention_depth: 0,
                parent: None,
                task_id: None,
                outputs: Vec::new(),
                chat_id: crate::runtime::OPERATOR_CHANNEL.to_string(),
                agent_id: crate::runtime::WORKFLOW_REPLY_AUTHOR.to_string(),
                text: "visible report".to_string(),
                steps: Vec::new(),
                episode: None,
            },
        )
        .await
        .expect("journal the ordinary report");
    runtime
        .events()
        .append(
            &id,
            CompanyEvent::AgentReply {
                audience: Vec::new(),
                mentions: Vec::new(),
                mention_depth: 0,
                parent: None,
                task_id: None,
                outputs: Vec::new(),
                chat_id: crate::runtime::OPERATOR_CHANNEL.to_string(),
                agent_id: crate::runtime::OWNER_FALLBACK_REPORT_AUTHOR.to_string(),
                text: "admin-only report".to_string(),
                steps: Vec::new(),
                episode: None,
            },
        )
        .await
        .expect("journal the owner-fallback report");

    let as_member = history_for_desk(
        &runtime,
        crate::runtime::OPERATOR_CHANNEL,
        crate::runtime::OPERATOR_CHANNEL,
        &Viewer::Operator,
        None,
        1,
        false,
    )
    .await
    .expect("history");

    assert_eq!(
        as_member.len(),
        1,
        "a non-admin's page must fill with the next visible row, not come \
         back short: {as_member:?}"
    );
    assert_eq!(as_member[0].text, "visible report");
}

/// Issue #1781 review (Codex P2): `history_total_for_desk` must agree with
/// `history_for_desk` about which rows a non-admin can see. Pre-fix, this
/// count had no `is_admin` param at all — a non-admin querying a desk
/// holding an owner-fallback row (e.g. a grandfathered real desk at the
/// literal `operator` id) got a `total` one higher than `items.len()`
/// could ever be, breaking `Page.total`'s item-count contract and
/// revealing that a hidden admin report exists.
#[tokio::test]
async fn total_excludes_the_owner_fallback_row_for_a_non_admin_but_counts_it_for_an_admin() {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");

    runtime
        .events()
        .append(
            &id,
            CompanyEvent::AgentReply {
                audience: Vec::new(),
                mentions: Vec::new(),
                mention_depth: 0,
                parent: None,
                task_id: None,
                outputs: Vec::new(),
                chat_id: crate::runtime::OPERATOR_CHANNEL.to_string(),
                agent_id: crate::runtime::OWNER_FALLBACK_REPORT_AUTHOR.to_string(),
                text: "admin-only owner report".to_string(),
                steps: Vec::new(),
                episode: None,
            },
        )
        .await
        .expect("journal the owner-fallback report");
    runtime
        .events()
        .append(
            &id,
            CompanyEvent::AgentReply {
                audience: Vec::new(),
                mentions: Vec::new(),
                mention_depth: 0,
                parent: None,
                task_id: None,
                outputs: Vec::new(),
                chat_id: crate::runtime::OPERATOR_CHANNEL.to_string(),
                agent_id: crate::runtime::WORKFLOW_REPLY_AUTHOR.to_string(),
                text: "ordinary workflow report".to_string(),
                steps: Vec::new(),
                episode: None,
            },
        )
        .await
        .expect("journal the ordinary report");

    let as_member = history_total_for_desk(
        &runtime,
        crate::runtime::OPERATOR_CHANNEL,
        crate::runtime::OPERATOR_CHANNEL,
        None,
        false,
    )
    .await
    .expect("total");
    assert_eq!(
        as_member, 1,
        "a non-admin's total must match what history_for_desk would ever show them"
    );

    let as_admin = history_total_for_desk(
        &runtime,
        crate::runtime::OPERATOR_CHANNEL,
        crate::runtime::OPERATOR_CHANNEL,
        None,
        true,
    )
    .await
    .expect("total");
    assert_eq!(as_admin, 2, "an admin's total must count both rows");
}

/// Issue #1781 review (Codex P2, follow-up): `channel_attributed_replies`
/// must agree with `history_for_desk` / `history_total_for_desk` about
/// which rows a non-admin can see. Pre-fix, it had no `is_admin` param at
/// all — a Member polling `/chat/attribution-audit` around an
/// owner-fallback delivery watched `replies` tick up for a row neither
/// the transcript nor SSE ever showed them, confirming a hidden
/// admin-only message exists even though its content stayed hidden.
#[tokio::test]
async fn attribution_audit_excludes_the_owner_fallback_row_for_a_non_admin_but_counts_it_for_an_admin()
 {
    let home = tempfile::tempdir().expect("tempdir");
    let runtime = runtime(home.path()).await;
    let id = CompanyId::new("acme");
    let record = runtime
        .store()
        .load(&id)
        .await
        .expect("load")
        .expect("record exists");

    runtime
        .events()
        .append(
            &id,
            CompanyEvent::AgentReply {
                audience: Vec::new(),
                mentions: Vec::new(),
                mention_depth: 0,
                parent: None,
                task_id: None,
                outputs: Vec::new(),
                chat_id: crate::runtime::OPERATOR_CHANNEL.to_string(),
                agent_id: crate::runtime::OWNER_FALLBACK_REPORT_AUTHOR.to_string(),
                text: "admin-only owner report".to_string(),
                steps: Vec::new(),
                episode: None,
            },
        )
        .await
        .expect("journal the owner-fallback report");
    runtime
        .events()
        .append(
            &id,
            CompanyEvent::AgentReply {
                audience: Vec::new(),
                mentions: Vec::new(),
                mention_depth: 0,
                parent: None,
                task_id: None,
                outputs: Vec::new(),
                chat_id: crate::runtime::OPERATOR_CHANNEL.to_string(),
                agent_id: crate::runtime::WORKFLOW_REPLY_AUTHOR.to_string(),
                text: "ordinary workflow report".to_string(),
                steps: Vec::new(),
                episode: None,
            },
        )
        .await
        .expect("journal the ordinary report");

    let as_member = channel_attributed_replies(&runtime, &record, false)
        .await
        .expect("audit");
    assert_eq!(
        as_member.replies, 1,
        "a non-admin's replies count must match what history_for_desk would \
         ever show them: {as_member:?}"
    );

    let as_admin = channel_attributed_replies(&runtime, &record, true)
        .await
        .expect("audit");
    assert_eq!(as_admin.replies, 2, "an admin's count must count both rows");
}
