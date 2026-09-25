use super::*;
use crate::company::CompanyManifest;
#[cfg(feature = "openhuman")]
use crate::ports::types::CompanyRecord;
#[cfg(feature = "openhuman")]
use crate::runtime::RuntimeBuilder;
use crate::server::router;
#[cfg(feature = "openhuman")]
use crate::store::FsCompanyStore;
#[cfg(feature = "openhuman")]
use crate::{AppConfig, AppState};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::operator_test_support_1::*;

/// Issue #1152's "Just chatting" and the plain send now agree: neither opens
/// a card, and neither does `once`. The lexical triage used to card an
/// unmarked `Track` message and the chat intent was the operator's only way
/// to withhold it; with the route no longer carding on the triage at all,
/// the only intent that mints is `workflow` — and that one still does.
#[tokio::test]
async fn only_an_explicit_workflow_request_opens_a_card_from_the_route() {
    use crate::ports::tasks::TaskDeliverable;

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home, "running").await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();
    let app = router(state);

    // A message the triage reads as work, so "no card" is the route's doing.
    let text = "can you build the landing page?";
    assert!(
        matches!(
            crate::company::task_intent::triage_message(text),
            crate::company::task_intent::MessageTriage::Track(_)
        ),
        "fixture must be a message the triage calls work, or this proves nothing"
    );

    let chat = |intent: Option<&str>| {
        let body = match intent {
            Some(i) => format!(
                r#"{{"text":{},"deliverable":"{i}"}}"#,
                serde_json::json!(text)
            ),
            None => format!(r#"{{"text":{}}}"#, serde_json::json!(text)),
        };
        Request::builder()
            .method("POST")
            .uri("/api/v1/company/chat")
            .header("cookie", crate::server::test_support::fixed_cookie("acme"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    };

    for intent in [None, Some("chat"), Some("once")] {
        let r = app.clone().oneshot(chat(intent)).await.unwrap();
        assert_eq!(
            r.status(),
            StatusCode::OK,
            "{intent:?}: the message is still answered"
        );
        assert!(
            runtime.tasks().list(&id).await.unwrap().is_empty(),
            "{intent:?}: a chat message opens no card by itself"
        );
    }

    let r = app.oneshot(chat(Some("workflow"))).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let tasks = runtime.tasks().list(&id).await.unwrap();
    assert_eq!(
        tasks.len(),
        1,
        "`workflow` is the operator's positive statement of intent"
    );
    assert_eq!(
        tasks[0].deliverable,
        TaskDeliverable::Workflow,
        "and it routes its card to the builder pass: {tasks:?}"
    );
}

/// Issue #576: **who** asked decides whether the card self-promotes.
///
/// The promotion buys a planning pass, which is a model call. A person
/// spending one on their own typo is the cost the issue accepts; an agent
/// doing it is a loop — a card that plans, whose pass opens further cards,
/// which promote, which plan, with no human anywhere in it. So the branch is
/// on the actor, and this pins both sides of it.
///
/// Driven through `run_chat` directly rather than the route, because the
/// route's job is to *resolve* the actor and this test's job is to pin what
/// each resolved actor does. Going through HTTP would only ever exercise
/// whichever principal the test harness happens to authenticate as.
#[tokio::test]
async fn only_a_person_gets_a_self_promoting_card() {
    use crate::ports::tasks::{COLUMN_PLANNING, COLUMN_TODO};
    use crate::ports::types::{Actor, ActorKind};

    let ask = "build the landing page";
    let person = Actor {
        kind: ActorKind::User,
        id: "u-1".to_string(),
    };

    // Every actor that is not a person must leave the card where it has
    // always landed. `None` is a machine credential — the platform, or any
    // caller with no session behind it.
    for (label, by, expected) in [
        ("a signed-in user", Some(person.clone()), COLUMN_PLANNING),
        (
            "an operator",
            Some(Actor {
                kind: ActorKind::Operator,
                id: "op".to_string(),
            }),
            COLUMN_PLANNING,
        ),
        (
            "an agent",
            Some(Actor {
                kind: ActorKind::Agent,
                id: "ceo".to_string(),
            }),
            COLUMN_TODO,
        ),
        (
            "the runtime itself",
            Some(Actor {
                kind: ActorKind::System,
                id: "system".to_string(),
            }),
            COLUMN_TODO,
        ),
        ("a machine credential", None, COLUMN_TODO),
    ] {
        let home_dir = home();
        let state = state_with_company(home_dir.path(), "running").await;
        let id = CompanyId::new("acme");
        let runtime = state.registry().get(&id).unwrap();

        // The explicit workflow control: the one signal the route still
        // opens a card on by itself.
        let message = ChatMessage {
            mentions: None,
            text: ask.to_string(),
            chat: None,
            parent: None,
            deliverable: Some(crate::ports::types::MessageIntent::Workflow),
            detach: false,
            attachments: Vec::new(),
        };
        let accepted = accept_chat_turn(
            &runtime,
            &id,
            &message,
            by.as_ref(),
            None,
            crate::server::ops::language::DEFAULT_DESK,
        )
        .await
        .expect("the turn is accepted");
        run_chat(runtime.clone(), message, by, &accepted)
            .await
            .expect("the chat cycle runs");

        let tasks = runtime.tasks().list(&id).await.unwrap();
        assert_eq!(tasks.len(), 1, "{label}: one ask opens one card");
        assert_eq!(
            tasks[0].column, expected,
            "{label}: the card must land in `{expected}`"
        );
    }
}

/// End-to-end proof of the WS4 wire: with a [`HarnessBrain`] as the runtime's
/// cognition, `POST /company/chat` returns the **agent's** reply rather than
/// the echo brain's `"You said: …"`. The mock provider prefixes the routed
/// message, so `"mock: hi"` proves the operator message reached an openhuman
/// agent turn through the HTTP handler → `run_cycle` → brain path.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn chat_routes_through_the_harness_brain() {
    use crate::harness::provider::MockProvider;
    use crate::harness::{HarnessBrain, HarnessDeps, HarnessPool};
    use crate::ports::CompanyStore;
    use crate::store::{FsContextStore, FsOps};

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let id = CompanyId::new("acme");
    let manifest: CompanyManifest = toml::from_str(
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[agent]]\nid = \"ceo\"\nrole = \"Chief Executive\"\n",
    )
    .unwrap();

    let record = CompanyRecord {
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: id.clone(),
        manifest: manifest.clone(),
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
    };
    FsCompanyStore::new(home.to_path_buf())
        .save(&record)
        .await
        .unwrap();

    let deps = HarnessDeps {
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        run_supervisor: crate::runtime::RunSupervisor::default(),
        provider: Arc::new(MockProvider::new("mock: ")),
        provider_slug: "mock".to_string(),
        serves: None,
        context: Arc::new(FsContextStore::new(home.to_path_buf())),
        store: Arc::new(FsCompanyStore::new(home.to_path_buf())),
        meter: Some(Arc::new(FsOps::new(home.to_path_buf()))),
        workspace_root: home.to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: home.to_path_buf(),
        model_override: None,
        tasks: None,
        artifacts: None,
        skills: None,
        skills_source_dir: None,
        skills_registry: std::sync::Arc::from([]),
        default_mcp_servers: Vec::new(),
        mcp_servers: Vec::new(),
        facts: None,
        events: None,
        delegations: crate::harness::orchestrator::DelegationQueue::default(),
        workflow_runner: crate::harness::orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: crate::harness::mcp_probe::McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_revisions: None,
        approval_requests: crate::harness::policy::ApprovalRequestQueue::default(),
        approval_parker: None,
        secrets: None,
        web_allowed_domains: Vec::new(),
        capabilities: crate::harness::toolbelt::CapabilityFilter::AllowAll,
        workflow_source_dir: None,
        plan: None,
        media: None,
        composio: None,
        #[cfg(feature = "chargebee")]
        chargebee: None,
        #[cfg(feature = "paypal")]
        paypal: None,
        hosting: None,
        steer: crate::company::steer::InflightRegistry::default(),
        delivery: None,
        search: None,
        tenant_search: None,
        workspace: None,
        workflow_runs: None,
        deep_trace: None,
    };
    let brain = HarnessBrain::new(Arc::new(HarnessPool::new()), deps, record);

    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .with_brain(Arc::new(brain))
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/chat")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                // Issue #1725: not "hi". A bare pleasantry is answered by
                // the runtime without a turn, so it would reach no brain at
                // all — which is the opposite of what this asserts.
                .body(Body::from(r#"{"text":"ship the landing page"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let text = value["responses"][0]["text"].as_str().unwrap();
    // The mock provider's `mock: ` prefix proves the message went through an
    // openhuman agent turn; the trailing words are the operator message the
    // agent forwarded (the agent prepends a date/time context line).
    // Crucially it is NOT the echo brain's `"You said: …"`.
    assert!(text.starts_with("mock: "), "not an agent reply: {text:?}");
    assert!(
        text.trim_end().ends_with("ship the landing page"),
        "message not forwarded: {text:?}"
    );
    assert_ne!(
        text, "You said: ship the landing page",
        "still routing through the echo brain"
    );
    assert_eq!(value["responses"][0]["channel"], "operator");
}

/// The name a chat turn's row carries.
///
/// Reproduced against a live company before this was written: a turn sent to
/// `#general` recorded `agent_id == chat_id == "main"`. A console that
/// reloaded mid-turn therefore had a durable row, a live turn, and nobody to
/// name — so its re-armed indicator could say only a bare "Working…", which
/// reads exactly like a console that has lost the turn.
#[test]
fn a_chat_turn_records_who_answers_rather_than_the_desk_it_was_sent_to() {
    let record = record_with(desk_manifest());
    assert_eq!(
        chat_turn_responder(&record, "studio"),
        "ceo",
        "a desk's turn is answered by the desk's lead, and that is the name \
         the reload leg has to render"
    );
    assert_ne!(
        chat_turn_responder(&record, "studio"),
        "studio",
        "recording the desk is the regression: it is what made every chat \
         row read `agent_id == chat_id`"
    );
}

/// A DM addresses the teammate directly — its thread id *is* a roster id —
/// so the row names that teammate rather than falling through to the
/// orchestrator.
#[test]
fn a_direct_message_records_the_teammate_it_addresses() {
    let record = record_with(desk_manifest());
    assert_eq!(chat_turn_responder(&record, "eng"), "eng");
}

/// Every spelling of the company's own line folds to one answer, so the
/// indicator does not name a different teammate depending on how the
/// console happened to address General.
#[test]
fn every_general_spelling_records_the_same_answer() {
    let record = record_with(desk_manifest());
    let folded: Vec<String> = ["", "main", "general", "General"]
        .into_iter()
        .map(|spelling| chat_turn_responder(&record, spelling))
        .collect();
    assert!(
        folded.windows(2).all(|pair| pair[0] == pair[1]),
        "the General spellings disagreed about who answers: {folded:?}"
    );
}

/// The floor. A company with nobody to name records the desk — which is
/// precisely what every chat turn recorded before this change, so the worst
/// case is the old behaviour rather than a row naming a teammate that does
/// not exist.
#[test]
fn a_company_with_no_roster_records_the_desk_exactly_as_before() {
    let empty: CompanyManifest =
        toml::from_str("[company]\nname = \"Acme\"\n").expect("a roster-less manifest");
    assert_eq!(chat_turn_responder(&record_with(empty), "studio"), "studio");
}

/// Adding an overlay member persists it and surfaces it in `list_desks` as
/// both an effective member and a removable overlay member.
#[tokio::test]
async fn add_desk_member_persists_and_shows_in_list() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(&home, desk_manifest()).await;
    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");

    let add = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/desks/studio/members")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"agent_id":"eng"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(add.status(), StatusCode::NO_CONTENT);

    let desks = get_desks(&app, &cookie).await;
    assert_eq!(desks[0]["id"], "studio");
    // Manifest member first, overlay member appended.
    assert_eq!(desks[0]["members"][0], "ceo");
    assert_eq!(desks[0]["members"][1], "eng");
    assert_eq!(desks[0]["overlayMembers"][0], "eng");
}

/// Issue #1781 review (Codex P2): an overlay desk whose own id is a
/// General spelling (`general` or `main`) must not appear in `GET
/// .../desks` — `POST .../desks` has refused those ids since issue #1743,
/// so the only way one exists is a company upgraded from before that
/// guard, and `CompanyRecord::resolve_desk_id` already excludes exactly
/// this desk from routing. Listing it anyway would let `buildChannels`
/// (frontend) treat it as the company-wide line and suppress the real
/// built-in `#general` — showing edit/delete controls and a membership
/// list that has nothing to do with where a message actually lands.
///
/// Seeded directly on the stored record, not through `POST .../desks`:
/// that route's own guard means this shape can only be reached by data
/// that predates it, exactly the grandfathered case this proves.
#[tokio::test]
async fn list_desks_hides_an_overlay_desk_shadowing_general() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(&home, desk_manifest()).await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();

    let mut record = runtime.store().load(&id).await.unwrap().unwrap();
    record.overlay_desks.push(OverlayDesk {
        id: "general".to_string(),
        name: "General".to_string(),
        description: None,
        members: vec!["ceo".to_string()],
        responder: ResponderMode::Lead,
        hive: Default::default(),
    });
    record.overlay_desks.push(OverlayDesk {
        id: "main".to_string(),
        name: "Front office".to_string(),
        description: None,
        members: vec!["eng".to_string()],
        responder: ResponderMode::Lead,
        hive: Default::default(),
    });
    runtime.store().save(&record).await.unwrap();

    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");
    let desks = get_desks(&app, &cookie).await;
    let ids: Vec<&str> = desks
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();

    assert!(
        !ids.contains(&"general"),
        "an overlay desk at the reserved `general` id must not be listed: {ids:?}"
    );
    assert!(
        !ids.contains(&"main"),
        "an overlay desk at the reserved `main` id must not be listed: {ids:?}"
    );
    // The manifest desk and a non-shadowing overlay desk are unaffected —
    // this narrows one id, it does not hide desks generally.
    assert!(ids.contains(&"studio"), "unrelated desk dropped: {ids:?}");
}

