use super::*;
#[cfg(feature = "openhuman")]
use crate::ports::tasks::TaskTitle;
use crate::server::router;
use axum::body::Body;
#[cfg(feature = "openhuman")]
use axum::body::to_bytes;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::operator_test_support_1::*;
use super::operator_test_support_2::*;
use super::operator_test_support_3::*;

/// A thread reply intercepted as review feedback re-dispatches its card
/// instead of answering with `responses` here. Codex #3903907771:
/// `ChatView.send` reads an empty `responses` as "the turn produced
/// nothing" and renders a synthetic "(no reply)" bubble underneath the
/// operator's own feedback, even though the card was re-dispatched and
/// will answer through its later relay. `reviewFeedbackApplied` is what
/// tells the console this empty `responses` is expected.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn thread_reply_review_feedback_marks_the_response_not_empty_handed() {
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

    runtime
        .events()
        .append(
            runtime.id(),
            crate::ports::types::CompanyEvent::DeskTaskCompleted {
                task_id: "t-1".to_string(),
                desk: "ceo".to_string(),
                output: "done".to_string(),
                column: crate::ports::tasks::COLUMN_IN_REVIEW.to_string(),
                artifact_ids: Vec::new(),
                origin_chat_id: Some("strategy".to_string()),
                origin_parent: None,
            },
        )
        .await
        .unwrap();
    let relay_seq = runtime
        .events()
        .append(
            runtime.id(),
            crate::ports::types::CompanyEvent::AgentReply {
                audience: Vec::new(),
                chat_id: "strategy".to_string(),
                agent_id: "ceo".to_string(),
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
        .await
        .unwrap();

    let message = ChatMessage {
        text: "needs another pass".to_string(),
        chat: Some("strategy".to_string()),
        parent: Some(relay_seq.value().to_string()),
        deliverable: None,
        detach: false,
        mentions: None,
        attachments: Vec::new(),
    };

    let outcome = chat_and_emit(&state, &id, runtime.clone(), message, None)
        .await
        .expect("review feedback applies");
    let ChatOk::Settled(body) = outcome else {
        panic!("a synchronous review-feedback intercept must not detach");
    };
    assert!(body.responses.is_empty());
    assert_eq!(
        body.review_feedback_applied,
        Some(true),
        "an empty `responses` here must be marked expected, not read as \
         a silent turn"
    );

    let after = runtime
        .tasks()
        .list(runtime.id())
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.id == "t-1")
        .unwrap();
    assert_eq!(
        after.column,
        crate::ports::tasks::COLUMN_IN_PROGRESS,
        "the reply still re-dispatches the card"
    );
}

/// An unrecognized `decision` string rejects with `InvalidRequest` (400)
/// rather than falling through to either verdict.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn review_card_rejects_an_unknown_decision() {
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
            task_id: "t-1".to_string(),
            decision: "yeet".to_string(),
            note: None,
        }),
    )
    .await
    .expect_err("an unknown decision string must not settle the card");
    assert_eq!(
        axum::response::IntoResponse::into_response(err).status(),
        StatusCode::BAD_REQUEST
    );
}

/// `POST {scope}/chat/review` end to end through the real router: proves
/// the route is actually mounted by [`with_review_routes`] (not just that
/// the handler function works when called directly) and that the wire
/// body deserializes and settles the card via HTTP.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn chat_review_route_is_mounted_and_settles_via_http() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home, "running").await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();

    runtime
        .tasks()
        .upsert(
            runtime.id(),
            &crate::ports::tasks::TaskRecord {
                id: "t-1".to_string(),
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

    let app = router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/chat/review")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "chatId": "strategy",
                        "taskId": "t-1",
                        "decision": "approve",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["taskId"], "t-1");
    assert_eq!(value["column"], "done");
}

/// No card is `in_review` on the desk at all — as opposed to a `taskId`
/// naming the wrong card, covered above — must also 404, through the same
/// HTTP path the console calls.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn chat_review_route_404s_when_no_card_is_in_review() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home, "running").await;

    let app = router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/chat/review")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "chatId": "strategy",
                        "taskId": "t-1",
                        "decision": "approve",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Two review verdicts racing the same `in_review` card (PR #1981 review
/// finding, Codex P1) must not both resolve it before either applies —
/// same `task_writes`-serialized load-modify-save shape
/// `add_desk_member_serializes_against_the_company_write_lock` proves
/// above, applied to `review_card`.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn review_card_serializes_against_the_task_writes_lock() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();

    runtime
        .tasks()
        .upsert(runtime.id(), &card_in_review("t-1", "strategy"))
        .await
        .unwrap();

    let guard = runtime.task_writes.lock().await;

    let runtime_for_task = runtime.clone();
    let mut task = tokio::spawn(async move {
        let scope = ScopedCompany {
            runtime: runtime_for_task,
            actor: None,
            may_read_contents: true,
            is_admin: true,
        };
        review_card(
            scope,
            Json(ChatReviewRequest {
                chat_id: "strategy".to_string(),
                task_id: "t-1".to_string(),
                decision: "approve".to_string(),
                note: None,
            }),
        )
        .await
    });

    let raced_ahead = tokio::time::timeout(Duration::from_millis(200), &mut task)
        .await
        .is_ok();
    assert!(
        !raced_ahead,
        "review_card resolved and applied a verdict while task_writes was \
         held elsewhere — it is not serializing against concurrent board \
         writers"
    );

    drop(guard);
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("review_card never resumed after task_writes was released")
        .expect("review_card task panicked");
    assert!(result.is_ok());
}

