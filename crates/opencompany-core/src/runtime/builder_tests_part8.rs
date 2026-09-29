use super::tests_core::*;
use super::*;

/// The same defect one field over, and the one that would have made this
/// PR's whole promise false: a console rename and a console removal must
/// survive a rebuild.
///
/// Neither is written back to `company.toml` — that is the point of the
/// overlay model — so the seed manifest a rebuild starts from still names
/// the teammate as it launched and still declares the one that was removed.
/// `build()` ends in an unconditional `store.save`, and while that save
/// wrote `Vec::new()` for these two fields every restart, every harness
/// pool swap and every inference-settings change quietly reverted the
/// rename and walked the removed teammate back onto the roster. An operator
/// on a hosted tenant has no file to edit and no redeploy to make, so
/// "it comes back on the next restart" is the whole feature failing.
///
/// Asserted through `effective_agents` rather than the raw overlay vectors,
/// because that is the roster everything downstream actually reads.
#[tokio::test]
async fn a_rebuild_keeps_a_console_rename_and_a_console_removal() {
    use crate::ports::types::AgentOverride;
    use crate::store::FsCompanyStore;

    let home_dir = tmp_home("oc-roster-rebuild-");
    let home = home_dir.path().to_path_buf();
    let id = CompanyId::new("roster-co");
    let manifest = parse(
        r#"
        [company]
        name = "Roster Co"

        [[agent]]
        id = "ceo"
        role = "Chief Executive"

        [[agent]]
        id = "cto"
        role = "Chief Technologist"
        "#,
    );

    // First build materializes the record.
    RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();

    // The console writes: rename one blueprint teammate, remove another.
    let store = FsCompanyStore::new(home.clone());
    let mut record = store.load(&id).await.unwrap().unwrap();
    record.overlay_agent_edits.push(AgentOverride {
        agent_id: "ceo".to_string(),
        name: None,
        role: Some("Managing Director".to_string()),
        description: None,
        tools: None,
        instructions: None,
        avatar: None,
        ..Default::default()
    });
    record.retire_agent("cto");
    store.save(&record).await.unwrap();

    // Rebuild, exactly as a restart does.
    RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();

    let rebuilt = store.load(&id).await.unwrap().unwrap();
    let roster = rebuilt.effective_agents();
    let ceo = roster
        .iter()
        .find(|agent| agent.id == "ceo")
        .expect("the renamed teammate is still on the roster");
    assert_eq!(
        ceo.role, "Managing Director",
        "the rebuild reverted a console rename — `overlay_agent_edits` was not carried \
         forward; roster: {roster:?}"
    );
    assert!(
        !roster.iter().any(|agent| agent.id == "cto"),
        "the rebuild resurrected a removed teammate — `overlay_retired_agents` was not \
         carried forward; roster: {roster:?}"
    );

    // And the blueprint really does still declare both, which is exactly why
    // carrying the overlay is the only thing that can have produced the two
    // assertions above.
    assert!(
        rebuilt
            .manifest
            .agents
            .iter()
            .any(|agent| agent.id == "cto"),
        "the manifest no longer declares the removed teammate, so this test proves nothing"
    );
}

