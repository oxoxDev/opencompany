use super::tests_core::*;
use super::*;

/// A seed `[tools]` change clears the console grants — version control wins
/// when it speaks.
///
/// **The security half, and the sharper one.** This is the only overlay in
/// the product that widens capability, so a grant outliving a seed edit
/// would be a runtime grant surviving the operator revoking it in version
/// control: the named harm that makes `[tools]` seed-authoritative at all.
#[test]
fn a_changed_seed_tools_block_clears_the_grants() {
    let before = seed_tools(&["*", "chargebee"]);
    let revoked = seed_tools(&["*"]);
    assert!(
        carry_tool_grants_override(&before, &revoked, Some(&held_grants(&["paypal"]))).is_none(),
        "a seed that edited `[tools]` must clear the console grants"
    );

    // Widening the seed clears them too. The rule is "the seed spoke", not
    // "the seed got stricter" — an operator who edits `[tools]` at all has
    // turned their attention to the company's grant.
    let widened = seed_tools(&["*", "hosting"]);
    assert!(
        carry_tool_grants_override(&revoked, &widened, Some(&held_grants(&["paypal"]))).is_none()
    );
}

/// Any field of `[tools]` counts as the seed speaking, not just `allow`.
/// The Composio toolkit allowlist narrows what a granted namespace can
/// reach, so an edit to it that left a console grant standing would be the
/// same hole through a different field.
#[test]
fn every_tools_field_counts_as_the_seed_speaking() {
    let base = seed_tools(&["*"]);
    let mut narrowed = base.clone();
    narrowed.composio.toolkits = vec!["gmail".to_string()];
    assert!(
        carry_tool_grants_override(&base, &narrowed, Some(&held_grants(&["composio"]))).is_none()
    );
}

/// With no grants held there is nothing to carry, whatever the seed did.
#[test]
fn no_tool_grants_carry_nothing() {
    let before = seed_tools(&["*"]);
    let after = seed_tools(&["files"]);
    assert!(carry_tool_grants_override(&before, &before.clone(), None).is_none());
    assert!(carry_tool_grants_override(&before, &after, None).is_none());
}

/// The carry rule compares **seeds**, and the record's manifest is not one:
/// it is materialised seed-plus-grants. `seed_allow` is what recovers the
/// seed side, and without it a company with any console grant would report
/// "version control spoke" on its very first rebuild and lose the grant —
/// making the whole layer inert one restart after it was clicked.
#[test]
fn the_seed_is_recovered_from_the_materialised_manifest() {
    let seed = seed_tools(&["*"]);
    let held = held_grants(&["chargebee"]);
    // What the record actually stores after a grant: the fold.
    let mut materialised = seed.clone();
    materialised.allow.push("chargebee".to_string());

    assert_eq!(seed_allow(&materialised, Some(&held)).allow, seed.allow);
    assert!(
        carry_tool_grants_override(&seed_allow(&materialised, Some(&held)), &seed, Some(&held))
            .is_some(),
        "an untouched seed must keep the grant across a rebuild"
    );
}

/// A namespace the seed *now* grants on its own clears the override: the
/// subtraction makes the seed look changed, which is both the honest read
/// (version control did edit `[tools]`) and the safe one — the company
/// keeps the grant either way, and the console stops claiming credit for it.
#[test]
fn a_seed_that_adopts_the_grant_clears_the_override() {
    let held = held_grants(&["chargebee"]);
    let materialised = seed_tools(&["*", "chargebee"]);
    let next_seed = seed_tools(&["*", "chargebee"]);
    assert!(
        carry_tool_grants_override(
            &seed_allow(&materialised, Some(&held)),
            &next_seed,
            Some(&held)
        )
        .is_none()
    );
}

#[test]
fn merge_enabled_appends_overlay_only_ids() {
    let merged = merge_enabled_workflows(
        &["seed_one".to_string()],
        &[overlay("console_made"), overlay("also_console")],
    );
    assert_eq!(merged, vec!["seed_one", "console_made", "also_console"]);
}

#[test]
fn merge_enabled_dedupes_at_the_seed_position() {
    // `shared` is in both lists: it keeps its seed slot (first), and the
    // overlay does not append a second copy at the end.
    let merged = merge_enabled_workflows(
        &["shared".to_string(), "seed_only".to_string()],
        &[overlay("shared"), overlay("overlay_only")],
    );
    assert_eq!(merged, vec!["shared", "seed_only", "overlay_only"]);
}

#[test]
fn merge_enabled_first_boot_leaves_seed_unchanged() {
    let seed = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    assert_eq!(merge_enabled_workflows(&seed, &[]), seed);
}

