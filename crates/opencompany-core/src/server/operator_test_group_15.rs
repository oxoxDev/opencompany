use super::*;
#[cfg(feature = "openhuman")]
use crate::ports::tasks::TaskTitle;
#[cfg(feature = "openhuman")]
use axum::http::StatusCode;

use super::operator_test_support_1::*;
use super::operator_test_support_3::*;

/// [`mention_context`] canonicalizes a **`dm:`-prefixed** noncanonical key
/// too. An API client can address a DM with the console's channel shape but
/// a noncanonical payload — `dm:BACKEND_ENGINEER` for the teammate whose id
/// is `backend_engineer`. The routing resolves that case-insensitively, so
/// the stored context has to carry the canonical agent id: filing the raw
/// key under `dm:BACKEND_ENGINEER` badges a rail channel that does not
/// exist, and opening the actual DM can never clear it. Pre-fix, the
/// `dm:`-prefixed branch returned the key verbatim and bypassed
/// `assignee::resolve` entirely.
#[tokio::test]
async fn mention_context_canonicalizes_prefixed_dm_keys() {
    let home_dir = home();
    let state = state_with_roster(home_dir.path()).await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).expect("company registered");

    // A case-variant of the teammate's id, carrying the `dm:` prefix the
    // console mints.
    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, &[], "dm:BACKEND_ENGINEER")
            .await,
        "dm:backend_engineer",
        "a `dm:`-prefixed noncanonical teammate key has to store dm:<agent-id>"
    );
    // The already-canonical shape stays unchanged — the resolution must
    // not move a key that was already right.
    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, &[], "dm:backend_engineer")
            .await,
        "dm:backend_engineer",
        "a canonical dm:<teammate-id> key is kept as-is"
    );
    // A `dm:` key whose bare half names a desk (the desk-first ordering the
    // routing uses) files under the desk id, not a nonexistent `dm:<desk>`.
    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, &[], "dm:Engineering")
            .await,
        "engineering",
        "a `dm:` key that resolves to a desk has to store the desk id"
    );
}

/// A desk id that collides with a **human user id** still files under the
/// desk. `assignee::resolve`'s desk-first ordering — the same one
/// `responder_for` uses — outranks the user directory, and the directory
/// must not get a say ahead of it. Pre-fix, a `users` pre-check ran before
/// the resolution and returned `dm:<id>` for any bare key matching a human,
/// so a mention aimed at a desk whose id happened to match a human id would
/// badge a nonexistent DM channel and could never be cleared from the desk
/// it was meant for.
#[tokio::test]
async fn mention_context_a_human_id_matching_a_desk_id_stays_a_desk() {
    let home_dir = home();
    let state = state_with_roster(home_dir.path()).await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).expect("company registered");

    // A human whose id collides with the `engineering` desk's id. The human
    // directory must not win: the message is aimed at the desk.
    let human = crate::ports::users::UserRecord {
        id: "engineering".to_string(),
        email: "human@example.test".to_string(),
        display_name: None,
        avatar: None,
        role: crate::ports::users::UserRole::Member,
        status: crate::ports::users::UserStatus::Active,
        password_hash: None,
        must_change_password: false,
        created_at_millis: crate::ports::now_millis(),
        last_seen_at_millis: None,
        updated_at_millis: crate::ports::now_millis(),
    };

    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, std::slice::from_ref(&human), "engineering")
            .await,
        "engineering",
        "a desk id that matches a human id files under the desk, not dm:<id>"
    );
    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, std::slice::from_ref(&human), "dm:engineering")
            .await,
        "engineering",
        "the same collision through a dm:-prefixed key still files under the desk"
    );
    // A DM the human is actually a teammate of still badges as a DM.
    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, &[human], "dm:backend_engineer")
            .await,
        "dm:backend_engineer",
        "a real DM channel is unaffected by the collision guard"
    );
}