/// Issue #707: a desk reorder reaches a **resident** runtime, with no
/// rebuild and no restart.
///
/// This is the assertion that was missing. The neighbouring #133 test is
/// named `..._after_rebuild` and rebuilds the brain before asserting, so it
/// only ever proved the builder *seeds* the order — nobody had asked what a
/// live company does when the operator saves one. The answer was: keep
/// routing to the old lead until the process restarted, because
/// `HarnessBrain.record` was a build-time snapshot and the only caller of
/// `rebuild_company` is an inference-settings change.
///
/// So the write here goes through the store exactly as
/// `set_desk_order` (`src/server/operator.rs`) does — load, mutate, save —
/// and then the SAME runtime object runs a second turn. No rebuild happens
/// anywhere in this test, which is the whole point of it.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn a_desk_reorder_reaches_a_resident_runtime_without_a_rebuild() {
    use crate::harness::HarnessPool;
    use crate::ports::types::{CompanyEvent, OverlayDeskOrder};
    use crate::store::FsCompanyStore;

    let home_dir = tmp_home("oc-707-order-");
    let home = home_dir.path().to_path_buf();
    let id = CompanyId::new("order-co");

    let manifest = parse(
        r#"
        [company]
        name = "Order Co"

        [policy]
        mode = "full"

        [[agent]]
        id = "eng1"
        role = "Engineer One"

        [[agent]]
        id = "eng2"
        role = "Engineer Two"

        [[group_chat]]
        id = "eng"
        name = "Engineering"
        members = ["eng1", "eng2"]
        "#,
    );

    // The blueprint lead is `eng1`; no operator order yet.
    let store = FsCompanyStore::new(home.clone());
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            overlay_desk_hive: Vec::new(),
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
        })
        .await
        .unwrap();

    let stub = spawn_stub("desk lead reply").await;
    let runtime = RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .with_harness(Arc::new(HarnessPool::new()))
        .with_harness_inference(
            HostedProviderConfig {
                base_url: stub,
                credential: crate::company::Credential::from_value("k"),
                extra_headers: Vec::new(),
            },
            Some("stub-model".to_string()),
        )
        .build()
        .await
        .unwrap();

    let desk_turn = |text: &'static str| CompanyEvent::OperatorMessage {
        mentions: Vec::new(),
        parent: None,
        text: text.to_string(),
        by: None,
        chat: Some("eng".to_string()),
        deliverable: None,
        attachments: Vec::new(),
    };

    // Baseline: the blueprint lead is the primary seat of the episode the
    // desk message opens (plan hive-desks: a desk of two answers as a room;
    // its opening plan puts the lead first). Asserted rather than assumed,
    // so a later failure cannot be explained away as "the desk never routed".
    runtime
        .run_cycle(vec![desk_turn("who leads?")])
        .await
        .expect("first cycle");
    let before = episode_participants(&runtime, &id, 1).await;
    assert_eq!(
        before.first().map(String::as_str),
        Some("eng1"),
        "the blueprint lead must lead before the reorder; saw {before:?}"
    );

    // The console write: load, mutate, save. Nothing rebuilds.
    let mut record = store.load(&id).await.unwrap().expect("record");
    record.overlay_desk_order.push(OverlayDeskOrder {
        desk_id: "eng".to_string(),
        ordered: vec!["eng2".to_string(), "eng1".to_string()],
    });
    store.save(&record).await.unwrap();

    // The same runtime, a second turn — a new episode, whose primary is the
    // reordered lead.
    runtime
        .run_cycle(vec![desk_turn("who leads now?")])
        .await
        .expect("second cycle");
    let after = episode_participants(&runtime, &id, 2).await;
    assert_eq!(
        after.first().map(String::as_str),
        Some("eng2"),
        "the reordered lead eng2 never led — the resident brain routed on a stale \
         record; saw {after:?}"
    );
}

/// The participants of the `nth` episode the runtime opened (1-based), in
/// plan order, waiting for the journal row: the episode is driven on its own
/// task once the cycle accepted the message.
#[cfg(feature = "openhuman")]
async fn episode_participants(
    runtime: &crate::company::runtime::CompanyRuntime,
    id: &CompanyId,
    nth: usize,
) -> Vec<String> {
    use crate::ports::types::{CompanyEvent, EventSeq};
    for _ in 0..400 {
        let rows = runtime
            .events()
            .read_from(id, EventSeq::new(0), usize::MAX)
            .await
            .expect("read the journal");
        let opened: Vec<Vec<String>> = rows
            .into_iter()
            .filter_map(|stored| match stored.event {
                CompanyEvent::EpisodeOpened { participants, .. } => Some(participants),
                _ => None,
            })
            .collect();
        if opened.len() >= nth {
            return opened[nth - 1].clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    Vec::new()
}

/// Issue #707, the same defect through `overlay_desks` + `overlay_desk_members`:
/// a desk the operator creates on a **resident** runtime is reachable.
///
/// Sharper than staleness alone, because it pins a divergence: the store
/// resolves the new desk's lead while the runtime routes as though the desk
/// does not exist. Both are asserted, at the same instant.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn a_new_overlay_desk_is_reachable_on_a_resident_runtime() {
    use crate::harness::HarnessPool;
    use crate::ports::types::{CompanyEvent, OverlayDesk, OverlayDeskMember};
    use crate::store::{FsCompanyStore, FsContextStore};

    let home_dir = tmp_home("oc-707-desk-");
    let home = home_dir.path().to_path_buf();
    let id = CompanyId::new("desk-co");

    let manifest = parse(
        r#"
        [company]
        name = "Desk Co"

        [policy]
        mode = "full"

        [[agent]]
        id = "eng1"
        role = "Engineer One"

        [[agent]]
        id = "eng2"
        role = "Engineer Two"
        "#,
    );

    let store = FsCompanyStore::new(home.clone());
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            overlay_desk_hive: Vec::new(),
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
        })
        .await
        .unwrap();

    let stub = spawn_stub("desk reply").await;
    let runtime = RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .with_harness(Arc::new(HarnessPool::new()))
        .with_harness_inference(
            HostedProviderConfig {
                base_url: stub,
                credential: crate::company::Credential::from_value("k"),
                extra_headers: Vec::new(),
            },
            Some("stub-model".to_string()),
        )
        .build()
        .await
        .unwrap();

    // The console creates a desk and puts `eng2` on it.
    let mut record = store.load(&id).await.unwrap().expect("record");
    // Deliberately EMPTY: the `OverlayDeskMember` row below is the only
    // membership source, so this test cannot pass by way of a desk's own
    // founding members. Without that, it would still be green if
    // `effective_desk_members` ignored `overlay_desk_members` outright —
    // which is half of what it is here to prove.
    record.overlay_desks.push(OverlayDesk {
        id: "design".to_string(),
        name: "Design".to_string(),
        description: None,
        members: Vec::new(),
        responder: crate::ports::types::ResponderMode::default(),
        hive: Default::default(),
    });
    record.overlay_desk_members.push(OverlayDeskMember {
        desk_id: "design".to_string(),
        agent_id: "eng2".to_string(),
    });
    store.save(&record).await.unwrap();

    // What every already-correct consumer sees at this instant.
    let fresh = store.load(&id).await.unwrap().unwrap();
    assert_eq!(
        crate::runtime::delegation_tools::desk_lead(&fresh, "design"),
        Some("eng2".to_string()),
        "the stored record must resolve the new desk, or this test proves nothing"
    );

    runtime
        .run_cycle(vec![CompanyEvent::OperatorMessage {
            mentions: Vec::new(),
            parent: None,
            text: "hello design".to_string(),
            by: None,
            chat: Some("design".to_string()),
            deliverable: None,
            attachments: Vec::new(),
        }])
        .await
        .expect("cycle");

    let context: Arc<dyn ContextStore> = Arc::new(FsContextStore::new(home.clone()));
    let routed: Vec<String> = context
        .list(&id, "task-outcome/")
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.label)
        .collect();
    assert!(
        routed.contains(&"task-outcome/eng2".to_string()),
        "a desk chat must reach the desk's member; the runtime routed as though the desk \
         did not exist; saw {routed:?}"
    );
}