#[test]
fn merge_enabled_preserves_order_and_dedupes_within_each_list() {
    let merged = merge_enabled_workflows(
        &["b".to_string(), "a".to_string(), "b".to_string()],
        &[overlay("z"), overlay("a"), overlay("z")],
    );
    assert_eq!(merged, vec!["b", "a", "z"]);
}

#[test]
fn merge_enabled_of_nothing_is_empty() {
    assert!(merge_enabled_workflows(&[], &[]).is_empty());
}

/// Issue #208: a workflow created at runtime through the real create path
/// (console `POST …/workflows` / orchestrator `create_workflow`) is still
/// enabled after the runtime is rebuilt on the same home dir — and the
/// `enabled_workflow_ids` accessor both REST `list_workflows` and the
/// GraphQL `Company.workflows` resolver read still reports it.
#[tokio::test]
async fn runtime_created_workflow_stays_enabled_across_a_rebuild() {
    let home_dir = tempfile::Builder::new()
        .prefix("oc-wf-enabled-")
        .tempdir()
        .expect("tempdir");
    let home = home_dir.path().to_path_buf();
    let manifest = wf_manifest("[workflows]\nenabled=[\"seeded_pipeline\"]\n");
    let id = CompanyId::new("acme");

    let runtime = RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    // The real writer: overlay body + enabled id in one save.
    crate::company::create_company_workflow(
        &id,
        None,
        runtime.store(),
        None,
        wf_draft("daily_digest", "Daily Digest"),
        None,
        None,
    )
    .await
    .unwrap();
    let created = runtime.store().load(&id).await.unwrap().unwrap();
    assert_eq!(
        created.manifest.workflows.enabled,
        vec!["seeded_pipeline", "daily_digest"]
    );
    drop(runtime);

    // Rebuild from the same seed manifest — the seed knows nothing about
    // `daily_digest`, so this is exactly the boot that used to lose it.
    let runtime = RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let rebuilt = runtime.store().load(&id).await.unwrap().unwrap();
    assert_eq!(
        rebuilt.manifest.workflows.enabled,
        vec!["seeded_pipeline", "daily_digest"],
        "the rebuild dropped the runtime-enabled workflow"
    );
    assert!(
        rebuilt
            .overlay_workflows
            .iter()
            .any(|w| w.id == "daily_digest"),
        "the graph body should be untouched by this fix"
    );
    // What the REST + GraphQL workflow lists actually read.
    assert_eq!(
        runtime.enabled_workflow_ids().await.unwrap(),
        vec!["seeded_pipeline", "daily_digest"]
    );
}

/// Issue #208: a record written during the bug era — overlay graph body
/// intact, its enabled id already wiped by an earlier restart — is healed
/// by the next rebuild, with no migration.
#[tokio::test]
async fn rebuild_reenables_a_bug_era_orphaned_overlay_body() {
    let home_dir = tempfile::Builder::new()
        .prefix("oc-wf-heal-")
        .tempdir()
        .expect("tempdir");
    let home = home_dir.path().to_path_buf();
    let manifest = wf_manifest("");
    let id = CompanyId::new("acme");

    let runtime = RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let store = runtime.store().clone();
    let mut record = store.load(&id).await.unwrap().unwrap();
    // Bug-era shape: body present, `enabled` clobbered back to the seed's.
    record.overlay_workflows.push(OverlayWorkflow {
        id: "orphaned".to_string(),
        toml: "id = \"orphaned\"\n".to_string(),
    });
    record.manifest.workflows.enabled.clear();
    store.save(&record).await.unwrap();
    drop(runtime);

    let runtime = RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    assert_eq!(
        runtime.enabled_workflow_ids().await.unwrap(),
        vec!["orphaned"],
        "an orphaned bug-era overlay body was not re-enabled"
    );
}

/// Issue #208: `[workflows].enabled` is the ONLY merged field. A
/// seed-authoritative field that diverged on the record — here a
/// runtime-granted tool, the case where record-wins would let privilege
/// outlive a seed rollback — is overwritten by the seed on rebuild.
#[tokio::test]
async fn rebuild_keeps_every_other_manifest_field_seed_authoritative() {
    let home_dir = tempfile::Builder::new()
        .prefix("oc-wf-seedwins-")
        .tempdir()
        .expect("tempdir");
    let home = home_dir.path().to_path_buf();
    let manifest = wf_manifest("[tools]\nallow=[\"memory.*\"]\n");
    let id = CompanyId::new("acme");

    let runtime = RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let store = runtime.store().clone();
    let mut record = store.load(&id).await.unwrap().unwrap();
    record.manifest.tools.allow.push("email.*".to_string());
    record.manifest.company.name = "Renamed At Runtime".to_string();
    store.save(&record).await.unwrap();
    drop(runtime);

    let runtime = RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let rebuilt = runtime.store().load(&id).await.unwrap().unwrap();
    assert_eq!(
        rebuilt.manifest.tools.allow,
        vec!["memory.*"],
        "a runtime tool grant survived a seed rollback"
    );
    assert_eq!(rebuilt.manifest.company.name, "Acme");
}