/// [`mention_context`] resolves a `dm:`-prefixed key **as sent** before
/// stripping the prefix, so a desk literally named `dm:engineering` keeps
/// that id. Pre-fix, the unconditional strip resolved `engineering` instead
/// and filed the badge under the wrong transcript — the exact claim
/// [`assignee::dm_key`]'s contract warns about.
#[tokio::test]
async fn mention_context_a_desk_literally_named_dm_prefix_keeps_its_id() {
    let home_dir = home();
    let state = state_with_dm_prefixed_desk(home_dir.path()).await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).expect("company registered");

    // The literal `dm:engineering` desk resolves as sent; stripping would
    // misroute to the plain `engineering` desk.
    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, &[], "dm:engineering")
            .await,
        "dm:engineering",
        "a desk literally named dm:<…> keeps its id — the raw key resolves first"
    );
    // The un-prefixed desk is untouched by the collision.
    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, &[], "engineering")
            .await,
        "engineering",
        "the un-prefixed desk still resolves to its own id"
    );
    // A genuine DM still re-keys onto the rail's DM channel.
    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, &[], "dm:backend_engineer")
            .await,
        "dm:backend_engineer",
        "a real DM channel is unaffected by the literal dm: desk"
    );
}

/// [`mention_context`] stores the **canonical** id for a key typed in a
/// noncanonical shape — a desk by its display name, a teammate by a
/// case-variant of their id. `assignee::resolve` already returns canonical
/// ids (issue #214); storing the raw key instead would file the badge under
/// a channel id the rail never has, so it could neither render nor clear.
#[tokio::test]
async fn mention_context_stores_canonical_ids_for_noncanonical_keys() {
    let home_dir = home();
    let state = state_with_roster(home_dir.path()).await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).expect("company registered");

    // A desk addressed by its display name files under the desk's id —
    // `"Engineering"` names the desk whose id is `engineering`.
    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, &[], "Engineering")
            .await,
        "engineering",
        "a desk named by its display name has to store the desk id, not the raw key"
    );
    // A teammate addressed by a case-variant of their id files under the
    // canonical agent id, re-keyed into the console's DM channel space.
    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, &[], "BACKEND_ENGINEER")
            .await,
        "dm:backend_engineer",
        "a teammate named by a noncanonical key has to store dm:<agent-id>"
    );
}

/// [`mention_context`] files a mention in the General desk — the default an
/// unaddressed message lands in — under the console's canonical main-thread
/// id even when this company has no desk named/id `General`. This fixture's
/// only desk is `engineering`, so every general-chat spelling would
/// otherwise fall through to the raw string and badge a rail row that does
/// not exist (issue #1665 follow-up).
#[tokio::test]
async fn mention_context_maps_unresolvable_general_spellings_to_main() {
    let home_dir = home();
    let state = state_with_roster(home_dir.path()).await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).expect("company registered");

    for general in ["General", "general", "main", ""] {
        assert_eq!(
            runtime
                .mention_seam()
                .mention_context(&id, &[], general)
                .await,
            crate::ports::general_channel::GENERAL_CHANNEL_ID,
            "a mention in the General desk ({general:?}) has to store the console's \
             main-thread id, which the rail aliases onto its first rendered desk \
             channel"
        );
    }
    // A desk that does resolve keeps its canonical id — the general-chat
    // mapping must not swallow a real desk.
    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, &[], "Engineering")
            .await,
        "engineering",
        "a real desk keeps its canonical id even when its name looks general"
    );
}

/// [`mention_context`] canonicalizes a **memberless** desk too. A desk that
/// exists but has nobody seated on it is still a real desk with a real rail
/// channel, so a key typed as its display name must file under its canonical
/// id: `"Sales"` has to badge `#sales`, and opening `#sales` has to clear it.
/// Pre-fix, `EmptyDesk` fell through the same wildcard as `Unknown` and
/// stored the raw key — a channel id no desk renders, so the badge was
/// invisible and could never clear.
#[tokio::test]
async fn mention_context_canonicalizes_a_memberless_desk() {
    let home_dir = home();
    let state = state_with_memberless_desk(home_dir.path()).await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).expect("company registered");

    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, &[], "Sales")
            .await,
        "sales",
        "a memberless desk named by its display name has to store the desk id, \
         not the raw key — the rail's channel id is `sales`"
    );
    // The desk that does have a lead keeps behaving as before.
    assert_eq!(
        runtime
            .mention_seam()
            .mention_context(&id, &[], "Engineering")
            .await,
        "engineering",
        "a desk with a lead still stores its canonical id"
    );
}

