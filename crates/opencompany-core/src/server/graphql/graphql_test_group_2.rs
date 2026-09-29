use crate::ports::tasks::TaskTitle;
use crate::ports::types::CompanyId;
use crate::server::router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::graphql_test_group_1::query;
use super::graphql_test_support_1::*;

/// Issue #246 + #65: the card a reply opened is projected on **both** history
/// surfaces, from the one shared `MessageView` field. The console reads REST,
/// but GraphQL is the paginated surface, and #65 exists precisely because the
/// two drifting apart is how a transcript ends up meaning different things
/// depending on which door you came in.
#[tokio::test]
async fn chat_history_projects_the_card_a_reply_opened() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_rich_company(&home).await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    // The card has to actually be on the board: the history projection
    // reports `taskId` only for a card that still exists, so that a chip
    // cannot come back pointing at a card someone deleted (issue #984).
    runtime
        .tasks()
        .upsert(
            runtime.id(),
            &crate::ports::tasks::TaskRecord {
                id: "t-77".to_string(),
                title: TaskTitle::authored("Draft the launch note"),
                note: None,
                column: crate::ports::tasks::COLUMN_TODO.to_string(),
                priority: "medium".to_string(),
                assignee: String::new(),
                updated_at_millis: 1,
                origin: None,
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

    for (text, task_id) in [
        ("opened a card", Some("t-77".to_string())),
        ("opened nothing", None),
    ] {
        runtime
            .events()
            .append(
                runtime.id(),
                crate::ports::types::CompanyEvent::AgentReply {
                    audience: Vec::new(),
                    mentions: Vec::new(),
                    mention_depth: 0,
                    parent: None,
                    task_id,
                    outputs: Vec::new(),
                    chat_id: "General".to_string(),
                    agent_id: "maya".to_string(),
                    text: text.to_string(),
                    steps: Vec::new(),
                    episode: None,
                },
            )
            .await
            .unwrap();
    }

    let app = router(state);
    let value = query(
        app,
        r#"{"query":"{ company(id:\"acme\"){ chat(id:\"general\"){ history(first: 10) { items { text taskId } } } } }"}"#,
    )
    .await;
    let items = value["data"]["company"]["chat"]["history"]["items"]
        .as_array()
        .unwrap();
    let opened = items
        .iter()
        .find(|m| m["text"] == "opened a card")
        .expect("the card-opening reply is in history");
    assert_eq!(opened["taskId"], "t-77");
    let plain = items
        .iter()
        .find(|m| m["text"] == "opened nothing")
        .expect("the ordinary reply is in history");
    assert!(
        plain["taskId"].is_null(),
        "an ordinary reply carries no card: {plain}"
    );
}

/// Issue #364 + #65: a thread parent and a message's reactions are projected on
/// **both** history surfaces, from the one shared `MessageView`.
///
/// The same parity rule #246 is held to one test up. A console that hydrates a
/// transcript over GraphQL must see the same threads and the same reactions the
/// REST route returns, or the two doors show different conversations.
#[tokio::test]
async fn chat_history_projects_threads_and_reactions() {
    use crate::ports::types::CompanyEvent;

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_rich_company(&home).await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    let root = runtime
        .events()
        .append(
            runtime.id(),
            CompanyEvent::AgentReply {
                audience: Vec::new(),
                mentions: Vec::new(),
                mention_depth: 0,
                parent: None,
                task_id: None,
                outputs: Vec::new(),
                chat_id: "General".to_string(),
                agent_id: "maya".to_string(),
                text: "the root".to_string(),
                steps: Vec::new(),
                episode: None,
            },
        )
        .await
        .unwrap();
    runtime
        .events()
        .append(
            runtime.id(),
            CompanyEvent::AgentReply {
                audience: Vec::new(),
                mentions: Vec::new(),
                mention_depth: 0,
                parent: Some(root),
                task_id: None,
                outputs: Vec::new(),
                chat_id: "General".to_string(),
                agent_id: "maya".to_string(),
                text: "in the thread".to_string(),
                steps: Vec::new(),
                episode: None,
            },
        )
        .await
        .unwrap();
    runtime
        .events()
        .append(
            runtime.id(),
            CompanyEvent::ReactionToggled {
                message_seq: root,
                emoji: "👍".to_string(),
                on: true,
                by: None,
            },
        )
        .await
        .unwrap();

    let app = router(state);
    let value = query(
        app,
        r#"{"query":"{ company(id:\"acme\"){ chat(id:\"general\"){ history(first: 10) { items { id text parentId reactions { emoji by mine } } } } } }"}"#,
    )
    .await;
    let items = value["data"]["company"]["chat"]["history"]["items"]
        .as_array()
        .unwrap();
    let root_id = root.value().to_string();
    let parent = items
        .iter()
        .find(|m| m["text"] == "the root")
        .expect("the root is in history");
    assert!(parent["parentId"].is_null(), "the root is not a reply");
    assert_eq!(parent["reactions"][0]["emoji"], "👍");
    assert_eq!(parent["reactions"][0]["by"], "operator");
    let threaded = items
        .iter()
        .find(|m| m["text"] == "in the thread")
        .expect("the threaded reply is in history");
    assert_eq!(threaded["parentId"], root_id);
    assert_eq!(
        threaded["reactions"].as_array().unwrap().len(),
        0,
        "an un-reacted message carries no rows: {threaded}"
    );
}

/// Issue #1682 + #65: an operator message's attachments project on the GraphQL
/// history surface with the same store-authored metadata the REST route
/// returns, from the one shared `MessageView`. The console downloads over REST,
/// but a transcript hydrated through either door must name the same files —
/// the drift #65 exists to prevent.
#[tokio::test]
async fn chat_history_projects_attachments() {
    use crate::ports::types::{Attachment, CompanyEvent};

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_rich_company(&home).await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    runtime
        .events()
        .append(
            runtime.id(),
            CompanyEvent::OperatorMessage {
                text: "here is the file".to_string(),
                by: None,
                chat: Some("General".to_string()),
                parent: None,
                deliverable: None,
                mentions: Vec::new(),
                attachments: vec![Attachment {
                    node_id: "node-42".to_string(),
                    name: "diagram.png".to_string(),
                    mime: "image/png".to_string(),
                    size: 2048,
                    extracted_text: None,
                }],
            },
        )
        .await
        .unwrap();

    let app = router(state);
    let value = query(
        app,
        r#"{"query":"{ company(id:\"acme\"){ chat(id:\"general\"){ history(first: 10) { items { text attachments { nodeId name mime size } } } } } }"}"#,
    )
    .await;
    let items = value["data"]["company"]["chat"]["history"]["items"]
        .as_array()
        .unwrap();
    let msg = items
        .iter()
        .find(|m| m["text"] == "here is the file")
        .expect("the operator message is in history");
    let attachments = msg["attachments"].as_array().expect("attachments project");
    assert_eq!(attachments.len(), 1, "exactly one attachment: {msg}");
    assert_eq!(attachments[0]["nodeId"], "node-42");
    assert_eq!(attachments[0]["name"], "diagram.png");
    assert_eq!(attachments[0]["mime"], "image/png");
    assert_eq!(attachments[0]["size"], 2048.0);
}

#[tokio::test]
async fn connections_reflect_manifest_intent_disconnected() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let app = router(state_with_rich_company(&home).await);
    let value = query(
        app,
        r#"{"query":"{ company(id:\"acme\"){ connections { provider connected reason } } }"}"#,
    )
    .await;
    let conns = value["data"]["company"]["connections"].as_array().unwrap();
    assert_eq!(conns.len(), 1);
    assert_eq!(conns[0]["provider"], "slack");
    assert_eq!(conns[0]["connected"], false);
    assert_eq!(conns[0]["reason"], "Post updates.");
}