/// Builder-level regression for the `overlay_desk_order` seeding path (#133).
/// The harness test `desk_order_change_updates_routing_after_rebuild` exercises
/// `brain_over(record)` directly; this one drives the real
/// [`RuntimeBuilder::build`] wiring end-to-end: a persisted record carries a
/// NON-EMPTY `overlay_desk_order` that promotes `eng2` over the blueprint lead
/// `eng1`, and after `build()` a desk-addressed cycle must run on `eng2` — the
/// reordered lead — proving the builder seeds the operator order into the brain
/// rather than an empty default. The harness records each turn under a
/// `task-outcome/{agent_id}` context chunk, which is the observable seam.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn build_seeds_desk_order_into_brain_routing() {
    use crate::harness::HarnessPool;
    use crate::ports::types::{CompanyEvent, OverlayDeskOrder};
    use crate::store::FsCompanyStore;

    let home_dir = tmp_home("oc-seed-order-");
    let home = home_dir.path().to_path_buf();
    let id = CompanyId::new("order-co");

    // A desk `eng` whose blueprint lead is `eng1` (declared first).
    let manifest = parse(
        r#"
        [company]
        name = "Order Co"

        [policy]
        mode = "full"

        [[agent]]
        id = "eng1"
        role = "Engineer One"

        [[agent]]
        id = "eng2"
        role = "Engineer Two"

        [[group_chat]]
        id = "eng"
        name = "Engineering"
        members = ["eng1", "eng2"]
        # This test is about WHO LEADS a desk, and a two-member desk now
        # answers as a deliberating room by default (`crate::hivemind`) —
        # where there is no lead, every member speaks, and the operator's
        # desk order decides nothing. Opted out here so the fixture keeps
        # exercising the single-responder ladder it was written for; the
        # order still governs `delegate_to_desk` and the console's crown.
        hive = { enabled = false }
        "#,
    );

    // Persist a record whose operator order promotes `eng2` above `eng1`.
    let store = FsCompanyStore::new(home.clone());
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            overlay_desk_hive: Vec::new(),
            id: id.clone(),
            manifest: manifest.clone(),
            ledger: Vec::new(),
            lifecycle: "running".to_string(),
            overlay_agents: Vec::new(),
            overlay_desk_members: Vec::new(),
            overlay_desk_order: vec![OverlayDeskOrder {
                desk_id: "eng".to_string(),
                ordered: vec!["eng2".to_string(), "eng1".to_string()],
            }],
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

    // Build the runtime with an embedded harness pool + a stub inference
    // backend, so `build()` constructs the seeded `HarnessBrain`.
    let stub = spawn_stub("desk lead reply").await;
    let runtime = RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .with_harness(Arc::new(HarnessPool::new()))
        .with_harness_inference(
            HostedProviderConfig {
                base_url: stub,
                credential: crate::company::Credential::from_value("k"),
                extra_headers: Vec::new(),
            },
            Some("stub-model".to_string()),
        )
        .build()
        .await
        .unwrap();

    // A message addressed to the `eng` desk must be answered by the reordered
    // lead `eng2`, not the blueprint lead `eng1`.
    runtime
        .run_cycle(vec![CompanyEvent::OperatorMessage {
            mentions: Vec::new(),
            parent: None,
            text: "who leads?".to_string(),
            by: None,
            chat: Some("eng".to_string()),
            deliverable: None,
            attachments: Vec::new(),
        }])
        .await
        .expect("cycle");

    // The desk of two opens an episode whose primary seat is the reordered
    // lead: the blueprint lead sits second.
    let participants = episode_participants(&runtime, &id, 1).await;
    assert_eq!(
        participants.first().map(String::as_str),
        Some("eng2"),
        "desk turn did not route to the reordered lead eng2; saw {participants:?}"
    );
    assert_ne!(
        participants.first().map(String::as_str),
        Some("eng1"),
        "desk turn routed to the blueprint lead eng1 — the builder dropped the operator desk order; saw {participants:?}"
    );
}