/// Every desk mutation aimed at a bare General spelling — no legacy
/// overlay row at all — is refused with a reason, under **every** spelling
/// the host folds into the General conversation (issue #1743; restored PR
/// #1781 review, CodeRabbit P2).
///
/// This is the `is_general_channel` guard originally added by `da98130c1`
/// and its own regression test; an unrelated refactor (`3cbdb7a5f`) deleted
/// the guard, the four call sites, and this test together, and only the
/// read-side projection filter (`list_desks`/`resolve_desk_id`) was ever
/// restored (`0c07873db`) — this proves the write side is closed again.
///
/// The point of the assertion is the pair: a `409` **and** the sentence.
/// Before this guard, each of these was a bare `404`/`CompanyNotFound` —
/// "there is no such desk" — which is a different and wrong claim.
/// `#general` is not missing; it is reserved, and the caller needs to be
/// told which.
#[tokio::test]
async fn every_desk_mutation_aimed_at_a_bare_general_spelling_is_refused_with_a_reason() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(&home, desk_manifest()).await;
    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");

    for spelling in ["general", "General", "GENERAL", "main", "Main"] {
        let cases: [(&str, String, &str); 4] = [
            ("DELETE", format!("/api/v1/company/desks/{spelling}"), ""),
            (
                "POST",
                format!("/api/v1/company/desks/{spelling}/members"),
                r#"{"agent_id":"eng"}"#,
            ),
            (
                "DELETE",
                format!("/api/v1/company/desks/{spelling}/members/ceo"),
                "",
            ),
            (
                "PUT",
                format!("/api/v1/company/desks/{spelling}/order"),
                r#"{"ordered_member_ids":["ceo"]}"#,
            ),
        ];
        for (method, uri, body) in cases {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(&uri)
                        .header("cookie", &cookie)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::CONFLICT,
                "{method} {uri} must be refused, not answered 404"
            );
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                text.contains("company-wide channel"),
                "{method} {uri} must say why: got {text}"
            );
        }
    }
}

