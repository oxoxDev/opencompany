use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use super::TinyhumansTokenSource;
use crate::company::CompanyManifest;
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::ports::usage::{SampleKind, UsageSample};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-capabilities-")
        .tempdir()
        .expect("tempdir")
}

async fn state_with_manifest(home: &std::path::Path, manifest_toml: &str) -> AppState {
    state_with(home, manifest_toml, None).await
}

/// [`state_with_manifest`], optionally over a caller-supplied
/// [`SecretStore`](crate::ports::SecretStore) — the seam the issue #886
/// store-error case needs, since an unreadable store is the one input the
/// filesystem-backed default cannot produce.
async fn state_with(
    home: &std::path::Path,
    manifest_toml: &str,
    secrets: Option<std::sync::Arc<dyn crate::ports::SecretStore>>,
) -> AppState {
    use crate::ports::CompanyStore;
    let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
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
        })
        .await
        .unwrap();
    let mut builder = RuntimeBuilder::new(home.to_path_buf(), manifest).with_id(id.clone());
    if let Some(secrets) = secrets {
        builder = builder.with_secrets(secrets);
    }
    let runtime = builder.build().await.unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

async fn get_capabilities(state: &AppState) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/company/capabilities")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::empty())
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// Issue #1735: a build whose runtime is on the offline echo brain must
/// never report itself as able to think — with or without a `[plan]`, since
/// the DTO is built in two places.
///
/// The runtimes `state_with_manifest` builds never call
/// `app::harness::attach`, so **no pool is attached in either lane** and the
/// honest answer is `unavailable` in both: nothing an operator saves in
/// Settings → Inference reaches a harness that was never wired. This read
/// `unconfigured` under `openhuman` while the state was derived from
/// `cfg!` alone — a settings link offered on a runtime it could not help
/// (codex review of PR #1740). The lane-independent expectation is the
/// point: the answer turns on what this runtime holds, not on which lane
/// compiled it.
#[tokio::test]
async fn a_company_on_the_echo_brain_never_reports_itself_configured() {
    let expected = "unavailable";

    // No `[plan]` — the `unconfigured()` construction site.
    let home_a_dir = home();
    let state = state_with_manifest(
        home_a_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n",
    )
    .await;
    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["configured"], false, "{dto}");
    assert_eq!(
        dto["cognition"], expected,
        "a runtime built with no inference source runs the echo brain: {dto}"
    );

    // With a `[plan]` — the other construction site. A field wired into one
    // of them alone reports honestly here and lies to every company that
    // has a budget configured, which is the trap this file already warns
    // about twice.
    let home_b_dir = home();
    let state_b = state_with_manifest(
        home_b_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[plan]\nname = \"starter\"\nperiod = \"daily\"\ntotal_tokens = 1000\n",
    )
    .await;
    let (status_b, dto_b) = get_capabilities(&state_b).await;
    assert_eq!(status_b, StatusCode::OK);
    assert_eq!(dto_b["configured"], true, "{dto_b}");
    assert_eq!(
        dto_b["cognition"], expected,
        "the plan-configured construction site must carry the same answer: {dto_b}"
    );
}

/// A harness pool *is* attached and the company still resolved no inference
/// source, so the echo brain won — the one state where Settings → Inference
/// is a real remedy (issue #1735).
///
/// The counterpart to the test above, and the pair is what pins the
/// distinction codex's review turned on: same echo brain, same manifest,
/// same lane, and the answer flips on whether a pool was attached. Gated on
/// `openhuman` because `with_harness` — and the very idea of an attached
/// pool — only exists under it; `feature-lanes.txt` records that lane as
/// `tested`, so this runs rather than merely compiling.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn an_attached_harness_with_no_inference_is_a_settings_problem() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let manifest_toml = "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n";
    // Seeds the record and a harness-less runtime, then replaces the
    // runtime with one holding a pool over the same record — so the only
    // difference from the test above is the attached harness.
    let state = state_with_manifest(&home, manifest_toml).await;
    let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
    let id = CompanyId::new("acme");
    let runtime = RuntimeBuilder::new(home, manifest)
        .with_id(id.clone())
        .with_harness(std::sync::Arc::new(crate::harness::HarnessPool::new()))
        .build()
        .await
        .unwrap();
    // The premise: no inference source resolved, so this is still the echo
    // brain. Asserted rather than assumed — if a future default put a brain
    // behind it, the `unconfigured` below would pass for the wrong reason.
    assert_eq!(
        runtime.cognition().path,
        crate::ports::brain::ECHO_PATH,
        "no inference configured, so the runtime must still be on the echo brain",
    );
    state.registry().insert(id, std::sync::Arc::new(runtime));

    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        dto["cognition"], "unconfigured",
        "a pool is attached, so a provider really is one settings page away: {dto}"
    );
}