/// Issue #1781 review (Codex P1): [`company_events`]'s periodic refresh
/// must re-derive admin access from the live user record, not keep
/// answering with whatever it was when the SSE stream opened. Proven
/// directly against [`refreshed_is_admin`] — the seam that refresh loop
/// calls on every tick — rather than the SSE handler itself, since the
/// handler's own timing (a real `EventSource`, a 60s interval) is not
/// what this bug is about.
#[tokio::test]
async fn refreshed_is_admin_reflects_a_mid_stream_demotion() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();

    let mut user = crate::ports::users::UserRecord {
        id: "u1".to_string(),
        email: "admin@acme.test".to_string(),
        display_name: None,
        avatar: None,
        role: crate::ports::users::UserRole::Admin,
        status: crate::ports::users::UserStatus::Active,
        password_hash: None,
        must_change_password: false,
        created_at_millis: crate::ports::now_millis(),
        last_seen_at_millis: None,
        updated_at_millis: crate::ports::now_millis(),
    };
    runtime
        .users()
        .upsert_user(runtime.id(), &user)
        .await
        .unwrap();
    let actor = Actor {
        kind: ActorKind::User,
        id: user.id.clone(),
    };

    assert!(
        refreshed_is_admin(&runtime, Some(&actor), false).await,
        "an active admin's record must resolve to admin, even starting from a stale `false`"
    );

    // The demotion itself: same shape `PATCH …/users/{id}` writes, and —
    // critically — it does not touch sessions, so a connection opened
    // before this write stays open exactly as it would in production.
    user.role = crate::ports::users::UserRole::Member;
    runtime
        .users()
        .upsert_user(runtime.id(), &user)
        .await
        .unwrap();

    assert!(
        !refreshed_is_admin(&runtime, Some(&actor), true).await,
        "a demoted user's live record must flip a stale `true` to `false` — this is \
         exactly the check `company_events` failed to make before this fix, leaking the \
         owner-fallback admin-only report to a demoted viewer for the rest of their stream"
    );

    // Suspension revokes admin the same way, even if role were untouched.
    user.role = crate::ports::users::UserRole::Admin;
    user.status = crate::ports::users::UserStatus::Suspended;
    runtime
        .users()
        .upsert_user(runtime.id(), &user)
        .await
        .unwrap();

    assert!(
        !refreshed_is_admin(&runtime, Some(&actor), true).await,
        "a suspended admin must not keep admin-only visibility either"
    );
}

/// Issue #1781 review, Codex P1 second follow-up: a human actor whose
/// current role cannot be confirmed — `Ok(None)` because the user record
/// has gone missing, folded in here with a genuine store error since both
/// hit the same match arm — must resolve to `false`, not `previous`.
///
/// `previous: true` here stands in for exactly the dangerous case: a
/// cached "was admin" value from before whatever made this actor
/// unconfirmable, revalidated at the one call site
/// (`is_admin_for_item`) that gates the admin-only owner-fallback report
/// on this result directly. Before this fix, an actor deleted out from
/// under an open SSE stream — or a transient read failure landing at the
/// exact moment a report needed gating — fell back to `previous` and kept
/// leaking the report, silently, for as long as the failure (or the
/// missing record) persisted.
#[tokio::test]
async fn refreshed_is_admin_fails_closed_when_the_user_record_cannot_be_found() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();

    // Never upserted — `get_user` answers `Ok(None)`, the "record has
    // gone missing" half of the case this proves.
    let actor = Actor {
        kind: ActorKind::User,
        id: "ghost".to_string(),
    };

    assert!(
        !refreshed_is_admin(&runtime, Some(&actor), true).await,
        "a human actor with no resolvable user record must read as not-admin \
         even when the cached value being revalidated was `true` — trusting \
         `previous` here is exactly the fail-open gap this fix closes"
    );
}

