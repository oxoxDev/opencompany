//! `built_in`'s own inline tests, part 6 of 10. Split out of the
//! single inline `mod tests` block because it exceeded the 750-line file
//! limit; grouped in the original file's order, not by topic (the block
//! covered dozens of unrelated issues with no existing topical boundaries).
//! Shared setup lives in [`super::built_in_test_fixtures`] and
//! [`super::built_in_test_fixtures_2`].

use super::built_in_test_fixtures::*;
use super::built_in_test_fixtures_2::skill_scratch;
use super::built_in_test_fixtures_2::*;
use super::*;
use crate::harness::provider::MockProvider;

/// This file's own default `park()` call site — the top-level turn, not
/// a delegated re-park — stamps the marker with the ambient
/// `RedeemContext` a cycle sets around it (issue #1846 review, Codex
/// #3865812419/#3865812423/#3865812432). Same fixture as the test
/// above, wrapped in `with_redeem_context` the way
/// `CycleRunner::run_bracketed` does in production, with a non-default
/// parent/deliverable/mentions to prove they land on the marker instead
/// of being silently dropped the way the pre-fix `redeem_budget_pause`
/// dropped them on the OTHER side of a redeem.
#[tokio::test]
async fn a_top_level_budget_pause_parks_the_ambient_redeem_context() {
    use crate::ports::types::{Attachment, EventSeq, Mention, MentionTarget, MessageIntent};

    let dir = tempfile::tempdir().expect("tempdir");
    let company = CompanyId::new("acme-budget-redeem-context");
    let mut rec = record();
    rec.id = company.clone();
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(ScriptedProvider::new(vec![
            Err(
                "USER_INSUFFICIENT_CREDITS: insufficient budget for this account — add \
                 credits to continue"
                    .to_string(),
            );
            10
        ])),
        provider_slug: "scripted".to_string(),
        serves: None,
        context: Arc::new(MockContext::default()),
        store: Arc::new(RecordingStore::default()),
        meter: None,
        workspace_root: dir.path().to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.path().to_path_buf(),
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
        delegations: DelegationQueue::default(),
        workflow_runner: crate::harness::orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_runs: None,
        deep_trace: None,
        workflow_revisions: None,
        approval_requests: ApprovalRequestQueue::default(),
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
        run_supervisor: crate::runtime::RunSupervisor::default(),
        delivery: None,
        search: None,
        tenant_search: None,
        workspace: None,
    };

    let pool = HarnessPool::new();
    pool.ensure(&rec, &deps).await.expect("pool ensures");

    // Issue #1846 review (Codex #3866418891): `text`/`attachments` are the
    // same "raw operator message" pair `park_message` prefers over this
    // turn's own COMPOSED `message` — assert they reach the marker
    // through this top-level call site too, not just the direct
    // `budget_pauses_for(...).park(...)` unit test in `budget_pause.rs`.
    let redeem = crate::runtime::grants::RedeemContext {
        parent: Some(EventSeq::new(42)),
        deliverable: Some(MessageIntent::Workflow),
        mentions: vec![Mention {
            target: MentionTarget::Agent {
                id: "researcher".to_string(),
            },
            text: "@researcher".to_string(),
            offset: 0,
            quiet: false,
        }],
        text: Some("@researcher please summarize the attached standup notes.".to_string()),
        attachments: vec![Attachment {
            node_id: "node-top-level-1".to_string(),
            name: "standup-notes.txt".to_string(),
            mime: "text/plain".to_string(),
            size: 512,
            extracted_text: Some("stand-up highlights".to_string()),
        }],
    };

    crate::runtime::grants::with_redeem_context(redeem.clone(), async {
        pool.run(
            &company,
            "ceo",
            "@researcher please summarize today's standup notes.",
            &deps,
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await
        .expect("a budget pause is a graceful stop, not an error")
    })
    .await;

    let marker = crate::runtime::grants::budget_pauses_for(&company)
        .peek("ceo")
        .expect("a re-issue marker must be parked for the paused agent");
    assert_eq!(
        marker.parent, redeem.parent,
        "the marker must carry the ambient cycle's thread parent"
    );
    assert_eq!(
        marker.deliverable, redeem.deliverable,
        "the marker must carry the ambient cycle's deliverable choice"
    );
    assert_eq!(
        marker.mentions, redeem.mentions,
        "the marker must carry the ambient cycle's resolved mentions"
    );
    assert_eq!(
        marker.message,
        redeem.text.clone().unwrap(),
        "the marker must carry the ambient context's RAW text, not this turn's own \
         composed message"
    );
    assert_eq!(
        marker.attachments, redeem.attachments,
        "the marker must carry the ambient context's structured attachments"
    );
}