/// A harness is attached and the config cannot be **read** — which is not
/// the same as nothing being configured (codex review of PR #1740).
///
/// `ops::inference` already refuses this promise from the other side: its
/// `unreadable_inference_config_is_not_restartable` regression builds this
/// exact runtime — reachable harness over a failing `SecretStore` — and
/// asserts `RunnerGap::NotWired`, "not `InferenceRequired`", because saving
/// cannot resolve a configuration the host cannot read. Chat pointing that
/// same operator at Settings → Inference would make, on the same runtime,
/// the promise the workflow-run route declines to make.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn an_unreadable_inference_config_is_not_reported_as_unconfigured() {
    use crate::ports::types::SecretValue;

    struct FailingSecrets;
    #[async_trait::async_trait]
    impl crate::ports::SecretStore for FailingSecrets {
        async fn get(&self, _c: &CompanyId, _key: &str) -> crate::Result<Option<SecretValue>> {
            Err(crate::error::OpenCompanyError::Store(
                "secret store unreachable".into(),
            ))
        }
        async fn set(&self, _c: &CompanyId, _key: &str, _v: SecretValue) -> crate::Result<()> {
            Ok(())
        }
    }

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let manifest_toml = "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n";
    let state = state_with_manifest(&home, manifest_toml).await;
    let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
    let id = CompanyId::new("acme");
    let runtime = RuntimeBuilder::new(home, manifest)
        .with_id(id.clone())
        .with_harness(std::sync::Arc::new(crate::harness::HarnessPool::new()))
        .with_secrets(std::sync::Arc::new(FailingSecrets))
        .build()
        .await
        .unwrap();
    assert_eq!(
        runtime.cognition().path,
        crate::ports::brain::ECHO_PATH,
        "an unresolvable config leaves the runtime on the echo brain",
    );
    state.registry().insert(id, std::sync::Arc::new(runtime));

    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        dto["cognition"], "undetermined",
        "the host cannot read the config, so it must not name a remedy: {dto}"
    );
    assert_ne!(
        dto["cognition"], "unconfigured",
        "that would promise a settings page `runner_gap_for` refuses to promise: {dto}"
    );
}

/// A provider is saved and resolves, and the company is still on the echo
/// brain because its runtime predates the save (codex review of PR #1740).
///
/// The likeliest route to the echo brain in practice, and the one where
/// getting it wrong is rudest: the operator followed this very banner's
/// link, chose a provider, saved it — and `unconfigured` would send them
/// back to that page to redo work they did correctly. `ops::inference`
/// calls this same state `restartRequired` (issue #266).
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn a_saved_provider_awaiting_a_restart_reports_restart_required() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    // A manifest `[inference]` section resolves without a secret write, so
    // the config is set *before* the runtime is built and the runtime still
    // ends up on the echo brain — the same shape as a console save landing
    // after boot, without needing to rebuild anything mid-test.
    let manifest_toml = "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[inference]\nprovider = \"ollama\"\n";
    let state = state_with_manifest(&home, manifest_toml).await;
    let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
    let id = CompanyId::new("acme");
    // Built with a pool but with the brain forced to echo, which is exactly
    // what a runtime that predates the save is holding.
    let runtime = RuntimeBuilder::new(home, manifest)
        .with_id(id.clone())
        .with_harness(std::sync::Arc::new(crate::harness::HarnessPool::new()))
        .with_brain(std::sync::Arc::new(crate::brain::EchoBrain))
        .build()
        .await
        .unwrap();
    assert_eq!(runtime.cognition().path, crate::ports::brain::ECHO_PATH);
    state.registry().insert(id, std::sync::Arc::new(runtime));

    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        dto["cognition"], "restart-required",
        "a provider resolves, so the remedy is a restart, not another choice: {dto}"
    );
    assert_ne!(
        dto["cognition"], "unconfigured",
        "that would send the operator back to the page they just came from: {dto}"
    );
}

