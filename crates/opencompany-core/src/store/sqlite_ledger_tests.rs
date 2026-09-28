use super::tests::store;
use super::*;
use crate::store::conformance;
use futures::StreamExt;

#[tokio::test]
async fn conformance_run_store_workflow_join() {
    conformance::assert_run_store_workflow_join(store()).await;
}

#[tokio::test]
async fn conformance_schedule_fire_store() {
    conformance::assert_schedule_fire_store(store()).await;
}

/// A fire claim written to a file-backed database survives a reopen (issue
/// #241) — the restart durability the whole port exists for.
#[tokio::test]
async fn schedule_fire_claim_survives_reopen() {
    use crate::ports::schedule_fires::ScheduleFireStore;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("company.db");
    let id = CompanyId::new("acme");
    {
        let s = SqliteStore::open(&path).unwrap();
        assert!(s.claim_fire(&id, "workflow-x", 42).await.unwrap());
    }
    // A fresh handle over the same file loses the repeat and reads the anchor.
    let s = SqliteStore::open(&path).unwrap();
    assert!(
        !s.claim_fire(&id, "workflow-x", 42).await.unwrap(),
        "a reopened database must see the earlier claim and lose the repeat"
    );
    assert_eq!(
        s.latest_fire(&id, "workflow-x").await.unwrap(),
        Some(42),
        "the anchor is durable across a reopen"
    );
}

#[tokio::test]
async fn conformance_usage_meter() {
    conformance::assert_usage_meter(store()).await;
}

#[tokio::test]
async fn conformance_usage_retention() {
    conformance::assert_usage_retention(store()).await;
}

#[tokio::test]
async fn conformance_skill_state_store() {
    conformance::assert_skill_state_store(store()).await;
}

#[tokio::test]
async fn conformance_read_state_store() {
    conformance::assert_read_state_store(store()).await;
}

#[tokio::test]
async fn conformance_notification_store() {
    conformance::assert_notification_store(store()).await;
}

#[tokio::test]
async fn conformance_workspace_store() {
    conformance::assert_workspace_store(store()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conformance_workspace_conditional_write() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("conditional.db");
    conformance::assert_workspace_conditional_write(
        Arc::new(SqliteStore::open(&path).unwrap()),
        Arc::new(SqliteStore::open(&path).unwrap()),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conformance_workspace_revision_mutations() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("revisions.db");
    conformance::assert_workspace_revision_mutations(
        Arc::new(SqliteStore::open(&path).unwrap()),
        Arc::new(SqliteStore::open(&path).unwrap()),
    )
    .await;
}

#[tokio::test]
async fn conformance_workspace_binary_store() {
    conformance::assert_workspace_binary_store(store()).await;
}

#[tokio::test]
async fn conformance_workspace_read_capped() {
    conformance::assert_workspace_read_capped(store()).await;
}

/// Issue #759: the folder-claim primitive, including the eight-way
/// contention case an immediate transaction is what decides here.
#[tokio::test]
async fn conformance_workspace_folder_claims() {
    conformance::assert_workspace_folder_claims(store()).await;
}

#[tokio::test]
async fn conformance_workspace_create_rejects_an_absent_or_foreign_parent() {
    conformance::assert_workspace_create_rejects_an_absent_or_foreign_parent(store()).await;
}

#[tokio::test]
async fn conformance_workspace_sibling_names() {
    conformance::assert_workspace_sibling_names(store()).await;
}

/// Issue #1839: the adoption lease — a second claim marks the folder, and
/// `delete_if_empty` refuses it while a minted-unadopted twin still deletes.
/// SQLite inherits the trait-default `delete_if_empty`, so this pins that the
/// flag it persists in `node_json` under the adopt transaction is the flag
/// the default reads back.
#[tokio::test]
async fn conformance_workspace_adoption_lease() {
    conformance::assert_workspace_adoption_lease(store()).await;
}

/// **The race issue #894 is actually about**, and the reason the guard is an
/// `IMMEDIATE` transaction rather than a check in `create`'s caller.
///
/// One `SqliteStore` cannot reproduce it: `conn()` holds a `StdMutex` across
/// the read and the `INSERT`, so two tasks on one store serialize and the
/// old code passes. The reachable shape is **two stores over one file** —
/// the case `apply_pragmas`' `busy_timeout` note and
/// `adopt_or_create_folder`'s header both name. Before the fix both callers
/// read "free" and both inserted; after it the loser waits on the winner's
/// commit, then sees the name taken.
///
/// A barrier makes the overlap deterministic rather than hoping the
/// scheduler interleaves: neither task may enter `create` until both have
/// arrived.
#[tokio::test]
async fn two_stores_racing_one_name_have_one_winner() {
    use crate::ports::workspace::{NodeKind, WorkspaceOrigin, WorkspaceStore};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("opencompany.db");
    let one: Arc<dyn WorkspaceStore> =
        Arc::new(SqliteStore::open(&path).expect("first store over the file"));
    let two: Arc<dyn WorkspaceStore> =
        Arc::new(SqliteStore::open(&path).expect("second store over the SAME file"));

    let company = CompanyId::new("alpha");
    let origin = WorkspaceOrigin::Agent {
        id: "ceo".to_string(),
    };
    let node = |id: &str| crate::ports::workspace::WorkspaceNode {
        id: id.to_string(),
        name: "report.md".to_string(),
        kind: NodeKind::File,
        parent_id: None,
        created_by: origin.clone(),
        updated_by: origin.clone(),
        updated_at_millis: crate::ports::now_millis(),
        mime: None,
        size: None,
        sha256: None,
        adopted: false,
    };

    let gate = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for (store, id) in [(one.clone(), "racer-a"), (two.clone(), "racer-b")] {
        let gate = gate.clone();
        let node = node(id);
        let company = company.clone();
        tasks.push(tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("racer runtime");
            rt.block_on(async move {
                gate.wait().await;
                store.create(&company, &node, Some("payload")).await
            })
        }));
    }
    let mut outcomes = Vec::new();
    for task in tasks {
        outcomes.push(task.await.expect("racer did not panic"));
    }

    let winners = outcomes.iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        winners, 1,
        "exactly one create may take the name, got {outcomes:?}"
    );
    let loser = outcomes
        .iter()
        .find_map(|r| r.as_ref().err())
        .expect("the other create must fail");
    assert!(
        matches!(loser, OpenCompanyError::Conflict(_)),
        "the loser is refused, not broken: {loser:?}"
    );

    // The durable state is the real assertion: a passing error code with two
    // rows in the table would still be the bug.
    let tree = one
        .tree(&CompanyId::new("alpha"))
        .await
        .expect("tree reads");
    let named: Vec<_> = tree.iter().filter(|n| n.name == "report.md").collect();
    assert_eq!(
        named.len(),
        1,
        "one name, one node — a second row is the poisoned path #894 describes: {named:?}"
    );
}

