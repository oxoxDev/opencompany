use super::tests_core::*;
use super::*;

/// The narrower guarantee round one's fix protects, isolated from round
/// two's live-grandfather behavior above: a resume that does NOT clear
/// the grandfather condition (record already latched, or gate already
/// seen) must still forward the marker exactly as recorded rather than
/// touching it. `set_lifecycle`'s own guard
/// (`to == "running" && !gate_seen && activation_completed_at.is_none()`)
/// is what keeps a genuinely-new, still-onboarding company's `pause` /
/// `resume` cycle from being mistaken for the legacy grandfather case —
/// its first save already stamped the marker `true`, so the guard never
/// fires for it.
#[tokio::test]
async fn a_resume_mid_onboarding_does_not_falsely_grandfather_a_new_company() {
    let home_dir = tempfile::Builder::new()
        .prefix("oc-wf-resume-no-false-grandfather-")
        .tempdir()
        .expect("tempdir");
    let home = home_dir.path().to_path_buf();
    let manifest = wf_manifest("");
    let id = CompanyId::new("acme");

    // A brand-new company: first boot stamps `gate_seen: true` and
    // leaves `name_confirmed`/`activation_completed_at` unset — the
    // real funnel applies, matching
    // `a_brand_new_company_is_not_backfilled_as_activated`.
    let runtime = RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let store = runtime.store().clone();
    assert!(store.activation_gate_seen(&id).await.unwrap());

    use crate::ports::types::{Actor, ActorKind};
    runtime
        .set_lifecycle(
            "paused",
            Actor {
                kind: ActorKind::Operator,
                id: "test-op".to_string(),
            },
        )
        .await
        .unwrap();
    runtime
        .set_lifecycle(
            "running",
            Actor {
                kind: ActorKind::Operator,
                id: "test-op".to_string(),
            },
        )
        .await
        .unwrap();

    let record = store.load(&id).await.unwrap().unwrap();
    assert!(
        !record.name_confirmed,
        "a pause/resume cycle mid-onboarding must not silently confirm the \
         name for a company that never went near the name-confirm route"
    );
    assert!(
        record.activation_completed_at.is_none(),
        "a pause/resume cycle mid-onboarding must not activate a company \
         that has not actually cleared the funnel"
    );
}

