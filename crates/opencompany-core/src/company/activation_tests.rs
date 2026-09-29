use super::*;
use crate::company::CompanyManifest;
use crate::ports::events::EventStreamItem;
use crate::ports::types::StoredEvent;
use crate::store::fs::{FsCompanyStore, FsEventLog};

/// An [`EventLog`] decorator that counts `read_from` calls — the seam
/// [`compute_and_latch`]'s journal scan goes through — so a test can
/// assert the scan was SKIPPED, not merely that its result didn't matter.
/// Delegates every method to a real backend so `append`/`read_from`
/// behave exactly as production does; only the count is synthetic.
struct CountingEventLog {
    inner: Arc<dyn EventLog>,
    read_calls: std::sync::atomic::AtomicUsize,
}

impl CountingEventLog {
    fn new(inner: Arc<dyn EventLog>) -> Self {
        Self {
            inner,
            read_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl EventLog for CountingEventLog {
    async fn append(&self, id: &CompanyId, event: CompanyEvent) -> Result<EventSeq> {
        self.inner.append(id, event).await
    }

    async fn read_from(
        &self,
        id: &CompanyId,
        seq: EventSeq,
        limit: usize,
    ) -> Result<Vec<StoredEvent>> {
        self.read_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.read_from(id, seq, limit).await
    }

    fn subscribe(&self, id: &CompanyId) -> futures::stream::BoxStream<'static, EventStreamItem> {
        self.inner.subscribe(id)
    }
}

/// A fresh filesystem-backed store + journal pair, rooted at a throwaway
/// tempdir — the same real [`CompanyStore`]/[`EventLog`] implementations
/// the running app uses, not a hand-rolled fake, so `read_from`/`append`
/// behave exactly as [`compute_and_latch`] will see them in production.
fn stores() -> (Arc<dyn CompanyStore>, Arc<dyn EventLog>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn CompanyStore> = Arc::new(FsCompanyStore::new(dir.path()));
    let events: Arc<dyn EventLog> = Arc::new(FsEventLog::new(dir.path()));
    (store, events, dir)
}

fn manifest(allow: &[&str]) -> CompanyManifest {
    let allow_line = allow
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    toml::from_str(&format!(
        "[company]\nname = \"Acme\"\n[tools]\nallow = [{allow_line}]\n"
    ))
    .expect("valid manifest")
}

fn record(id: &CompanyId, allow: &[&str]) -> CompanyRecord {
    CompanyRecord {
        general_channel: Default::default(),
        overlay_desk_hive: Vec::new(),
        id: id.clone(),
        manifest: manifest(allow),
        ledger: Vec::new(),
        lifecycle: "running".to_string(),
        overlay_agents: Vec::new(),
        overlay_desk_members: Vec::new(),
        overlay_desk_order: Vec::new(),
        overlay_desks: Vec::new(),
        overlay_workflows: Vec::new(),
        overlay_agent_edits: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_policy: None,
        overlay_tool_grants: None,
        overlay_desk_tools: Default::default(),
        overlay_budgets: Vec::new(),
        disabled_workflows: Vec::new(),
        template_provenance: None,
        setup: None,
        name_confirmed: false,
        activation_completed_at: None,
        created_at_millis: None,
    }
}

// --- derive_steps: every permutation of the three inputs -----------------

#[test]
fn no_steps_complete_when_nothing_is_true() {
    // `composio` granted so the integration step is a genuine unmet
    // requirement here rather than a waiver — see
    // `no_composio_grant_at_all_waives_the_integration_step` for that case.
    let r = record(&CompanyId::new("acme"), &["composio"]);
    let status = derive_steps(&r, Some(false), false);
    assert!(!status.name_confirmed);
    assert!(!status.integration_connected);
    assert!(!status.workflow_run_succeeded);
    assert!(!status.all_steps_complete());
    assert!(!status.is_activated());
}

#[test]
fn name_confirmed_alone_is_not_activation() {
    let mut r = record(&CompanyId::new("acme"), &[]);
    r.name_confirmed = true;
    let status = derive_steps(&r, Some(false), false);
    assert!(status.name_confirmed);
    assert!(!status.all_steps_complete());
}

#[test]
fn no_composio_grant_at_all_waives_the_integration_step() {
    // Issue #1850 review: several bundled companies deliberately never
    // grant `composio` at all (`companies/math_lab`,
    // `companies/product_team`, `companies/research_lab`,
    // `companies/openhuman_demo`, `companies/signals_opportunity_studio`)
    // — requiring a connection nobody in those companies could ever make
    // would permanently block activation for every one of them. When the
    // manifest cannot grant the namespace, the step waives (reads
    // vacuously complete) regardless of connection state.
    let r = record(&CompanyId::new("acme"), &["files", "docs", "shell"]);
    let status = derive_steps(&r, /* has_composio_connection */ Some(false), false);
    assert!(
        status.integration_connected,
        "a company that can never grant composio has no lever to complete this step — it must waive, not permanently block"
    );
}

#[test]
fn wildcard_grant_also_waives_the_step_since_it_never_confers_composio() {
    // `*` deliberately excludes `composio` (see `grants_composio_explicit`),
    // so a wildcard-only company is in exactly the same "can never grant
    // composio" position as a narrow allow-list that omits it. The live
    // connection here is a red herring — it still cannot be used by any
    // agent (namespace never granted), so it neither blocks nor is
    // required; the waiver applies the same as with no connection at all.
    let r = record(&CompanyId::new("acme"), &["*"]);
    let status = derive_steps(&r, /* has_composio_connection */ Some(true), false);
    assert!(status.integration_connected);
}

#[test]
fn composio_not_compiled_waives_the_step_even_when_the_manifest_grants_it() {
    // Issue #1850 review, finding 2: `cargo run --bin opencompany --
    // serve` — AGENTS.md's own documented default command — compiles no
    // `composio` feature, so `ops::activation::has_composio_connection`'s
    // `#[cfg(not(feature = "composio"))]` fallback resolves to `None` for
    // every company regardless of what its manifest grants. Before this
    // fix, `None` collapsed to "not connected" and this company — which
    // DOES grant `composio`, unlike the manifest-waiver tests above —
    // could never complete this step in that build. `None` must waive
    // unconditionally, the same as the manifest-level waiver, because the
    // build has no lever either.
    let r = record(&CompanyId::new("acme"), &["composio"]);
    let status = derive_steps(&r, /* has_composio_connection */ None, false);
    assert!(
        status.integration_connected,
        "a build with no Composio client compiled in has no lever to complete this step — it must waive, not permanently block a company that grants composio"
    );
}

#[test]
fn grant_without_a_connection_is_not_integration_connected() {
    let r = record(&CompanyId::new("acme"), &["composio"]);
    let status = derive_steps(&r, /* has_composio_connection */ Some(false), false);
    assert!(!status.integration_connected);
}

#[test]
fn connection_and_explicit_grant_together_complete_the_step() {
    let r = record(&CompanyId::new("acme"), &["composio"]);
    let status = derive_steps(&r, Some(true), false);
    assert!(status.integration_connected);
}

#[test]
fn dotted_composio_subgrant_also_counts() {
    let r = record(&CompanyId::new("acme"), &["composio.gmail"]);
    let status = derive_steps(&r, Some(true), false);
    assert!(status.integration_connected);
}

#[test]
fn all_three_steps_true_is_activation() {
    let mut r = record(&CompanyId::new("acme"), &["composio"]);
    r.name_confirmed = true;
    let status = derive_steps(&r, Some(true), true);
    assert!(status.all_steps_complete());
    assert!(status.is_activated());
}

// --- latch monotonicity ---------------------------------------------------

#[test]
fn latched_company_reads_activated_even_with_every_live_step_false() {
    // Issue #1843: a Composio connection disconnected AFTER activation must
    // not un-activate the company. `is_activated` must answer from the
    // latch, not by re-deriving the three steps. `composio` stays granted
    // here so `integration_connected` is a genuine live "false" (a real
    // disconnect) rather than a waiver — see the `_waives_` tests above
    // for that case.
    let mut r = record(&CompanyId::new("acme"), &["composio"]);
    r.activation_completed_at = Some(1_700_000_000_000);
    let status = derive_steps(&r, Some(false), false);
    assert!(
        !status.all_steps_complete(),
        "the live steps really are false"
    );
    assert!(
        status.is_activated(),
        "the latch alone must be enough — monotonicity"
    );
}

// --- compute_and_latch: the async orchestration ---------------------------

#[tokio::test]
async fn compute_and_latch_stamps_the_record_and_journals_once_all_steps_complete() {
    let id = CompanyId::new("acme");
    let (store, events, _dir) = stores();

    let mut r = record(&id, &["composio"]);
    r.name_confirmed = true;
    store.save(&r).await.unwrap();

    // The workflow-run-succeeded signal comes from the journal, not the
    // record — append a successful, non-cancelled `WorkflowRunFinished`.
    events
        .append(
            &id,
            CompanyEvent::WorkflowRunFinished {
                workflow_id: "digest".to_string(),
                scheduled: false,
                run_id: Some("run-1".to_string()),
                deliveries: Vec::new(),
                pending_approvals: Vec::new(),
                error: None,
                cancelled: false,
                notices: Vec::new(),
                board: Vec::new(),
                blocked_nodes: Vec::new(),
                approvals: Vec::new(),
            },
        )
        .await
        .unwrap();

    let status = compute_and_latch(&id, &store, &events, async || Some(true))
        .await
        .unwrap();
    assert!(status.is_activated());
    assert!(status.activation_completed_at.is_some());

    let reloaded = store.load(&id).await.unwrap().unwrap();
    assert!(
        reloaded.activation_completed_at.is_some(),
        "the latch must be durably persisted, not just returned"
    );
}

/// The end-to-end shape of the issue #1850 review finding: a company
/// whose manifest never grants `composio` at all (the
/// `math_lab`/`product_team` pattern) must still be able
/// to latch activation once its other two steps are true — the waived
/// integration step must not block `compute_and_latch` from ever
/// stamping the record for these company types.
#[tokio::test]
async fn compute_and_latch_activates_a_company_that_never_grants_composio() {
    let id = CompanyId::new("acme");
    let (store, events, _dir) = stores();

    let mut r = record(&id, &["files", "docs", "shell"]);
    r.name_confirmed = true;
    store.save(&r).await.unwrap();

    events
        .append(
            &id,
            CompanyEvent::WorkflowRunFinished {
                workflow_id: "digest".to_string(),
                scheduled: false,
                run_id: Some("run-1".to_string()),
                deliveries: Vec::new(),
                pending_approvals: Vec::new(),
                error: None,
                cancelled: false,
                notices: Vec::new(),
                board: Vec::new(),
                blocked_nodes: Vec::new(),
                approvals: Vec::new(),
            },
        )
        .await
        .unwrap();

    // `Some(false)` here, not `None`: this proves the MANIFEST-level
    // waiver specifically — a build that DOES have a Composio client
    // compiled in, querying a company that structurally never grants the
    // namespace. See `compute_and_latch_activates_when_composio_is_not_compiled`
    // below for the build-level waiver (`None`, the real
    // `#[cfg(not(feature = "composio"))]` fallback shape).
    let status = compute_and_latch(&id, &store, &events, async || Some(false))
        .await
        .unwrap();
    assert!(
        status.is_activated(),
        "a company that structurally cannot grant composio must still be able to activate"
    );
    assert!(status.activation_completed_at.is_some());

    let reloaded = store.load(&id).await.unwrap().unwrap();
    assert!(
        reloaded.activation_completed_at.is_some(),
        "the latch must be durably persisted, not just returned"
    );
}

/// The build-level counterpart to the test above (issue #1850 review,
/// finding 2): this company DOES grant `composio` — unlike the
/// never-grants-it fixture — so under the OLD `Option`-less signature this
/// scenario could never activate in a build with no Composio feature
/// compiled in. `None` from the closure is the real
/// `#[cfg(not(feature = "composio"))]` fallback shape
/// (`ops::activation::has_composio_connection`), not a stand-in for
/// `Some(false)`.
#[tokio::test]
async fn compute_and_latch_activates_when_composio_is_not_compiled() {
    let id = CompanyId::new("acme");
    let (store, events, _dir) = stores();

    // Grants `composio` explicitly — the case the manifest-level waiver
    // does NOT cover, so only the build-level (`None`) waiver can let this
    // company ever complete the step.
    let mut r = record(&id, &["composio"]);
    r.name_confirmed = true;
    store.save(&r).await.unwrap();

    events
        .append(
            &id,
            CompanyEvent::WorkflowRunFinished {
                workflow_id: "digest".to_string(),
                scheduled: false,
                run_id: Some("run-1".to_string()),
                deliveries: Vec::new(),
                pending_approvals: Vec::new(),
                error: None,
                cancelled: false,
                notices: Vec::new(),
                board: Vec::new(),
                blocked_nodes: Vec::new(),
                approvals: Vec::new(),
            },
        )
        .await
        .unwrap();

    let status = compute_and_latch(&id, &store, &events, async || None)
        .await
        .unwrap();
    assert!(
        status.is_activated(),
        "a company that grants composio must still activate in a build with no composio client compiled in"
    );
    assert!(status.activation_completed_at.is_some());

    let reloaded = store.load(&id).await.unwrap().unwrap();
    assert!(
        reloaded.activation_completed_at.is_some(),
        "the latch must be durably persisted, not just returned"
    );
}

// --- any_workflow_run_succeeded: verdict, not just error/cancelled ------

/// The exact regression this guards (issue #1850 review): a run that
/// blocked on a human carries `error: None, cancelled: false` — the same
/// shape as a run that actually finished — so checking only those two
/// fields let a blocked run complete this activation step. Routing
/// through [`WorkflowRunVerdict::of`] instead reads `blocked_nodes` and
/// scores it `Blocked`, not `Ok`.
#[tokio::test]
async fn a_blocked_run_does_not_count_as_succeeded() {
    let id = CompanyId::new("acme");
    let (_store, events, _dir) = stores();

    events
        .append(
            &id,
            CompanyEvent::WorkflowRunFinished {
                workflow_id: "digest".to_string(),
                scheduled: false,
                run_id: Some("run-1".to_string()),
                deliveries: Vec::new(),
                pending_approvals: Vec::new(),
                error: None,
                cancelled: false,
                notices: Vec::new(),
                board: Vec::new(),
                blocked_nodes: vec![crate::ports::WorkflowBlockedNode {
                    node_id: "fetch_invoice".to_string(),
                    tools: vec!["gmail".to_string()],
                    approval_ids: Vec::new(),
                    unparkable: 0,
                    stranded: 0,
                    blockers: 0,
                }],
                approvals: Vec::new(),
            },
        )
        .await
        .unwrap();

    assert!(!any_workflow_run_succeeded(&id, &events).await.unwrap());
}

/// Same shape, the other trigger: a run parked on an approval gate
/// (`pending_approvals` non-empty, no blocked node) is `AwaitingApproval`,
/// not `Ok`.
#[tokio::test]
async fn a_run_awaiting_approval_does_not_count_as_succeeded() {
    let id = CompanyId::new("acme");
    let (_store, events, _dir) = stores();

    events
        .append(
            &id,
            CompanyEvent::WorkflowRunFinished {
                workflow_id: "digest".to_string(),
                scheduled: false,
                run_id: Some("run-1".to_string()),
                deliveries: Vec::new(),
                pending_approvals: vec!["publish".to_string()],
                error: None,
                cancelled: false,
                notices: Vec::new(),
                board: Vec::new(),
                blocked_nodes: Vec::new(),
                approvals: Vec::new(),
            },
        )
        .await
        .unwrap();

    assert!(!any_workflow_run_succeeded(&id, &events).await.unwrap());
}

#[tokio::test]
async fn compute_and_latch_does_not_latch_when_a_step_is_still_missing() {
    let id = CompanyId::new("acme");
    let (store, events, _dir) = stores();
    store.save(&record(&id, &["composio"])).await.unwrap();

    // No workflow run journaled at all — the third step is missing.
    let status = compute_and_latch(&id, &store, &events, async || Some(true))
        .await
        .unwrap();
    assert!(!status.is_activated());

    let reloaded = store.load(&id).await.unwrap().unwrap();
    assert!(reloaded.activation_completed_at.is_none());
}

/// A real, successful workflow run — the same fixture
/// `compute_and_latch_stamps_the_record_and_journals_once_all_steps_complete`
/// journals — appended for a test's `id` before `compute_and_latch` is
/// called on it.
async fn journal_a_succeeded_workflow_run(events: &Arc<dyn EventLog>, id: &CompanyId) {
    events
        .append(
            id,
            CompanyEvent::WorkflowRunFinished {
                workflow_id: "digest".to_string(),
                scheduled: false,
                run_id: Some("run-1".to_string()),
                deliveries: Vec::new(),
                pending_approvals: Vec::new(),
                error: None,
                cancelled: false,
                notices: Vec::new(),
                board: Vec::new(),
                blocked_nodes: Vec::new(),
                approvals: Vec::new(),
            },
        )
        .await
        .unwrap();
}

/// PR #1875 review finding: an operator who ran a real workflow before
/// confirming the company's name must still see step 3 of
/// `OnboardingGate`'s checklist as done — the screen's own contract is
/// "three quick steps, in any order" — not held to `false` by a shortcut
/// meant only to keep the *latch* decision cheap (issue #1850 review,
/// finding 2). This replaces
/// `compute_and_latch_skips_the_journal_scan_when_name_is_not_confirmed`,
/// which asserted the now-corrected behavior.
#[tokio::test]
async fn compute_and_latch_reports_workflow_success_even_when_name_is_not_confirmed() {
    let id = CompanyId::new("acme");
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn CompanyStore> = Arc::new(FsCompanyStore::new(dir.path()));
    let counting = Arc::new(CountingEventLog::new(Arc::new(FsEventLog::new(dir.path()))));
    let events: Arc<dyn EventLog> = counting.clone();

    // `composio` granted and a live connection answered below, so
    // `integration_connected` reads true — `name_confirmed` (defaulted
    // false by the `record()` fixture) is the ONLY step still missing.
    store.save(&record(&id, &["composio"])).await.unwrap();
    journal_a_succeeded_workflow_run(&events, &id).await;

    let status = compute_and_latch(&id, &store, &events, async || Some(true))
        .await
        .unwrap();
    assert!(
        !status.is_activated(),
        "name_confirmed is still false — the funnel as a whole is not done"
    );
    assert!(
        status.workflow_run_succeeded,
        "the run genuinely succeeded and must project as done regardless of the other steps"
    );
    assert_eq!(
        counting
            .read_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the scan must run to answer the checklist item honestly"
    );
}

/// The companion case: `name_confirmed` true but `integration_connected`
/// still false must ALSO report a genuine workflow success — the finding
/// was that the old shortcut hid the same fact behind either missing
/// step, not only the first of those. Replaces
/// `compute_and_latch_skips_the_journal_scan_when_integration_is_not_connected`.
#[tokio::test]
async fn compute_and_latch_reports_workflow_success_even_when_integration_is_not_connected() {
    let id = CompanyId::new("acme");
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn CompanyStore> = Arc::new(FsCompanyStore::new(dir.path()));
    let counting = Arc::new(CountingEventLog::new(Arc::new(FsEventLog::new(dir.path()))));
    let events: Arc<dyn EventLog> = counting.clone();

    let mut r = record(&id, &["composio"]);
    r.name_confirmed = true;
    store.save(&r).await.unwrap();
    journal_a_succeeded_workflow_run(&events, &id).await;

    // No live connection — `integration_connected` reads false.
    let status = compute_and_latch(&id, &store, &events, async || Some(false))
        .await
        .unwrap();
    assert!(!status.is_activated());
    assert!(
        status.workflow_run_succeeded,
        "the run genuinely succeeded and must project as done regardless of the other steps"
    );
    assert_eq!(
        counting
            .read_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the scan must run to answer the checklist item honestly"
    );
}

/// The scan runs once both cheaper steps ARE true too — unconditional
/// now (see the two tests above), but worth its own case since this is
/// the ordinary path that actually completes activation.
#[tokio::test]
async fn compute_and_latch_still_scans_once_name_and_integration_are_both_true() {
    let id = CompanyId::new("acme");
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn CompanyStore> = Arc::new(FsCompanyStore::new(dir.path()));
    let counting = Arc::new(CountingEventLog::new(Arc::new(FsEventLog::new(dir.path()))));
    let events: Arc<dyn EventLog> = counting.clone();

    let mut r = record(&id, &["composio"]);
    r.name_confirmed = true;
    store.save(&r).await.unwrap();

    let status = compute_and_latch(&id, &store, &events, async || Some(true))
        .await
        .unwrap();
    assert!(
        !status.is_activated(),
        "no workflow run journaled yet — the third step is genuinely missing"
    );
    assert_eq!(
        counting
            .read_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "once the two cheaper steps are true, the scan must run to answer the third honestly"
    );
}

#[tokio::test]
async fn compute_and_latch_short_circuits_once_latched_even_if_a_step_regresses() {
    let id = CompanyId::new("acme");
    let (store, events, _dir) = stores();

    let mut r = record(&id, &[]); // no composio grant at all — a regression
    r.activation_completed_at = Some(1_700_000_000_000);
    store.save(&r).await.unwrap();

    // The journal is empty and the composio closure below always answers
    // `false` — every live step reads false, and yet the company must still
    // read as activated. The closure also proves the *other* half of the
    // short-circuit contract: an already-latched company must not pay for a
    // Composio round trip at all, so it counts its own invocations and
    // asserts zero — the regression this test now guards is exactly the one
    // `GET {scope}/activation` shipped (issue #1850 review): the endpoint
    // fetched the live connection state before ever checking the latch.
    let composio_calls = std::sync::atomic::AtomicUsize::new(0);
    let status = compute_and_latch(&id, &store, &events, async || {
        composio_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(false)
    })
    .await
    .unwrap();
    assert!(status.is_activated());
    assert_eq!(status.activation_completed_at, Some(1_700_000_000_000));
    assert_eq!(
        composio_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "an already-latched company must not query Composio"
    );
}
