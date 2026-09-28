#[cfg(feature = "export")]
use super::tests::manifest;
use super::tests::{admin_actor, budget_manifest, company_record, fs_ports, tmp_root};
use super::*;
use crate::ports::types::{Actor, ActorKind};
#[cfg(feature = "export")]
use crate::runtime::RuntimeBuilder;

/// **A console tool grant must not be promoted to a seed grant by a
/// round-trip** (issue #1796).
///
/// `write_to_dir` serializes the bundle's manifest straight into
/// `company.toml`, and that file becomes the SEED for whatever host serves
/// the restored company. The record's manifest is materialised
/// seed-plus-grants, so carrying it verbatim would write a seed that already
/// grants `chargebee` — the next rebuild's carry rule would correctly read
/// that as "version control spoke", drop the override, and the operator's
/// attributed grant would have become a manifest grant that
/// `DELETE …/tools/grants` can never reach again.
///
/// So the bundle carries the seed and the override separately, and the
/// restored record is re-folded. Both halves are asserted: the `company.toml`
/// on disk must NOT name the namespace, and the imported record must.
#[tokio::test]
async fn a_console_tool_grant_survives_a_roundtrip_without_becoming_a_seed_grant() {
    let home1 = tmp_root("grants-src");
    let home2 = tmp_root("grants-dst");
    let dest = tmp_root("grants-bundle");
    let id = CompanyId::new("grants-co");

    let manifest: CompanyManifest = toml::from_str(
        r#"
            [company]
            name = "Grants Co"
            output = "widgets"

            [[agent]]
            id = "ceo"
            role = "Chief"

            [tools]
            allow = ["*", "search"]
        "#,
    )
    .expect("valid manifest");

    let held = ToolGrantsOverride {
        added: vec!["chargebee".to_string()],
        set_by: admin_actor(),
        at_millis: 1_700_000_000_003,
    };

    // The record exactly as `PUT …/tools/grants` leaves it: the override
    // stored, and the grant folded into the manifest every reader consults.
    let mut folded = manifest.clone();
    folded.tools.allow.push("chargebee".to_string());

    let (s1, e1, m1, c1) = fs_ports(&home1);
    s1.save(&CompanyRecord {
        general_channel: Default::default(),
        id: id.clone(),
        manifest: folded,
        ledger: Vec::new(),
        lifecycle: "running".into(),
        overlay_agents: Vec::new(),
        overlay_desk_members: Vec::new(),
        overlay_desk_order: Vec::new(),
        overlay_desks: Vec::new(),
        overlay_workflows: Vec::new(),
        overlay_budgets: Vec::new(),
        overlay_policy: None,
        overlay_tool_grants: Some(held.clone()),
        overlay_desk_tools: Default::default(),
        disabled_workflows: Vec::new(),
        overlay_agent_edits: Vec::new(),
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        template_provenance: None,
        setup: None,
        name_confirmed: false,
        activation_completed_at: None,
        created_at_millis: None,
    })
    .await
    .unwrap();

    export_bundle(&id, &dest, s1, e1, m1, c1, None, ExportOpts::default())
        .await
        .unwrap();

    // The bundle's `company.toml` IS the restored company's seed. It must
    // carry version control's own list and nothing the console added.
    let seed_toml = tokio::fs::read_to_string(dest.join(COMPANY_TOML))
        .await
        .expect("the bundle writes a company.toml");
    let seed: CompanyManifest = toml::from_str(&seed_toml).expect("a valid seed");
    assert_eq!(
        seed.tools.allow,
        vec!["*".to_string(), "search".to_string()],
        "the exported seed must not carry the console's grant"
    );

    let (s2, e2, m2, c2) = fs_ports(&home2);
    import_bundle(&dest, s2.clone(), e2, m2, c2, None)
        .await
        .unwrap();
    let dst = s2.load(&id).await.unwrap().unwrap();

    // The grant itself survives, still attributed to the operator...
    assert_eq!(
        dst.overlay_tool_grants.as_ref(),
        Some(&held),
        "the console grant was dropped by the bundle round-trip"
    );
    // ...and is folded back in, so the restored company grants it from the
    // first read rather than from its first rebuild.
    assert!(
        crate::company::grants_chargebee_explicit(&dst.manifest.tools.allow),
        "the restored record must grant it: {:?}",
        dst.manifest.tools.allow
    );
    assert_eq!(
        dst.manifest
            .tools
            .allow
            .iter()
            .filter(|g| *g == "chargebee")
            .count(),
        1,
        "folded twice"
    );
    // And the seed is still recoverable from it, which is what keeps the
    // grant revocable and keeps the next rebuild from clearing it.
    assert_eq!(
        crate::ports::types::seed_tool_allow(
            &dst.manifest.tools.allow,
            dst.overlay_tool_grants.as_ref()
        ),
        vec!["*".to_string(), "search".to_string()]
    );
}