/// The other direction, so the field is not a constant: a runtime holding a
/// brain that is not the echo brain reports `configured`.
#[tokio::test]
async fn a_company_with_a_real_brain_reports_configured() {
    use crate::ports::brain::{Brain, CycleHost};
    use crate::ports::types::{CycleRequest, CycleResult};

    /// A brain that does nothing but exist. It reports the default
    /// `Cognition` (path `custom`), which is what any embedder-injected
    /// brain reports — cognition of a kind this crate cannot name, but
    /// cognition all the same.
    struct InjectedBrain;

    #[async_trait::async_trait]
    impl Brain for InjectedBrain {
        async fn run_cycle(
            &self,
            _req: CycleRequest,
            _host: &dyn CycleHost,
        ) -> crate::Result<CycleResult> {
            Ok(CycleResult {
                channel_responses: Vec::new(),
                new_traces: Vec::new(),
                ledger_deltas: Vec::new(),
                token_usage: Default::default(),
            })
        }
    }

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let manifest_toml = "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n";
    // Seeds the company record and an echo-brain runtime; the insert below
    // replaces that runtime with one holding a real brain, over the same
    // record — so the only thing that differs from the test above is the
    // brain, which is the point.
    let state = state_with_manifest(&home, manifest_toml).await;
    let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
    let id = CompanyId::new("acme");
    let runtime = RuntimeBuilder::new(home, manifest)
        .with_id(id.clone())
        .with_brain(std::sync::Arc::new(InjectedBrain))
        .build()
        .await
        .unwrap();
    state.registry().insert(id, std::sync::Arc::new(runtime));

    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["cognition"], "configured", "{dto}");
}

#[tokio::test]
async fn reports_unconfigured_without_a_plan() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n",
    )
    .await;

    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["configured"], false);
    assert!(
        dto.get("tiers").is_none(),
        "no tiers when unconfigured: {dto}"
    );
    assert!(dto.get("plan").is_none());
}

/// Media generation (issue #109): the route surfaces `mediaGranted` from the
/// manifest tool grants (explicit `media`, never `*`), even with no `[plan]`.
#[tokio::test]
async fn reports_media_granted_from_explicit_grant_only() {
    // Explicit `media` grant, no `[plan]` → unconfigured but mediaGranted.
    let home_a_dir = home();
    let home_a = home_a_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home_a,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"media\"]\n",
    )
    .await;
    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["configured"], false);
    assert_eq!(dto["mediaGranted"], true, "{dto}");
    // The flags are always present so the console can render every state.
    assert!(dto.get("mediaInBuild").is_some(), "{dto}");
    assert!(dto.get("mediaCredentialConfigured").is_some(), "{dto}");

    // A `*` wildcard grant must NOT count as a media grant.
    let home_b_dir = home();
    let home_b = home_b_dir.path().to_path_buf();
    let state2 = state_with_manifest(
        &home_b,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"*\"]\n",
    )
    .await;
    let (_, dto2) = get_capabilities(&state2).await;
    assert_eq!(
        dto2["mediaGranted"], false,
        "the `*` wildcard must not grant the real-money media family: {dto2}"
    );
}

/// Per-tenant Composio (issue #110): the route surfaces `composioGranted`
/// from the explicit grant (never `*`) and the trio flags, even with no
/// `[plan]`.
#[tokio::test]
async fn reports_composio_flags_from_explicit_grant_only() {
    let home_a_dir = home();
    let home_a = home_a_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home_a,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"composio\"]\n",
    )
    .await;
    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["composioGranted"], true, "{dto}");
    assert_eq!(dto["composioTokenConfigured"], false, "no token yet: {dto}");
    assert!(dto.get("composioInBuild").is_some(), "{dto}");

    // A `*` wildcard grant must NOT count as a composio grant.
    let home_b_dir = home();
    let home_b = home_b_dir.path().to_path_buf();
    let state2 = state_with_manifest(
        &home_b,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"*\"]\n",
    )
    .await;
    let (_, dto2) = get_capabilities(&state2).await;
    assert_eq!(
        dto2["composioGranted"], false,
        "the `*` wildcard must not grant composio: {dto2}"
    );
}