/// No-regression on the delegated path: a turn that finishes normally
/// (the `ScriptedProvider` returns `Ok`, never an `Err`) reports no
/// budget pause and parks no marker — the negative control that stops a
/// hardcoded `Some`/an always-park bug passing every test above.
#[tokio::test]
async fn a_turn_that_finishes_normally_reports_no_budget_pause_and_parks_no_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let company = CompanyId::new("acme-budget-noregress");
    let mut rec = record();
    rec.id = company.clone();
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(ScriptedProvider::new(vec![Ok(
            "Standup notes: all green.".to_string()
        )])),
        provider_slug: "scripted".to_string(),
        serves: None,
        context: Arc::new(MockContext::default()),
        store: Arc::new(RecordingStore::default()),
        meter: None,
        workspace_root: dir.path().to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.path().to_path_buf(),
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
        delegations: DelegationQueue::default(),
        workflow_runner: crate::harness::orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_runs: None,
        deep_trace: None,
        workflow_revisions: None,
        approval_requests: ApprovalRequestQueue::default(),
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
        run_supervisor: crate::runtime::RunSupervisor::default(),
        delivery: None,
        search: None,
        tenant_search: None,
        workspace: None,
    };

    let pool = HarnessPool::new();
    pool.ensure(&rec, &deps).await.expect("pool ensures");

    let outcome = pool
        .run(
            &company,
            "ceo",
            "How did standup go?",
            &deps,
            crate::runtime::delegation::ChatTarget::default(),
        )
        .await
        .expect("a normal turn returns Ok");

    assert!(
        outcome.budget_paused.is_none(),
        "a turn that never hit an Err must report no budget pause"
    );
    assert!(
        crate::runtime::grants::budget_pauses_for(&company)
            .peek("ceo")
            .is_none(),
        "and must park no re-issue marker"
    );
}

/// A console-added MCP server reaches the agent on the NEXT `ensure`, with no
/// restart — the roster rebuilds because the effective set, re-resolved from
/// the LIVE secret store (not the boot snapshot), changed its fingerprint.
/// This is the Parallel-Search / BrowserBase freshness bug proven end-to-end,
/// and the CI guard for issue #566: the effective-MCP fingerprint is a *term*
/// of [`HarnessPool::ensure`]'s staleness check. Both directions are pinned —
/// an unchanged set holds the fingerprint (no needless rebuild), an MCP-only
/// change moves it (rebuilt in place, without a restart). A refactor that
/// drops the term makes the post-change `ensure` early-return without storing
/// the new fingerprint: the value stops moving across the mutation and the
/// `assert_ne!` fails, rather than the restart requirement quietly returning.
#[tokio::test]
async fn ensure_rebuilds_when_a_runtime_mcp_server_is_added() {
    let secrets: Arc<dyn SecretStore> = Arc::new(MemSecrets::default());
    let dir = tempfile::tempdir().unwrap();
    let deps = HarnessDeps {
        takeovers: Default::default(),
        emergency_gate: None,
        notifications: None,
        ledgers: None,
        ledger_registry: Default::default(),
        provider: Arc::new(MockProvider::new("mock: ")),
        provider_slug: "mock".to_string(),
        serves: None,
        context: Arc::new(MockContext::default()),
        store: Arc::new(RecordingStore::default()),
        meter: None,
        workspace_root: dir.path().to_path_buf(),
        mcp_home: None,
        workspace_git_enabled: false,
        audit_root: dir.path().to_path_buf(),
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
        delegations: DelegationQueue::default(),
        workflow_runner: crate::harness::orchestrator::WorkflowRunnerHandle::default(),
        mcp_failures: McpFailureQueue::default(),
        pending_publishes: crate::harness::publish::PendingPublishQueue::default(),
        workflow_refs: crate::harness::workflow_refs::WorkflowRefQueue::default(),
        run_outputs: crate::harness::orchestrator::RunOutputCache::default(),
        run_output_store: None,
        workflow_runs: None,
        deep_trace: None,
        workflow_revisions: None,
        approval_requests: ApprovalRequestQueue::default(),
        approval_parker: None,
        secrets: Some(secrets.clone()),
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
        run_supervisor: crate::runtime::RunSupervisor::default(),
        delivery: None,
        search: None,
        tenant_search: None,
        workspace: None,
    };
    let pool = HarnessPool::new();
    let rec = record();

    pool.ensure(&rec, &deps).await.expect("first ensure");
    let before = pool
        .mcp_fingerprint_of(&rec.id)
        .await
        .expect("fingerprinted");

    // Stability direction: with no axis changed, a redundant `ensure` is a
    // no-op — the gate reuses the cached roster and the fingerprint holds, so
    // the change-direction assertion below can't pass by coincidence.
    pool.ensure(&rec, &deps).await.expect("redundant ensure");
    assert_eq!(
        pool.mcp_fingerprint_of(&rec.id).await,
        Some(before),
        "an unchanged MCP set must not move the fingerprint"
    );

    // Console-add a runtime MCP server directly into the live secret store.
    crate::company::mcp::save_runtime_index(
        &rec.id,
        secrets.as_ref(),
        &[crate::company::McpServer {
            name: "browserbase".into(),
            endpoint: "https://api.browserbase.com/mcp".into(),
            description: None,
            command: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            read_only_tools: Vec::new(),
            timeout_secs: 30,
            enabled: true,
            auth_secret: None,
        }],
    )
    .await
    .unwrap();

    // Change direction: the next ensure re-resolves from the live store →
    // fingerprint changes → roster rebuilt, so the new server reaches the
    // agent without a restart.
    pool.ensure(&rec, &deps).await.expect("post-add ensure");
    let after = pool
        .mcp_fingerprint_of(&rec.id)
        .await
        .expect("fingerprinted");
    assert_ne!(
        before, after,
        "an MCP-only change must move the staleness fingerprint (issue #566)"
    );
    assert_eq!(
        pool.resident_companies().await,
        1,
        "same company, rebuilt in place — not a new residency"
    );

    // Stability after the change too: a further ensure with no new change is
    // a no-op and the fingerprint holds at its post-change value.
    pool.ensure(&rec, &deps).await.expect("final no-op ensure");
    assert_eq!(pool.mcp_fingerprint_of(&rec.id).await, Some(after));
}

