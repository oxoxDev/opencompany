use super::tests::{age_blobs_past_the_sweep_threshold, drop_db, store};
use super::*;

/// The boot sweep reclaims a payload whose node document never landed.
///
/// This is the crash the write ordering deliberately allows: blob first,
/// document second, so an interrupted `create_binary` leaves bytes nothing
/// references. Seeded here directly — uploading to the bucket without ever
/// inserting the node — because that is precisely the state a crash between
/// the two writes produces, and it is not reachable through the port.
///
/// The node-backed blob beside it is the half that must be left alone: a
/// sweep that reclaimed live payloads would be far worse than the leak it
/// fixes.
#[tokio::test]
async fn the_boot_sweep_reclaims_orphaned_blobs_and_spares_live_ones() {
    let Some(s) = store().await else { return };
    let company = CompanyId::new("sweep-co");

    // A live binary node, written through the port.
    let node = crate::ports::workspace::WorkspaceNode {
        id: "keep".to_string(),
        name: "keep.png".to_string(),
        kind: crate::ports::workspace::NodeKind::File,
        parent_id: None,
        updated_at_millis: now_millis(),
        created_by: crate::ports::workspace::WorkspaceOrigin::Operator,
        updated_by: crate::ports::workspace::WorkspaceOrigin::Operator,
        mime: Some("image/png".to_string()),
        size: None,
        sha256: None,
        adopted: false,
    };
    crate::ports::workspace::WorkspaceStore::create_binary(&*s, &company, &node, b"live-bytes")
        .await
        .unwrap();

    // …and a dangling one: the bytes of an interrupted create.
    s.put_blob(&company, "vanished", "ghost.png", b"orphan-bytes")
        .await
        .unwrap();
    // A blob with no metadata at all — a shape this store never writes, and
    // therefore unmatchable to any node, so it is an orphan by definition.
    {
        use futures::io::AsyncWriteExt;
        let mut up = s
            .blobs()
            .open_upload_stream("nometa.bin")
            .await
            .expect("upload");
        up.write_all(b"no-metadata").await.unwrap();
        up.close().await.unwrap();
    }

    let before = s.blobs().find(doc! {}).await.unwrap();
    assert_eq!(
        before.try_collect::<Vec<_>>().await.unwrap().len(),
        3,
        "two orphans and one live payload are staged"
    );

    // The orphans have to be *old* to be orphans (issue #664). Staged
    // seconds ago they are indistinguishable from a peer's in-flight
    // upload, and the sweep now spares them for that reason — see
    // `a_recent_orphan_is_left_alone_because_it_may_be_an_in_flight_upload`.
    age_blobs_past_the_sweep_threshold(&s).await;

    // Constructing a store over the same database runs the sweep.
    let rebooted = MongoStore::from_database(s.db.clone()).await.unwrap();

    let files = rebooted
        .blobs()
        .find(doc! {})
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert_eq!(files.len(), 1, "both orphans are reclaimed");
    assert_eq!(files[0].filename.as_deref(), Some("keep.png"));

    // The live node still serves its bytes — the sweep did not touch it.
    let (_, stream) =
        crate::ports::workspace::WorkspaceStore::read_bytes(&rebooted, &company, "keep")
            .await
            .unwrap()
            .expect("the live payload survives the sweep");
    let mut got = Vec::new();
    {
        use futures::StreamExt;
        let mut stream = stream;
        while let Some(chunk) = stream.next().await {
            got.extend_from_slice(&chunk.unwrap());
        }
    }
    assert_eq!(got, b"live-bytes".to_vec());

    drop_db(&s).await;
}