/// The revalidation half of the same finding: a review reply parked on
/// `task_writes` while a second verdict already settled the card must see
/// the now-current column once it resumes, not the stale `in_review`
/// snapshot it would have clone from before it blocked — so it 404s
/// instead of silently re-applying on top of the settled card.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn review_card_404s_when_the_card_left_review_while_the_reply_was_in_flight() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();

    runtime
        .tasks()
        .upsert(runtime.id(), &card_in_review("t-1", "strategy"))
        .await
        .unwrap();

    let guard = runtime.task_writes.lock().await;

    let runtime_for_task = runtime.clone();
    let mut task = tokio::spawn(async move {
        let scope = ScopedCompany {
            runtime: runtime_for_task,
            actor: None,
            may_read_contents: true,
            is_admin: true,
        };
        review_card(
            scope,
            Json(ChatReviewRequest {
                chat_id: "strategy".to_string(),
                task_id: "t-1".to_string(),
                decision: "approve".to_string(),
                note: None,
            }),
        )
        .await
    });
    let _ = tokio::time::timeout(Duration::from_millis(200), &mut task).await;

    let card = runtime
        .review_card_in_review("t-1", "strategy")
        .await
        .expect("task store lookup")
        .expect("card is still in_review before the lock is released");
    runtime
        .apply_review_decision(
            &card,
            crate::harness::built_in::lifecycle::ReviewDecision::Revise,
            Some("send it back"),
            None,
        )
        .await
        .unwrap();

    drop(guard);
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("review_card never resumed after task_writes was released")
        .expect("review_card task panicked");
    assert!(
        result.is_err(),
        "a review reply that had already resolved the card must not \
         silently re-apply its verdict once the card is no longer \
         in_review"
    );

    let after = runtime
        .tasks()
        .list(runtime.id())
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.id == "t-1")
        .unwrap();
    assert_eq!(after.column, crate::ports::tasks::COLUMN_IN_PROGRESS);
    let note = after.note.expect("note");
    assert!(note.contains("send it back"), "{note}");
}