/// A company-only managed backend has no deployment handle to clone. The
/// pool must therefore retain its base handle so roster rebuilds keep the
/// same daily-call ledger instead of reopening the cap.
#[tokio::test]
async fn company_only_managed_search_keeps_its_ledger_across_resolution() {
    let secrets: Arc<dyn SecretStore> = Arc::new(MemSecrets::default());
    let dir = tempfile::tempdir().unwrap();
    let mut deps = deps_with_plan(dir.path(), Arc::new(MockContext::default()), None, None);
    deps.secrets = Some(secrets.clone());
    let mut rec = record();
    rec.manifest.tools.allow = vec!["search".to_string()];
    secrets
        .set(
            &rec.id,
            crate::company::search::MANAGED_KEY_SECRET,
            crate::ports::types::SecretValue("company-key".to_string()),
        )
        .await
        .unwrap();

    let pool = HarnessPool::new();
    let first = pool
        .resolve_managed_search(&rec, &deps, None)
        .await
        .expect("company key creates a managed backend");
    first.ledger().try_reserve(&rec.id, 10, 1).unwrap();

    let second = pool
        .resolve_managed_search(&rec, &deps, None)
        .await
        .expect("same backend resolves again");
    assert_eq!(second.ledger().used_today(&rec.id, 1), 1);
}

