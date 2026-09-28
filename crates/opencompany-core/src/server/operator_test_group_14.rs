use super::*;
use crate::ports::types::CompanyRecord;
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::operator_test_support_1::*;
use super::operator_test_support_3::*;

/// GRANT-012 (FAIL). `revoke_standing_grant` takes the grant out of the
/// live set **before** its durable journal append — the opposite order
/// from minting, and on purpose (see the function's own doc): a crash
/// here must fail toward no-permission, never toward a permission nobody
/// can see is still live. When the append then fails, the caller is told
/// the revoke failed, but the grant must already be gone from the live
/// set that actually governs future calls.
#[tokio::test]
async fn a_failed_revoke_append_still_removes_the_grant_from_the_live_set() {
    let home_dir = home();
    let store = std::sync::Arc::new(RefusingGrantRevokeStore {
        inner: crate::ports::journal::MemoryJournalStore::default(),
    });
    let m = manifest();
    let id = CompanyId::new("acme");
    let fs_store = FsCompanyStore::new(home_dir.path().to_path_buf());
    {
        use crate::ports::store::CompanyStore;
        fs_store
            .save(&CompanyRecord {
                general_channel: Default::default(),
                overlay_desk_hive: Vec::new(),
                overlay_retired_agents: Vec::new(),
                overlay_agent_edits: Vec::new(),
                id: id.clone(),
                manifest: m.clone(),
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
            })
            .await
            .unwrap();
    }
    let runtime = RuntimeBuilder::new(home_dir.path().to_path_buf(), m)
        .with_id(id.clone())
        .with_journal_store(store)
        .build()
        .await
        .unwrap();
    let runtime = Arc::new(runtime);
    runtime
        .grants
        .grant_standing(racing_standing_grant("g-append-fail"));

    let state = AppState::new(AppConfig::default());
    state.registry().insert(id.clone(), runtime.clone());
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;

    let app = router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/company/grants/g-append-fail")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "the forced append failure must surface"
    );
    assert_eq!(
        runtime.grants.standing().len(),
        0,
        "the live-set removal must land even though the durable record of it failed — \
         fail toward no permission, never toward one nobody can see is still granted"
    );
}

/// **The keystone (issue #469).** A turn that parks four sign-offs, all
/// approved, produces exactly ONE continuation — and an answer the operator
/// can actually see.
///
/// Before this, each resolve spawned its own follow-up cycle: four full
/// re-runs of one turn, each told about one decision. They did not race —
/// the per-company serial lock made them queue — but the later ones found
/// the grants the earlier ones had redeemed and produced nothing at all.
/// And none of it reached the operator either way, because the resolve
/// route never journaled a continuation's replies, so no `agent_reply`
/// frame was ever projected. Four approvals, four wasted turns, silence.
#[tokio::test]
async fn four_sign_offs_from_one_turn_produce_one_continuation() {
    let home_dir = home();
    let c = multi_park_company(home_dir.path(), 4, None, false).await;
    let before = c.cycles.load(std::sync::atomic::Ordering::SeqCst);

    let mut handles = Vec::new();
    for id in &c.approvals {
        let app = c.app.clone();
        let request = approve_detached(id);
        handles.push(tokio::spawn(
            async move { app.oneshot(request).await.unwrap() },
        ));
    }
    for handle in handles {
        assert_eq!(handle.await.unwrap().status(), StatusCode::OK);
    }
    settle(&c.runtime, 4).await;

    assert_eq!(
        c.cycles.load(std::sync::atomic::Ordering::SeqCst) - before,
        1,
        "one turn owes one continuation, not one per approval"
    );
    assert_eq!(
        c.decisions.lock().unwrap().len(),
        4,
        "the single continuation carries every decision, so the brain learns all four"
    );
    assert!(
        c.runtime.pending_approvals().is_empty(),
        "every sign-off was decided"
    );
    assert_eq!(
        agent_replies(&c.runtime).await.len(),
        4,
        "the continuation's answers must reach the event stream, or the operator \
         watches an approved action in silence"
    );
}

