use super::*;
use crate::AppConfig;
use crate::company::CompanyManifest;
use crate::ports::types::EventSeq;
use crate::server::router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::operator_test_support_1::*;
use super::operator_test_support_2::*;
use super::operator_test_support_3::*;

/// A second resolve of the same approval is a success, not a failure, and
/// mints nothing (issue #243). `detach` reports that as `alreadyResolved`,
/// which is what makes a retry after a timeout safe to *show* as a retry
/// rather than as an error — the thing #380's operator had no way to know.
#[tokio::test]
async fn a_second_resolve_reports_already_resolved_and_mints_nothing() {
    let home_dir = home();
    let c = stalled_company(home_dir.path()).await;
    c.release.notify_one();

    let first = c
        .app
        .clone()
        .oneshot(resolve_request(
            &c.approval_id,
            serde_json::json!({"verdict":"approve","detach":true}),
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let bytes = to_bytes(first.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["alreadyResolved"], false);
    assert!(await_continuation(&c.runtime).await);

    let second = c
        .app
        .clone()
        .oneshot(resolve_request(
            &c.approval_id,
            serde_json::json!({"verdict":"approve","detach":true}),
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let bytes = to_bytes(second.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        value,
        serde_json::json!({ "recorded": true, "alreadyResolved": true, "stillAwaiting": 0, "outcome": "already_resolved" })
    );
    assert_eq!(
        c.runtime.grants.live_count(),
        1,
        "re-approving minted no second grant"
    );
}

/// **Issue #1449 on the wire.** A card past its deadline answers `expired`,
/// on both response shapes, and journals no approval against the operator.
///
/// The two shapes matter independently. The **detached** receipt is what the
/// inline chat card reads; the **synchronous** `ChatResponse` is what the
/// Approvals page reads — the surface the defect was reported on — and it
/// never sees a receipt at all, so a discriminator that only rode on the
/// receipt would have left the reproduced bug in place.
#[tokio::test]
async fn a_resolve_past_the_deadline_answers_expired_on_both_shapes() {
    let home_dir = home();
    // `approval_ttl_hours = 0`: anything parked is past its deadline the
    // instant it lands, which is the state an operator meets when they get
    // to a queue late.
    let expiring: CompanyManifest = toml::from_str(
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\napproval_ttl_hours = 0\n",
    )
    .unwrap();
    let state = build_state_with_brain_and_manifest(
        home_dir.path(),
        "running",
        AppConfig::default(),
        Some(Arc::new(StalledContinuationBrain {
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
            parked: gated_tool_call(),
        })),
        expiring,
    )
    .await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    let app = router(state);

    let response = app.clone().oneshot(chat_request("do it")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let approval_id = runtime.pending_approvals()[0].id.clone();

    // The detached shape.
    let detached = app
        .clone()
        .oneshot(resolve_request(
            &approval_id,
            serde_json::json!({"verdict":"approve","detach":true}),
        ))
        .await
        .unwrap();
    assert_eq!(detached.status(), StatusCode::OK);
    let bytes = to_bytes(detached.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        value["outcome"], "expired",
        "the host default-denied this; the receipt has to be able to say so, got {value}"
    );
    assert_eq!(
        runtime.grants.live_count(),
        0,
        "and it minted nothing, as it always did"
    );

    // The synchronous shape, on a second card of the same company.
    let response = app
        .clone()
        .oneshot(chat_request("do it again"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let second = runtime.pending_approvals()[0].id.clone();
    let sync = app
        .clone()
        .oneshot(resolve_request(
            &second,
            serde_json::json!({"verdict":"approve"}),
        ))
        .await
        .unwrap();
    assert_eq!(sync.status(), StatusCode::OK);
    let bytes = to_bytes(sync.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        value.get("responses").is_some_and(|r| r.is_array()),
        "still a ChatResponse, got {value}"
    );
    assert_eq!(
        value["outcome"], "expired",
        "the Approvals page's own shape carries it too, got {value}"
    );
    assert_eq!(runtime.grants.live_count(), 0);
}

/// Both scope forms carry `detach` identically — the `/companies/{id}` route
/// and the single-company alias are the same handler, and a console pointed
/// at either must get the same contract.
#[tokio::test]
async fn detach_works_on_the_company_id_scope_too() {
    let home_dir = home();
    let c = stalled_company(home_dir.path()).await;
    c.release.notify_one();

    let response = c
        .app
        .clone()
        .oneshot(resolve_request_scoped(
            "/api/v1/companies/acme",
            &c.approval_id,
            serde_json::json!({"verdict":"approve","detach":true}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        value,
        serde_json::json!({ "recorded": true, "alreadyResolved": false, "stillAwaiting": 0, "outcome": "settled" })
    );
    assert!(await_continuation(&c.runtime).await);
    assert_eq!(c.runtime.grants.live_count(), 1);
}

#[tokio::test]
async fn deny_with_amended_payload_is_400() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home, "running").await;
    let app = router(state);

    // The contradiction is rejected before anything is settled, so `detach`
    // cannot turn it into a `200 { recorded: true }` over a decision that was
    // never taken (issue #383).
    for body in [
        r#"{"verdict":"deny","amended_payload":{"text":"edited"}}"#,
        r#"{"verdict":"deny","amended_payload":{"text":"edited"},"detach":true}"#,
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/company/approvals/missing")
                    .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "for {body}");
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["code"], "invalid_request");
    }
}

#[tokio::test]
async fn a_session_is_required_and_sufficient() {
    // Replaces `operator_token_guards_routes`. That token could never be
    // set, so the test only ever proved the guard worked in a state no
    // deployment could reach; every real host served this route to anyone.
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = build_state(&home, "running", AppConfig::default()).await;

    // No credential at all: closed.
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/companies")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // A garbage bearer buys nothing either — there is no bearer path in
    // prosumer mode at all now.
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/companies")
                .header("authorization", "Bearer nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // A signed-in human gets their own company.
    let cookie = crate::server::test_support::seed_admin(&state, "acme").await;
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/companies")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn projects_a_gap_with_structural_fields_only() {
    let value = super::project_stream_item_for_viewer(
        &EventStreamItem::Gap { missed: 44 },
        &std::collections::HashMap::new(),
        &crate::server::readable::DisplayNames::default(),
        &Viewer::Operator,
        true,
    )
    .expect("a gap must reach the console");
    assert_eq!(
        value,
        serde_json::json!({ "type": "stream_gap", "missed": 44 })
    );
}

#[test]
fn projects_agent_reply_with_chat_fields_and_steps() {
    use crate::ports::types::{TurnStep, TurnStepKind, TurnStepStatus};
    let v = super::project_event(&stored(CompanyEvent::AgentReply {
        audience: Vec::new(),
        episode: None,
        mentions: Vec::new(),
        mention_depth: 0,
        parent: None,
        task_id: None,
        outputs: Vec::new(),
        chat_id: "General".into(),
        agent_id: "ceo".into(),
        text: "shipped it".into(),
        steps: vec![TurnStep {
            kind: TurnStepKind::ToolCall,
            status: TurnStepStatus::Ok,
            label: "Reading messages".into(),
            detail: None,
            elapsed_ms: Some(12),
            ..TurnStep::default()
        }],
    }))
    .expect("agent_reply is an attention signal");
    assert_eq!(v["type"], "agent_reply");
    assert_eq!(v["seq"], 7);
    assert_eq!(v["atMillis"], 1_700_000_000_000_u64);
    assert_eq!(v["chatId"], "General");
    assert_eq!(v["agentId"], "ceo");
    assert_eq!(v["text"], "shipped it");
    // The scrubbed timeline rides along so a live listener sees the steps.
    assert_eq!(v["steps"][0]["label"], "Reading messages");
    assert_eq!(v["steps"][0]["status"], "ok");
    // A channel reply names no thread, so the legacy frame is unchanged.
    assert!(v.get("parentId").is_none(), "unexpected parentId: {v}");
}

/// **The live frame carries the model's own body, not only the operator's.**
///
/// `MessageView` has shipped both since it gained `cue_text`; the live frame
/// had only `text`, so anything needing the room's grammar had to scrape it
/// back out of the operator-facing body. `frontend/src/lib/hive/episode.ts`
/// does exactly that (`moveOf(m.text)`), which is why rewriting `text` here
/// costs the deliberation panel rather than merely tidying a bubble.
///
/// **A crossing tells the live stream which thread to re-read.**
///
/// The fold that renders a crossing is built by `attach_referral_origins`,
/// which runs in `history_for_desk` and nowhere else — so a crossing was
/// invisible live and appeared only once something re-read the thread. For
/// a desk crossing that meant waiting for settle; for a pair DM it meant
/// never, since its rows are journaled in the pair's own `dm:<a>+<b>`
/// conversation that no desk view subscribes to.
///
/// The frame carries no crossing content on purpose. Rebuilding the fold on
/// this path would be a second implementation of a rule this subsystem has
/// already had to fix in two places, four separate times.
#[test]
fn a_crossing_names_the_thread_whose_fold_changed() {
    let v = super::project_event(&stored(CompanyEvent::ReferralEnqueued {
        conversation: None,
        answers: None,
        from_desk: "engineering".into(),
        from_desk_name: "Engineering".into(),
        asker: "software_engineer".into(),
        asker_label: "software_engineer".into(),
        trigger_sequence: 41,
        to_desk: "design".into(),
        target: "product_designer".into(),
        returning: false,
        rows: None,
        episode_id: None,
        to_episode_id: None,
        hop: 0,
    }))
    .expect("a crossing is projected at all");

    assert_eq!(v["type"], "referral");
    assert_eq!(
        v["chatId"], "engineering",
        "the desk that ASKED is the one whose transcript gains the fold"
    );
    assert_eq!(v["sequence"], 41, "and the row it folds onto");
    assert_eq!(v["toDesk"], "design");
    assert_eq!(v["asker"], "software_engineer");
    assert_eq!(
        v["direct"], false,
        "a desk crossing is not a person-to-person exchange"
    );
    assert_eq!(
        v["returning"], false,
        "which leg, read as `ReferredFrom` is"
    );
    assert!(
        v.get("lines").is_none() && v.get("referralConversation").is_none(),
        "no crossing content travels on this frame: {v}"
    );
}

/// A return leg is addressed the other way round, and the frame must follow
/// the fold rather than the field name.
///
/// `mark` builds a return from the ANSWERING desk, so `from_desk` is the far
/// desk and `to_desk` is the desk that asked and is still waiting. Reading
/// `from_desk` on both legs sent the console to re-read the far desk exactly
/// on the leg that carries the answer — and because the forward frame goes
/// out before any answer exists, the asking desk was never refreshed at all
/// (Codex, #2341).
#[test]
fn a_returning_crossing_names_the_desk_that_asked() {
    let v = super::project_event(&stored(CompanyEvent::ReferralEnqueued {
        conversation: None,
        answers: Some(41),
        from_desk: "design".into(),
        from_desk_name: "Design".into(),
        asker: "product_designer".into(),
        asker_label: "product_designer".into(),
        trigger_sequence: 58,
        to_desk: "engineering".into(),
        target: "software_engineer".into(),
        returning: true,
        rows: None,
        episode_id: None,
        to_episode_id: None,
        hop: 0,
    }))
    .expect("a return is projected at all");

    assert_eq!(
        v["chatId"], "engineering",
        "the answer folds onto the asking desk, which a return names as `to_desk`"
    );
    assert_eq!(v["returning"], true);
}

/// Pinned now, while the two are equal, so the step that rewrites `text`
/// cannot quietly take `cueText` with it.
#[test]
fn projects_the_agents_own_body_beside_the_operators() {
    let stored = stored(CompanyEvent::AgentReply {
        audience: Vec::new(),
        episode: None,
        mentions: Vec::new(),
        mention_depth: 0,
        parent: None,
        task_id: None,
        outputs: Vec::new(),
        chat_id: "returns".into(),
        agent_id: "refunds".into(),
        text: "!support #kettle ^16 the swap is the customer's first preference".into(),
        steps: Vec::new(),
    });
    let value = super::project_event_for_viewer(
        &stored,
        &std::collections::HashMap::new(),
        &crate::server::readable::DisplayNames::default(),
        &Viewer::Operator,
        true,
    )
    .expect("agent_reply is an attention signal");

    assert_eq!(
        value["cueText"], "!support #kettle ^16 the swap is the customer's first preference",
        "the body as the model wrote it rides on the frame: {value}"
    );
    assert_eq!(
        value["text"], "!support #kettle ^16 the swap is the customer's first preference",
        "and nothing is rewritten for the operator since the move grammar retired: {value}"
    );
    assert!(
        value.get("episode").is_none(),
        "outside an episode: {value}"
    );
    assert!(value.get("audience").is_none(), "desk-visible: {value}");
}

/// The episode metadata and the audience ride on the live frame exactly as
/// the reload projects them (plan hive-desks, Phase 4), so a live row and its
/// rehydrated twin fold into the same round.
#[test]
fn projects_episode_and_audience_on_a_seat_reply() {
    use crate::ports::types::{ReplyEpisode, RoutedBy, UtteranceKind};
    let stored = stored(CompanyEvent::AgentReply {
        audience: vec!["engineer".into()],
        mentions: Vec::new(),
        mention_depth: 0,
        parent: None,
        task_id: None,
        outputs: Vec::new(),
        chat_id: "engineering".into(),
        agent_id: "ceo".into(),
        text: "quietly, the short form".into(),
        steps: Vec::new(),
        episode: Some(ReplyEpisode {
            id: "ep-1".into(),
            revision: 3,
            kind: UtteranceKind::Dm,
            to: vec!["engineer".into()],
            routed_by: Some(RoutedBy {
                plan: crate::hive::routing::RoutingPlanDto::One {
                    primary_id: "engineer".into(),
                },
                router: crate::hive::routing::Router::Fallback,
            }),
        }),
    });
    let value = super::project_event(&stored).expect("agent_reply is an attention signal");
    assert_eq!(value["episode"]["id"], "ep-1");
    assert_eq!(value["episode"]["revision"], 3);
    assert_eq!(value["episode"]["kind"], "dm");
    assert_eq!(value["episode"]["to"], serde_json::json!(["engineer"]));
    assert_eq!(value["episode"]["routedBy"]["router"], "fallback");
    assert_eq!(value["episode"]["routedBy"]["plan"]["kind"], "one");
    assert_eq!(value["audience"], serde_json::json!(["engineer"]));
}

/// And on a desk that does not deliberate the two are byte-equal, so no
/// consumer has to choose between them for an ordinary reply.
#[test]
fn a_reply_with_no_move_carries_the_same_body_twice() {
    let stored = stored(CompanyEvent::AgentReply {
        audience: Vec::new(),
        episode: None,
        mentions: Vec::new(),
        mention_depth: 0,
        parent: None,
        task_id: None,
        outputs: Vec::new(),
        chat_id: "general".into(),
        agent_id: "ceo".into(),
        text: "here is the summary you asked for".into(),
        steps: Vec::new(),
    });
    let value = super::project_event_for_viewer(
        &stored,
        &std::collections::HashMap::new(),
        &crate::server::readable::DisplayNames::default(),
        &Viewer::Operator,
        true,
    )
    .expect("agent_reply is an attention signal");

    assert_eq!(value["cueText"], value["text"]);
}

#[test]
fn projects_agent_reply_with_viewer_mention_metadata() {
    use crate::ports::types::{Mention, MentionTarget};
    let stored = stored(CompanyEvent::AgentReply {
        audience: Vec::new(),
        episode: None,
        mentions: vec![
            Mention {
                target: MentionTarget::User { id: "u-1".into() },
                text: "@Ada".into(),
                offset: 0,
                quiet: false,
            },
            Mention {
                target: MentionTarget::Everyone,
                text: "@everyone".into(),
                offset: 5,
                quiet: true,
            },
        ],
        mention_depth: 0,
        parent: None,
        task_id: None,
        outputs: Vec::new(),
        chat_id: "General".into(),
        agent_id: "ceo".into(),
        text: "@Ada @everyone".into(),
        steps: Vec::new(),
    });
    let authors = std::collections::HashMap::from([(String::from("u-1"), String::from("Ada"))]);
    let value = super::project_event_for_viewer(
        &stored,
        &authors,
        &crate::server::readable::DisplayNames::default(),
        &Viewer::User("u-1".into()),
        false,
    )
    .expect("agent_reply is an attention signal");
    assert_eq!(
        value["mentions"],
        serde_json::json!([
            { "text": "@Ada", "offset": 0, "label": "Ada", "mine": true },
            { "text": "@everyone", "offset": 5, "label": "everyone", "mine": true, "quiet": true },
        ])
    );
}

/// Issue #1781 review, Codex P1: `history_for_desk` already hides an
/// owner-fallback report from a non-admin on reload; this proves the live
/// SSE projection agrees, rather than handing a non-admin console the full
/// admin-only text the instant it lands.
#[test]
fn drops_owner_fallback_report_from_a_non_admin_viewer() {
    let event = stored(CompanyEvent::AgentReply {
        audience: Vec::new(),
        episode: None,
        mentions: Vec::new(),
        mention_depth: 0,
        parent: None,
        task_id: None,
        outputs: Vec::new(),
        chat_id: "operator".into(),
        agent_id: crate::runtime::OWNER_FALLBACK_REPORT_AUTHOR.to_string(),
        text: "no admin has a mailbox".into(),
        steps: Vec::new(),
    });

    let non_admin = super::project_event_for_viewer(
        &event,
        &std::collections::HashMap::new(),
        &crate::server::readable::DisplayNames::default(),
        &Viewer::User("member-1".into()),
        false,
    );
    assert!(
        non_admin.is_none(),
        "a non-admin viewer must not receive the admin-only report live: {non_admin:?}"
    );

    let admin = super::project_event_for_viewer(
        &event,
        &std::collections::HashMap::new(),
        &crate::server::readable::DisplayNames::default(),
        &Viewer::User("admin-1".into()),
        true,
    )
    .expect("an admin viewer still receives the report live");
    assert_eq!(
        admin["agentId"],
        crate::runtime::OWNER_FALLBACK_REPORT_AUTHOR
    );

    // The Operator viewer (issue #66's original, unrestricted principal)
    // must see it too — same as `project_event`'s `is_admin: true` default.
    let operator = super::project_event_for_viewer(
        &event,
        &std::collections::HashMap::new(),
        &crate::server::readable::DisplayNames::default(),
        &Viewer::Operator,
        true,
    )
    .expect("the operator viewer still receives the report live");
    assert_eq!(operator["text"], "no admin has a mailbox");
}

#[test]
fn projects_agent_reply_with_its_thread_parent() {
    let v = super::project_event(&stored(CompanyEvent::AgentReply {
        audience: Vec::new(),
        episode: None,
        mentions: Vec::new(),
        mention_depth: 0,
        parent: Some(EventSeq::new(4)),
        task_id: None,
        outputs: Vec::new(),
        chat_id: "General".into(),
        agent_id: "ceo".into(),
        text: "in the thread".into(),
        steps: Vec::new(),
    }))
    .expect("agent_reply is an attention signal");
    assert_eq!(v["parentId"], "4");
}

/// Issue #983: the accept frame carries the turn, the desk and the thread —
/// and **nothing else**.
///
/// The negative half is what this test is for. `TurnStarted` is the first
/// frame on this stream that brackets an operator's own message, so it is
/// the obvious place for somebody to "helpfully" add the text or the asker
/// — which is exactly the payload the deny-by-default projection exists to
/// keep off the wire, and which `OperatorMessage` is dropped to avoid.
#[test]
fn projects_turn_started_with_structural_keys_only() {
    use crate::ports::types::{Actor, ActorKind};
    let v = super::project_event(&stored(CompanyEvent::TurnStarted {
        turn_id: "turn-1".into(),
        chat_id: "General".into(),
        parent: Some(EventSeq::new(4)),
        by: Some(Actor {
            kind: ActorKind::User,
            id: "u-1".into(),
        }),
        agent_id: None,
        episode_id: None,
        round_revision: None,
    }))
    .expect("an accepted turn is an attention signal");
    assert_eq!(v["type"], "turn_started");
    assert_eq!(v["turnId"], "turn-1");
    assert_eq!(v["chatId"], "General");
    assert_eq!(v["parentId"], "4");
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["type", "seq", "atMillis", "turnId", "chatId", "parentId"],
        "the accept frame grew a key: {v}"
    );

    // A turn answering the channel itself omits the thread rather than
    // sending null, so the console's check is a presence check.
    let v = super::project_event(&stored(CompanyEvent::TurnStarted {
        turn_id: "turn-2".into(),
        chat_id: "General".into(),
        parent: None,
        by: None,
        agent_id: None,
        episode_id: None,
        round_revision: None,
    }))
    .expect("an accepted turn is an attention signal");
    assert!(v.get("parentId").is_none(), "unexpected parentId: {v}");
}

/// The settle frame says a turn is over and **not why**.
///
/// `TurnFailed::error` is a reason in our own words that can name
/// internals; the console learns the reason from the tenant-scoped run row.
#[test]
fn projects_turn_settled_without_the_failure_reason() {
    let v = super::project_event(&stored(CompanyEvent::TurnFailed {
        turn_id: "turn-1".into(),
        error: "connection to db-primary.internal refused".into(),
        agent_id: None,
        chat_id: None,
        episode_id: None,
        round_revision: None,
        outcome: None,
    }))
    .expect("a settled turn is an attention signal");
    assert_eq!(v["type"], "turn_settled");
    assert_eq!(v["turnId"], "turn-1");
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["type", "seq", "atMillis", "turnId", "outcome"],
        "the settle frame grew a key: {v}"
    );
    assert_eq!(v["outcome"], "failed");
}

/// A seat's turn bracket carries the seat and the round on both frames (plan
/// hive-desks, Phase 4), and a timed-out seat says so.
#[test]
fn projects_the_seat_and_round_on_a_hive_turn_bracket() {
    use crate::ports::types::TurnOutcome;
    let started = super::project_event(&stored(CompanyEvent::TurnStarted {
        turn_id: "turn-7".into(),
        chat_id: "engineering".into(),
        parent: None,
        by: None,
        agent_id: Some("ceo".into()),
        episode_id: Some("ep-1".into()),
        round_revision: Some(2),
    }))
    .expect("turn_started");
    assert_eq!(started["type"], "turn_started");
    assert_eq!(started["agentId"], "ceo");
    assert_eq!(started["episodeId"], "ep-1");
    assert_eq!(started["roundRevision"], 2);
    let settled = super::project_event(&stored(CompanyEvent::TurnSettled {
        turn_id: "turn-7".into(),
        agent_id: Some("ceo".into()),
        chat_id: Some("engineering".into()),
        episode_id: Some("ep-1".into()),
        round_revision: Some(2),
        outcome: TurnOutcome::NoUtterance,
    }))
    .expect("turn_settled");
    assert_eq!(settled["type"], "turn_settled");
    assert_eq!(settled["outcome"], "no_utterance");
    assert_eq!(settled["chatId"], "engineering");
    assert_eq!(settled["episodeId"], "ep-1");
    assert_eq!(settled["roundRevision"], 2);
    let timed_out = super::project_event(&stored(CompanyEvent::TurnFailed {
        turn_id: "turn-8".into(),
        error: "ran past 600s".into(),
        agent_id: Some("engineer".into()),
        chat_id: Some("engineering".into()),
        episode_id: Some("ep-1".into()),
        round_revision: Some(2),
        outcome: Some(TurnOutcome::TimedOut),
    }))
    .expect("turn_settled");
    assert_eq!(timed_out["outcome"], "timed_out");
    assert_eq!(timed_out["agentId"], "engineer");
    assert!(
        timed_out.get("error").is_none(),
        "the reason stays off the wire: {timed_out}"
    );
}

/// The episode frames project one to one with their journal rows, camelCase,
/// every one carrying the desk and the episode (plan hive-desks, Phase 4).
#[test]
fn projects_the_episode_frames() {
    use crate::hive::routing::{Router, RoutingPlanDto};
    use crate::ports::types::{EpisodeReason, RoundUtteranceRecord, UtteranceKind};
    let plan = RoutingPlanDto::Hive {
        primary_id: "engineer".into(),
        invited_ids: vec!["ceo".into()],
    };
    let opened = super::project_event(&stored(CompanyEvent::EpisodeOpened {
        chat_id: "engineering".into(),
        episode_id: "ep-1".into(),
        opened_by_seq: 4,
        parent: Some(crate::ports::types::EventSeq::new(2)),
        participants: vec!["engineer".into(), "ceo".into()],
        plan: plan.clone(),
        hop: 0,
    }))
    .expect("episode_opened");
    assert_eq!(opened["type"], "episode_opened");
    assert_eq!(opened["chatId"], "engineering");
    assert_eq!(opened["episodeId"], "ep-1");
    assert_eq!(opened["openedBySeq"], 4);
    assert_eq!(opened["parentId"], "2");
    assert_eq!(
        opened["participants"],
        serde_json::json!(["engineer", "ceo"])
    );
    assert_eq!(opened["plan"]["invitedIds"], serde_json::json!(["ceo"]));

    let round = super::project_event(&stored(CompanyEvent::RoundStarted {
        chat_id: "engineering".into(),
        episode_id: "ep-1".into(),
        revision: 0,
        agent_ids: vec!["engineer".into(), "ceo".into()],
    }))
    .expect("round_started");
    assert_eq!(round["type"], "round_started");
    assert_eq!(round["revision"], 0);
    assert_eq!(round["agentIds"], serde_json::json!(["engineer", "ceo"]));

    let committed = super::project_event(&stored(CompanyEvent::RoundCommitted {
        chat_id: "engineering".into(),
        episode_id: "ep-1".into(),
        revision: 0,
        utterances: vec![
            RoundUtteranceRecord {
                agent_id: "engineer".into(),
                sequence: 7,
                kind: UtteranceKind::Broadcast,
                message_seq: Some(7),
                to: Vec::new(),
            },
            RoundUtteranceRecord {
                agent_id: "ceo".into(),
                sequence: 8,
                kind: UtteranceKind::Dm,
                message_seq: Some(8),
                to: vec!["engineer".into()],
            },
        ],
        actions: vec![serde_json::json!({"kind": "run_agents"})],
    }))
    .expect("round_committed");
    assert_eq!(committed["type"], "round_committed");
    assert_eq!(committed["utterances"][0]["messageSeq"], 7);
    assert_eq!(committed["utterances"][0]["kind"], "broadcast");
    assert!(committed["utterances"][0].get("to").is_none());
    assert_eq!(
        committed["utterances"][1]["to"],
        serde_json::json!(["engineer"])
    );
    assert_eq!(committed["actions"][0]["kind"], "run_agents");

    let routed = super::project_event(&stored(CompanyEvent::BroadcastRouted {
        chat_id: "engineering".into(),
        episode_id: "ep-1".into(),
        revision: 0,
        agent_id: "engineer".into(),
        message_seq: 7,
        plan: RoutingPlanDto::Fallback {
            primary_id: "ceo".into(),
            reason: "provider_unavailable".into(),
        },
        probabilities: None,
        router: Router::Fallback,
    }))
    .expect("broadcast_routed");
    assert_eq!(routed["type"], "broadcast_routed");
    assert_eq!(routed["messageSeq"], 7);
    assert_eq!(routed["router"], "fallback");
    assert_eq!(routed["plan"]["reason"], "provider_unavailable");
    assert!(routed.get("probabilities").is_none());

    let dm = super::project_event(&stored(CompanyEvent::DmDelivered {
        chat_id: "engineering".into(),
        episode_id: "ep-1".into(),
        from: "ceo".into(),
        to: vec!["engineer".into()],
        message_seq: 8,
    }))
    .expect("dm_delivered");
    assert_eq!(dm["type"], "dm_delivered");
    assert_eq!(dm["from"], "ceo");
    assert_eq!(dm["to"], serde_json::json!(["engineer"]));

    let done = super::project_event(&stored(CompanyEvent::EpisodeCompleted {
        chat_id: "engineering".into(),
        episode_id: "ep-1".into(),
        revision: 4,
        completed_by: Some("ceo".into()),
        rounds: 2,
        reason: EpisodeReason::CompleteEpisode,
        summary_seq: Some(11),
    }))
    .expect("episode_completed");
    assert_eq!(done["type"], "episode_completed");
    assert_eq!(done["revision"], 4);
    assert_eq!(done["completedBy"], "ceo");
    assert_eq!(done["rounds"], 2);
    assert_eq!(done["reason"], "complete_episode");
    assert_eq!(done["summarySeq"], 11);

    let configured = super::project_event(&stored(CompanyEvent::DeskRoutingConfigured {
        desk_id: "engineering".into(),
        reset: true,
        by: None,
    }))
    .expect("desk_routing_configured");
    assert_eq!(configured["type"], "desk_routing_configured");
    assert_eq!(configured["reset"], true);

    // The checkpoint is the runtime's own and never reaches the stream.
    assert!(
        super::project_event(&stored(CompanyEvent::EpisodeStateSaved {
            episode_id: "ep-1".into(),
            desk: "engineering".into(),
            thread_root: None,
            revision: 4,
            state: serde_json::json!({}),
            sharing: Default::default(),
            hop: 0,
            origin: None,
        }))
        .is_none()
    );
}

/// The operator's own message is **still** dropped (issue #983).
///
/// Pinned because #983 added the two arms above right beside it, and the
/// natural next step — "the console needs the message too, project it" —
/// would put operator-authored free text onto this stream for the first
/// time. It does not need it: the message is already in the POST's own
/// response and in `chat/history`, which is the point of journaling it at
/// accept time. If somebody later decides otherwise, they say so here.
#[test]
fn projects_nothing_for_the_operators_own_message() {
    assert!(
        super::project_event(&stored(CompanyEvent::OperatorMessage {
            mentions: Vec::new(),
            text: "the operator's own words".into(),
            by: None,
            chat: Some("General".into()),
            parent: None,
            deliverable: None,
            attachments: Vec::new(),
        }))
        .is_none(),
        "the operator's own message must not reach the console over SSE"
    );
}

/// A reaction is deliberately NOT on the attention stream (issue #364).
///
/// Pinned rather than left to the deny-by-default fall-through, because the
/// omission is a decision and not an oversight: the frame would have to
/// carry the reacting person, and this stream has no per-viewer projection
/// to turn an actor into a label. Reload-visibility is what the issue asks
/// for. If someone later decides reactions should stream, this test is
/// where they say so out loud.
#[test]
fn projects_nothing_for_a_reaction() {
    assert!(
        super::project_event(&stored(CompanyEvent::ReactionToggled {
            message_seq: EventSeq::new(4),
            emoji: "👍".into(),
            on: true,
            by: None,
        }))
        .is_none(),
        "a reaction must not reach the console over SSE"
    );
}

/// Issue #379: the park frame carries an id, a kind and the channel — and
/// **nothing else**.
///
/// The negative half is the load-bearing one. The effect's arguments are
/// redacted in exactly one place (`pending_approvals`), and if this frame
/// ever grew a `payload` key it would become a second surface that has to
/// redact and one day will not. Asserting the absence is what makes that a
/// build failure rather than a leak.
#[test]
fn projects_approval_parked_with_a_channel_and_no_payload() {
    let v = super::project_event(&stored(CompanyEvent::ApprovalParked {
        approval_id: ApprovalId::new("appr-1"),
        effect_kind: "payment.send".into(),
        thread: Some("desk-finance".into()),
    }))
    .expect("a parked approval is an attention signal");
    assert_eq!(v["type"], "approval_parked");
    assert_eq!(v["approvalId"], "appr-1");
    assert_eq!(v["kind"], "payment.send");
    assert_eq!(v["chatId"], "desk-finance");
    for forbidden in ["payload", "agent", "amountUsd", "effect", "args"] {
        assert!(
            v.get(forbidden).is_none(),
            "the park frame must stay thin — `{forbidden}` leaked: {v}",
        );
    }
    assert_eq!(
        v.as_object().unwrap().len(),
        6,
        "type, seq, atMillis, approvalId, kind, chatId — and nothing more: {v}",
    );
}
