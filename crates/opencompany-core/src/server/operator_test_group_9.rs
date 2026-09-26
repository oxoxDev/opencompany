use super::*;
use crate::server::router;
use axum::body::Body;
#[cfg(not(feature = "openhuman"))]
use axum::body::to_bytes;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::operator_test_support_1::*;
use super::operator_test_support_2::*;

#[cfg(feature = "openhuman")]
#[tokio::test]
async fn a_blocker_verdict_with_an_amended_payload_is_refused() {
    assert_refused(
        serde_json::json!({
            "verdict": "approve",
            "blocker_verdict": "skip",
            "amended_payload": { "text": "edited" },
        }),
        "cannot accompany amended_payload",
    )
    .await;
}

#[cfg(feature = "openhuman")]
#[tokio::test]
async fn a_blocker_verdict_with_a_tool_scope_is_refused() {
    assert_refused(
        serde_json::json!({
            "verdict": "approve",
            "blocker_verdict": "skip",
            "scope": "tool",
            "expires_in_millis": 3_600_000,
        }),
        "cannot accompany scope",
    )
    .await;
}

/// A `blocker_verdict` on an approval that is not a parked blocker is a 400,
/// not a quiet fall-through to the two-value path — which would lose the
/// operator's verdict without telling anyone.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn a_blocker_verdict_on_an_ordinary_approval_is_refused() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home, "running").await;
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();
    let app = router(state);
    let ordinary = park_for_extend(&runtime, "ordinary-1", crate::ports::now_millis()).await;

    let (status, answer) = post_resolve(
        &app,
        &ordinary,
        serde_json::json!({ "verdict": "approve", "blocker_verdict": "skip" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert!(
        answer["error"]
            .as_str()
            .unwrap_or_default()
            .contains("is not a parked blocker"),
        "{answer}"
    );
    assert!(
        runtime.pending_approvals().iter().any(|p| p.id == ordinary),
        "a refused request must leave the approval parked"
    );
    assert!(banked_resolutions(&home, &company).await.is_empty());
}

/// A stepless blocker uses its task link to settle the card it paused.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn skipping_an_agent_question_settles_the_card_its_approval_is_linked_to() {
    use crate::ports::blockers::{BlockerKind, BlockerPayload, BlockerSource};
    use crate::runtime::journal::{ApprovalConversation, TaskLink};

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home, "running").await;
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();
    let app = router(state);

    runtime
        .tasks()
        .upsert(
            runtime.id(),
            &crate::ports::tasks::TaskRecord {
                id: "t-9".to_string(),
                title: crate::ports::tasks::TaskTitle::authored("Draft the launch note"),
                note: None,
                column: crate::ports::tasks::COLUMN_PAUSED.to_string(),
                priority: "medium".to_string(),
                assignee: "eng".to_string(),
                updated_at_millis: 1,
                origin: None,
                origin_message_seq: None,
                parent_task_id: None,
                output: Some(crate::ports::tasks::TaskOutput {
                    source: crate::ports::tasks::TaskOutputSource::Run {
                        run_id: "old-run".to_string(),
                        attempt: Some(1),
                    },
                    at_millis: 1,
                    artifacts: Vec::new(),
                    workflows: Vec::new(),
                }),
                plan: None,
                planning_attempts: Vec::new(),
                deliverable: crate::ports::tasks::TaskDeliverable::Once,
                workflow_proposal: None,
                origin_run_id: None,
                origin_workflow_id: None,
                bounced: Some("stale failure".to_string()),
            },
        )
        .await
        .unwrap();

    let payload = BlockerPayload {
        kind: BlockerKind::Information,
        source: BlockerSource::AgentQuestion,
        step: None,
        reason: "which of the two briefs is current?".to_string(),
        needed: "an answer from you".to_string(),
        group_key: None,
    };
    let approval = ApprovalId::new("question-1");
    let effect = crate::ports::types::Effect {
        kind: payload.effect_kind(),
        group: crate::ports::types::EffectGroup::Other,
        amount_usd: None,
        established_thread: false,
        first_time_counterparty: false,
        payload: serde_json::to_value(&payload).unwrap(),
        agent: None,
        run_id: None,
    };
    let at = crate::ports::now_millis();
    runtime
        .approval_gate
        .rehydrate(approval.clone(), effect.clone(), at);
    runtime
        .journal
        .record_parked(
            &approval,
            &effect,
            at,
            TaskLink::from_task_id(Some("t-9")),
            ApprovalConversation {
                thread: Some("dm:eng".to_string()),
                parent: None,
            },
            None,
        )
        .await
        .unwrap();

    let (status, answer) = post_resolve(
        &app,
        &approval,
        serde_json::json!({ "verdict": "approve", "blocker_verdict": "skip" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(
        answer["settledIds"],
        serde_json::json!(["question-1"]),
        "the non-detached body names what it settled too: {answer}"
    );

    let banked = banked_resolutions(&home, &company).await;
    assert_eq!(banked.len(), 1);
    assert_eq!(
        banked[0]["resolution"]["verdict"], "skip",
        "the operator's verdict is banked whatever the resume can do with it"
    );
    assert!(
        banked[0]["resolution"].get("step").is_none(),
        "the durable record keeps the stepless park the blocker carried: {}",
        banked[0]
    );
    let card = runtime
        .tasks()
        .list(runtime.id())
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.id == "t-9")
        .expect("the card still exists");
    assert_eq!(
        card.column,
        crate::ports::tasks::COLUMN_IN_REVIEW,
        "the skipped card is ready for human review"
    );
    assert!(card.output.is_none(), "a skip produces no output");
    assert!(card.bounced.is_none(), "a skip clears the old bounce chip");
    assert_eq!(card.origin_chat_id(), Some("dm:eng"));
    assert!(
        card.note
            .as_deref()
            .is_some_and(|note| { note.contains("blocker question waived by the operator") })
    );
    assert!(
        runtime
            .runs()
            .list_runs(
                runtime.id(),
                &crate::ports::runs::RunFilter::for_task("t-9"),
            )
            .await
            .unwrap()
            .is_empty(),
        "a skip must not open another attempt"
    );
}

/// An agent's own question names its asker on the card even though the
/// parked effect behind it carries no `agent` — the grant/re-dispatch
/// machinery a blocked tool call's card relies on stays untouched by this
/// path (see `blockers::asked_by`).
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn an_agent_question_names_its_asker_on_the_card() {
    use crate::ports::blockers::{BlockerKind, BlockerPayload, BlockerSource};
    use crate::runtime::journal::{ApprovalConversation, TaskLink};

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home, "running").await;
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();

    let payload = BlockerPayload {
        kind: BlockerKind::Information,
        source: BlockerSource::AgentQuestion,
        step: None,
        reason: "which of the two briefs is current?".to_string(),
        needed: "an answer from you".to_string(),
        group_key: None,
    };
    let mut payload_json = serde_json::to_value(&payload).expect("payload serializes");
    payload_json
        .as_object_mut()
        .expect("payload is an object")
        .insert("asked_by".to_string(), serde_json::json!("eng"));
    let approval = ApprovalId::new("question-asker");
    let effect = crate::ports::types::Effect {
        kind: payload.effect_kind(),
        group: crate::ports::types::EffectGroup::Other,
        amount_usd: None,
        established_thread: false,
        first_time_counterparty: false,
        payload: payload_json,
        agent: None,
        run_id: None,
    };
    let at = crate::ports::now_millis();
    runtime
        .approval_gate
        .rehydrate(approval.clone(), effect.clone(), at);
    runtime
        .journal
        .record_parked(
            &approval,
            &effect,
            at,
            TaskLink::from_task_id(None),
            ApprovalConversation {
                thread: Some("dm:eng".to_string()),
                parent: None,
            },
            None,
        )
        .await
        .unwrap();

    let pending = runtime.pending_approvals();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].agent.as_deref(),
        Some("eng"),
        "the console renders \"Asked by\" from this field"
    );
}