/// The other half of the migration: a genuinely new company (no `existing`
/// record at all) gets no grace — the real funnel applies from boot one.
#[tokio::test]
async fn a_brand_new_company_is_not_backfilled_as_activated() {
    let home_dir = tempfile::Builder::new()
        .prefix("oc-wf-nobackfill-")
        .tempdir()
        .expect("tempdir");
    let home = home_dir.path().to_path_buf();
    let manifest = wf_manifest("");
    let id = CompanyId::new("acme");

    let runtime = RuntimeBuilder::new(home, manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let record = runtime.store().load(&id).await.unwrap().unwrap();
    assert!(!record.name_confirmed);
    assert!(record.activation_completed_at.is_none());
}

/// PR #1875 review finding: a genuinely new company's *second* boot must
/// not be mistaken for the pre-#1843 grandfather case. The first boot
/// (`existing` is `None`) persists a record with `lifecycle == "running"`
/// and `activation_completed_at: None` — exactly the shape the grandfather
/// arm in `RuntimeBuilder::build` matches on. A restart before the operator
/// finishes onboarding (e.g. the app container bouncing mid-checklist) must
/// not let that second `build()` call activate the company via the
/// grandfather arm; only a company that predates activation tracking
/// entirely gets that grace.
#[tokio::test]
async fn a_restart_before_onboarding_completes_does_not_grandfather_activation() {
    let home_dir = tempfile::Builder::new()
        .prefix("oc-wf-no-grandfather-on-restart-")
        .tempdir()
        .expect("tempdir");
    let home = home_dir.path().to_path_buf();
    let manifest = wf_manifest("");
    let id = CompanyId::new("acme");

    // First boot: a brand-new company. Not activated — matches
    // `a_brand_new_company_is_not_backfilled_as_activated` above — but its
    // persisted record now has `lifecycle == "running"`.
    RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();

    // Second boot: the same company restarting before the operator ever
    // opened the activation funnel. `existing` now loads the record the
    // first boot just wrote — `lifecycle == "running"`,
    // `activation_completed_at: None` — the same shape a genuine
    // pre-#1843 legacy record has.
    RuntimeBuilder::new(home, manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();

    let store = crate::store::FsCompanyStore::new(home_dir.path().to_path_buf());
    let record = store.load(&id).await.unwrap().unwrap();
    assert!(
        !record.name_confirmed,
        "a container restart must not silently confirm the name for a company \
         that never went near the name-confirm route"
    );
    assert!(
        record.activation_completed_at.is_none(),
        "a container restart before onboarding completes must not activate \
         the company — that is the exact funnel the activation gate exists \
         to enforce"
    );
}

/// Issue #208: an enabled id with no surviving graph body — a seed entry
/// the operator deleted from `company.toml` — is dropped rather than
/// carried forward forever with nothing to run.
#[tokio::test]
async fn rebuild_drops_an_enabled_id_with_no_body() {
    let home_dir = tempfile::Builder::new()
        .prefix("oc-wf-zombie-")
        .tempdir()
        .expect("tempdir");
    let home = home_dir.path().to_path_buf();
    let id = CompanyId::new("acme");

    // First boot from a seed that enables `retired`.
    let runtime = RuntimeBuilder::new(
        home.clone(),
        wf_manifest("[workflows]\nenabled=[\"retired\"]\n"),
    )
    .with_id(id.clone())
    .build()
    .await
    .unwrap();
    assert_eq!(
        runtime.enabled_workflow_ids().await.unwrap(),
        vec!["retired"]
    );
    drop(runtime);

    // The operator removes it from the version-controlled seed. No overlay
    // body was ever written for it, so nothing carries it forward.
    let runtime = RuntimeBuilder::new(home.clone(), wf_manifest(""))
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    assert!(
        runtime.enabled_workflow_ids().await.unwrap().is_empty(),
        "a bodiless enabled id zombied past its removal from the seed"
    );
}

#[test]
fn effective_grants_no_roster_is_company_allow() {
    let manifest = parse("[company]\nname=\"X\"\n[tools]\nallow=[\"email.*\",\"email.*\"]\n");
    assert_eq!(effective_grants(&manifest), vec!["email.*".to_string()]);
}

#[test]
fn effective_grants_agent_without_tools_inherits_allow() {
    let manifest = parse(
        "[company]\nname=\"X\"\n[[agent]]\nid=\"a\"\nrole=\"A\"\n[tools]\nallow=[\"email.*\"]\n",
    );
    assert_eq!(effective_grants(&manifest), vec!["email.*".to_string()]);
}

#[test]
fn effective_grants_agent_tools_intersect_allow() {
    let manifest = parse(
        r#"
        [company]
        name = "X"
        [[agent]]
        id = "a"
        role = "A"
        tools = ["email.send", "payment.send"]
        [tools]
        allow = ["email.*"]
        "#,
    );
    // `email.send` is covered by `email.*`; `payment.send` is not.
    assert_eq!(effective_grants(&manifest), vec!["email.send".to_string()]);
}

/// A manifest that names the retired out-of-process OpenHuman daemon as its
/// tool and channel provider builds like any other: the grant-enforcing
/// built-in tools, and the operator channel alone. The daemon's JSON-RPC seam
/// was deleted with the `openhuman-rpc` feature (plan hive-desks, Phase 7);
/// the embedded harness serves tools in-process over MCP instead.
#[tokio::test]
async fn an_openhuman_provider_manifest_builds_on_builtins_alone() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimeBuilder::new(dir.path(), openhuman_manifest())
        .build()
        .await
        .unwrap();

    // The declared `openhuman` channel is skipped; only the operator surface
    // is wired, and since issue #1757 it is a durable delivery target.
    assert_eq!(runtime.channels.len(), 1);
    assert_eq!(runtime.channels[0].channel_id(), "operator");
    assert_eq!(
        runtime.deliverable_channel_ids(),
        vec!["operator".to_string()],
        "an operator-only runtime delivers to its Operator channel: {:?}",
        runtime.deliverable_channel_ids()
    );

    // Tools are the grant-enforcing built-in: ungranted rejected, granted
    // returns a well-formed not-implemented result.
    let ungranted = runtime
        .tools
        .invoke(
            runtime.id(),
            ToolCall {
                tool: "payment.send".into(),
                args: serde_json::Value::Null,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        ungranted,
        crate::OpenCompanyError::ToolNotGranted(t) if t == "payment.send"
    ));

    let granted = runtime
        .tools
        .invoke(
            runtime.id(),
            ToolCall {
                tool: "email.send".into(),
                args: serde_json::Value::Null,
            },
        )
        .await
        .unwrap();
    assert!(!granted.ok);
}

#[tokio::test]
async fn wires_manifest_and_overlay_desks_as_delivery_channels() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = parse(
        r#"
        [company]
        name = "Acme"
        [[agent]]
        id = "ceo"
        role = "Chief"
        [[group_chat]]
        id = "engineering"
        name = "Engineering"
        members = ["ceo"]
        "#,
    );
    let id = CompanyId::new("acme");
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
            overlay_desks: vec![crate::ports::types::OverlayDesk {
                id: "research".to_string(),
                name: "Research".to_string(),
                description: None,
                members: vec!["ceo".to_string()],
                responder: crate::ports::types::ResponderMode::default(),
                hive: Default::default(),
            }],
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
    let runtime = RuntimeBuilder::new(dir.path(), manifest)
        .with_id(id)
        .build()
        .await
        .unwrap();

    let ids: Vec<_> = runtime
        .channels
        .iter()
        .map(|channel| channel.channel_id())
        .collect();
    assert!(ids.contains(&"engineering"));
    assert!(ids.contains(&"research"));

    // Both desks are real delivery targets — they write to the company's
    // durable event log — and since issue #1757 so is `operator`, whose
    // report now lands durably in the standing Operator channel.
    let deliverable = runtime.deliverable_channel_ids();
    assert!(
        deliverable.contains(&"engineering".to_string()),
        "{deliverable:?}"
    );
    assert!(
        deliverable.contains(&"research".to_string()),
        "{deliverable:?}"
    );
    assert!(
        deliverable.contains(&"operator".to_string()),
        "operator is now a durable, offerable delivery channel: {deliverable:?}"
    );
}