/// Issue #887's no-torn-read contract. This backend passed it before the
/// `fs` fix and passes it after — which is the point of stating it on the
/// port: the guarantee is owed by every backend a tenant can run, not by
/// whichever one happened to have it for free.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conformance_workspace_read_never_tears() {
    conformance::assert_workspace_read_never_tears(store()).await;
}

/// The stat-then-open race fixed in the `fs` backend's `read_capped`.
/// This backend measures and reads under one query and passes on both
/// sides of that fix — the contract is the port's, not one backend's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conformance_workspace_read_capped_race() {
    conformance::assert_workspace_read_capped_race(store()).await;
}

/// A cap above `i64::MAX` must still admit a small body. SQLite binds the
/// comparison as a signed integer, so an unclamped cast wraps negative and
/// withholds every note regardless of size.
#[tokio::test]
async fn read_capped_clamps_oversized_max_bytes() {
    use crate::ports::workspace::{NodeKind, WorkspaceNode, WorkspaceOrigin, WorkspaceStore};

    let store = store();
    let company = CompanyId::new("clamp-co");
    let origin = WorkspaceOrigin::Operator;
    let node = WorkspaceNode {
        id: "clamp-note".to_string(),
        name: "clamp.md".to_string(),
        kind: NodeKind::File,
        parent_id: None,
        updated_at_millis: crate::ports::now_millis(),
        created_by: origin.clone(),
        updated_by: origin,
        mime: None,
        size: None,
        sha256: None,
        adopted: false,
    };
    store
        .create(&company, &node, Some("small body"))
        .await
        .expect("create the note");

    let (_, body, len) = store
        .read_capped(&company, "clamp-note", u64::MAX)
        .await
        .expect("read with an oversized cap")
        .expect("the note exists");
    assert_eq!(
        body, "small body",
        "a cap above i64::MAX must not withhold a body far under it"
    );
    assert_eq!(len, "small body".len() as u64);
}

