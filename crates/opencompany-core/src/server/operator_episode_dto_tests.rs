//! Serialization coverage for the episode metadata a history row and an
//! `agent_reply` frame carry (plan hive-desks, Phase 4).

use crate::hive::routing::{Router, RoutingPlanDto};
use crate::ports::types::{ReplyEpisode, RoutedBy, UtteranceKind};

/// The wire shape the console binds to (`MessageEpisodeDto` in
/// `frontend/src/api/types.ts`).
#[test]
fn the_episode_metadata_reaches_the_wire_as_camel_case() {
    let episode = ReplyEpisode {
        id: "ep-1".into(),
        revision: 2,
        kind: UtteranceKind::Broadcast,
        to: Vec::new(),
        routed_by: Some(RoutedBy {
            plan: RoutingPlanDto::One {
                primary_id: "ceo".into(),
            },
            router: Router::Jev,
        }),
    };
    let wire = super::episode_json(&episode);
    assert_eq!(
        wire,
        serde_json::json!({
            "id": "ep-1",
            "revision": 2,
            "kind": "broadcast",
            "routedBy": {"plan": {"kind": "one", "primaryId": "ceo"}, "router": "jev"},
        }),
        "camelCase, `to` omitted when empty: {wire}"
    );
    let dm = ReplyEpisode {
        id: "ep-1".into(),
        revision: 3,
        kind: UtteranceKind::Dm,
        to: vec!["engineer".into()],
        routed_by: None,
    };
    let wire = super::episode_json(&dm);
    assert_eq!(wire["kind"], "dm");
    assert_eq!(wire["to"], serde_json::json!(["engineer"]));
    assert!(wire.get("routedBy").is_none());
    let done = ReplyEpisode {
        kind: UtteranceKind::CompleteEpisode,
        ..dm
    };
    assert_eq!(super::episode_json(&done)["kind"], "complete_episode");
}

/// The history DTO carries `episode` and `audience` beside the row, and
/// neither key appears on a row outside an episode.
#[test]
fn a_history_row_carries_episode_and_audience_only_when_set() {
    let mut view = crate::server::chat_history::MessageView::for_test(
        "7",
        "ceo",
        "Use the short form.",
        vec!["engineer".into()],
    );
    view.episode = Some(ReplyEpisode {
        id: "ep-1".into(),
        revision: 1,
        kind: UtteranceKind::Dm,
        to: vec!["engineer".into()],
        routed_by: None,
    });
    let dto = super::ChatHistoryMessageDto::from(view);
    let wire = serde_json::to_value(&dto).expect("the DTO serializes");
    assert_eq!(wire["episode"]["id"], "ep-1");
    assert_eq!(wire["episode"]["kind"], "dm");
    assert_eq!(wire["audience"], serde_json::json!(["engineer"]));
    assert!(wire.get("asideConversation").is_none());
    assert!(
        wire.get("cueText").is_none(),
        "text and cueText are equal: {wire}"
    );

    let plain = crate::server::chat_history::MessageView::for_test("8", "ceo", "hi", Vec::new());
    let wire = serde_json::to_value(super::ChatHistoryMessageDto::from(plain)).unwrap();
    assert!(wire.get("episode").is_none());
    assert!(wire.get("audience").is_none());
}

/// Every `RoutingPlanDto` kind on the wire, byte for byte against the
/// console's `RoutingPlanDto` union (`frontend/src/api/types.ts`): tagged by
/// `kind`, camelCase fields, a clarification's question omitted when absent,
/// and a fallback that still names the seat it fell back to.
#[test]
fn every_routing_plan_kind_reaches_the_wire_tagged_and_camel_cased() {
    let wire = |plan: &RoutingPlanDto| serde_json::to_value(plan).expect("the plan serializes");
    assert_eq!(
        wire(&RoutingPlanDto::One {
            primary_id: "ceo".into()
        }),
        serde_json::json!({"kind": "one", "primaryId": "ceo"})
    );
    assert_eq!(
        wire(&RoutingPlanDto::Hive {
            primary_id: "engineer".into(),
            invited_ids: vec!["ceo".into()],
        }),
        serde_json::json!({"kind": "hive", "primaryId": "engineer", "invitedIds": ["ceo"]})
    );
    assert_eq!(
        wire(&RoutingPlanDto::Clarify { question: None }),
        serde_json::json!({"kind": "clarify"})
    );
    assert_eq!(
        wire(&RoutingPlanDto::Clarify {
            question: Some("Which release?".into())
        }),
        serde_json::json!({"kind": "clarify", "question": "Which release?"})
    );
    assert_eq!(
        wire(&RoutingPlanDto::Fallback {
            primary_id: "ceo".into(),
            reason: "provider_unavailable".into(),
        }),
        serde_json::json!({"kind": "fallback", "primaryId": "ceo", "reason": "provider_unavailable"})
    );
    assert_eq!(
        serde_json::to_value(Router::Explicit).unwrap(),
        serde_json::json!("explicit")
    );
}