/// Issue #1781 review (Codex P2): a grandfathered manifest desk at the
/// literal id `operator` predates `company/manifest.rs`'s "operator is
/// reserved" validation (which only runs at upload/create time, never at
/// boot) and still wires **both** the built-in `OperatorChannel` and a
/// `DeskChannel("operator")` into `runtime.channels` — `desk_exists`
/// resolves the manifest group chat and the desk-wiring loop has no idea
/// the built-in channel already claimed the same id. `deliverable_channel_ids`
/// must not leak that internal duplication to the console: it feeds
/// `/workflows/wired-channels`, and `WorkflowCreateDialog` renders one
/// `SelectItem` per id — a repeated `operator` collides as a React key.
///
/// `861a8fbad` (landed after this test's fixture was first written) made
/// `build()` run the STRICT `validate()` whenever no persisted record
/// exists yet for the company (`existing.is_none()`), and only grandfather
/// a reserved id/name collision when `existing.is_some()` — i.e. on a real
/// reboot. A bare `RuntimeBuilder::new(..).build()` off a freshly parsed
/// manifest is a first boot by construction, so the reserved `operator`
/// group-chat id below now fails strict validation before the fixture
/// ever reaches the collision state this test means to exercise.
///
/// A follow-up review (Codex P1) found `existing.is_some()` alone too
/// broad — it grandfathered a collision introduced by editing
/// `company.toml` *between* two restarts, not just one already present
/// when the company was first stored. So this seeds the persisted record
/// **directly**, the same way
/// `a_reboot_still_grandfathers_an_already_registered_operator_agent_id`
/// (agent-id case) now does, rather than via a first `build()` on a safe
/// manifest followed by a second `build()` that newly introduces the
/// collision — that two-step shape is exactly the vulnerability, and
/// `a_reboot_refuses_a_newly_introduced_reserved_desk_id` below proves it
/// is refused now.
#[tokio::test]
async fn deliverable_channel_ids_dedupes_a_grandfathered_operator_desk() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = parse(
        r#"
        [company]
        name = "Acme"
        [[agent]]
        id = "ceo"
        role = "Chief"
        [[group_chat]]
        id = "operator"
        name = "Operator"
        members = ["ceo"]
        "#,
    );
    let id = company_id_from_name("Acme");
    FsCompanyStore::new(dir.path())
        .save(&CompanyRecord {
            general_channel: Default::default(),
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
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            overlay_desk_hive: Vec::new(),
            disabled_workflows: Vec::new(),
            template_provenance: None,
            setup: None,
            name_confirmed: false,
            activation_completed_at: None,
            created_at_millis: None,
        })
        .await
        .unwrap();

    let runtime = RuntimeBuilder::new(dir.path(), manifest)
        .with_id(id)
        .build()
        .await
        .expect(
            "a company whose STORED record already carries this desk collision must still \
             reboot, even though the manifest being loaded only clears the relaxed loader",
        );

    // Both adapters really are present internally — this asserts the
    // fixture reaches the collision state the fix has to survive, not just
    // that the picker happens to look right for some other reason.
    let operator_channels = runtime
        .channels
        .iter()
        .filter(|c| c.channel_id() == "operator")
        .count();
    assert_eq!(
        operator_channels, 2,
        "fixture must actually wire both the built-in Operator channel and \
         the grandfathered desk under the same id"
    );

    let deliverable = runtime.deliverable_channel_ids();
    let operator_count = deliverable.iter().filter(|id| *id == "operator").count();
    assert_eq!(
        operator_count, 1,
        "operator must appear exactly once in the picker's set — ordering \
         preserved, duplicates dropped: {deliverable:?}"
    );
}