/// The two connection projections are one shape: whatever credential tier the
/// REST route reports for a provider, the GraphQL resolver must report the same
/// (issue #319). They share `connect_route_from_env`, and this pins that they
/// keep sharing it — a second copy of the resolution order is a second chance to
/// tell the console a hosted instance can run a local Connect.
#[tokio::test]
async fn rest_and_graphql_agree_on_the_connection_credential_source() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_rich_company(&home).await;

    let value = query(
        router(state.clone()),
        r#"{"query":"{ company(id:\"acme\"){ connections { provider credentialSource } } }"}"#,
    )
    .await;
    let gql: Vec<(String, String)> = value["data"]["company"]["connections"]
        .as_array()
        .expect("connections")
        .iter()
        .map(|row| {
            (
                row["provider"].as_str().unwrap().to_string(),
                row["credentialSource"]
                    .as_str()
                    .expect("every GraphQL row carries a credentialSource")
                    .to_string(),
            )
        })
        .collect();
    assert!(!gql.is_empty(), "expected at least one connection: {value}");

    let response = router(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/company/connections")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let rest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let rest_rows: Vec<(String, String)> = rest
        .as_array()
        .expect("array")
        .iter()
        .map(|row| {
            (
                row["provider"].as_str().unwrap().to_string(),
                row["credentialSource"]
                    .as_str()
                    .expect("every REST row carries a credentialSource")
                    .to_string(),
            )
        })
        .collect();

    assert_eq!(
        rest_rows, gql,
        "REST and GraphQL disagree on the connection credential source"
    );
}