/// The sharpest case in this file. `may_read_approval_contents` already
/// refuses a member the payload and the amount an approval carries, so
/// before this guard a member could approve a payment they were forbidden
/// to look at.
///
/// The approval id is deliberately one that does not exist: authority is
/// settled before the approval is resolved, so the answer must be `403` and
/// not the `404` a permitted caller would get.
#[tokio::test]
async fn a_member_may_not_resolve_an_approval() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    crate::server::test_support::seed_fixed_member(&state, "acme").await;
    let cookie = crate::server::test_support::member_cookie("acme");
    let app = router(state);

    for scope in APPROVAL_SCOPES {
        let denied = app
            .clone()
            .oneshot(resolve_as(scope, "appr-nobody-parked", Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(
            denied.status(),
            StatusCode::FORBIDDEN,
            "{scope} let a member decide an approval"
        );
    }
}

/// Extending is the deadline's other side: an approval nobody decides
/// default-denies when its window runs out, so being able to push that
/// window out indefinitely is a decision about the effect, made for the
/// company. It is held to the same authority as deciding it outright.
#[tokio::test]
async fn a_member_may_not_extend_an_approval_deadline() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    crate::server::test_support::seed_fixed_member(&state, "acme").await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    let id = park_for_extend(&runtime, "appr-member-ext", 1_000).await;
    let cookie = crate::server::test_support::member_cookie("acme");
    let app = router(state);

    for scope in APPROVAL_SCOPES {
        let denied = app
            .clone()
            .oneshot(extend_as(scope, id.as_ref(), Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(
            denied.status(),
            StatusCode::FORBIDDEN,
            "{scope} let a member extend an approval deadline"
        );
    }
}

/// The other half of the guard: refusing a member must not also refuse the
/// admin the routes exist for, under either address form.
#[tokio::test]
async fn an_admin_may_still_resolve_an_approval() {
    for scope in APPROVAL_SCOPES {
        let home_dir = home();
        let state = state_with_company(home_dir.path(), "running").await;
        let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
        let id = park_for_extend(&runtime, "appr-admin-resolve", 1_000).await;
        let cookie = crate::server::test_support::fixed_cookie("acme");
        let app = router(state);

        let allowed = app
            .oneshot(resolve_as(scope, id.as_ref(), Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(
            allowed.status(),
            StatusCode::OK,
            "{scope} refused an admin the decision"
        );
    }
}

#[tokio::test]
async fn an_admin_may_still_extend_an_approval_deadline() {
    for scope in APPROVAL_SCOPES {
        let home_dir = home();
        let state = state_with_company(home_dir.path(), "running").await;
        let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
        let id = park_for_extend(&runtime, "appr-admin-ext", 1_000).await;
        let cookie = crate::server::test_support::fixed_cookie("acme");
        let app = router(state);

        let allowed = app
            .oneshot(extend_as(scope, id.as_ref(), Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(
            allowed.status(),
            StatusCode::OK,
            "{scope} refused an admin the extension"
        );
    }
}

/// No credential at all is `401`, not `403` — the authority guard must not
/// turn an anonymous request into a role decision.
#[tokio::test]
async fn an_unauthenticated_caller_cannot_decide_or_extend_an_approval() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let app = router(state);

    for scope in APPROVAL_SCOPES {
        for request in [
            resolve_as(scope, "appr-anon", None),
            extend_as(scope, "appr-anon", None),
        ] {
            let uri = request.uri().to_string();
            let denied = app.clone().oneshot(request).await.unwrap();
            assert_eq!(
                denied.status(),
                StatusCode::UNAUTHORIZED,
                "{uri} answered an anonymous caller with {}",
                denied.status()
            );
        }
    }
}

/// The second defect these routes carried: the temporary-password boundary
/// lived only on the single-company alias, so an admin who had never set a
/// password could decide and extend every approval through the `{id}` form.
///
/// An admin is the right principal to prove it with — the role check passes,
/// so a refusal here can only be the password boundary.
#[tokio::test]
async fn an_admin_on_a_temporary_password_may_not_decide_or_extend() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let cookie = crate::server::test_support::seed_temp_password_admin(&state, "acme").await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    let id = park_for_extend(&runtime, "appr-temp-pass", 1_000).await;
    let app = router(state);

    for scope in APPROVAL_SCOPES {
        for request in [
            resolve_as(scope, id.as_ref(), Some(&cookie)),
            extend_as(scope, id.as_ref(), Some(&cookie)),
        ] {
            let uri = request.uri().to_string();
            let denied = app.clone().oneshot(request).await.unwrap();
            assert_eq!(
                denied.status(),
                StatusCode::FORBIDDEN,
                "{uri} served an admin who has not set a password"
            );
            assert_eq!(
                body_json(denied).await["code"],
                "password_change_required",
                "{uri} refused for the wrong reason"
            );
        }
    }
}

// -- Deciding an approval: which states admit it, and what a failure costs --

/// STATE. `run_resolve` asks `ensure_running` before it touches the gate,
/// and the ordering is the guarantee: a company that has stopped accepting
/// work must refuse the decision *and leave the approval parked*, so the
/// operator still has a card to decide once it is running again.
///
/// A refusal that consumed the park would be worse than no refusal at all —
/// the effect would be neither approved nor decidable.
#[tokio::test]
async fn resolving_on_a_paused_company_is_refused_and_leaves_the_approval_parked() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    let approval = park_for_extend(&runtime, "appr-paused", crate::ports::now_millis()).await;
    let cookie = crate::server::test_support::fixed_cookie("acme");
    let app = router(state);

    let lifecycle = |verb: &str| {
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/companies/acme/{verb}"))
            .header("cookie", crate::server::test_support::fixed_cookie("acme"))
            .body(Body::empty())
            .unwrap()
    };

    let paused = app.clone().oneshot(lifecycle("pause")).await.unwrap();
    assert_eq!(paused.status(), StatusCode::OK, "the company is now paused");

    for verdict in ["approve", "deny"] {
        let refused = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/company/approvals/{approval}"))
                    .header("cookie", &cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "verdict": verdict }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            refused.status(),
            StatusCode::CONFLICT,
            "a paused company answered a {verdict} instead of refusing it"
        );
    }

    assert!(
        runtime.pending_approvals().iter().any(|p| p.id == approval),
        "the refusal must leave the approval decidable, not spend it"
    );
    assert_eq!(
        runtime.grants.live_count(),
        0,
        "and it must mint nothing on the way out"
    );

    let resumed = app.clone().oneshot(lifecycle("resume")).await.unwrap();
    assert_eq!(resumed.status(), StatusCode::OK);
    let allowed = app
        .oneshot(resolve_request(
            &approval,
            serde_json::json!({ "verdict": "approve", "detach": true }),
        ))
        .await
        .unwrap();
    assert_eq!(
        allowed.status(),
        StatusCode::OK,
        "the same decision lands once the company is running again"
    );
}