/// The link is followed only to a card the board still holds.
///
/// A stepless question's approval carries a task link because a card was in
/// hand when it was asked, not because the card is the thing to re-enter.
/// When that card is gone — deleted, or never on this board — reading the
/// link as a card resume answers the operator with *that card is no longer
/// on the board*, which is a report about a card in place of the answer to
/// the question they just gave. The answer goes back into the conversation
/// instead, exactly as it does for a question that was never linked.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn an_agent_question_linked_to_a_card_the_board_lost_still_answers_the_question() {
    use crate::ports::blockers::{BlockerKind, BlockerPayload, BlockerSource};
    use crate::runtime::journal::{ApprovalConversation, TaskLink};

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home, "running").await;
    let company = CompanyId::new("acme");
    let runtime = state.registry().get(&company).unwrap();
    let app = router(state);

    let payload = BlockerPayload {
        kind: BlockerKind::Information,
        source: BlockerSource::AgentQuestion,
        step: None,
        reason: "which of the two briefs is current?".to_string(),
        needed: "an answer from you".to_string(),
        group_key: None,
    };
    let approval = ApprovalId::new("question-2");
    let effect = crate::ports::types::Effect {
        kind: payload.effect_kind(),
        group: crate::ports::types::EffectGroup::Other,
        amount_usd: None,
        established_thread: false,
        first_time_counterparty: false,
        payload: serde_json::to_value(&payload).unwrap(),
        agent: None,
        run_id: None,
    };
    let at = crate::ports::now_millis();
    runtime
        .approval_gate
        .rehydrate(approval.clone(), effect.clone(), at);
    // The link names a card that is not on the board, which is the whole
    // case: nothing is seeded for `t-gone`.
    runtime
        .journal
        .record_parked(
            &approval,
            &effect,
            at,
            TaskLink::from_task_id(Some("t-gone")),
            ApprovalConversation {
                thread: Some("dm:eng".to_string()),
                parent: None,
            },
            None,
        )
        .await
        .unwrap();

    let (status, answer) = post_resolve(
        &app,
        &approval,
        serde_json::json!({ "verdict": "approve", "blocker_verdict": "retry" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");

    let banked = banked_resolutions(&home, &company).await;
    assert_eq!(banked.len(), 1);
    assert_eq!(
        banked[0]["resolution"]["verdict"], "retry",
        "the operator's answer is banked whatever the resume finds: {}",
        banked[0]
    );
    let notes: Vec<String> = runtime
        .events
        .read_from(
            runtime.id(),
            crate::ports::types::EventSeq::new(0),
            usize::MAX,
        )
        .await
        .expect("read events")
        .into_iter()
        .filter_map(|stored| match stored.event {
            crate::ports::types::CompanyEvent::AgentReply { chat_id, text, .. }
                if chat_id == "dm:eng" =>
            {
                Some(text)
            }
            _ => None,
        })
        .collect();
    assert!(
        !notes
            .iter()
            .any(|note| note.contains("no longer on the board")),
        "answering a question must not report on a card the asker never mentioned; \
         posted: {notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|note| note == "Got it — picking that back up now."),
        "the answer must still reach the conversation it was asked in; posted: {notes:?}"
    );
}

/// A build with no blocker resume refuses the field outright. Accepting and
/// ignoring it would answer `200` to a skip that silently became a retry —
/// the exact defect, reintroduced by a feature flag.
#[cfg(not(feature = "openhuman"))]
#[tokio::test]
async fn a_build_without_the_resume_refuses_a_blocker_verdict() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home, "running").await;
    let app = router(state);

    let response = app
        .oneshot(resolve_request(
            &ApprovalId::new("missing"),
            serde_json::json!({ "verdict": "approve", "blocker_verdict": "skip" }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        value["error"]
            .as_str()
            .unwrap_or_default()
            .contains("not supported by this build"),
        "{value}"
    );
}

/// The keystone (issue #1805): extending a parked approval pushes its
/// deadline out to a fresh full window, and the receipt names the new one —
/// the console can redraw the countdown without re-fetching the list.
#[tokio::test]
async fn extending_a_parked_approval_moves_its_deadline() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    // Parked long ago, so its original deadline is `1_000 + ttl`.
    let id = park_for_extend(&runtime, "appr-ext", 1_000).await;
    let before = runtime.pending_approvals()[0]
        .expires_at_millis
        .expect("a deadline is projected");

    let app = router(state);
    let response = app.oneshot(extend_request(&id)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    let after = runtime.pending_approvals()[0]
        .expires_at_millis
        .expect("a deadline is still projected");
    assert!(
        after > before,
        "the deadline moved out: before={before} after={after}"
    );
    assert!(body["extended"].as_bool().unwrap());
    assert_eq!(
        body["expiresAtMillis"].as_f64().unwrap() as u64,
        after,
        "the receipt's deadline is the one the card now projects"
    );
}

/// Extending something that is not parked — an unknown id, or one already
/// resolved or expired — is a 404, not a 200 over nothing.
#[tokio::test]
async fn extending_an_unknown_approval_is_404() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let app = router(state);
    let response = app
        .oneshot(extend_request(&ApprovalId::new("does-not-exist")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// POL-011 (INPUT). `{aid}` is an opaque path segment carried straight
/// into `ApprovalId::new` with no format check of its own — the id space
/// is "whatever a park was given", so the whole of input-safety here is
/// that an adversarial or malformed segment resolves to the same ordinary
/// 404 an unknown id does, never a panic or a 500.
#[tokio::test]
async fn extending_a_malformed_approval_id_is_404_not_a_crash() {
    fn percent_encode_path_segment(raw: &str) -> String {
        let mut out = String::new();
        for byte in raw.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(byte as char);
                }
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    }

    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let app = router(state);

    let hostile_ids = [
        "../../../etc/passwd".to_string(),
        "🎉💥-not-an-approval".to_string(),
        "a".repeat(10_000),
        "'; DROP TABLE approvals; --".to_string(),
        "appr\u{0}-null-byte".to_string(),
    ];
    for raw in hostile_ids {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/v1/company/approvals/{}/extend",
                        percent_encode_path_segment(&raw)
                    ))
                    .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "a malformed id ({raw:?}) must answer the same 404 an unknown id does, not crash"
        );
    }
}