/// Issue #700's emptiness predicate, against the backend that can actually
/// hold the shape it has to survive.
///
/// The sweep refuses a folder holding an unaddressable child — a name
/// carrying a path separator, which every path-shaped index drops and the
/// port's recursive `delete` would still take (issue #671). Its own module
/// tests pin that on a hand-built node list, because `FsOps` rejects such a
/// name at creation (`reject_unsafe_name`) and cannot produce one.
///
/// This backend does not reject it, and hosted tenants run this backend — so
/// the shape is reachable data rather than a thought experiment, and that is
/// what this asserts: sqlite stores `q/r.md` under `agents/ghost/`, and the
/// sweep leaves `ghost` alone. It lives here because
/// `cargo test --features sqlite --lib store::sqlite` is the only lane that
/// runs it.
#[tokio::test]
async fn workspace_sweep_keeps_a_folder_whose_only_child_has_no_renderable_path() {
    use crate::company::workspace_sweep::sweep_empty_agent_folders;
    use crate::ports::workspace::{NodeKind, WorkspaceNode, WorkspaceOrigin, WorkspaceStore};

    let store = store();
    let company = CompanyId::new("acme");
    let node = |id: &str, name: &str, kind, parent: Option<&str>| WorkspaceNode {
        id: id.to_string(),
        name: name.to_string(),
        kind,
        parent_id: parent.map(str::to_string),
        updated_at_millis: 1,
        created_by: WorkspaceOrigin::Seed,
        updated_by: WorkspaceOrigin::Seed,
        mime: None,
        size: None,
        sha256: None,
        adopted: false,
    };

    WorkspaceStore::create(
        store.as_ref(),
        &company,
        &node("root", "Agents", NodeKind::Folder, None),
        None,
    )
    .await
    .unwrap();
    WorkspaceStore::create(
        store.as_ref(),
        &company,
        &node("ghost", "ceo", NodeKind::Folder, Some("root")),
        None,
    )
    .await
    .unwrap();
    WorkspaceStore::create(
        store.as_ref(),
        &company,
        &node("empty", "cto", NodeKind::Folder, Some("root")),
        None,
    )
    .await
    .unwrap();
    // The name `fs` would have refused. If this create ever starts failing,
    // the premise of the whole guard has changed and this test should say so
    // rather than the guard quietly becoming untested.
    WorkspaceStore::create(
        store.as_ref(),
        &company,
        &node("hidden", "q/r.md", NodeKind::File, Some("ghost")),
        Some("# quarterly"),
    )
    .await
    .expect("sqlite accepts a separator-carrying name — that is why the guard exists");

    let removed = sweep_empty_agent_folders(store.as_ref(), &company, false)
        .await
        .unwrap();

    assert_eq!(
        removed.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        vec!["empty"],
        "only the genuinely empty folder may go"
    );
    let ids: Vec<String> = WorkspaceStore::tree(store.as_ref(), &company)
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert!(
        ids.contains(&"ghost".to_string()) && ids.contains(&"hidden".to_string()),
        "the folder and its unaddressable child must both survive, got {ids:?}"
    );
}