/// Sibling to [`list_desks_hides_an_overlay_desk_shadowing_general`]: the
/// same grandfathered overlay desk at the reserved `general` id — which
/// that test proves is hidden from `GET .../desks` and unroutable through
/// [`CompanyRecord::resolve_desk_id`] — must also be unreachable through
/// every desk *mutation* (issue #1781 review, CodeRabbit P2). Before this
/// guard was restored, `desk_exists("general")` was `true` for exactly this
/// desk (it really is in `overlay_desks`), so `add_desk_member`,
/// `remove_desk_member`, `set_desk_order`, and `delete_desk` — which
/// checked only `desk_exists` — would staff, reorder, or delete a desk no
/// read surface exposes at all.
///
/// Seeded directly on the stored record, the same way the read-side sibling
/// test is: `POST .../desks` has refused this id since issue #1743, so the
/// only way this shape exists is data that predates that guard.
#[tokio::test]
async fn desk_mutations_refuse_a_grandfathered_overlay_desk_shadowing_general() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(&home, desk_manifest()).await;
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();

    let mut record = runtime.store().load(&id).await.unwrap().unwrap();
    record.overlay_desks.push(OverlayDesk {
        id: "general".to_string(),
        name: "General".to_string(),
        description: None,
        members: vec!["ceo".to_string()],
        responder: ResponderMode::Lead,
        hive: Default::default(),
    });
    runtime.store().save(&record).await.unwrap();

    let app = router(state);
    let cookie = crate::server::test_support::fixed_cookie("acme");

    let cases: [(&str, &str, &str); 4] = [
        ("DELETE", "/api/v1/company/desks/general", ""),
        (
            "POST",
            "/api/v1/company/desks/general/members",
            r#"{"agent_id":"eng"}"#,
        ),
        ("DELETE", "/api/v1/company/desks/general/members/ceo", ""),
        (
            "PUT",
            "/api/v1/company/desks/general/order",
            r#"{"ordered_member_ids":["ceo"]}"#,
        ),
    ];
    for (method, uri, body) in cases {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("cookie", &cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "{method} {uri} must be refused even though the desk really \
             exists in the overlay — desk_exists alone is not enough"
        );
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("company-wide channel"),
            "{method} {uri} must say why: got {text}"
        );
    }
}