/// Saving or rotating a key in Settings → Billing must reach the agent on
/// its next turn.
///
/// The fingerprint is the observable that makes "no restart" testable: a
/// credential that fails to move it leaves the roster cached, and the agent
/// keeps authenticating with the old key — or holds no billing tools at all
/// — until the process restarts. That failure is invisible from the tool
/// list alone, which is why this asserts the fingerprint directly.
#[tokio::test]
#[cfg(feature = "chargebee")]
async fn ensure_rebuilds_when_a_chargebee_credential_is_saved_or_rotated() {
    use crate::chargebee::types::{API_KEY_SECRET, SITE_SECRET};

    let secrets: Arc<dyn SecretStore> = Arc::new(MemSecrets::default());
    let dir = tempfile::tempdir().unwrap();
    let mut deps = deps_with_plan(dir.path(), Arc::new(MockContext::default()), None, None);
    deps.secrets = Some(secrets.clone());

    // The explicit grant is what opens this axis. A `*` wildcard does not
    // confer it — see the module docs.
    let mut rec = record();
    rec.manifest.tools.allow = vec!["chargebee".to_string()];

    let write = |key: &'static str, value: &'static str| {
        let secrets = secrets.clone();
        async move {
            secrets
                .set(
                    &CompanyId::new("acme"),
                    key,
                    crate::ports::types::SecretValue(value.to_string()),
                )
                .await
                .expect("write secret");
        }
    };

    let pool = HarnessPool::new();
    pool.ensure(&rec, &deps).await.expect("first ensure");
    let unset = pool
        .billing_fingerprint_of(&rec.id)
        .await
        .expect("fingerprint");

    // Stability first, so every change assertion below cannot pass by
    // coincidence.
    pool.ensure(&rec, &deps).await.expect("redundant ensure");
    assert_eq!(
        pool.billing_fingerprint_of(&rec.id).await,
        Some(unset),
        "an unchanged credential must not move the fingerprint"
    );

    // Half a credential is not a connection, so it must not move either —
    // the pair is meaningless apart.
    write(SITE_SECRET, "acme-test").await;
    pool.ensure(&rec, &deps).await.expect("half ensure");
    assert_eq!(
        pool.billing_fingerprint_of(&rec.id).await,
        Some(unset),
        "a site with no key is still no connection"
    );

    // Connect.
    write(API_KEY_SECRET, "cb_first").await;
    pool.ensure(&rec, &deps).await.expect("post-connect ensure");
    let connected = pool
        .billing_fingerprint_of(&rec.id)
        .await
        .expect("fingerprint");
    assert_ne!(unset, connected, "saving a credential must rebuild");

    // Rotate: same site, new key. This is the one a fingerprint over the
    // site alone would miss, leaving the agent on the revoked key.
    write(API_KEY_SECRET, "cb_rotated").await;
    pool.ensure(&rec, &deps).await.expect("post-rotate ensure");
    let rotated = pool
        .billing_fingerprint_of(&rec.id)
        .await
        .expect("fingerprint");
    assert_ne!(
        connected, rotated,
        "a rotation must rebuild even though the site is identical"
    );

    // Disconnect.
    write(API_KEY_SECRET, "").await;
    pool.ensure(&rec, &deps).await.expect("post-clear ensure");
    assert_eq!(
        pool.billing_fingerprint_of(&rec.id).await,
        Some(unset),
        "clearing the key must land back on the unconnected fingerprint"
    );
    assert_eq!(
        pool.resident_companies().await,
        1,
        "same company, rebuilt in place — not a new residency"
    );
}

/// A company that does not explicitly grant `chargebee` never reads the
/// billing secrets, so this axis is inert for it — and a credential sitting
/// in its store confers nothing. Fail closed, as the module docs promise.
#[tokio::test]
#[cfg(feature = "chargebee")]
async fn a_company_without_the_chargebee_grant_never_moves_on_this_axis() {
    use crate::chargebee::types::{API_KEY_SECRET, SITE_SECRET};

    let secrets: Arc<dyn SecretStore> = Arc::new(MemSecrets::default());
    let dir = tempfile::tempdir().unwrap();
    let mut deps = deps_with_plan(dir.path(), Arc::new(MockContext::default()), None, None);
    deps.secrets = Some(secrets.clone());

    // A wildcard, deliberately: it must NOT confer billing.
    let mut rec = record();
    rec.manifest.tools.allow = vec!["*".to_string()];

    let pool = HarnessPool::new();
    pool.ensure(&rec, &deps).await.expect("first ensure");
    let before = pool
        .billing_fingerprint_of(&rec.id)
        .await
        .expect("fingerprint");

    for (key, value) in [(SITE_SECRET, "acme-test"), (API_KEY_SECRET, "cb_key")] {
        secrets
            .set(
                &CompanyId::new("acme"),
                key,
                crate::ports::types::SecretValue(value.to_string()),
            )
            .await
            .expect("write secret");
    }

    pool.ensure(&rec, &deps).await.expect("post-write ensure");
    assert_eq!(
        pool.billing_fingerprint_of(&rec.id).await,
        Some(before),
        "an ungranted company must not read the billing secrets, let alone rebuild on them"
    );
}