#[tokio::test]
async fn one_store_serves_every_port_through_arc() {
    // A single Arc<SqliteStore> satisfies all five port trait objects — the
    // shape a platform-mode `build_runtime` injects into every `with_*`.
    let s = store();
    let company: Arc<dyn CompanyStore> = s.clone();
    let events: Arc<dyn EventLog> = s.clone();
    let memory: Arc<dyn MemoryStore> = s.clone();
    let context: Arc<dyn ContextStore> = s.clone();
    let secrets: Arc<dyn SecretStore> = s.clone();

    let id = CompanyId::new("acme");
    company
            .save(&CompanyRecord {
                       general_channel: Default::default(),
                       overlay_desk_hive: Vec::new(),
                overlay_retired_agents: Vec::new(),
                id: id.clone(),
                manifest: toml::from_str(
                    "[company]\nname=\"Acme\"\noutput=\"widgets\"\n[[agent]]\nid=\"ceo\"\nrole=\"Chief\"\n[policy]\nmode=\"supervised\"\n",
                )
                .unwrap(),
                ledger: Vec::new(),
                lifecycle: "running".into(),
                overlay_agents: Vec::new(),
                overlay_desk_members: Vec::new(),
                overlay_desk_order: Vec::new(),
                overlay_desks: Vec::new(),
                overlay_workflows: Vec::new(),
                overlay_budgets: Vec::new(),
                overlay_agent_edits: Vec::new(),
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
    events
        .append(
            &id,
            CompanyEvent::OperatorMessage {
                mentions: Vec::new(),
                parent: None,
                text: "hi".into(),
                by: None,
                chat: None,
                deliverable: None,
                attachments: Vec::new(),
            },
        )
        .await
        .unwrap();
    memory
        .save_trace(&id, CompressedTrace::now("c0", "s0"))
        .await
        .unwrap();
    context
        .put(
            &id,
            ContextChunk {
                label: "notes".into(),
                body: "body".into(),
            },
        )
        .await
        .unwrap();
    secrets
        .set(&id, "token", SecretValue("secret".into()))
        .await
        .unwrap();

    assert!(company.load(&id).await.unwrap().is_some());
    assert_eq!(
        events
            .read_from(&id, EventSeq::new(0), 10)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(memory.recent_traces(&id, 10).await.unwrap().len(), 1);
    assert_eq!(context.list(&id, "").await.unwrap().len(), 1);
    assert_eq!(
        secrets.get(&id, "token").await.unwrap(),
        Some(SecretValue("secret".into()))
    );
}

#[tokio::test]
async fn subscribe_delivers_new_event() {
    let s = store();
    let id = CompanyId::new("acme");
    let mut stream = s.subscribe(&id);
    s.append(
        &id,
        CompanyEvent::OperatorMessage {
            mentions: Vec::new(),
            parent: None,
            text: "hi".into(),
            by: None,
            chat: None,
            deliverable: None,
            attachments: Vec::new(),
        },
    )
    .await
    .unwrap();
    let received = stream.next().await.expect("event delivered");
    let EventStreamItem::Event(received) = received else {
        panic!("subscription unexpectedly reported a gap");
    };
    assert_eq!(
        received.event,
        CompanyEvent::OperatorMessage {
            mentions: Vec::new(),
            parent: None,
            text: "hi".into(),
            by: None,
            chat: None,
            deliverable: None,
            attachments: Vec::new(),
        }
    );
}

#[tokio::test]
async fn task_result_upserts_by_id() {
    let s = store();
    let id = CompanyId::new("acme");
    s.save_task_result(
        &id,
        TaskResult {
            task_id: "t1".into(),
            ok: false,
            output: serde_json::json!({"v": 1}),
        },
    )
    .await
    .unwrap();
    // Same id again overwrites rather than duplicating.
    s.save_task_result(
        &id,
        TaskResult {
            task_id: "t1".into(),
            ok: true,
            output: serde_json::json!({"v": 2}),
        },
    )
    .await
    .unwrap();
    let count: i64 = s
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM memory_tasks WHERE company_id = ?1 AND id = ?2",
            params![id.as_ref(), "t1"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn evict_keep_recent_and_older_than() {
    let s = store();
    let id = CompanyId::new("acme");
    for i in 0..5 {
        s.save_trace(&id, CompressedTrace::now(format!("c{i}"), format!("s{i}")))
            .await
            .unwrap();
    }
    let removed = s
        .evict(&id, EvictionPolicy::KeepRecent { n: 2 })
        .await
        .unwrap();
    assert_eq!(removed, 3);
    let kept = s.recent_traces(&id, 10).await.unwrap();
    assert_eq!(kept.len(), 2);
    assert_eq!(kept[1].cycle_id, "c4");

    // A cutoff comfortably in the future evicts every remaining trace.
    let removed = s
        .evict(
            &id,
            EvictionPolicy::OlderThan {
                before_millis: now_millis() + 60_000,
            },
        )
        .await
        .unwrap();
    assert_eq!(removed, 2);
    assert!(s.recent_traces(&id, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn data_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("company.db");
    let id = CompanyId::new("acme");
    {
        let s = SqliteStore::open(&path).unwrap();
        s.append(
            &id,
            CompanyEvent::OperatorMessage {
                mentions: Vec::new(),
                parent: None,
                text: "persist".into(),
                by: None,
                chat: None,
                deliverable: None,
                attachments: Vec::new(),
            },
        )
        .await
        .unwrap();
    }
    // A fresh handle over the same file sees the durable event.
    let s = SqliteStore::open(&path).unwrap();
    let events = s.read_from(&id, EventSeq::new(0), 10).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].event,
        CompanyEvent::OperatorMessage {
            mentions: Vec::new(),
            parent: None,
            text: "persist".into(),
            by: None,
            chat: None,
            deliverable: None,
            attachments: Vec::new(),
        }
    );
}