/// Issue #343: a bundle carrying two overrides for one teammate is **refused**
/// at import, not silently reduced to whichever row deserialized first.
///
/// Import is the only boundary where `overlay_budgets` arrives from outside
/// this process, so it is the only place the write path's one-per-teammate
/// invariant can be broken. The two rows here disagree ($0 versus $50, set by
/// different people), which is the point: there is no correct row to pick,
/// and picking silently would either mute a teammate or restore an allowance
/// an admin revoked, with the wrong name on the attribution either way.
#[tokio::test]
async fn a_bundle_with_duplicate_budget_overrides_is_rejected() {
    let home1 = tmp_root("dup-src");
    let home2 = tmp_root("dup-dst");
    let dest = tmp_root("dup-bundle");
    let id = CompanyId::new("dup-co");

    let (s1, e1, m1, c1) = fs_ports(&home1);
    s1.save(&CompanyRecord {
        general_channel: Default::default(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        overlay_desk_hive: Vec::new(),
        id: id.clone(),
        manifest: budget_manifest(),
        ledger: Vec::new(),
        lifecycle: "running".into(),
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
    export_bundle(&id, &dest, s1, e1, m1, c1, None, ExportOpts::default())
        .await
        .unwrap();

    // Forge the tampered/foreign bundle by rewriting its meta.json — the shape
    // an import can be handed but the write path can never produce.
    let meta_path = dest.join(META_JSON);
    let mut meta: BundleMeta =
        serde_json::from_str(&tokio::fs::read_to_string(&meta_path).await.unwrap()).unwrap();
    meta.overlay_budgets = vec![
        BudgetOverride {
            agent_id: "ceo".into(),
            budget_usd_daily: Some(0.0),
            set_by: admin_actor(),
            at_millis: 1_700_000_000_000,
        },
        BudgetOverride {
            agent_id: "ceo".into(),
            budget_usd_daily: Some(50.0),
            set_by: Actor {
                kind: ActorKind::User,
                id: "user-other".into(),
            },
            at_millis: 1_700_000_000_002,
        },
    ];
    tokio::fs::write(&meta_path, serde_json::to_string(&meta).unwrap())
        .await
        .unwrap();

    let (s2, e2, m2, c2) = fs_ports(&home2);
    let err = import_bundle(&dest, s2.clone(), e2, m2, c2, None)
        .await
        .expect_err("import must refuse a bundle with two overrides for one teammate");
    let message = err.to_string();
    assert!(
        message.contains("ceo") && message.contains("budget override"),
        "the refusal must name the teammate so an operator can fix the bundle: {message}"
    );

    // And nothing was written: a refused import must not half-apply.
    assert!(
        s2.load(&id).await.unwrap().is_none(),
        "a rejected bundle must not persist a partial company record"
    );

    for dir in [home1, home2, dest] {
        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}

/// The same refusal for the roster edits, which arrive through the same one
/// door and are read the same first-match way.
///
/// The two rows here disagree about the teammate's role and were set by
/// different people, which is the point: there is no correct row to pick.
/// Applying whichever deserialized first would restore a name an operator
/// changed — or, through `tools`, a grant they narrowed — and attribute it to
/// somebody who did not do it.
#[tokio::test]
async fn a_bundle_with_duplicate_agent_edits_is_rejected() {
    let home1 = tmp_root("dupedit-src");
    let home2 = tmp_root("dupedit-dst");
    let dest = tmp_root("dupedit-bundle");
    let id = CompanyId::new("dupedit-co");

    let (s1, e1, m1, c1) = fs_ports(&home1);
    s1.save(&CompanyRecord {
        general_channel: Default::default(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        overlay_desk_hive: Vec::new(),
        id: id.clone(),
        manifest: budget_manifest(),
        ledger: Vec::new(),
        lifecycle: "running".into(),
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
    export_bundle(&id, &dest, s1, e1, m1, c1, None, ExportOpts::default())
        .await
        .unwrap();

    // The shape an import can be handed but `upsert_agent_override` can never
    // produce: it replaces in place, so a second row for one teammate only
    // exists in a bundle written elsewhere.
    let meta_path = dest.join(META_JSON);
    let mut meta: BundleMeta =
        serde_json::from_str(&tokio::fs::read_to_string(&meta_path).await.unwrap()).unwrap();
    meta.overlay_agent_edits = vec![
        AgentOverride {
            agent_id: "ceo".into(),
            role: Some("Chief Vibes".into()),
            ..Default::default()
        },
        AgentOverride {
            agent_id: "ceo".into(),
            role: Some("Interim Chief".into()),
            tools: Some(Some(vec!["docs.read".into()])),
            ..Default::default()
        },
    ];
    tokio::fs::write(&meta_path, serde_json::to_string(&meta).unwrap())
        .await
        .unwrap();

    let (s2, e2, m2, c2) = fs_ports(&home2);
    let err = import_bundle(&dest, s2.clone(), e2, m2, c2, None)
        .await
        .expect_err("import must refuse a bundle with two edits for one teammate");
    let message = err.to_string();
    assert!(
        message.contains("ceo") && message.contains("more than one edit"),
        "the refusal must name the teammate so an operator can fix the bundle: {message}"
    );

    // And nothing was written: a refused import must not half-apply.
    assert!(
        s2.load(&id).await.unwrap().is_none(),
        "a rejected bundle must not persist a partial company record"
    );

    for dir in [home1, home2, dest] {
        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}

#[cfg(feature = "export")]
#[tokio::test]
async fn tar_pack_unpack_roundtrip() {
    let home = tmp_root("tar-home");
    let runtime = RuntimeBuilder::fs_defaults(home.clone(), manifest())
        .await
        .expect("build");
    let id = runtime.id().clone();
    runtime
        .run_cycle(vec![CompanyEvent::OperatorMessage {
            mentions: Vec::new(),
            parent: None,
            text: "hi".into(),
            by: None,
            chat: None,
            deliverable: None,
            attachments: Vec::new(),
        }])
        .await
        .unwrap();

    let (s, e, m, c) = fs_ports(&home);
    let bundle_dir = tmp_root("tar-bundle").join(id.as_ref());
    export_bundle(&id, &bundle_dir, s, e, m, c, None, ExportOpts::default())
        .await
        .unwrap();

    let tar_path = tmp_root("tar-out").join("company.tar");
    tokio::fs::create_dir_all(tar_path.parent().unwrap())
        .await
        .unwrap();
    pack_tar(&bundle_dir, &tar_path).unwrap();
    assert!(tar_path.is_file());

    let unpacked = tmp_root("tar-unpacked");
    unpack_tar(&tar_path, &unpacked).unwrap();
    let root = find_bundle_root(&unpacked).unwrap();

    // Import the unpacked bundle into a fresh home.
    let home2 = tmp_root("tar-dst");
    let (s2, e2, m2, c2) = fs_ports(&home2);
    let imported = import_bundle(&root, s2.clone(), e2, m2, c2, None)
        .await
        .unwrap();
    assert_eq!(imported, id);
    let rec = s2.load(&id).await.unwrap().unwrap();
    assert_eq!(rec.manifest.company.name, "Export Co");

    for dir in [
        home,
        home2,
        bundle_dir.parent().unwrap().to_path_buf(),
        tar_path.parent().unwrap().to_path_buf(),
        unpacked,
    ] {
        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}

/// The third knowledge port finally travels: facts written on the source
/// come back from the imported bundle, and the bundle carries them in
/// `facts.jsonl (bundle root)` beside the traces they conceptually sit with.
#[tokio::test]
async fn operator_facts_travel_with_the_bundle() {
    use crate::ports::facts::FactStore;
    use crate::ports::{FactKind, FactRecord};
    use crate::store::FsOps;

    let home1 = tmp_root("facts-src");
    let home2 = tmp_root("facts-dst");
    let dest = tmp_root("facts-bundle");
    let id = CompanyId::new("facts-co");

    let (s1, e1, m1, c1) = fs_ports(&home1);
    s1.save(&company_record(&id)).await.unwrap();
    let f1: Arc<dyn FactStore> = Arc::new(FsOps::new(home1.clone()));
    f1.upsert(
        &id,
        &FactRecord {
            id: "supplier".into(),
            kind: FactKind::Fact,
            title: "supplier".into(),
            body: "lathe parts come from Initech".into(),
            source: "cto".into(),
            updated_at_millis: 1,
        },
    )
    .await
    .unwrap();

    export_bundle(&id, &dest, s1, e1, m1, c1, Some(f1), ExportOpts::default())
        .await
        .unwrap();
    assert!(
        dest.join(FACTS_JSONL).is_file(),
        "the bundle must carry the facts file at the bundle root, where \
             the live fs layout keeps it"
    );

    let (s2, e2, m2, c2) = fs_ports(&home2);
    let f2: Arc<dyn FactStore> = Arc::new(FsOps::new(home2.clone()));
    let imported = import_bundle(&dest, s2, e2, m2, c2, Some(f2.clone()))
        .await
        .unwrap();
    let listed = f2.list(&imported, None, None).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].body, "lathe parts come from Initech");
}

/// Both compatibility directions: a bundle written without facts (an old
/// host, or an export run without the port) imports clean into a target
/// that has one — empty, never an error — and a bundle WITH facts refuses
/// a target with no fact port rather than dropping them silently.
#[tokio::test]
async fn facts_compatibility_is_explicit_in_both_directions() {
    use crate::ports::facts::FactStore;
    use crate::ports::{FactKind, FactRecord};
    use crate::store::FsOps;

    // Old bundle (no facts file) into a facts-capable target: clean.
    let home1 = tmp_root("factless-src");
    let home2 = tmp_root("factless-dst");
    let dest = tmp_root("factless-bundle");
    let id = CompanyId::new("factless-co");
    let (s1, e1, m1, c1) = fs_ports(&home1);
    s1.save(&company_record(&id)).await.unwrap();
    export_bundle(&id, &dest, s1, e1, m1, c1, None, ExportOpts::default())
        .await
        .unwrap();
    assert!(!dest.join(FACTS_JSONL).exists());
    let (s2, e2, m2, c2) = fs_ports(&home2);
    let f2: Arc<dyn FactStore> = Arc::new(FsOps::new(home2.clone()));
    let imported = import_bundle(&dest, s2, e2, m2, c2, Some(f2.clone()))
        .await
        .unwrap();
    assert!(f2.list(&imported, None, None).await.unwrap().is_empty());

    // Facts-bearing bundle into a target with no fact port: a refusal
    // naming the loss, not a silent drop.
    let home3 = tmp_root("factful-src");
    let home4 = tmp_root("factful-dst");
    let dest2 = tmp_root("factful-bundle");
    let id2 = CompanyId::new("factful-co");
    let (s3, e3, m3, c3) = fs_ports(&home3);
    s3.save(&company_record(&id2)).await.unwrap();
    let f3: Arc<dyn FactStore> = Arc::new(FsOps::new(home3.clone()));
    f3.upsert(
        &id2,
        &FactRecord {
            id: "f".into(),
            kind: FactKind::Fact,
            title: "t".into(),
            body: "b".into(),
            source: "s".into(),
            updated_at_millis: 1,
        },
    )
    .await
    .unwrap();
    export_bundle(
        &id2,
        &dest2,
        s3,
        e3,
        m3,
        c3,
        Some(f3),
        ExportOpts::default(),
    )
    .await
    .unwrap();
    let (s4, e4, m4, c4) = fs_ports(&home4);
    let err = import_bundle(&dest2, s4.clone(), e4, m4, c4, None)
        .await
        .expect_err("facts with no target port must refuse");
    assert!(err.to_string().contains("fact"), "{err}");
    // The property the refuse-before-write ordering exists for: NOTHING
    // landed. A refusal after `store.save` would leave a half-import
    // whose append-only retry duplicates history.
    assert!(
        s4.load(&id2).await.unwrap().is_none(),
        "the refusal must precede every write"
    );
}
/// The fact-port failure case of the ordering guarantee: facts are the
/// FIRST write, so a failing fact port leaves zero company state behind —
/// the retry-safety claim, asserted rather than narrated.
#[tokio::test]
async fn a_failing_fact_port_leaves_nothing_written() {
    use crate::ports::facts::FactStore;
    use crate::ports::{FactKind, FactRecord};
    use crate::store::FsOps;

    struct FailingFacts;
    #[async_trait::async_trait]
    impl FactStore for FailingFacts {
        async fn list(
            &self,
            _: &CompanyId,
            _: Option<&str>,
            _: Option<FactKind>,
        ) -> crate::Result<Vec<FactRecord>> {
            Ok(Vec::new())
        }
        async fn upsert(&self, _: &CompanyId, _: &FactRecord) -> crate::Result<()> {
            Err(crate::error::OpenCompanyError::Store(
                "injected fact-port failure".into(),
            ))
        }
        async fn delete(&self, _: &CompanyId, _: &str) -> crate::Result<bool> {
            Ok(false)
        }
    }

    let home_src = tmp_root("factfail-src");
    let home_dst = tmp_root("factfail-dst");
    let dest = tmp_root("factfail-bundle");
    let id = CompanyId::new("factfail-co");
    let (s1, e1, m1, c1) = fs_ports(&home_src);
    s1.save(&company_record(&id)).await.unwrap();
    let f1: Arc<dyn FactStore> = Arc::new(FsOps::new(home_src.clone()));
    f1.upsert(
        &id,
        &FactRecord {
            id: "f".into(),
            kind: FactKind::Fact,
            title: "t".into(),
            body: "b".into(),
            source: "s".into(),
            updated_at_millis: 1,
        },
    )
    .await
    .unwrap();
    export_bundle(&id, &dest, s1, e1, m1, c1, Some(f1), ExportOpts::default())
        .await
        .unwrap();

    let (s2, e2, m2, c2) = fs_ports(&home_dst);
    let err = import_bundle(&dest, s2.clone(), e2, m2, c2, Some(Arc::new(FailingFacts)))
        .await
        .expect_err("the injected fact failure must surface");
    assert!(err.to_string().contains("injected"), "{err}");
    assert!(
        s2.load(&id).await.unwrap().is_none(),
        "a fact-port failure must precede every append-only write"
    );
}

/// Re-exporting a now-factless company into the SAME directory must not
/// leave the previous export's facts behind for a later import to
/// resurrect.
#[tokio::test]
async fn a_factless_reexport_removes_the_stale_facts_file() {
    use crate::ports::facts::FactStore;
    use crate::ports::{FactKind, FactRecord};
    use crate::store::FsOps;

    let home = tmp_root("stale-src");
    let dest = tmp_root("stale-bundle");
    let id = CompanyId::new("stale-co");
    let (s1, e1, m1, c1) = fs_ports(&home);
    s1.save(&company_record(&id)).await.unwrap();
    let facts: Arc<dyn FactStore> = Arc::new(FsOps::new(home.clone()));
    facts
        .upsert(
            &id,
            &FactRecord {
                id: "f".into(),
                kind: FactKind::Fact,
                title: "t".into(),
                body: "b".into(),
                source: "s".into(),
                updated_at_millis: 1,
            },
        )
        .await
        .unwrap();
    export_bundle(
        &id,
        &dest,
        s1.clone(),
        e1.clone(),
        m1.clone(),
        c1.clone(),
        Some(facts.clone()),
        ExportOpts::default(),
    )
    .await
    .unwrap();
    assert!(dest.join(FACTS_JSONL).is_file());

    // The operator deletes the fact, then re-exports into the same dir.
    assert!(facts.delete(&id, "f").await.unwrap());
    export_bundle(
        &id,
        &dest,
        s1,
        e1,
        m1,
        c1,
        Some(facts),
        ExportOpts::default(),
    )
    .await
    .unwrap();
    assert!(
        !dest.join(FACTS_JSONL).exists(),
        "a factless re-export must remove the stale facts file"
    );
}