/// APPR-004: extend must be able to win a race the sweep has not yet run —
/// an approval whose deadline has already passed but that is still
/// physically parked (nothing has swept it out of the gate) must still be
/// extendable, and the extension must genuinely move the deadline rather
/// than just answer as if it had.
///
/// `resolve`'s own past-deadline check (`gate.rs`'s TTL math) and
/// `extend`'s (`ParkedApprovals::extend`, existence-only) are two
/// different tests over the same map — that gap is exactly the window
/// `/extend` exists to rescue something in, per issue #1805.
#[tokio::test]
async fn extending_beats_a_pending_sweep_on_an_already_past_deadline_approval() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();

    // Parked at the epoch: this host's TTL has long since passed, and
    // nothing has swept either entry out of the gate yet.
    let control = park_for_extend(&runtime, "appr-control", 1).await;
    let target = park_for_extend(&runtime, "appr-target", 1).await;

    let app = router(state.clone());

    // The control proves the premise: resolving an untouched twin of the
    // same stale park reports `expired`.
    let resolved = app
        .clone()
        .oneshot(resolve_request(
            &control,
            serde_json::json!({ "verdict": "approve", "detach": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resolved.status(), StatusCode::OK);
    let body = body_json(resolved).await;
    assert_eq!(
        body["outcome"], "expired",
        "premise: a park this old is already past this host's TTL, got {body}"
    );

    // Extending the other twin, before anything else touches it, must
    // still succeed — this is the whole reason `/extend` exists.
    let extended = app.clone().oneshot(extend_request(&target)).await.unwrap();
    assert_eq!(
        extended.status(),
        StatusCode::OK,
        "extend must be able to rescue a park the sweep has not yet reclaimed"
    );

    // And now resolving it must NOT report `expired` — the deadline
    // genuinely moved, not just the extend receipt's word for it.
    let resolved = app
        .oneshot(resolve_request(
            &target,
            serde_json::json!({ "verdict": "approve", "detach": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resolved.status(), StatusCode::OK);
    let body = body_json(resolved).await;
    assert_ne!(
        body["outcome"], "expired",
        "extend must genuinely push the deadline out, not just answer as if it did: {body}"
    );
}

/// PLAT-014 (Member ⇒ approve): the sharpest of the auth-matrix's four
/// rows. A Member sees a money-bearing approval exists (issue #468's
/// "waiting on approval" indicator has to survive for them) but not what
/// it is about (issue #618) — and cannot act on it at all: both
/// `POST {scope}/approvals/{aid}` and `/extend` are `AdminScopedCompany`.
/// All three properties are asserted against the same parked approval, so
/// the redaction and the auth gate cannot silently disagree about which
/// one is doing the protecting.
#[tokio::test]
async fn a_member_cannot_read_or_act_on_a_money_bearing_approval() {
    let home_dir = home();
    let state = state_with_company(home_dir.path(), "running").await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    let approval = park_for_extend(&runtime, "appr-member", crate::ports::now_millis()).await;
    crate::server::test_support::seed_fixed_member(&state, "acme").await;
    let member_cookie = crate::server::test_support::member_cookie("acme");
    let app = router(state);

    // Sees it exists, but not what it costs.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/company/approvals")
                .header("cookie", &member_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let listed = body
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == approval.to_string())
        .expect("the approval is visible to a member");
    assert_eq!(
        listed["contents_hidden"], true,
        "a member must be told the contents were withheld: {listed}"
    );
    assert!(
        listed["amount_usd"].is_null(),
        "a member must not receive the dollar amount: {listed}"
    );
    assert!(
        listed["payload"].is_null(),
        "a member must not receive the payload either: {listed}"
    );

    // Cannot resolve it.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/company/approvals/{approval}"))
                .header("cookie", &member_cookie)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "verdict": "approve" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a member must not be able to approve a parked effect"
    );

    // Cannot extend it either.
    let response = app
        .oneshot(extend_request_with_cookie(&approval, member_cookie))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a member must not be able to extend a parked effect's deadline"
    );
}