/// Issue #1781 review, Codex P1 follow-up: even with the periodic refresh
/// the test above covers, `company_events` still only re-checked on its
/// own `LABEL_REFRESH_EVERY` (60s) tick — a demotion landing right after
/// one tick left an open SSE stream projecting an owner-fallback report
/// under a stale cached `true` for up to another 60s. `is_admin_for_item`
/// is the fix: it revalidates fresh for that one content class instead of
/// trusting `cached`, no matter how long ago the last periodic tick was —
/// proven here by feeding it a `cached: true` that is already wrong the
/// instant this call happens, with no `sleep` at all.
///
/// The second half is the other side of the same fix: an *ordinary* event
/// must keep using `cached` untouched, or every SSE item would pay a
/// store read regardless of content — the whole reason the fix is scoped
/// to the owner-fallback content class rather than revalidating every
/// item.
#[tokio::test]
async fn is_admin_for_item_revalidates_only_the_owner_fallback_report() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();

    let mut user = crate::ports::users::UserRecord {
        id: "u1".to_string(),
        email: "admin@acme.test".to_string(),
        display_name: None,
        avatar: None,
        role: crate::ports::users::UserRole::Admin,
        status: crate::ports::users::UserStatus::Active,
        password_hash: None,
        must_change_password: false,
        created_at_millis: crate::ports::now_millis(),
        last_seen_at_millis: None,
        updated_at_millis: crate::ports::now_millis(),
    };
    runtime
        .users()
        .upsert_user(runtime.id(), &user)
        .await
        .unwrap();
    let actor = Actor {
        kind: ActorKind::User,
        id: user.id.clone(),
    };

    // The demotion: no wait, no periodic tick — the very next item must
    // already see it for the gated content class.
    user.role = crate::ports::users::UserRole::Member;
    runtime
        .users()
        .upsert_user(runtime.id(), &user)
        .await
        .unwrap();

    let owner_fallback_item = EventStreamItem::Event(stored(CompanyEvent::AgentReply {
        audience: Vec::new(),
        mentions: Vec::new(),
        mention_depth: 0,
        parent: None,
        task_id: None,
        outputs: Vec::new(),
        chat_id: "operator".into(),
        agent_id: crate::runtime::OWNER_FALLBACK_REPORT_AUTHOR.to_string(),
        text: "no admin has a mailbox".into(),
        steps: Vec::new(),
        episode: None,
    }));
    assert!(
        !super::is_admin_for_item(&owner_fallback_item, &runtime, Some(&actor), true).await,
        "an owner-fallback report must revalidate fresh and see the demotion \
         immediately — a stale cached `true` must never leak this content, \
         regardless of when the last periodic refresh ran"
    );

    let ordinary_item = EventStreamItem::Event(stored(CompanyEvent::AgentReply {
        audience: Vec::new(),
        mentions: Vec::new(),
        mention_depth: 0,
        parent: None,
        task_id: None,
        outputs: Vec::new(),
        chat_id: "General".into(),
        agent_id: "ceo".into(),
        text: "ordinary reply".into(),
        steps: Vec::new(),
        episode: None,
    }));
    assert!(
        super::is_admin_for_item(&ordinary_item, &runtime, Some(&actor), true).await,
        "an ordinary event must keep using the cached snapshot untouched — \
         revalidating every item, not just the gated content class, would \
         add a store read to the hot path for no reason"
    );
}

/// The machine principal has no user record to look up — `actor: None` —
/// and [`ScopedCompany::is_admin`]'s own doc says it is unrestricted by
/// construction, so the refresh must leave it alone rather than treating
/// a missing actor as "look up nothing, therefore not admin".
#[tokio::test]
async fn refreshed_is_admin_leaves_the_machine_principal_unchanged() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();

    assert!(refreshed_is_admin(&runtime, None, true).await);
    assert!(!refreshed_is_admin(&runtime, None, false).await);
}