/// Issue #664: the boot sweep must not delete a concurrent writer's upload.
///
/// The state staged here is not a crash — it is the perfectly ordinary
/// instant *inside* a healthy `create_binary`, after `put_blob` has
/// returned and before the node insert lands. A second process booting then
/// (a rolling deploy, a restarted replica, another tenant's container on a
/// shared database) sees a blob with no node and, before this fix, reclaimed
/// it. The writer's insert would then land on top, leaving a node whose
/// download 404s forever and whose `size` still counts against the quota —
/// unreclaimable, because the sweep only deletes blobs without nodes and
/// never nodes without blobs.
///
/// Deliberately *not* backdated: recency is the whole signal.
#[tokio::test]
async fn a_recent_orphan_is_left_alone_because_it_may_be_an_in_flight_upload() {
    let Some(s) = store().await else { return };
    let company = CompanyId::new("inflight-co");

    // Exactly what a concurrent `create_binary` has written so far.
    s.put_blob(&company, "arriving", "arriving.png", b"in-flight-bytes")
        .await
        .unwrap();

    // A second process boots against the same database and sweeps.
    let rebooted = MongoStore::from_database(s.db.clone()).await.unwrap();

    let files = rebooted
        .blobs()
        .find(doc! {})
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert_eq!(
        files.len(),
        1,
        "a blob younger than the threshold is an upload in progress, not an orphan"
    );
    assert_eq!(files[0].filename.as_deref(), Some("arriving.png"));

    // And the writer's insert, landing after the sweep, yields a node whose
    // bytes are actually there — the whole point.
    let node = crate::ports::workspace::WorkspaceNode {
        id: "arriving".to_string(),
        name: "arriving.png".to_string(),
        kind: crate::ports::workspace::NodeKind::File,
        parent_id: None,
        updated_at_millis: now_millis(),
        created_by: crate::ports::workspace::WorkspaceOrigin::Operator,
        updated_by: crate::ports::workspace::WorkspaceOrigin::Operator,
        mime: Some("image/png".to_string()),
        size: None,
        sha256: None,
        adopted: false,
    };
    rebooted
        .collection("workspace_nodes")
        .insert_one(doc! {
            "company_id": company.as_ref(),
            "node_id": &node.id,
            "node_json": serde_json::to_string(&node).unwrap(),
            "content": "",
            "updated_ms": node.updated_at_millis as i64,
        })
        .await
        .unwrap();

    let (_, stream) =
        crate::ports::workspace::WorkspaceStore::read_bytes(&rebooted, &company, "arriving")
            .await
            .unwrap()
            .expect("the upload that was in flight during the sweep still has its bytes");
    let mut got = Vec::new();
    {
        use futures::StreamExt;
        let mut stream = stream;
        while let Some(chunk) = stream.next().await {
            got.extend_from_slice(&chunk.unwrap());
        }
    }
    assert_eq!(got, b"in-flight-bytes".to_vec());

    drop_db(&s).await;
}

/// A `create_binary` name conflict must not strand the payload it just
/// uploaded.
///
/// This backend writes blob-first (issue #894), so a sibling-name collision
/// is detected only when the node-document insert fails — by which time the
/// bytes are already in GridFS. Before the conflict path reclaimed them, the
/// error returned with that blob still present: no node document referenced
/// it, and the boot sweep (which runs only at store construction, and only
/// for blobs older than an hour) was the sole reclaim path. The
/// chat-attachment flow reaches this branch every time a repeated filename
/// is disambiguated and retried, so a long-lived tenant would accumulate
/// invisible GridFS copies. The conflict path must own the payload it
/// uploaded before the caller learns of the conflict.
#[tokio::test]
async fn a_name_conflict_reclaims_the_blob_it_just_uploaded() {
    let Some(s) = store().await else { return };
    let company = CompanyId::new("conflict-co");

    let first = crate::ports::workspace::WorkspaceNode {
        id: "winner".to_string(),
        name: "image.png".to_string(),
        kind: crate::ports::workspace::NodeKind::File,
        parent_id: None,
        updated_at_millis: now_millis(),
        created_by: crate::ports::workspace::WorkspaceOrigin::Operator,
        updated_by: crate::ports::workspace::WorkspaceOrigin::Operator,
        mime: Some("image/png".to_string()),
        size: None,
        sha256: None,
        adopted: false,
    };
    crate::ports::workspace::WorkspaceStore::create_binary(&*s, &company, &first, b"first")
        .await
        .unwrap();

    let loser = crate::ports::workspace::WorkspaceNode {
        id: "loser".to_string(),
        ..first.clone()
    };
    let err =
        crate::ports::workspace::WorkspaceStore::create_binary(&*s, &company, &loser, b"second")
            .await
            .unwrap_err();
    assert!(
        matches!(err, crate::error::OpenCompanyError::Conflict(_)),
        "a taken sibling name is a Conflict, not a storage fault: {err:?}"
    );

    // The loser's payload must not survive the conflict as an orphan.
    let files = s
        .blobs()
        .find(MongoStore::blob_filter(&company, "loser"))
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert_eq!(
        files.len(),
        0,
        "the conflicted upload leaves no orphan blob for the sweep to find later"
    );

    // The winner still serves its bytes.
    let (_, stream) = crate::ports::workspace::WorkspaceStore::read_bytes(&*s, &company, "winner")
        .await
        .unwrap()
        .expect("the winner's payload is untouched by the refusal");
    let mut got = Vec::new();
    {
        use futures::StreamExt;
        let mut stream = stream;
        while let Some(chunk) = stream.next().await {
            got.extend_from_slice(&chunk.unwrap());
        }
    }
    assert_eq!(got, b"first".to_vec());

    drop_db(&s).await;
}