/// A build applies the carried console override to the live gate, and marks
/// the runtime so the per-cycle refresh (issue #1455) knows the gate is the
/// real one. A test-injected gate is exempt on both counts: it carries its
/// own policy/TTL on purpose.
#[tokio::test]
async fn build_applies_the_effective_policy_to_the_gate_but_not_an_injected_one() {
    use crate::ports::approvals::ApprovalGate;
    use crate::ports::types::{
        Actor, ActorKind, Effect, EffectGroup, PolicyDecision, PolicyOverride,
    };
    use crate::store::FsCompanyStore;

    let dir = tmp_home("oc-policy-build-");
    let manifest = parse(
        "[company]\nname = \"Acme\"\n\
         [[agent]]\nid = \"ceo\"\nrole = \"Chief\"\n\
         [policy]\nmode = \"supervised\"\n\
         always_approve = [\"payment.send\"]\n\
         auto_approve_under_usd = 5.0\n\
         approval_ttl_hours = 24\n",
    );
    let id = CompanyId::new("acme");
    let overlay = PolicyOverride {
        mode: Some("full".to_string()),
        always_approve: None,
        auto_approve_under_usd: None,
        approval_ttl_hours: None,
        set_by: Actor {
            kind: ActorKind::User,
            id: "admin-1".to_string(),
        },
        at_millis: 1_700_000_000_000,
    };
    FsCompanyStore::new(dir.path())
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            overlay_desk_hive: Vec::new(),
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
            overlay_policy: Some(overlay),
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

    let runtime = RuntimeBuilder::new(dir.path(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    assert!(!runtime.gate_injected);
    // The override moved the tier: a $30 spend, above the manifest cap of
    // $5, now `Allow`s under the carried `full` mode.
    let spend = Effect {
        kind: "x402.spend".to_string(),
        group: EffectGroup::Spend,
        amount_usd: Some(30.0),
        established_thread: false,
        first_time_counterparty: false,
        payload: serde_json::Value::Null,
        agent: None,
        run_id: None,
    };
    assert!(matches!(
        runtime.approval_gate.evaluate(&id, &spend).await.unwrap(),
        PolicyDecision::Allow
    ));

    // An injected gate wins: the build must not clobber its fixture.
    let injected = Arc::new(
        ManifestApprovalGate::new(seed_policy("readonly", &[], None)).with_ttl_millis(999),
    );
    let injected_runtime = RuntimeBuilder::new(dir.path(), manifest)
        .with_id(id.clone())
        .with_approvals(injected.clone())
        .build()
        .await
        .unwrap();
    assert!(injected_runtime.gate_injected);
    assert_eq!(injected_runtime.approval_gate.ttl_millis(), 999);
    assert_eq!(
        injected_runtime.approval_gate.parked_ids(),
        injected.parked_ids()
    );
    assert!(
        matches!(
            injected_runtime
                .approval_gate
                .evaluate(&id, &spend)
                .await
                .unwrap(),
            PolicyDecision::RequireApproval
        ),
        "the injected readonly gate must keep its own policy, not the carried override"
    );
}