/// Metered web search (issue #238): the route surfaces `searchGranted` from
/// the explicit grant (never `*`) and the company's daily call cap, even
/// with no `[plan]` — the cap is a call ceiling on `[tools]`, not a token
/// budget on `[plan]`.
#[tokio::test]
async fn reports_search_flags_from_explicit_grant_only() {
    let home_a_dir = home();
    let home_a = home_a_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home_a,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"search\"]\nsearch_daily_calls = 25\n",
    )
    .await;
    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["searchGranted"], true, "{dto}");
    assert_eq!(dto["searchDailyCallCap"], 25, "{dto}");
    assert!(dto.get("searchInBuild").is_some(), "{dto}");
    assert!(dto.get("searchCredentialConfigured").is_some(), "{dto}");

    // A `*` wildcard grant must NOT count as a search grant — every call is
    // a priced request, so it can never ride in on the wildcard a company
    // set for its file and shell tools.
    let home_b_dir = home();
    let home_b = home_b_dir.path().to_path_buf();
    let state2 = state_with_manifest(
        &home_b,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"*\"]\n",
    )
    .await;
    let (_, dto2) = get_capabilities(&state2).await;
    assert_eq!(
        dto2["searchGranted"], false,
        "the `*` wildcard must not grant metered search: {dto2}"
    );
    assert_eq!(
        dto2["searchDailyCallCap"],
        crate::company::DEFAULT_SEARCH_DAILY_CALLS,
        "an unset cap reports the built-in default: {dto2}"
    );
}

/// Issue #567: the MCP bridge's build state travels on every response, with
/// a `[plan]` and without one. The `/mcp/servers` management routes ship in
/// every build while the agent-side registry is pushed onto the belt behind
/// `#[cfg(feature = "mcp")]`, so without this flag a console cannot tell a
/// deployment that will honour a server from one that never can — the
/// operator finds out by asking an agent and watching nothing happen.
///
/// Asserted on **both** response paths deliberately: the DTO is built in two
/// places (`unconfigured` and the configured branch), so a flag added to one
/// alone would report honestly for a company with no plan and lie to every
/// company that has one.
#[tokio::test]
async fn reports_whether_the_mcp_bridge_is_in_this_build() {
    let unplanned_dir = home();
    let unplanned = unplanned_dir.path().to_path_buf();
    let state = state_with_manifest(
        &unplanned,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"*\"]\n",
    )
    .await;
    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["configured"], false);
    assert_eq!(
        dto["mcpInBuild"],
        cfg!(feature = "mcp"),
        "the unconfigured response states the bridge's build state: {dto}"
    );

    let planned_dir = home();
    let planned = planned_dir.path().to_path_buf();
    let state2 = state_with_manifest(
        &planned,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[plan]\nname = \"starter\"\n",
    )
    .await;
    let (_, dto2) = get_capabilities(&state2).await;
    assert_eq!(dto2["configured"], true, "{dto2}");
    assert_eq!(
        dto2["mcpInBuild"],
        cfg!(feature = "mcp"),
        "a configured plan reports the same build state: {dto2}"
    );

    // The without-feature path is the one the console must not misreport:
    // pinned as a literal so the honest answer cannot regress into a
    // vacuously-true comparison against the same `cfg!`.
    #[cfg(not(feature = "mcp"))]
    {
        assert_eq!(
            dto["mcpInBuild"], false,
            "a build without the bridge must say so: {dto}"
        );
        assert_eq!(dto2["mcpInBuild"], false, "{dto2}");
    }
    #[cfg(feature = "mcp")]
    {
        assert_eq!(
            dto["mcpInBuild"], true,
            "a build with the bridge must say so: {dto}"
        );
        assert_eq!(dto2["mcpInBuild"], true, "{dto2}");
    }
}