/// A same-id `create_binary` race must not reclaim the winning payload.
///
/// Two callers racing the same fresh node id both pass the `contains_key`
/// pre-check (neither document is visible when the reads land), both upload
/// a GridFS payload under that id, and the node-document insert then loses
/// on the unique `(company_id, node_id)` index for exactly one of them.
/// The loser's conflict cleanup used to sweep *every* blob for the id —
/// including the winner's, the bytes its live node now points at — leaving
/// the download irrecoverable on hosted MongoDB deployments. The cleanup
/// must name and delete only the upload this losing call made.
///
/// Spawned rather than staged: the whole point is the interleaving, and the
/// runtime guarantees it here — each racer awaits a database read before
/// either inserts, so both `contains_key` checks necessarily see the empty
/// tree regardless of which document insert finally wins.
#[tokio::test]
async fn a_same_id_race_preserves_the_winning_payload() {
    let Some(s) = store().await else { return };
    let company = CompanyId::new("dupe-race-co");

    let node = crate::ports::workspace::WorkspaceNode {
        id: "dupe".to_string(),
        name: "dupe.png".to_string(),
        kind: crate::ports::workspace::NodeKind::File,
        parent_id: None,
        updated_at_millis: now_millis(),
        created_by: crate::ports::workspace::WorkspaceOrigin::Operator,
        updated_by: crate::ports::workspace::WorkspaceOrigin::Operator,
        mime: Some("image/png".to_string()),
        size: None,
        sha256: None,
        adopted: false,
    };

    let racer_a = {
        let s = s.clone();
        let company = company.clone();
        let node = node.clone();
        tokio::spawn(async move {
            crate::ports::workspace::WorkspaceStore::create_binary(
                &*s,
                &company,
                &node,
                b"payload-a",
            )
            .await
        })
    };
    let racer_b = {
        let s = s.clone();
        let company = company.clone();
        let node = node.clone();
        tokio::spawn(async move {
            crate::ports::workspace::WorkspaceStore::create_binary(
                &*s,
                &company,
                &node,
                b"payload-b",
            )
            .await
        })
    };

    let (outcome_a, outcome_b) = (racer_a.await.unwrap(), racer_b.await.unwrap());
    assert_eq!(
        outcome_a.is_ok() as u8 + outcome_b.is_ok() as u8,
        1,
        "exactly one same-id caller wins the insert; the other must be a Conflict"
    );

    // The survivor's node still serves its bytes: the loser's cleanup
    // deleted only its own upload, never the winner's.
    let (_, stream) = crate::ports::workspace::WorkspaceStore::read_bytes(&*s, &company, "dupe")
        .await
        .unwrap()
        .expect("the winning payload survives the same-id race");
    let mut got = Vec::new();
    {
        use futures::StreamExt;
        let mut stream = stream;
        while let Some(chunk) = stream.next().await {
            got.extend_from_slice(&chunk.unwrap());
        }
    }
    assert!(
        got == b"payload-a" || got == b"payload-b",
        "the surviving payload is exactly one racer's, not a mix: {got:?}"
    );

    // And exactly one blob remains for the id — the losing upload is gone,
    // the winner's is untouched.
    let files = s
        .blobs()
        .find(MongoStore::blob_filter(&company, "dupe"))
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert_eq!(
        files.len(),
        1,
        "the losing upload is reclaimed without touching the winner's"
    );

    drop_db(&s).await;
}