#[tokio::test]
async fn tasks_page_reflects_upserts_and_column_filter() {
    use crate::ports::tasks::TaskRecord;
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_rich_company(&home).await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    runtime
        .tasks()
        .upsert(
            runtime.id(),
            &TaskRecord {
                id: "t1".into(),
                title: TaskTitle::authored("Launch"),
                note: None,
                column: "todo".into(),
                priority: "high".into(),
                assignee: "maya".into(),
                updated_at_millis: 1_700_000_000_000,
                origin: None,
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
    let value = query(
        app.clone(),
        r#"{"query":"{ company(id:\"acme\"){ tasks(column:\"todo\"){ total items { id title column } } } }"}"#,
    )
    .await;
    assert_eq!(value["data"]["company"]["tasks"]["total"], 1);
    assert_eq!(value["data"]["company"]["tasks"]["items"][0]["id"], "t1");

    // A different column filters it out.
    let none = query(
        app,
        r#"{"query":"{ company(id:\"acme\"){ tasks(column:\"done\"){ total } } }"}"#,
    )
    .await;
    assert_eq!(none["data"]["company"]["tasks"]["total"], 0);
}

#[tokio::test]
async fn memory_page_reflects_upserts() {
    use crate::ports::facts::{FactKind, FactRecord};
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_rich_company(&home).await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    runtime
        .facts()
        .upsert(
            runtime.id(),
            &FactRecord {
                id: "f1".into(),
                kind: FactKind::Preference,
                title: "Tone".into(),
                body: "Friendly.".into(),
                source: "general".into(),
                updated_at_millis: 1_700_000_000_000,
            },
        )
        .await
        .unwrap();
    let app = router(state);
    let value = query(
        app,
        r#"{"query":"{ company(id:\"acme\"){ memory(kind: PREFERENCE){ total items { id kind title updatedAt } } } }"}"#,
    )
    .await;
    assert_eq!(value["data"]["company"]["memory"]["total"], 1);
    assert_eq!(
        value["data"]["company"]["memory"]["items"][0]["kind"],
        "PREFERENCE"
    );
    assert!(
        value["data"]["company"]["memory"]["items"][0]["updatedAt"]
            .as_str()
            .unwrap()
            .starts_with("2023-")
    );
}

/// An unpopulated surface resolves to `[]`, never to `null` or an error.
///
/// `workspaceTree` is the exception and states why: since issue #551 a company
/// is never born with an empty tree — boot scaffolds the reserved `agents/`
/// root (and, until issue #645, an empty `desks/` beside it) — so what it
/// proves here is that the resolver answers with exactly that and invents
/// nothing else. A member folder is *not* part of that baseline; this mints one
/// to pin the authorship projection (#326), which is the only place in the
/// GraphQL surface where `WorkspaceOrigin` is rendered with an agent id.
#[tokio::test]
async fn empty_surfaces_resolve_to_empty_lists() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_rich_company(&home).await;

    let id = CompanyId::new("acme");
    let workspace = state.registry().get(&id).unwrap().workspace().clone();
    // Nothing is inside the roots until somebody produces something; standing
    // in for that producer is what makes the `agent` projection assertable.
    crate::company::workspace_scaffold::ensure_agent_folder(workspace.as_ref(), &id, "maya")
        .await
        .unwrap();

    let value = query(
        router(state),
        r#"{"query":"{ company(id:\"acme\"){ workspaceTree { name createdBy { kind agentId } } inboxes { key } skills { id } workflows { id } } }"}"#,
    )
    .await;
    let company = &value["data"]["company"];
    let tree = company["workspaceTree"].as_array().unwrap();
    let mut names: Vec<&str> = tree
        .iter()
        .map(|node| node["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "agents",
            "artifacts",
            "maya",
            "readme.md",
            "readme.md",
            "secrets"
        ]
    );
    let root = tree
        .iter()
        .find(|node| node["name"] == serde_json::json!("agents"))
        .unwrap();
    assert_eq!(root["createdBy"]["kind"], "seed");
    assert!(root["createdBy"]["agentId"].is_null());
    let folder = tree
        .iter()
        .find(|node| node["name"] == serde_json::json!("maya"))
        .unwrap();
    assert_eq!(folder["createdBy"]["kind"], "agent");
    assert_eq!(folder["createdBy"]["agentId"], "maya");
    assert_eq!(company["inboxes"].as_array().unwrap().len(), 0);
    assert_eq!(own_skills(&company["skills"]).len(), 0);
    // The global baseline is listed in every company, so "empty" here means the
    // company has no graphs of its own.
    assert_eq!(own_workflows(&company["workflows"]).len(), 0);
}

#[tokio::test]
async fn smtp_status_is_unconfigured_without_credentials() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let app = router(state_with_rich_company(&home).await);
    let value = query(
        app,
        r#"{"query":"{ company(id:\"acme\"){ smtp { host port configured } domain { domain } } }"}"#,
    )
    .await;
    assert_eq!(value["data"]["company"]["smtp"]["configured"], false);
    assert_eq!(value["data"]["company"]["smtp"]["host"], "");
    assert!(value["data"]["company"]["domain"].is_null());
}