/// The desk-collision sibling of
/// `a_reboot_refuses_a_newly_introduced_reserved_agent_id`: a reboot whose
/// manifest *newly* adds a desk colliding with the Operator channel's id
/// — one the stored record did not carry — must be refused too, not just
/// the agent-id case. `reserved_problems()`'s diff has to catch both
/// arms of `validate_with`'s reservation (agent id, desk id/name), or
/// this exact vulnerability survives at the desk arm even after the
/// agent arm is closed.
#[tokio::test]
async fn a_reboot_refuses_a_newly_introduced_reserved_desk_id() {
    let dir = tempfile::tempdir().unwrap();
    let safe = parse(
        r#"
        [company]
        name = "Acme"
        [[agent]]
        id = "ceo"
        role = "Chief"
        "#,
    );
    RuntimeBuilder::new(dir.path(), safe)
        .build()
        .await
        .expect("the first boot with a safe manifest must succeed and persist a record");

    let manifest = parse(
        r#"
        [company]
        name = "Acme"
        [[agent]]
        id = "ceo"
        role = "Chief"
        [[group_chat]]
        id = "operator"
        name = "Operator"
        members = ["ceo"]
        "#,
    );
    let err = RuntimeBuilder::new(dir.path(), manifest)
        .build()
        .await
        .expect_err(
            "editing company.toml to add a reserved desk id between two restarts must not \
             be excused just because a record already existed",
        );
    match err {
        crate::OpenCompanyError::ManifestInvalid { problems, .. } => {
            assert!(
                problems.iter().any(|p| p.contains("operator")),
                "expected a reserved-id problem, got: {problems:?}"
            );
        }
        other => panic!("expected ManifestInvalid, got {other}"),
    }
}