/// `GET {scope}/episodes` answers `EpisodeDto` rows newest first, in the
/// shape the console's `EpisodeDto` binds to (plan hive-desks, contract
/// addendum 1): camelCase, `parentId` as a message id string, the optional
/// completion fields present only once the episode closed.
#[tokio::test]
async fn the_episodes_route_answers_episode_dtos_newest_first() {
    use super::operator_test_support_1::*;
    use crate::ports::types::{CompanyEvent, EpisodeReason, EventSeq};
    use crate::server::router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(&home, desk_manifest()).await;
    let id = crate::ports::types::CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();
    let events = runtime.events();
    let plan = RoutingPlanDto::Hive {
        primary_id: "ceo".into(),
        invited_ids: vec!["eng".into()],
    };
    events
        .append(
            &id,
            CompanyEvent::EpisodeOpened {
                chat_id: "studio".into(),
                episode_id: "ep-open".into(),
                opened_by_seq: 3,
                parent: Some(EventSeq::new(3)),
                participants: vec!["ceo".into(), "eng".into()],
                plan: plan.clone(),
                hop: 0,
            },
        )
        .await
        .unwrap();
    events
        .append(
            &id,
            CompanyEvent::EpisodeOpened {
                chat_id: "studio".into(),
                episode_id: "ep-done".into(),
                opened_by_seq: 5,
                parent: None,
                participants: vec!["ceo".into()],
                plan: RoutingPlanDto::One {
                    primary_id: "ceo".into(),
                },
                hop: 0,
            },
        )
        .await
        .unwrap();
    events
        .append(
            &id,
            CompanyEvent::EpisodeCompleted {
                chat_id: "studio".into(),
                episode_id: "ep-done".into(),
                revision: 2,
                completed_by: Some("ceo".into()),
                rounds: 1,
                reason: EpisodeReason::CompleteEpisode,
                summary_seq: Some(9),
            },
        )
        .await
        .unwrap();

    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/company/episodes?desk=studio")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let rows = body.as_array().expect("an array of episodes");
    assert_eq!(rows.len(), 2, "{body}");

    // Newest first: the completed one opened later.
    let done = &rows[0];
    assert_eq!(done["id"], "ep-done");
    assert_eq!(done["chatId"], "studio");
    assert_eq!(done["openedBySeq"], 5);
    assert!(done.get("parentId").is_none(), "{done}");
    assert_eq!(done["participants"], serde_json::json!(["ceo"]));
    assert_eq!(
        done["plan"],
        serde_json::json!({"kind": "one", "primaryId": "ceo"})
    );
    assert_eq!(done["revision"], 2);
    assert_eq!(done["status"], "completed");
    assert!(done["openedAtMillis"].is_u64(), "{done}");
    assert!(done["completedAtMillis"].is_u64(), "{done}");
    assert_eq!(done["completedBy"], "ceo");
    assert_eq!(done["reason"], "complete_episode");
    let keys: Vec<&str> = done
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "id",
            "chatId",
            "openedBySeq",
            "participants",
            "plan",
            "revision",
            "status",
            "openedAtMillis",
            "completedAtMillis",
            "completedBy",
            "reason",
        ],
        "the episode row grew or lost a key: {done}"
    );

    let open = &rows[1];
    assert_eq!(open["id"], "ep-open");
    assert_eq!(open["status"], "open");
    assert_eq!(open["parentId"], "3");
    assert_eq!(open["plan"]["invitedIds"], serde_json::json!(["eng"]));
    for absent in ["completedAtMillis", "completedBy", "reason"] {
        assert!(
            open.get(absent).is_none(),
            "{absent} on an open episode: {open}"
        );
    }
}

/// A seat turn's run row carries its episode and round on both read
/// surfaces (plan hive-desks, contract addendum 5): `GET /runs` as
/// `episodeId` / `roundRevision`, omitted on every other attempt, and the
/// GraphQL `AgentRun` type declares the same two nullable fields.
#[test]
fn a_seat_turn_run_carries_its_episode_on_rest_and_graphql() {
    use crate::ports::runs::NewRun;
    let seat = NewRun::for_chat("turn-1", "engineering", "ceo")
        .in_thread(Some(crate::ports::types::EventSeq::new(4)))
        .in_episode("ep-1", 2);
    assert_eq!(seat.episode_id.as_deref(), Some("ep-1"));
    assert_eq!(seat.round_revision, Some(2));
    let plain = NewRun::for_chat("turn-2", "general", "ceo");
    assert!(plain.episode_id.is_none() && plain.round_revision.is_none());

    let sdl = crate::server::graphql::sdl();
    let agent_run = sdl
        .split("type AgentRun {")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("the SDL declares AgentRun");
    assert!(agent_run.contains("episodeId: ID\n"), "{agent_run}");
    assert!(agent_run.contains("roundRevision: Int\n"), "{agent_run}");
}