/// The regression: a skill authored in the console after the first roster
/// build reaches the agent on the NEXT `ensure` — the fingerprint changes,
/// the roster rebuilds in place, and the skill's `SKILL.md` materializes —
/// even though MCP / overlay / capability / composio are all unchanged.
#[tokio::test]
async fn ensure_rebuilds_when_a_custom_skill_is_authored() {
    let skills = Arc::new(MemSkills::default());
    let mut fx = fixture();
    fx.deps.skills = Some(skills.clone());
    let ws = fx._dir.path().to_path_buf();
    let pool = HarnessPool::new();
    let rec = record();

    pool.ensure(&rec, &fx.deps).await.expect("first ensure");
    let before = pool
        .skill_fingerprint_of(&rec.id)
        .await
        .expect("fingerprinted");
    assert!(
        !skill_scratch(&ws, "standup-digest").exists(),
        "no skill authored yet"
    );

    // Author a custom skill in the "console" (the live store) — no restart.
    skills
        .set(&rec.id, &custom_skill("standup-digest", true, STANDUP_MD))
        .await
        .unwrap();

    pool.ensure(&rec, &fx.deps).await.expect("second ensure");
    let after = pool
        .skill_fingerprint_of(&rec.id)
        .await
        .expect("fingerprinted");
    assert_ne!(
        before, after,
        "authoring a skill must change the fingerprint"
    );
    assert_eq!(
        pool.resident_companies().await,
        1,
        "same company, rebuilt in place"
    );
    assert!(
        skill_scratch(&ws, "standup-digest").is_file(),
        "the authored skill must surface to the agent with no restart"
    );

    // A third ensure with no change is a no-op (fingerprint stable).
    pool.ensure(&rec, &fx.deps).await.expect("third ensure");
    assert_eq!(pool.skill_fingerprint_of(&rec.id).await, Some(after));
}

/// An unchanged delta set across two `ensure` calls keeps the fingerprint
/// stable and reuses the cached roster (the common fast path).
#[tokio::test]
async fn ensure_skill_fast_path_is_stable() {
    let skills = Arc::new(MemSkills::default());
    let rec = record();
    skills
        .set(&rec.id, &custom_skill("standup-digest", true, STANDUP_MD))
        .await
        .unwrap();
    let mut fx = fixture();
    fx.deps.skills = Some(skills.clone());
    let pool = HarnessPool::new();

    pool.ensure(&rec, &fx.deps).await.expect("first ensure");
    let first = pool.skill_fingerprint_of(&rec.id).await.unwrap();
    pool.ensure(&rec, &fx.deps).await.expect("second ensure");
    let second = pool.skill_fingerprint_of(&rec.id).await.unwrap();
    assert_eq!(
        first, second,
        "unchanged deltas keep the fingerprint stable"
    );
    assert_eq!(
        pool.resident_companies().await,
        1,
        "roster reused, not grown"
    );
}

/// Disabling a skill in the console drops it from the rebuilt scratch tree
/// on the next `ensure` (fingerprint moves, `SKILL.md` gone).
#[tokio::test]
async fn ensure_rebuilds_when_a_skill_is_disabled() {
    let skills = Arc::new(MemSkills::default());
    let rec = record();
    skills
        .set(&rec.id, &custom_skill("standup-digest", true, STANDUP_MD))
        .await
        .unwrap();
    let mut fx = fixture();
    fx.deps.skills = Some(skills.clone());
    let ws = fx._dir.path().to_path_buf();
    let pool = HarnessPool::new();

    pool.ensure(&rec, &fx.deps).await.expect("first ensure");
    let enabled_fp = pool.skill_fingerprint_of(&rec.id).await.unwrap();
    let path = skill_scratch(&ws, "standup-digest");
    assert!(path.is_file(), "an enabled skill materializes");

    // Disable it in the console.
    skills
        .set(&rec.id, &custom_skill("standup-digest", false, STANDUP_MD))
        .await
        .unwrap();
    pool.ensure(&rec, &fx.deps).await.expect("second ensure");
    let disabled_fp = pool.skill_fingerprint_of(&rec.id).await.unwrap();
    assert_ne!(enabled_fp, disabled_fp, "disabling changes the fingerprint");
    assert!(
        !path.exists(),
        "a disabled skill is dropped from the rebuilt scratch tree"
    );
    assert_eq!(pool.resident_companies().await, 1, "rebuilt in place");
}

/// The fingerprint is order-agnostic (the store gives no ordering contract)
/// but content-sensitive (an edited `custom_doc` must trigger a rebuild).
#[test]
fn skill_delta_fingerprint_is_order_agnostic_but_content_sensitive() {
    let a = custom_skill("alpha", true, "---\nname: A\ndescription: a\n---\n");
    let b = custom_skill("beta", true, "---\nname: B\ndescription: b\n---\n");
    assert_eq!(
        skill_delta_fingerprint(&[a.clone(), b.clone()]),
        skill_delta_fingerprint(&[b, a.clone()]),
        "row order must not change the fingerprint"
    );

    let a_edited = custom_skill("alpha", true, "---\nname: A\ndescription: EDITED\n---\n");
    assert_ne!(
        skill_delta_fingerprint(&[a]),
        skill_delta_fingerprint(&[a_edited]),
        "an edited custom_doc must change the fingerprint"
    );
}