/// The two orders an operator can decide in must end in the same place.
///
/// Approving four at once and approving them one at a time are the same
/// request spread over a different span, and the gate is the last decision
/// rather than a time window — so neither can produce more continuations
/// than the other. A design that coalesced only what arrived together would
/// pass the test above and still re-run the turn four times here.
#[tokio::test]
async fn deciding_one_at_a_time_ends_where_deciding_all_at_once_does() {
    let home_dir = home();
    let c = multi_park_company(home_dir.path(), 4, None, false).await;
    let before = c.cycles.load(std::sync::atomic::Ordering::SeqCst);

    for (i, id) in c.approvals.iter().enumerate() {
        let response = c.app.clone().oneshot(approve_detached(id)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let ran = c.cycles.load(std::sync::atomic::Ordering::SeqCst) - before;
        if i < 3 {
            assert_eq!(
                ran,
                0,
                "the turn is still blocked on {} more sign-off(s); continuing now \
                 would re-park them",
                3 - i
            );
        }
    }
    settle(&c.runtime, 4).await;

    assert_eq!(
        c.cycles.load(std::sync::atomic::Ordering::SeqCst) - before,
        1,
        "the last decision unblocks the turn, and it runs once"
    );
    assert_eq!(c.decisions.lock().unwrap().len(), 4);
    assert_eq!(agent_replies(&c.runtime).await.len(), 4);
}

/// The continuation answers in the conversation the sign-off was raised in.
///
/// Not on the answering agent's own line: a desk channel's request and a
/// direct message to that channel's lead are answered by the same teammate,
/// so keying the reply on the agent delivers a channel's continuation into a
/// private thread nobody is watching (issue #379's lesson, which the reply
/// path had never learned — only the re-park had).
#[tokio::test]
async fn a_continuation_answers_in_the_thread_the_sign_off_was_raised_in() {
    let home_dir = home();
    let c = multi_park_company(home_dir.path(), 2, Some("sales"), false).await;

    for id in &c.approvals {
        let response = c.app.clone().oneshot(approve_detached(id)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    settle(&c.runtime, 2).await;

    let replies = agent_replies(&c.runtime).await;
    assert_eq!(replies.len(), 2, "both re-issues answered");
    assert!(
        replies.iter().all(|r| r.starts_with("sales|")),
        "the continuation must land in the channel the approval was raised in, got {replies:?}"
    );
}

/// **Issue #1092.** A workflow node's parked call, once approved, answers
/// on its run — never as a direct message from the teammate that ran it.
///
/// This is the wiring test for `continuation_fallback_chat_id`: the unit
/// tests pin what the fallback *returns*, and this pins that
/// `publish_continuation` actually uses it, through a real park, a real
/// resolve and the journal the console reads back.
///
/// The assertion is written against the agent id rather than only for the
/// run id, because that is the regression: the leak put the re-issued
/// turn's narration into `chat/history?desk=<teammate>`, where it rendered
/// as an unprompted DM.
#[tokio::test]
async fn a_workflow_parks_continuation_answers_on_the_run_not_in_a_dm() {
    let home_dir = home();
    let c = multi_park_company_run(home_dir.path(), 1, None, false, Some("run-1092"), None).await;

    let response = c
        .app
        .clone()
        .oneshot(approve_detached(&c.approvals[0]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    settle(&c.runtime, 1).await;

    let replies = agent_replies(&c.runtime).await;
    assert_eq!(replies.len(), 1, "the re-issue answered once");
    let (chat_id, _) = replies[0].split_once('|').expect("chat_id|text");
    assert_eq!(
        chat_id, "run-1092",
        "a workflow park's continuation belongs to its run, got {replies:?}"
    );
    // The regression, stated as itself: before this fix the fallback was
    // the answering teammate's own id, so this is what the leaked row held.
    assert_ne!(
        chat_id, "ceo",
        "the re-issue must not be journaled as a DM from the teammate that ran it"
    );
}

/// **Codex P1 (pass 2).** A continuation's reply is journaled through
/// `publish_continuation`, not the `/chat` turn — so a mention an agent
/// types back in an approval follow-up used to render as a chip and
/// nothing else: no badge, no durable row, exactly the person it is meant
/// to reach (offline when the reply lands) getting neither.
///
/// Both paths file through the same writer now; this pins that an `@user`
/// in a continuation reply lands as a mention notification whose audience
/// carries the person named, under the chat the continuation answered in.
#[tokio::test]
async fn a_continuation_reply_that_mentions_a_user_files_a_notification() {
    let home_dir = home();
    let c = multi_park_company_run(
        home_dir.path(),
        1,
        Some("sales"),
        false,
        None,
        Some("@harness-admin"),
    )
    .await;

    let users = c
        .runtime
        .users()
        .list_users(&CompanyId::new("acme"))
        .await
        .unwrap();
    let admin = users
        .iter()
        .find(|u| u.email == "harness-admin@example.test")
        .expect("the fixed admin is seeded");
    assert_eq!(
        admin.status,
        crate::ports::users::UserStatus::Active,
        "the admin must be an active, mentionable target"
    );

    let response = c
        .app
        .clone()
        .oneshot(approve_detached(&c.approvals[0]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    settle(&c.runtime, 1).await;
    // The notification is filed inside `publish_continuation`, after the
    // reply is journaled — `settle` only waits for the reply. A loaded CI
    // runner can reach this point before the notification append finishes,
    // so poll for it (issue #1665, Codex P1 regression).
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let notes = c
                .runtime
                .notifications()
                .list(&CompanyId::new("acme"), &admin.id)
                .await
                .unwrap();
            if notes.iter().any(|n| n.notification.kind == "mention") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the mention notification never appeared");

    let notes = c
        .runtime
        .notifications()
        .list(&CompanyId::new("acme"), &admin.id)
        .await
        .unwrap();
    let mentions: Vec<_> = notes
        .into_iter()
        .filter(|n| n.notification.kind == "mention")
        .collect();
    assert_eq!(
        mentions.len(),
        1,
        "the continuation's mention must badge the person it names"
    );
    let note = &mentions[0].notification;
    assert_eq!(note.context.as_deref(), Some("sales"));
    assert_eq!(
        note.title, "Someone mentioned you in sales",
        "a continuation has no author, so the generic label is the honest one"
    );
    assert!(
        note.audience
            .as_ref()
            .is_some_and(|a| a.contains(&admin.id)),
        "the named user must be in the notification's audience"
    );
}

/// **Issue #379's routing, re-homed (issue #469).** The continuation
/// resumes in the thread the sign-off was raised in — and in no other.
///
/// Asserted in **both directions**, because either alone would pass on a
/// mistake. A desk channel's request and a direct message to that channel's
/// lead are answered by the same teammate, so a reply keyed on the agent
/// lands a channel's continuation in a private line nobody is watching, and
/// a reply keyed on the channel does the reverse.
///
/// This used to be pinned inside the harness brain, against a hand-built
/// grant. It moved here with the journaling: the thread comes off the park
/// record now, so the strong version of the test is the one that lets a real
/// turn stamp it and a real resolve read it back.
#[tokio::test]
async fn a_continuation_resumes_in_the_thread_it_was_raised_in_and_no_other() {
    async fn threads_for(chat: &str) -> Vec<String> {
        let home_dir = home();
        let c = multi_park_company(home_dir.path(), 1, Some(chat), false).await;
        let response = c
            .app
            .clone()
            .oneshot(approve_detached(&c.approvals[0]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        settle(&c.runtime, 1).await;
        agent_replies(&c.runtime)
            .await
            .into_iter()
            .map(|r| r.split('|').next().unwrap().to_string())
            .collect()
    }

    // Raised in a desk channel: the continuation belongs to the channel.
    let desk = threads_for("desk-finance").await;
    assert_eq!(desk, vec!["desk-finance".to_string()]);
    assert_ne!(
        desk[0], "ceo",
        "a channel's approval must not resume in the desk lead's private DM"
    );

    // Raised in a direct message with that same lead: the mirror image.
    let dm = threads_for("ceo").await;
    assert_eq!(dm, vec!["ceo".to_string()]);
    assert_ne!(
        dm[0], "desk-finance",
        "a private line's approval must not resume in the desk channel"
    );
}

/// A single-approval turn is unchanged: it continues on that one decision,
/// exactly as it did before the gate existed.
#[tokio::test]
async fn a_lone_sign_off_still_continues_on_its_own_decision() {
    let home_dir = home();
    let c = multi_park_company(home_dir.path(), 1, None, false).await;
    let before = c.cycles.load(std::sync::atomic::Ordering::SeqCst);

    let response = c
        .app
        .clone()
        .oneshot(approve_detached(&c.approvals[0]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    settle(&c.runtime, 1).await;

    assert_eq!(
        c.cycles.load(std::sync::atomic::Ordering::SeqCst) - before,
        1
    );
    assert_eq!(agent_replies(&c.runtime).await.len(), 1);
}

/// **Defect 4.** A continuation that fails tells the person waiting for it.
///
/// The verdict and the grant are already durable at this point, so the
/// failure is recoverable — but only for somebody who knows it happened.
/// Before this the entire report was one `tracing::error!`: the agent was
/// not told the outcome, and neither was the operator, who saw an approval
/// they had granted produce nothing and had no way to tell a slow turn from
/// a dead one.
#[tokio::test]
async fn a_failed_continuation_tells_the_operator() {
    let home_dir = home();
    let c = multi_park_company(home_dir.path(), 2, Some("sales"), true).await;

    for id in &c.approvals {
        let response = c.app.clone().oneshot(approve_detached(id)).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the verdict is durable regardless of what the turn then does"
        );
    }
    settle(&c.runtime, 1).await;

    let replies = agent_replies(&c.runtime).await;
    assert_eq!(
        replies.len(),
        1,
        "the operator is told exactly once that the work did not resume, got {replies:?}"
    );
    assert!(
        replies[0].starts_with("sales|"),
        "and told in the thread they approved in, got {replies:?}"
    );
    assert!(
        replies[0].contains("approving again is safe"),
        "the notice has to say what to do about it, got {replies:?}"
    );
    // Issue #966, asserted on the journaled row rather than on the
    // constructor: this drives the real approve path, so it pins that
    // `announce_continuation_failure` *calls* the named notice. Asserting
    // the constructor alone leaves the call site free to go back to an
    // inline `AgentReply` authored by the operator channel — a correct
    // system row byte-identical to one the pre-#885 defect damaged.
    let authors = agent_reply_authors(&c.runtime).await;
    assert_eq!(
        authors,
        vec![crate::ports::SYSTEM_AUTHOR.to_string()],
        "the runtime authored this notice, so it must not be stored under its destination"
    );
}

/// Codex review finding: a stream that errors mid-read used to fall
/// straight through to extraction on whatever partial bytes it had
/// collected. This pins the fix directly against a synthetic stream,
/// without needing a real workspace store behind it — a chunk, then an
/// error, must discard everything read so far rather than handing back
/// a truncated payload that looks complete.
#[tokio::test]
async fn drain_bounded_discards_everything_on_a_mid_stream_error() {
    use bytes::Bytes;
    use futures::stream;

    let items: Vec<crate::error::Result<Bytes>> = vec![
        Ok(Bytes::from_static(b"the first chunk read fine")),
        Err(crate::error::OpenCompanyError::Store(
            "transient read failure".to_string(),
        )),
    ];
    let synthetic: crate::ports::workspace::BlobStream = Box::pin(stream::iter(items));

    assert_eq!(drain_bounded(synthetic, 1_000_000).await, None);
}

/// The success twin: a stream with no error drains to its bytes, in
/// order, across however many chunks it arrives in.
#[tokio::test]
async fn drain_bounded_concatenates_every_chunk_when_the_stream_never_errors() {
    use bytes::Bytes;
    use futures::stream;

    let items: Vec<crate::error::Result<Bytes>> = vec![
        Ok(Bytes::from_static(b"hello ")),
        Ok(Bytes::from_static(b"world")),
    ];
    let synthetic: crate::ports::workspace::BlobStream = Box::pin(stream::iter(items));

    assert_eq!(
        drain_bounded(synthetic, 1_000_000).await,
        Some(b"hello world".to_vec())
    );
}

/// A stream that never errors but exceeds the cap is also discarded, not
/// truncated — the belt-and-braces the doc comment describes.
#[tokio::test]
async fn drain_bounded_discards_when_the_stream_exceeds_the_cap() {
    use bytes::Bytes;
    use futures::stream;

    let items: Vec<crate::error::Result<Bytes>> =
        vec![Ok(Bytes::from_static(b"way more than the cap allows"))];
    let synthetic: crate::ports::workspace::BlobStream = Box::pin(stream::iter(items));

    assert_eq!(drain_bounded(synthetic, 4).await, None);
}

/// **The Codex P1 finding:** `journal_chat_replies` resolved an agent
/// reply's mentions and stored them on `CompanyEvent::AgentReply`, but never
/// called `notify_mentions` — so an `@user` an agent typed *back* rendered
/// as a chip and left the named person with no durable notification and no
/// rail badge, unlike the operator's own message a few lines above it in
/// the very same function. Missing it worst for exactly the person it is
/// meant to reach: offline when the reply lands.
#[tokio::test]
async fn a_mention_in_an_agent_reply_notifies_the_person_it_names() {
    let home_dir = home();
    let state = build_state_with_brain(
        home_dir.path(),
        "running",
        AppConfig::default(),
        Some(Arc::new(MentioningReplyBrain)),
    )
    .await;
    // A second person for `@everyone` to reach — the sender is always
    // excluded from their own broadcast, so proving this needs somebody
    // else on the roster.
    crate::server::test_support::seed_fixed_member(&state, "acme").await;
    let app = router(state.clone());
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).expect("company registered");

    let member_id = runtime
        .users()
        .list_users(&id)
        .await
        .unwrap()
        .into_iter()
        .find(|u| u.email == "harness-member@example.test")
        .expect("seeded member")
        .id;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/chat")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"status?"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let notified = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let notes = runtime.notifications().list(&id, &member_id).await.unwrap();
            if !notes.is_empty() {
                return notes;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the mentioned member was notified");

    assert_eq!(notified.len(), 1);
    assert_eq!(
        notified[0].notification.kind, "mention",
        "the reply's @everyone mention has to file the same kind of row an \
         operator message's does"
    );
}

/// **The Codex P1 finding:** the context a DM mention stores was decided by
/// the human user directory, but a DM's thread id is a roster teammate's
/// agent id — which no user record has — so a mention in a normal DM stored
/// the bare id. The console's rail keys a DM by `dm:<teammate-id>` (and the
/// console sends that bare id as the `chat` for a DM), so no rail row
/// displayed the badge and opening the DM could neither match nor clear it.
#[tokio::test]
async fn a_mention_in_a_dm_stores_the_console_dm_channel_id() {
    let home_dir = home();
    let state = state_with_roster(home_dir.path()).await;
    // A second person for the broadcast to reach — the author is always
    // excluded from their own `@everyone`.
    crate::server::test_support::seed_fixed_member(&state, "acme").await;
    let app = router(state.clone());
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).expect("company registered");

    let member_id = runtime
        .users()
        .list_users(&id)
        .await
        .unwrap()
        .into_iter()
        .find(|u| u.email == "harness-member@example.test")
        .expect("seeded member")
        .id;

    // A message addressed to the `designer` DM thread — the bare roster
    // teammate id, exactly what the console sends for a DM.
    let response = app
        .clone()
        .oneshot(chat_to("cc @everyone on this", Some("designer")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let notified = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let notes = runtime.notifications().list(&id, &member_id).await.unwrap();
            if !notes.is_empty() {
                return notes;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the mentioned member was notified");

    // The offline echo brain answers the same text, so `@everyone` may land
    // twice — once for the operator's message, once for the echoed reply.
    // The count is incidental; the invariant is that *every* mention filed
    // out of this exchange is keyed to the console's `dm:designer` channel,
    // not the bare roster thread id.
    assert!(!notified.is_empty(), "the mentioned member was notified");
    let contexts: Vec<_> = notified
        .iter()
        .map(|n| n.notification.context.as_deref())
        .collect();
    assert!(
        contexts.iter().all(|c| *c == Some("dm:designer")),
        "every mention in a DM has to store the console's DM channel id, \
         not the bare roster thread id — got {contexts:?}"
    );
}