/// Issue #1796: a grant written through the **overlay** survives a rebuild,
/// where the raw manifest write above does not.
///
/// The two tests are a pair, and the pair is the design. A runtime write
/// straight into `record.manifest.tools.allow` is still discarded — the
/// seed-wins property is untouched — while a console grant, which is an
/// attributed operator decision the seed never spoke about, is carried and
/// re-folded. Without this the one-click grant would work until the next
/// restart and then silently revert, which is the dead end #1796 is about
/// with a delay attached.
#[tokio::test]
async fn a_console_tool_grant_survives_a_rebuild() {
    let home_dir = tempfile::Builder::new()
        .prefix("oc-tool-grant-rebuild-")
        .tempdir()
        .expect("tempdir");
    let home = home_dir.path().to_path_buf();
    let manifest = wf_manifest("[tools]\nallow=[\"*\"]\n");
    let id = CompanyId::new("acme");

    let runtime = RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let store = runtime.store().clone();
    let mut record = store.load(&id).await.unwrap().unwrap();
    assert!(
        !crate::company::grants_chargebee_explicit(&record.manifest.tools.allow),
        "the catch-all must not confer it to begin with"
    );
    record.overlay_tool_grants = Some(held_grants(&["chargebee"]));
    record.manifest.tools.allow = record.effective_tool_allow();
    store.save(&record).await.unwrap();
    drop(runtime);

    let runtime = RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let rebuilt = runtime.store().load(&id).await.unwrap().unwrap();
    assert!(
        crate::company::grants_chargebee_explicit(&rebuilt.manifest.tools.allow),
        "the console grant must survive the rebuild: {:?}",
        rebuilt.manifest.tools.allow
    );
    assert_eq!(
        rebuilt
            .overlay_tool_grants
            .as_ref()
            .map(|o| o.added.clone()),
        Some(vec!["chargebee".to_string()]),
        "and it must still be attributed to the operator, not to the seed"
    );
    // Folded exactly once, however many rebuilds run.
    assert_eq!(
        rebuilt
            .manifest
            .tools
            .allow
            .iter()
            .filter(|g| *g == "chargebee")
            .count(),
        1
    );
    drop(runtime);

    // And version control still wins when it speaks: a seed that edits
    // `[tools]` drops the console grant wholesale.
    let runtime = RuntimeBuilder::new(home, wf_manifest("[tools]\nallow=[\"files\"]\n"))
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let after_seed_edit = runtime.store().load(&id).await.unwrap().unwrap();
    assert_eq!(after_seed_edit.manifest.tools.allow, vec!["files"]);
    assert!(after_seed_edit.overlay_tool_grants.is_none());
}