#[tokio::test]
async fn reports_tiers_and_exhaustion_for_a_configured_plan() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[plan]\nname = \"starter\"\n",
    )
    .await;

    // Seed 250k inference tokens into the company meter — past starter's 200k.
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();
    runtime
        .usage()
        .record(
            &id,
            &UsageSample {
                at_millis: crate::ports::now_millis(),
                agent: "ceo".into(),
                provider: "managed".into(),
                input_tokens: 200_000,
                output_tokens: 50_000,
                cached_input_tokens: 0,
                cost_usd: 0.0,
                kind: SampleKind::Inference,
                run_id: None,
                model: None,
            },
        )
        .await
        .unwrap();

    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["configured"], true);
    assert_eq!(dto["plan"], "starter");
    assert_eq!(dto["period"], "daily");
    assert_eq!(dto["spentTokens"], 250_000);

    let tiers = dto["tiers"].as_array().expect("tiers present");
    assert_eq!(tiers.len(), 2, "starter budgets shell + code: {dto}");
    for tier in tiers {
        assert_eq!(tier["budgetTokens"], 200_000);
        assert_eq!(tier["spentTokens"], 250_000);
        assert_eq!(tier["remainingTokens"], 0);
        assert_eq!(tier["exhausted"], true);
    }
    // No `[plan].total_tokens` → the hard-ceiling row is absent.
    assert!(
        dto.get("total").is_none(),
        "no total ceiling configured: {dto}"
    );
}

/// The plan-level total token ceiling (issue #188) surfaces its own `total`
/// row — budget, spend, remaining, exhausted — alongside the per-namespace
/// tiers, and reports `exhausted` once period spend crosses it.
#[tokio::test]
async fn reports_total_ceiling_row_when_configured() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[plan]\nname = \"starter\"\ntotal_tokens = 300000\n",
    )
    .await;

    // Seed 250k inference tokens — under the 300k total ceiling.
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();
    runtime
        .usage()
        .record(
            &id,
            &UsageSample {
                at_millis: crate::ports::now_millis(),
                agent: "ceo".into(),
                provider: "managed".into(),
                input_tokens: 200_000,
                output_tokens: 50_000,
                cached_input_tokens: 0,
                cost_usd: 0.0,
                kind: SampleKind::Inference,
                run_id: None,
                model: None,
            },
        )
        .await
        .unwrap();

    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    let total = &dto["total"];
    assert!(total.is_object(), "total ceiling row present: {dto}");
    assert_eq!(total["budgetTokens"], 300_000);
    assert_eq!(total["spentTokens"], 250_000);
    assert_eq!(total["remainingTokens"], 50_000);
    assert_eq!(total["exhausted"], false, "250k < 300k is under budget");
}

// ---- issue #886: the Composio verdict comes from the resolver -----------

pub(super) const GRANTS_COMPOSIO: &str =
    "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"composio\"]\n";

/// A store whose reads always fail — the transient-hiccup case, mirroring
/// `company_key`'s own fixture.
pub(super) struct BrokenSecrets;

#[async_trait::async_trait]
impl crate::ports::SecretStore for BrokenSecrets {
    async fn get(
        &self,
        _c: &CompanyId,
        _key: &str,
    ) -> crate::Result<Option<crate::ports::types::SecretValue>> {
        Err(crate::error::OpenCompanyError::Store("boom".into()))
    }
    async fn set(
        &self,
        _c: &CompanyId,
        _key: &str,
        _value: crate::ports::types::SecretValue,
    ) -> crate::Result<()> {
        Err(crate::error::OpenCompanyError::Store("boom".into()))
    }
}

/// The instance identity the platform hands a hosted pod. Built directly
/// rather than through `from_env` so the tier matrix never touches the
/// process environment.
pub(super) fn platform_identity() -> std::sync::Arc<TinyhumansTokenSource> {
    std::sync::Arc::new(TinyhumansTokenSource::projected_file(
        "/var/run/secrets/tinyhumans.ai/token",
    ))
}
