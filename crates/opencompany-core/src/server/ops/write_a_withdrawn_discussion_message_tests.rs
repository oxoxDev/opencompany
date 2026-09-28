//! Integration tests for the `ops` write plane: tasks, memory, workspace,
//! skills, team, inbox-read, and desk chat — exercised end-to-end over the
//! router against a real fs-backed company.

use axum::http::StatusCode;
use serde_json::json;

use super::write_test_support::*;
use crate::ports::types::CompanyId;

/// #358: a posted message can be withdrawn, and the text stops being served.
///
/// The shape the issue asks for, asserted end to end over the real HTTP stack:
/// the row survives (position, author, time), the text does not, the withdrawal
/// is attributed, the journal keeps both events, and nothing about a message
/// nobody withdrew changes.
#[tokio::test]
async fn a_withdrawn_discussion_message_stops_being_served() {
    use crate::ports::types::CompanyEvent;

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home).await;
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();
    runtime
        .tasks()
        .upsert(&company, &discussion_card("t-1", "Ship it"))
        .await
        .unwrap();

    const SECRET: &str = "sk-live-0000-DO-NOT-KEEP";
    let (status, posted) = send(
        &state,
        "POST",
        "/api/v1/company/tasks/t-1/discussion",
        Some(json!({ "text": format!("blocked on the API key: {SECRET}") })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let seq = posted["seq"].as_u64().expect("the post carries its seq");

    let (status, _) = send(
        &state,
        "POST",
        "/api/v1/company/tasks/t-1/discussion",
        Some(json!({ "text": "rotated it, we are unblocked" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, withdrawn) = send(
        &state,
        "DELETE",
        &format!("/api/v1/company/tasks/t-1/discussion/{seq}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(withdrawn["redacted"], true);
    assert_eq!(
        withdrawn["redactedBy"], "Harness Admin",
        "a withdrawal nobody's name is on is a message that can vanish quietly"
    );
    assert_eq!(
        withdrawn["seq"], seq,
        "the row keeps its place in the thread"
    );

    // The reload: what every reader of this card now gets.
    let (status, body) = send(&state, "GET", "/api/v1/company/tasks/t-1", None).await;
    assert_eq!(status, StatusCode::OK);
    let thread = body["discussion"].as_array().expect("discussion array");
    assert_eq!(thread.len(), 2, "the row is withdrawn, not deleted: {body}");
    assert_eq!(thread[0]["seq"], seq);
    assert_eq!(thread[0]["redacted"], true);
    assert_eq!(thread[0]["redactedBy"], "Harness Admin");
    assert_eq!(
        thread[0]["author"], "Harness Admin",
        "the poster is still named"
    );
    assert_eq!(
        thread[1]["text"], "rotated it, we are unblocked",
        "withdrawing one message must not touch another"
    );
    assert!(
        thread[1].get("redacted").is_none(),
        "an ordinary row must keep the shape a pre-#358 console renders: {thread:?}"
    );
    assert!(
        !serde_json::to_string(&body).unwrap().contains(SECRET),
        "the withdrawn text is still being served on the detail read: {body}"
    );

    // The journal keeps both events: the post's existence is a fact, and the
    // withdrawal is a second fact about it.
    let events = runtime
        .events()
        .read_from(&company, crate::ports::types::EventSeq::new(0), usize::MAX)
        .await
        .unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.event,
            CompanyEvent::TaskDiscussionRedacted { task_id, seq: s, .. }
                if task_id == "t-1" && *s == seq
        )),
        "the withdrawal was not journaled"
    );

    // Idempotent: asking twice is not an error, and does not grow the journal.
    let before = events.len();
    let (status, again) = send(
        &state,
        "DELETE",
        &format!("/api/v1/company/tasks/t-1/discussion/{seq}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["redacted"], true);
    let after = runtime
        .events()
        .read_from(&company, crate::ports::types::EventSeq::new(0), usize::MAX)
        .await
        .unwrap()
        .len();
    assert_eq!(
        before, after,
        "a repeated withdrawal appended a second tombstone"
    );
}

/// #358: a `seq` that is not a discussion post on *this* card is a `404`, not a
/// tombstone written into the journal against something else.
#[tokio::test]
async fn withdrawing_something_that_is_not_this_cards_post_is_refused() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home).await;
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();
    for card in [
        discussion_card("t-1", "Ship it"),
        discussion_card("t-other", "Unrelated"),
    ] {
        runtime.tasks().upsert(&company, &card).await.unwrap();
    }

    let (_, posted) = send(
        &state,
        "POST",
        "/api/v1/company/tasks/t-other/discussion",
        Some(json!({ "text": "another card's message" })),
    )
    .await;
    let seq = posted["seq"].as_u64().unwrap();

    // Another card's post, addressed through this card.
    let (status, _) = send(
        &state,
        "DELETE",
        &format!("/api/v1/company/tasks/t-1/discussion/{seq}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A sequence position that holds no event at all.
    let (status, _) = send(
        &state,
        "DELETE",
        "/api/v1/company/tasks/t-1/discussion/99999",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The other card's thread is untouched by either attempt.
    let (_, other) = send(&state, "GET", "/api/v1/company/tasks/t-other", None).await;
    let thread = other["discussion"].as_array().unwrap();
    assert_eq!(thread.len(), 1);
    assert_eq!(thread[0]["text"], "another card's message");
    assert!(thread[0].get("redacted").is_none());
}

/// #358 + #335's paging: a withdrawal is applied even when the tombstone sits
/// *newer* than the cursor the caller is paging back through.
///
/// The trap this pins: the discussion arm skips events at or after
/// `discussionBefore`, and a tombstone is always newer than the post it
/// withdraws. Applying the cursor to tombstones too would serve the original
/// text to anybody who scrolled far enough back — the one reader most likely to
/// be looking for it.
#[tokio::test]
async fn a_withdrawal_survives_paging_back_past_it() {
    use crate::ports::types::CompanyEvent;

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home).await;
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();
    runtime
        .tasks()
        .upsert(&company, &discussion_card("t-1", "Ship it"))
        .await
        .unwrap();

    const SECRET: &str = "sk-live-PAGED-BACK";
    let (_, posted) = send(
        &state,
        "POST",
        "/api/v1/company/tasks/t-1/discussion",
        Some(json!({ "text": SECRET })),
    )
    .await;
    let seq = posted["seq"].as_u64().unwrap();

    // Enough newer posts that the first one falls off the first page.
    for n in 0..60 {
        runtime
            .events()
            .append(
                &company,
                CompanyEvent::TaskDiscussionPosted {
                    task_id: "t-1".into(),
                    text: format!("message {n}"),
                    by: None,
                },
            )
            .await
            .unwrap();
    }

    let (status, _) = send(
        &state,
        "DELETE",
        &format!("/api/v1/company/tasks/t-1/discussion/{seq}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Page back to the start of the thread, past the tombstone's own position.
    let (_, first) = send(&state, "GET", "/api/v1/company/tasks/t-1", None).await;
    let oldest_on_page = first["discussion"].as_array().unwrap()[0]["seq"]
        .as_u64()
        .unwrap();
    let (status, older) = send(
        &state,
        "GET",
        &format!("/api/v1/company/tasks/t-1?discussionBefore={oldest_on_page}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !serde_json::to_string(&older).unwrap().contains(SECRET),
        "paging back served the withdrawn text: {older}"
    );
    let row = older["discussion"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["seq"] == seq)
        .expect("the withdrawn row is on the older page");
    assert_eq!(row["redacted"], true);
}

/// #335: the per-task Discussion tab's whole contract — a post persists, reads
/// back on the card's own detail, and belongs to exactly one card.
///
/// The acceptance criterion is "posts survive a reload and are visible from
/// another browser", which is the same thing as: the message lives in the
/// company journal, not in the posting session. So the assertions are made
/// through a *second, independent request* rather than off the POST's echo.
///
/// The scoping half matters as much: the journal is company-scoped, so a fold
/// that forgot to compare `task_id` would show every card the same thread.
#[tokio::test]
async fn task_discussion_posts_persist_and_are_scoped_to_their_card() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home).await;
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();

    for card in [
        discussion_card("t-1", "Ship it"),
        discussion_card("t-other", "Unrelated"),
        discussion_card("t-quiet", "Nobody has said anything"),
    ] {
        runtime.tasks().upsert(&company, &card).await.unwrap();
    }

    // Surrounding whitespace is trimmed, and the poster is named from the
    // roster — never by user id, and never by email address.
    let (status, posted) = send(
        &state,
        "POST",
        "/api/v1/company/tasks/t-1/discussion",
        Some(json!({ "text": "  blocked on the API key  " })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(posted["text"], "blocked on the API key");
    assert_eq!(posted["author"], "Harness Admin");

    for (task, text) in [
        ("t-1", "unblocked, the key was rotated"),
        ("t-other", "someone else's thread"),
    ] {
        let (status, _) = send(
            &state,
            "POST",
            &format!("/api/v1/company/tasks/{task}/discussion"),
            Some(json!({ "text": text })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    // The reload: a fresh read of the card, which reaches the journal rather
    // than anything the posting request kept.
    let (status, body) = send(&state, "GET", "/api/v1/company/tasks/t-1", None).await;
    assert_eq!(status, StatusCode::OK);
    let thread = body["discussion"].as_array().expect("discussion array");
    let texts: Vec<&str> = thread.iter().map(|m| m["text"].as_str().unwrap()).collect();
    assert_eq!(
        texts,
        vec!["blocked on the API key", "unblocked, the key was rotated"],
        "the thread reads back oldest-first"
    );
    assert!(
        thread[0]["seq"].as_u64().unwrap() < thread[1]["seq"].as_u64().unwrap(),
        "seq is the thread's strict order: {thread:?}"
    );
    assert!(
        !serde_json::to_string(&body["discussion"])
            .unwrap()
            .contains("someone else's thread"),
        "another card's message leaked onto this thread"
    );

    // The two projections stay apart: a discussion post is not a run event, so
    // it must not appear on the timeline the Timeline tab renders.
    let off_card: Vec<&str> = body["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .filter(|kind| *kind != "card")
        .collect();
    assert!(
        off_card.is_empty(),
        "a discussion post must not land on the run timeline: {body}"
    );

    // The other card sees only its own message, and a card nobody has posted on
    // reads back an empty thread — what keeps the tab's empty state honest.
    let (_, other) = send(&state, "GET", "/api/v1/company/tasks/t-other", None).await;
    let other_thread = other["discussion"].as_array().unwrap();
    assert_eq!(other_thread.len(), 1);
    assert_eq!(other_thread[0]["text"], "someone else's thread");

    let (_, quiet) = send(&state, "GET", "/api/v1/company/tasks/t-quiet", None).await;
    assert_eq!(quiet["discussion"].as_array().unwrap().len(), 0);

    // Both scope forms serve the same thread.
    let (status, scoped) = send(&state, "GET", "/api/v1/companies/acme/tasks/t-1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(scoped["discussion"].as_array().unwrap().len(), 2);
}

/// #335: what the write boundary refuses, and what it forgives.
///
/// An empty message is refused because there is no delete in v1 — a blank row
/// would be permanent noise. An unknown card is refused because the post would
/// otherwise be journaled somewhere no read surface can reach. An over-long
/// message is *not* refused: it is truncated, so a long paste still posts.
#[tokio::test]
async fn task_discussion_rejects_an_empty_message_and_an_unknown_card() {
    use crate::ports::tasks::MAX_DISCUSSION_CHARS;

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home).await;
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();
    runtime
        .tasks()
        .upsert(&company, &discussion_card("t-1", "Ship it"))
        .await
        .unwrap();

    for text in ["", "   \n\t "] {
        let (status, _) = send(
            &state,
            "POST",
            "/api/v1/company/tasks/t-1/discussion",
            Some(json!({ "text": text })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "empty text: {text:?}");
    }

    let (status, _) = send(
        &state,
        "POST",
        "/api/v1/company/tasks/nope/discussion",
        Some(json!({ "text": "into the void" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A long paste posts, capped on a character boundary.
    let long = "é".repeat(MAX_DISCUSSION_CHARS + 500);
    let (status, posted) = send(
        &state,
        "POST",
        "/api/v1/company/tasks/t-1/discussion",
        Some(json!({ "text": long })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        posted["text"].as_str().unwrap().chars().count(),
        MAX_DISCUSSION_CHARS
    );

    // Only the accepted post is on the thread: the three refusals journaled
    // nothing.
    let (_, body) = send(&state, "GET", "/api/v1/company/tasks/t-1", None).await;
    assert_eq!(body["discussion"].as_array().unwrap().len(), 1);
}