/// The console grant reaches the **tool provider's** grant list, which is
/// what `call_tool` enforces against (issue #1796).
///
/// This is the reader the first shape of the fold missed, and missing it was
/// worse than not shipping the feature. `effective_grants(&self.manifest)`
/// runs near the top of `build`, ~800 lines ahead of where the overlays used
/// to load, so folding into a local clone at the save site left the provider
/// holding the seed's list — permanently, since a rebuild re-parses
/// `company.toml`. The operator would see every console surface report
/// "granted" while the very next tool call was refused as ungranted.
///
/// Asserted through `effective_grants` on the runtime's own manifest rather
/// than by poking the provider, because that function IS the provider's
/// input: `build` passes its result straight to `StubToolProvider::new`.
#[tokio::test]
async fn a_console_tool_grant_reaches_the_grant_list_the_provider_enforces() {
    let home_dir = tempfile::Builder::new()
        .prefix("oc-tool-grant-provider-")
        .tempdir()
        .expect("tempdir");
    let home = home_dir.path().to_path_buf();
    // A catch-all company: `*` covers shell/code/web and confers none of the
    // namespaces this layer deals in, which is the manifest shape the issue
    // was reported against.
    let manifest = wf_manifest("[tools]\nallow=[\"*\"]\n");
    let id = CompanyId::new("acme");

    let runtime = RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let store = runtime.store().clone();
    assert!(
        !crate::company::grants_search_explicit(&effective_grants(&manifest)),
        "the catch-all must not confer it to begin with"
    );

    // Exactly what `PUT …/tools/grants` writes.
    let mut record = store.load(&id).await.unwrap().unwrap();
    record.overlay_tool_grants = Some(held_grants(&["search"]));
    record.manifest.tools.allow = record.effective_tool_allow();
    store.save(&record).await.unwrap();
    drop(runtime);

    let runtime = RuntimeBuilder::new(home, manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let rebuilt = runtime.store().load(&id).await.unwrap().unwrap();

    // The record — the readers that were already right.
    assert!(crate::company::grants_search_explicit(
        &rebuilt.manifest.tools.allow
    ));
    // And the grant list the provider is constructed from, which is the one
    // that was wrong. `build` computes this from `self.manifest`, so this
    // fails unless the fold reached the source rather than a local clone.
    assert!(
        crate::company::grants_search_explicit(&effective_grants(&rebuilt.manifest)),
        "the provider's grant list must carry the console grant: {:?}",
        effective_grants(&rebuilt.manifest)
    );
}

/// Issue #1844: once `name_confirmed` is set, the *record's* name — not
/// the seed's — survives a rebuild. The mirror image of the test just
/// above: before confirmation the seed wins (asserted there), and after
/// confirmation the record does, which is the whole point of gating the
/// carry on the flag rather than always preferring one side.
#[tokio::test]
async fn rebuild_carries_forward_a_confirmed_company_name() {
    let home_dir = tempfile::Builder::new()
        .prefix("oc-wf-namecarry-")
        .tempdir()
        .expect("tempdir");
    let home = home_dir.path().to_path_buf();
    let manifest = wf_manifest("");
    let id = CompanyId::new("acme");

    let runtime = RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let store = runtime.store().clone();
    let mut record = store.load(&id).await.unwrap().unwrap();
    record.manifest.company.name = "Operator Chosen Name".to_string();
    record.name_confirmed = true;
    store.save(&record).await.unwrap();
    drop(runtime);

    let runtime = RuntimeBuilder::new(home.clone(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let rebuilt = runtime.store().load(&id).await.unwrap().unwrap();
    assert_eq!(
        rebuilt.manifest.company.name, "Operator Chosen Name",
        "a confirmed name must survive a redeploy, not revert to company.toml's seed value"
    );
    assert!(rebuilt.name_confirmed);
}

/// Issue #1843/#1844: a company that predates activation tracking and was
/// already `running` at its next boot is back-filled as activated —
/// `name_confirmed` included — rather than gated behind an onboarding
/// screen it has no memory of starting. See `RuntimeBuilder::build`'s own
/// migration comment for why `running` (not `existing.is_some()` alone) is
/// the condition: a paused/archived company is left exactly as recorded.
#[tokio::test]
async fn rebuild_backfills_activation_for_a_pre_existing_running_company() {
    let home_dir = tempfile::Builder::new()
        .prefix("oc-wf-backfill-")
        .tempdir()
        .expect("tempdir");
    let home = home_dir.path().to_path_buf();
    let manifest = wf_manifest("");
    let id = CompanyId::new("acme");

    // Simulate a bundle written before activation tracking existed:
    // company.toml + a meta.json that has never gone near the
    // `activation_gate_seen` marker (`#[serde(default)]` reads its
    // absence as `false`), with the company already `running`. Written
    // as raw files rather than through `FsCompanyStore::save` —
    // `save` always stamps `activation_gate_seen: true` now (every save
    // in this build is activation-aware by definition), which would
    // defeat the one thing this fixture needs to be true: a bundle NO
    // save from this build has ever touched.
    let bundle = crate::store::Bundle::new(home.clone(), &id);
    tokio::fs::create_dir_all(bundle.dir()).await.unwrap();
    let toml_src = toml::to_string(&manifest).unwrap();
    tokio::fs::write(bundle.company_toml(), toml_src)
        .await
        .unwrap();
    tokio::fs::write(bundle.meta_json(), r#"{"lifecycle":"running"}"#)
        .await
        .unwrap();

    let runtime = RuntimeBuilder::new(home, manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let rebuilt = runtime.store().load(&id).await.unwrap().unwrap();
    assert!(
        rebuilt.name_confirmed,
        "a pre-existing running company must be back-filled as named"
    );
    assert!(
        rebuilt.activation_completed_at.is_some(),
        "and as activated — it has been operating all along"
    );
}

/// PR #1875 review finding, two rounds on the same scenario: a legacy
/// pre-#1843 record that is `paused` (or `archived`) at its first
/// post-upgrade boot is deliberately left un-migrated by the "existing
/// but not running" arm above.
///
/// Round one's bug was premature: an unconditional `save` used to stamp
/// `activation_gate_seen: true` regardless, both at the end of `build`
/// and from a bare lifecycle transition (`CompanyRuntime::set_lifecycle`,
/// the console's pause/resume control) — poisoning the tiebreaker before
/// any migration had actually run, which permanently blocked the
/// grandfather arm's `!gate_already_seen` guard from ever firing again.
/// The fix at the time made `set_lifecycle` a pure passthrough for the
/// marker: never touch it, so a later `build()` could still decide.
///
/// Round two's bug was the opposite failure mode of that same fix: a
/// company already registered in `state.registry()` never goes through
/// another `build()` across pause/resume — `server/provision.rs`'s
/// `transition` calls straight into the live runtime's `set_lifecycle`.
/// On a long-lived hosted tenant process, "its own next running boot"
/// might be days away, so a passthrough-only fix left an established
/// operator staring at the onboarding gate for the rest of that
/// process's uptime. The real fix is for `set_lifecycle` itself to make
/// the same decision the grandfather arm makes — gated on the identical
/// `!gate_already_seen` (and unset-latch) condition, so it still cannot
/// fire on a genuinely new company mid-onboarding — the moment a resume
/// puts an unmigrated record back to `running`, rather than only ever
/// forwarding the marker untouched.
#[tokio::test]
async fn a_paused_legacy_company_is_grandfathered_the_moment_it_resumes() {
    let home_dir = tempfile::Builder::new()
        .prefix("oc-wf-paused-backfill-")
        .tempdir()
        .expect("tempdir");
    let home = home_dir.path().to_path_buf();
    let manifest = wf_manifest("");
    let id = CompanyId::new("acme");

    // Same legacy-fixture shape as
    // `rebuild_backfills_activation_for_a_pre_existing_running_company`
    // (raw files, not `FsCompanyStore::save`, for the same reason given
    // there), except `paused` rather than `running`.
    let bundle = crate::store::Bundle::new(home.clone(), &id);
    tokio::fs::create_dir_all(bundle.dir()).await.unwrap();
    let toml_src = toml::to_string(&manifest).unwrap();
    tokio::fs::write(bundle.company_toml(), toml_src)
        .await
        .unwrap();
    tokio::fs::write(bundle.meta_json(), r#"{"lifecycle":"paused"}"#)
        .await
        .unwrap();

    // Boot 1: `paused`, so the "existing but not running" arm leaves the
    // record un-migrated — and must leave the gate marker unseen too.
    // Unaffected by round two's fix: `set_lifecycle` is never called
    // here, only `build`.
    let runtime = RuntimeBuilder::new(home.clone(), manifest.clone())
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let store = runtime.store().clone();
    assert!(
        !store.activation_gate_seen(&id).await.unwrap(),
        "an unmigrated paused legacy record must not be marked gate-seen \
         by an ordinary rebuild"
    );

    // Resume: the console's pause/resume control, on the same
    // already-registered runtime — no rebuild in between, exactly the
    // in-place-resume shape round two's finding describes.
    use crate::ports::types::{Actor, ActorKind};
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

    // Round two's fix: the resume itself must grandfather the record
    // immediately — an established operator must not see the onboarding
    // gate reappear for however long this process happens to stay up.
    assert!(
        store.activation_gate_seen(&id).await.unwrap(),
        "a resume that puts an unmigrated legacy record back to `running` \
         must grandfather it in place, not leave it waiting for a restart \
         that may not come for a long time on a long-lived process"
    );
    let resumed = store.load(&id).await.unwrap().unwrap();
    assert!(
        resumed.name_confirmed,
        "the grandfathered record must read as name-confirmed immediately \
         after resume"
    );
    assert!(
        resumed.activation_completed_at.is_some(),
        "the grandfathered record must read as activated immediately \
         after resume, not only after the next full boot"
    );
    drop(runtime);

    // Boot 2: already grandfathered by the resume above, so a rebuild
    // must simply carry that forward — the "already latched" arm, not
    // the grandfather arm, now applies.
    let runtime = RuntimeBuilder::new(home, manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let rebuilt = runtime.store().load(&id).await.unwrap().unwrap();
    assert!(rebuilt.name_confirmed);
    assert!(rebuilt.activation_completed_at.is_some());
    assert_eq!(
        rebuilt.activation_completed_at, resumed.activation_completed_at,
        "a rebuild after the resume must carry the latch forward untouched, \
         not recompute a fresh timestamp"
    );
    assert!(runtime.store().activation_gate_seen(&id).await.unwrap());
}