/// Two cards can be `in_review` on the same desk at once. Approving the
/// pill the operator actually clicked must move that card and leave the
/// other alone — resolving the desk's most-recently-updated card instead
/// (Codex #3903031183) moves the wrong one whenever the older pill is
/// clicked after a newer card has settled.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn review_card_settles_the_clicked_task_not_the_desks_latest() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();

    for (task_id, updated_at_millis) in [("t-old", 1u64), ("t-new", 2u64)] {
        runtime
            .tasks()
            .upsert(
                runtime.id(),
                &crate::ports::tasks::TaskRecord {
                    id: task_id.to_string(),
                    title: TaskTitle::authored("Ship it"),
                    note: None,
                    column: crate::ports::tasks::COLUMN_IN_REVIEW.to_string(),
                    priority: "medium".to_string(),
                    assignee: "ceo".to_string(),
                    updated_at_millis,
                    origin: crate::ports::TaskOrigin::new(Some("strategy".to_string()), None),
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
                },
            )
            .await
            .unwrap();
    }

    let scope = ScopedCompany {
        runtime: runtime.clone(),
        actor: None,
        may_read_contents: true,
        is_admin: true,
    };
    let receipt = review_card(
        scope,
        Json(ChatReviewRequest {
            chat_id: "strategy".to_string(),
            task_id: "t-old".to_string(),
            decision: "approve".to_string(),
            note: None,
        }),
    )
    .await
    .expect("the clicked card is settled")
    .0;
    assert_eq!(receipt.task_id, "t-old");
    assert_eq!(receipt.column, crate::ports::tasks::COLUMN_DONE);

    let cards = runtime.tasks().list(runtime.id()).await.unwrap();
    let old = cards.iter().find(|t| t.id == "t-old").unwrap();
    let new = cards.iter().find(|t| t.id == "t-new").unwrap();
    assert_eq!(
        old.column,
        crate::ports::tasks::COLUMN_DONE,
        "the clicked pill's card must settle"
    );
    assert_eq!(
        new.column,
        crate::ports::tasks::COLUMN_IN_REVIEW,
        "the desk's newer card must be untouched by a verdict on the older pill"
    );
}

/// A `task_id` naming a card outside the reviewed desk (or one that has
/// already left `in_review`) must not resolve to some other card in the
/// conversation — the request is rejected rather than silently falling
/// back to "whatever is in review here".
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn review_card_rejects_a_task_id_not_in_review_on_this_desk() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();

    runtime
        .tasks()
        .upsert(
            runtime.id(),
            &crate::ports::tasks::TaskRecord {
                id: "t-review".to_string(),
                title: TaskTitle::authored("Ship it"),
                note: None,
                column: crate::ports::tasks::COLUMN_IN_REVIEW.to_string(),
                priority: "medium".to_string(),
                assignee: "ceo".to_string(),
                updated_at_millis: 1,
                origin: crate::ports::TaskOrigin::new(Some("strategy".to_string()), None),
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
            },
        )
        .await
        .unwrap();

    let scope = ScopedCompany {
        runtime: runtime.clone(),
        actor: None,
        may_read_contents: true,
        is_admin: true,
    };
    let err = review_card(
        scope,
        Json(ChatReviewRequest {
            chat_id: "strategy".to_string(),
            task_id: "does-not-exist".to_string(),
            decision: "approve".to_string(),
            note: None,
        }),
    )
    .await
    .expect_err("an unknown task id must not fall back to the desk's own card");
    assert_eq!(
        axum::response::IntoResponse::into_response(err).status(),
        StatusCode::NOT_FOUND
    );
}

/// `apply_review_decision`'s `Revise` arm through the HTTP handler: the
/// card re-enters `in_progress` with the operator's note appended, rather
/// than settling to `done` the way `Approve` does.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn review_card_revise_re_enters_in_progress_with_the_note() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();

    runtime
        .tasks()
        .upsert(
            runtime.id(),
            &crate::ports::tasks::TaskRecord {
                id: "t-1".to_string(),
                title: TaskTitle::authored("Ship it"),
                note: Some("[writer] first draft".to_string()),
                column: crate::ports::tasks::COLUMN_IN_REVIEW.to_string(),
                priority: "medium".to_string(),
                assignee: "ceo".to_string(),
                updated_at_millis: 1,
                origin: crate::ports::TaskOrigin::new(Some("strategy".to_string()), None),
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
            },
        )
        .await
        .unwrap();

    let scope = ScopedCompany {
        runtime: runtime.clone(),
        actor: None,
        may_read_contents: true,
        is_admin: true,
    };
    let receipt = review_card(
        scope,
        Json(ChatReviewRequest {
            chat_id: "strategy".to_string(),
            task_id: "t-1".to_string(),
            decision: "revise".to_string(),
            note: Some("tighten the intro".to_string()),
        }),
    )
    .await
    .expect("revise applies")
    .0;
    assert_eq!(receipt.task_id, "t-1");
    assert_eq!(receipt.column, crate::ports::tasks::COLUMN_IN_PROGRESS);

    let after = runtime
        .tasks()
        .list(runtime.id())
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.id == "t-1")
        .unwrap();
    let note = after.note.expect("note");
    assert!(note.contains("tighten the intro"), "{note}");
}