#[tokio::test]
async fn usage_is_empty_without_samples() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let app = router(state_with_rich_company(&home).await);
    let value = query(
        app,
        r#"{"query":"{ company(id:\"acme\"){ usage(range: D7){ totals { tokens costUsd connections } series { date } } } }"}"#,
    )
    .await;
    let usage = &value["data"]["company"]["usage"];
    assert_eq!(usage["totals"]["tokens"], 0.0);
    assert_eq!(usage["totals"]["connections"], 0);
    // D7 still yields a zero-filled 7-day series.
    assert_eq!(usage["series"].as_array().unwrap().len(), 7);
}

#[tokio::test]
async fn usage_reflects_recorded_samples() {
    use crate::ports::usage::{SampleKind, UsageSample};
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_rich_company(&home).await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    let now = super::now_millis();
    runtime
        .usage()
        .record(
            runtime.id(),
            &UsageSample {
                at_millis: now,
                agent: "maya".into(),
                provider: "managed".into(),
                input_tokens: 100,
                output_tokens: 40,
                cached_input_tokens: 0,
                cost_usd: 0.5,
                kind: SampleKind::Inference,
                run_id: None,
                model: None,
            },
        )
        .await
        .unwrap();
    let app = router(state);
    let value = query(
        app,
        r#"{"query":"{ company(id:\"acme\"){ usage(range: D30){ totals { inputTokens tokens costUsd } byAgent { name tokens } } } }"}"#,
    )
    .await;
    let usage = &value["data"]["company"]["usage"];
    assert_eq!(usage["totals"]["inputTokens"], 100.0);
    assert_eq!(usage["totals"]["tokens"], 140.0);
    assert_eq!(usage["totals"]["costUsd"], 0.5);
    assert_eq!(usage["byAgent"][0]["tokens"], 140.0);
}

#[tokio::test]
async fn finances_fold_the_ledger() {
    use crate::ports::types::LedgerEntry;
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_rich_company(&home).await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    let now = super::now_millis();
    runtime
        .store()
        .append_ledger(
            runtime.id(),
            LedgerEntry {
                at_millis: now,
                kind: "inference.spend".into(),
                amount_usd: -2.0,
                memo: "tokens".into(),
            },
        )
        .await
        .unwrap();
    runtime
        .store()
        .append_ledger(
            runtime.id(),
            LedgerEntry {
                at_millis: now,
                kind: "payment.received".into(),
                amount_usd: 10.0,
                memo: "invoice".into(),
            },
        )
        .await
        .unwrap();
    let app = router(state);
    let value = query(
        app,
        r#"{"query":"{ company(id:\"acme\"){ finances { spentUsd revenueUsd netUsd transactions { id direction amountUsd } byCategory { category amount } } } }"}"#,
    )
    .await;
    let fin = &value["data"]["company"]["finances"];
    assert_eq!(fin["spentUsd"], 2.0);
    assert_eq!(fin["revenueUsd"], 10.0);
    assert_eq!(fin["netUsd"], 8.0);
    assert_eq!(fin["transactions"].as_array().unwrap().len(), 2);
}