/// Issue #1077: the orphan report composes `list()` and `owners()`
/// correctly against a real server.
///
/// The pure set difference is unit-tested in `app::orphans`. What only a
/// live backend can prove is that the two reads are *comparable* — that
/// `owners()` keys on the same id string `list()` returns. They do
/// (`company_id` in both collections), but nothing in the type system says
/// so: both sides are `CompanyId`, and if one had been namespaced and the
/// other bare, the report would have called every company on the platform
/// an orphan while still type-checking and still passing every unit test.
///
/// Namespaced ids specifically, because that is the only mode in which the
/// `owners` collection is load-bearing at all.
#[tokio::test]
async fn orphaned_companies_are_found_through_the_real_ports() {
    let Some(s) = store().await else { return };

    let manifest: CompanyManifest = toml::from_str("[company]\nname = \"Acme\"\n").unwrap();
    let owned = crate::app::namespace_company_id("tenant-a", CompanyId::new("owned"));
    let orphan = crate::app::namespace_company_id("tenant-a", CompanyId::new("orphan"));

    for id in [&owned, &orphan] {
        let record = CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            id: id.clone(),
            manifest: manifest.clone(),
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
        };
        s.save(&record).await.expect("save company");
    }
    // Only one of the two gets an owner row. The other is exactly the state
    // a failed `set_owner` used to leave behind before #1050 was fixed.
    s.set_owner(&owned, "tenant-a").await.expect("record owner");
    // ...plus a row naming a company that was never saved, which is the
    // benign direction #1073 deliberately prefers on a rolled-back provision.
    let ghost = crate::app::namespace_company_id("tenant-a", CompanyId::new("ghost"));
    s.set_owner(&ghost, "tenant-a").await.expect("record ghost");

    let companies = CompanyStore::list(s.as_ref()).await.expect("list");
    let owners = s.owners().await.expect("owners");
    let report = crate::app::find_orphans(&companies, &owners);

    let unowned: Vec<&str> = report.unowned.iter().map(|c| c.id.as_ref()).collect();
    assert!(
        unowned.contains(&orphan.as_ref()),
        "the company with no owner row must be reported: {report:?}"
    );
    assert!(
        !unowned.contains(&owned.as_ref()),
        "the company WITH an owner row must not be: {report:?}"
    );
    let dangling: Vec<&str> = report.dangling.iter().map(|r| r.id.as_ref()).collect();
    assert!(
        dangling.contains(&ghost.as_ref()),
        "the owner row naming no company must be reported: {report:?}"
    );
    assert!(
        !dangling.contains(&owned.as_ref()),
        "a row whose company exists must not be: {report:?}"
    );

    drop_db(&s).await;
}

/// Shared-single-DB namespacing: two tenants registering the same template
/// name land distinct namespaced ids in one database, so the `companies`
/// unique index never conflicts, and the `owners` rows carry the right
/// tenant for each. Mirrors what the workload does when
/// `OPENCOMPANY_TENANT_ID` is set (see `AppConfig::namespaced_company_id`).
#[tokio::test]
async fn shared_db_namespaced_companies_do_not_conflict() {
    let Some(s) = store().await else { return };

    let manifest: CompanyManifest = toml::from_str("[company]\nname = \"Acme\"\n").unwrap();
    let id_a = crate::app::namespace_company_id(
        "tenant-a",
        crate::runtime::company_id_from_name(&manifest.company.name),
    );
    let id_b = crate::app::namespace_company_id(
        "tenant-b",
        crate::runtime::company_id_from_name(&manifest.company.name),
    );
    assert_eq!(id_a.as_ref(), "tenant-a--acme");
    assert_eq!(id_b.as_ref(), "tenant-b--acme");

    for (id, tenant) in [(&id_a, "tenant-a"), (&id_b, "tenant-b")] {
        let record = CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            id: id.clone(),
            manifest: manifest.clone(),
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
        };
        // Same template name under two tenants: distinct namespaced ids, no
        // `companies` unique-index conflict.
        s.save(&record).await.expect("save namespaced company");
        s.set_owner(id, tenant).await.expect("record owner");
    }

    let mut owners = s.owners().await.unwrap();
    owners.sort_by(|a, b| a.0.as_ref().cmp(b.0.as_ref()));
    assert_eq!(
        owners,
        vec![
            (id_a.clone(), "tenant-a".to_string()),
            (id_b.clone(), "tenant-b".to_string()),
        ]
    );

    // Both companies remain addressable and carry the shared template name.
    assert_eq!(
        s.load(&id_a).await.unwrap().unwrap().manifest.company.name,
        "Acme"
    );
    assert_eq!(
        s.load(&id_b).await.unwrap().unwrap().manifest.company.name,
        "Acme"
    );

    drop_db(&s).await;
}
