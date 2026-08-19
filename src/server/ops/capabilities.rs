//! Capability-budget read surface (issue #108): the company's effective tier
//! plan and how much of each tier's token budget the current period has spent.
//!
//! A read-only companion to the harness gate — the console renders one row per
//! configured tier (budget, spend, remaining, whether its tools are disabled).
//! With no `[plan]` configured the response is `{ configured: false }` and the
//! console shows a "no token plan configured" note. The heavy lifting is the
//! pure math in [`crate::metering::capability`]; this handler only queries the
//! [`UsageMeter`](crate::ports::UsageMeter) for the period and projects it.

use axum::Json;
use axum::Router;
use axum::routing::get;
use serde::Serialize;

use crate::AppState;
use crate::company::credentials::{CredentialSource, TinyhumansTokenSource};
use crate::company::runtime::CompanyRuntime;
use crate::metering::capability::{CapabilityPlan, tokens_in};
use crate::ports::now_millis;
use crate::server::error::ApiError;
use crate::server::ops::{ScopedCompany, scoped};

/// Builds the capability-budget route fragment.
pub fn router() -> Router<AppState> {
    scoped("/capabilities", get(get_status))
}

/// The company's capability-budget status as the console renders it.
///
/// When no `[plan]` is configured only `configured: false` is sent; the extra
/// fields are omitted so the console can branch on presence.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CapabilityStatusDto {
    /// Whether the company has a capability plan at all.
    configured: bool,
    /// The configured built-in tier name, if any (`null` for a bare
    /// `token_budgets` plan).
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<String>,
    /// The budget window (`daily` / `monthly`).
    #[serde(skip_serializing_if = "Option::is_none")]
    period: Option<String>,
    /// Epoch-millis start of the current budget period (the spend window).
    #[serde(skip_serializing_if = "Option::is_none")]
    period_start_millis: Option<u64>,
    /// Total inference tokens spent by the company this period (the figure every
    /// tier threshold is compared against).
    #[serde(skip_serializing_if = "Option::is_none")]
    spent_tokens: Option<u64>,
    /// One row per configured tier, namespace-sorted. Omitted when unconfigured.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tiers: Vec<TierDto>,
    /// The plan-level **total token ceiling** (issue #188), when one is
    /// configured. Unlike the per-namespace `tiers` — a *soft* gate that only
    /// trims exec tools — crossing this is a *hard* stop: the harness refuses to
    /// dispatch further turns this period. Omitted when no ceiling is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<TotalDto>,
    /// Media generation (issue #109): whether this company **explicitly** grants
    /// the real-money `media` namespace (a `*` wildcard does NOT count). Sent
    /// regardless of whether a `[plan]` is configured, since media is opt-in per
    /// tool grant, not per plan.
    media_granted: bool,
    /// Whether the `media` feature is compiled into this build at all (the tools
    /// only exist under it). `false` lets the console show a "not in this build"
    /// state rather than implying a missing credential.
    media_in_build: bool,
    /// Whether a MANAGED media credential is resolvable from the environment on
    /// this build (feature on + env present). Never reflects a tenant secret.
    media_credential_configured: bool,
    /// Per-tenant Composio (issue #110): whether this company **explicitly**
    /// grants the `composio` namespace (a `*` wildcard does NOT count). Opt-in
    /// per tool grant, independent of a `[plan]`.
    composio_granted: bool,
    /// Whether the `composio` feature is compiled into this build at all.
    composio_in_build: bool,
    /// Chargebee billing (issue #788): whether this company **explicitly** grants
    /// the `chargebee` namespace (a `*` wildcard does NOT count). What the
    /// Settings UI reads to say whether billing tools would reach an agent even
    /// once credentials are saved.
    chargebee_granted: bool,
    /// Whether the `chargebee` feature is compiled into this build at all. The
    /// grant and the credentials can both be in place and still wire no tools if
    /// the running binary was not built with it.
    chargebee_in_build: bool,
    /// Whether a non-empty per-tenant Composio **BYO override** token is stored
    /// under `composio/token` — never the token itself. Unlike media's env
    /// credential, this is a tenant secret.
    ///
    /// Deliberately narrow, and **not** the answer to "can this company reach
    /// Composio" (issue #886): the BYO slot is the first of three tiers, and on
    /// a hosted tenant the third one answers, so this reads `false` for a
    /// company whose Composio tools are wired and working. Read
    /// [`Self::composio_credential_source`] for the resolution verdict; this
    /// field is retained with its original meaning for the console surface that
    /// asks whether *this company pasted a token*.
    composio_token_configured: bool,
    /// Which tier this company's Composio credential actually resolves from
    /// (issue #886) — `attested` (the instance's platform identity), `company`
    /// (the company's own TinyHumans key), `static` (a pasted BYO token or a
    /// static instance key), or `none` (nothing resolves, so no tools are
    /// wired).
    ///
    /// Sourced from
    /// [`resolve_credential`](crate::company::composio::resolve_credential) —
    /// the same derivation the toolbelt gates on — rather than a second copy of
    /// its precedence, so the console can never name a tier the agents are not
    /// on. Matches the `credentialSource` field
    /// [`ops::composio`](crate::server::ops::composio) already reports.
    ///
    /// A **resolution** verdict, not a liveness one: `attested` says a bearer
    /// can be obtained, not that Composio answered or that any account is
    /// connected. `GET …/connections` is the axis that answers those.
    ///
    /// Omitted entirely when the secret store could not be read — an unknown
    /// answer is not `none`, and reporting a confident "no credential" for a
    /// transient store hiccup is the same class of lie #886 is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    composio_credential_source: Option<CredentialSource>,
    /// Metered web search (issue #238): whether this company **explicitly**
    /// grants the `search` namespace (a `*` wildcard does NOT count).
    search_granted: bool,
    /// Whether the harness that carries `web_search` is compiled into this
    /// build. There is no `search` Cargo feature — the tool rides the plain
    /// `openhuman` harness feature deliberately, so CI's gated lane compiles and
    /// tests it rather than a real-money surface shipping untested.
    search_in_build: bool,
    /// Whether a MANAGED search credential is resolvable from the environment on
    /// this build. Never reflects a tenant secret — search runs only on the
    /// platform identity.
    search_credential_configured: bool,
    /// The company's daily `web_search` call ceiling
    /// (`[tools].search_daily_calls`, else the built-in default). Reaching it
    /// makes the tool refuse loudly rather than return an empty result set.
    search_daily_call_cap: u32,
    /// Bound repositories (issue #245, agent half): whether this company
    /// **explicitly** grants the `repo` namespace (a `*` wildcard does NOT
    /// count).
    ///
    /// The grant alone is not the whole story, and the console says so: a
    /// company can grant `repo` and bind nothing (the tools are not wired), or
    /// bind repositories and grant nothing (nobody can read them). Both are
    /// silent misconfigurations that look like a working setup from one page
    /// each, which is why this flag travels beside the repositories list rather
    /// than only inside the manifest.
    repo_granted: bool,
    /// Whether the agent-side MCP bridge is compiled into this build (issue
    /// #567). Unlike media/composio/search this is **not** a grant question: the
    /// `/mcp/servers` management routes ship in every build, so an operator can
    /// add a server, store a token and watch it probe healthy on a build that
    /// hands agents no MCP tool at all — `registry_for_agent` is pushed onto the
    /// belt behind `#[cfg(feature = "mcp")]`. The most misleading case is a
    /// build with `openhuman` but without `mcp`: live tool discovery and health
    /// probes answer for real (they ride the harness feature), so every read in
    /// the console looks correct while no agent can call the server. `false`
    /// lets the MCP surfaces state that plainly instead of the operator finding
    /// out by asking an agent and watching nothing happen.
    mcp_in_build: bool,
}

/// One tier's budget row.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TierDto {
    /// The exec tool namespace this tier gates.
    namespace: String,
    /// Tokens allowed this period.
    budget_tokens: u64,
    /// Tokens spent this period (company-wide — spend has no per-tier
    /// attribution, so this is the same across rows).
    spent_tokens: u64,
    /// `budget - spent`, saturating at zero.
    remaining_tokens: u64,
    /// Whether spend has reached the threshold — the tier's tools are disabled.
    exhausted: bool,
}

/// The plan-level total token ceiling row (issue #188).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TotalDto {
    /// Total tokens allowed this period before dispatch is refused.
    budget_tokens: u64,
    /// Tokens spent this period (the same company-wide figure the tiers compare
    /// against).
    spent_tokens: u64,
    /// `budget - spent`, saturating at zero.
    remaining_tokens: u64,
    /// Whether spend has reached the ceiling — the harness refuses to dispatch
    /// further turns until the period resets.
    exhausted: bool,
}

/// The opt-in-capability status flags carried on every response (media +
/// composio), independent of whether a `[plan]` is configured.
struct OptInFlags {
    media_granted: bool,
    chargebee_granted: bool,
    composio_granted: bool,
    composio_token_configured: bool,
    /// The resolved Composio credential tier (issue #886), or `None` when it
    /// could not be determined. Travels on the flags rather than being computed
    /// per DTO site because the DTO is built in two places, and a field wired
    /// into one of them alone reports honestly for a company with no plan and
    /// lies to every company that has one — the failure the issue #567 test
    /// below exists to catch.
    composio_credential_source: Option<CredentialSource>,
    search_granted: bool,
    search_daily_call_cap: u32,
    repo_granted: bool,
}

impl OptInFlags {
    /// All-false — used when no company record is present.
    fn none() -> Self {
        Self {
            media_granted: false,
            chargebee_granted: false,
            composio_granted: false,
            composio_token_configured: false,
            // `None` (undetermined), never `Some(CredentialSource::None)`:
            // there is no company record to resolve a credential for, which is
            // not the same answer as "no credential resolves".
            composio_credential_source: None,
            search_granted: false,
            search_daily_call_cap: crate::company::DEFAULT_SEARCH_DAILY_CALLS,
            repo_granted: false,
        }
    }
}

/// The unconfigured response: `{ configured: false }` plus the opt-in-capability
/// flags (media + composio are opt-in per tool grant, independent of a `[plan]`).
fn unconfigured(flags: OptInFlags) -> CapabilityStatusDto {
    CapabilityStatusDto {
        configured: false,
        plan: None,
        period: None,
        period_start_millis: None,
        spent_tokens: None,
        tiers: Vec::new(),
        total: None,
        media_granted: flags.media_granted,
        media_in_build: cfg!(feature = "media"),
        media_credential_configured: media_credential_configured(),
        composio_granted: flags.composio_granted,
        composio_in_build: cfg!(feature = "composio"),
        chargebee_granted: flags.chargebee_granted,
        chargebee_in_build: cfg!(feature = "chargebee"),
        composio_token_configured: flags.composio_token_configured,
        composio_credential_source: flags.composio_credential_source,
        search_granted: flags.search_granted,
        search_in_build: cfg!(feature = "openhuman"),
        search_credential_configured: search_credential_configured(),
        search_daily_call_cap: flags.search_daily_call_cap,
        repo_granted: flags.repo_granted,
        mcp_in_build: cfg!(feature = "mcp"),
    }
}

/// Which tier this company's Composio credential resolves from (issue #886), or
/// `None` when the secret store could not be read.
///
/// Asks
/// [`resolve_credential`](crate::company::composio::resolve_credential) rather
/// than restating its precedence. The three-tier resolution — BYO
/// `composio/token`, then the company's own TinyHumans key, then this instance's
/// platform identity — is the *same* one
/// [`TenantComposio::resolve`](crate::harness::composio::TenantComposio::resolve)
/// gates the toolbelt on, and the whole point of #886 is that this panel had a
/// second, one-tier copy of the question that disagreed with it. There must be
/// exactly one derivation, and this is not it — it is a caller of it.
///
/// Takes the instance identity **already resolved** rather than an `&dyn
/// EnvSource`, mirroring
/// [`ops::composio`](crate::server::ops::composio)'s `credential_source_for`: a
/// trait object with no `Send + Sync` bound held across the await below makes
/// the whole handler future non-`Send`, which axum rejects. Passing the resolved
/// value also keeps the tier matrix testable without mutating the process
/// environment.
///
/// A store error yields `None` and a warning, never `Some(CredentialSource::None)`.
/// The rest of `/capabilities` is budget and tier data with nothing to do with
/// Composio, so failing the whole response would be the wrong trade — but
/// answering "no credential" for a transient hiccup would send an operator to
/// paste a token they already have, which is the #886 failure in the other
/// direction. An omitted field is the only honest "we do not know".
async fn composio_credential_source(
    runtime: &CompanyRuntime,
    token_source: Option<std::sync::Arc<TinyhumansTokenSource>>,
) -> Option<CredentialSource> {
    match crate::company::composio::resolve_credential(
        runtime.id(),
        runtime.secrets().as_ref(),
        token_source,
    )
    .await
    {
        Ok(credential) => Some(credential.source()),
        Err(err) => {
            tracing::warn!(
                company = %runtime.id(),
                error = %err,
                "[capabilities] could not resolve the Composio credential tier; omitting \
                 `composioCredentialSource` rather than reporting a confident `none`"
            );
            None
        }
    }
}

/// Whether a MANAGED search credential (issue #238) is resolvable from the
/// environment on this build. Env-only, never a tenant secret, matching the
/// harness's fail-closed resolution. Off the harness feature this is always
/// `false` — there is no agent to search with.
fn search_credential_configured() -> bool {
    #[cfg(feature = "openhuman")]
    {
        use crate::app::config::ProcessEnv;
        crate::harness::provider::search_backend_from_env(&ProcessEnv).is_some()
    }
    #[cfg(not(feature = "openhuman"))]
    {
        false
    }
}

/// Whether a MANAGED media credential (issue #109) is resolvable from the
/// environment on this build. `true` only under the `media` feature with a
/// credential present — env-only, never a tenant secret, matching the harness's
/// fail-closed resolution. Off the feature this is always `false`.
fn media_credential_configured() -> bool {
    #[cfg(feature = "media")]
    {
        use crate::app::config::ProcessEnv;
        crate::harness::provider::media_backend_from_env(&ProcessEnv).is_some()
    }
    #[cfg(not(feature = "media"))]
    {
        false
    }
}

/// Resolves the capability-budget status DTO for a company.
async fn effective_status(runtime: &CompanyRuntime) -> Result<CapabilityStatusDto, ApiError> {
    let record = runtime.store().load(runtime.id()).await.map_err(ApiError)?;
    let Some(record) = record else {
        return Ok(unconfigured(OptInFlags::none()));
    };
    // Media + composio are opt-in per tool grant (explicit namespace, never `*`)
    // and live on the manifest regardless of whether a `[plan]` is configured.
    let flags = OptInFlags {
        media_granted: crate::company::grants_media_explicit(&record.manifest.tools.allow),
        chargebee_granted: crate::company::grants_chargebee_explicit(&record.manifest.tools.allow),
        composio_granted: crate::company::grants_composio_explicit(&record.manifest.tools.allow),
        // Degrade to "unconfigured" on a transient secret-store error rather
        // than failing the whole /capabilities response (budget/tier data is
        // unrelated to Composio). Mirrors other opt-in-credential probes.
        composio_token_configured: crate::company::composio::token_configured(
            runtime.id(),
            runtime.secrets().as_ref(),
        )
        .await
        .unwrap_or(false),
        // Issue #886: the field above answers only whether a BYO token was
        // pasted, which on a hosted tenant is `false` for a company whose
        // Composio tools work. This one asks the resolver what the toolbelt
        // will actually present.
        //
        // The instance identity is read straight from the process environment
        // here, as `ops::composio` and `ops::company_key` already do. It would
        // be better held once on `CompanyRuntime` — this is the fourth
        // `from_env` call site on a console read path — but inventing that
        // accessor is a wider change than this fix, so it is left as a
        // follow-up rather than half-done here.
        composio_credential_source: composio_credential_source(
            runtime,
            TinyhumansTokenSource::from_env(&crate::app::config::ProcessEnv)
                .map(std::sync::Arc::new),
        )
        .await,
        // Issue #238: search is opt-in per tool grant like media/composio, and
        // its daily cap lives on `[tools]` rather than `[plan]` — a call
        // ceiling, not a token budget — so both travel with the plan-independent
        // flags.
        search_granted: crate::company::grants_search_explicit(&record.manifest.tools.allow),
        search_daily_call_cap: record
            .manifest
            .tools
            .search_daily_calls
            .unwrap_or(crate::company::DEFAULT_SEARCH_DAILY_CALLS),
        // Issue #245: opt-in per tool grant like the three above, and read from
        // the same manifest field, so the repositories card can tell an operator
        // which half of the setup is missing.
        repo_granted: crate::company::grants_repo_explicit(&record.manifest.tools.allow),
    };
    let manifest_plan = &record.manifest.plan;
    let Some(plan) = CapabilityPlan::from_manifest(manifest_plan) else {
        return Ok(unconfigured(flags));
    };

    let now = now_millis();
    let since = plan.period.period_start_millis(now);
    let samples = runtime
        .usage()
        .query(runtime.id(), since)
        .await
        .map_err(ApiError)?;
    let spent = tokens_in(&samples);

    let tiers = plan
        .status(spent)
        .into_iter()
        .map(|tier| TierDto {
            namespace: tier.namespace,
            budget_tokens: tier.budget,
            spent_tokens: tier.spent,
            remaining_tokens: tier.remaining,
            exhausted: tier.exhausted,
        })
        .collect();

    // The plan-level total ceiling (issue #188): present only when the manifest
    // set `[plan].total_tokens`. This is the hard gate the harness enforces by
    // refusing dispatch — the console renders it alongside the soft per-namespace
    // tiers.
    let total = plan.total_status(spent).map(|status| TotalDto {
        budget_tokens: status.budget,
        spent_tokens: status.spent,
        remaining_tokens: status.remaining,
        exhausted: status.exhausted,
    });

    Ok(CapabilityStatusDto {
        configured: true,
        plan: manifest_plan.name.clone(),
        period: Some(plan.period.as_str().to_string()),
        period_start_millis: Some(since),
        spent_tokens: Some(spent),
        tiers,
        total,
        media_granted: flags.media_granted,
        media_in_build: cfg!(feature = "media"),
        media_credential_configured: media_credential_configured(),
        composio_granted: flags.composio_granted,
        composio_in_build: cfg!(feature = "composio"),
        chargebee_granted: flags.chargebee_granted,
        chargebee_in_build: cfg!(feature = "chargebee"),
        composio_token_configured: flags.composio_token_configured,
        composio_credential_source: flags.composio_credential_source,
        search_granted: flags.search_granted,
        search_in_build: cfg!(feature = "openhuman"),
        search_credential_configured: search_credential_configured(),
        search_daily_call_cap: flags.search_daily_call_cap,
        repo_granted: flags.repo_granted,
        mcp_in_build: cfg!(feature = "mcp"),
    })
}

/// `GET …/capabilities` — the company's capability-budget status.
async fn get_status(company: ScopedCompany) -> Result<Json<CapabilityStatusDto>, ApiError> {
    Ok(Json(effective_status(company.runtime.as_ref()).await?))
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    use super::{CredentialSource, TinyhumansTokenSource};
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
                overlay_desk_tools: Default::default(),
                disabled_workflows: Vec::new(),
                template_provenance: None,
                setup: None,
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

    const GRANTS_COMPOSIO: &str =
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"composio\"]\n";

    /// A store whose reads always fail — the transient-hiccup case, mirroring
    /// `company_key`'s own fixture.
    struct BrokenSecrets;

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
    fn platform_identity() -> std::sync::Arc<TinyhumansTokenSource> {
        std::sync::Arc::new(TinyhumansTokenSource::projected_file(
            "/var/run/secrets/tinyhumans.ai/token",
        ))
    }

    /// The whole of issue #886 in one test: the panel's Composio verdict must
    /// walk **all three** credential tiers, not just the BYO slot.
    ///
    /// The hosted case is the one that was wrong. Nobody pastes a
    /// `composio/token` on a hosted tenant — the pod's platform identity
    /// answers, the toolbelt wires up, the agents call `GITHUB_*` — and the
    /// old one-tier probe called that `false`, sending an operator looking for
    /// a missing credential that was never missing.
    #[tokio::test]
    async fn the_composio_verdict_walks_every_credential_tier() {
        use crate::company::{company_key, composio};

        let home_dir = home();
        let home = home_dir.path().to_path_buf();
        let state = state_with_manifest(&home, GRANTS_COMPOSIO).await;
        let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
        let secrets = runtime.secrets().clone();

        // Nothing stored and no instance identity — fail closed, and say so.
        assert_eq!(
            super::composio_credential_source(runtime.as_ref(), None).await,
            Some(CredentialSource::None),
            "with no tier able to answer, `none` is the honest verdict"
        );

        // The hosted shape: nothing stored, the pod's projected identity
        // answers. This is the reported bug.
        assert_eq!(
            super::composio_credential_source(runtime.as_ref(), Some(platform_identity())).await,
            Some(CredentialSource::Attested),
        );
        assert!(
            !composio::token_configured(runtime.id(), secrets.as_ref())
                .await
                .unwrap(),
            "and the BYO slot is empty in exactly that case — the two fields \
             answer different questions, which is why the panel needs both"
        );

        // The company's own TinyHumans key outranks the instance identity.
        company_key::store_key(runtime.id(), secrets.as_ref(), "th_company")
            .await
            .unwrap();
        assert_eq!(
            super::composio_credential_source(runtime.as_ref(), Some(platform_identity())).await,
            Some(CredentialSource::Company),
        );

        // A pasted BYO token outranks everything.
        composio::store_token(runtime.id(), secrets.as_ref(), "cmp_byo")
            .await
            .unwrap();
        assert_eq!(
            super::composio_credential_source(runtime.as_ref(), Some(platform_identity())).await,
            Some(CredentialSource::Static),
        );
    }

    /// The gate. The DTO's verdict must **equal what the resolver says**, not a
    /// value this route computed for itself.
    ///
    /// Asserted as an equality against a live `resolve_credential` call rather
    /// than against a literal, deliberately: a literal would be satisfied by a
    /// second hardcoded copy of the precedence living in this file, and a second
    /// copy is the entire defect. Issue #586 removed one from the sibling status
    /// route; #886 is the one it missed here.
    ///
    /// Run across the tiers a store can produce on its own, so the equality is
    /// exercised with more than one answer.
    #[tokio::test]
    async fn the_dto_reports_exactly_what_the_resolver_resolves() {
        use crate::company::{company_key, composio};

        let home_dir = home();
        let home = home_dir.path().to_path_buf();
        let state = state_with_manifest(&home, GRANTS_COMPOSIO).await;
        let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
        let secrets = runtime.secrets().clone();

        // The route reads the instance identity from the process environment,
        // so the expectation must be derived from the same place — otherwise
        // this asserts against the test host's env rather than against the
        // resolver.
        let resolver_says = || async {
            composio::resolve_credential(
                runtime.id(),
                secrets.as_ref(),
                TinyhumansTokenSource::from_env(&crate::app::config::ProcessEnv)
                    .map(std::sync::Arc::new),
            )
            .await
            .unwrap()
            .source()
        };

        for label in ["nothing stored", "company key", "byo token"] {
            match label {
                "company key" => {
                    company_key::store_key(runtime.id(), secrets.as_ref(), "th_company")
                        .await
                        .unwrap()
                }
                "byo token" => composio::store_token(runtime.id(), secrets.as_ref(), "cmp_byo")
                    .await
                    .unwrap(),
                _ => {}
            }
            let (status, dto) = get_capabilities(&state).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                dto["composioCredentialSource"],
                resolver_says().await.as_str(),
                "the panel must never name a tier the toolbelt is not on ({label}): {dto}"
            );
        }
    }

    /// Both DTO construction sites carry the field — `unconfigured()` and the
    /// configured branch — per the issue #567 precedent above. A field wired
    /// into one alone reports honestly for a company with no plan and lies to
    /// every company that has one.
    ///
    /// The legacy `composioTokenConfigured` is pinned alongside it, keeping its
    /// original narrow meaning: `false` with no BYO token, `true` with one.
    /// Nothing about #886 changes what that field answers — only what the
    /// console reads for the question it was being misused for.
    #[tokio::test]
    async fn both_response_paths_carry_the_credential_tier() {
        use crate::company::composio;

        for manifest in [
            GRANTS_COMPOSIO,
            &format!("{GRANTS_COMPOSIO}[plan]\nname = \"starter\"\n"),
        ] {
            let home_dir = home();
            let home = home_dir.path().to_path_buf();
            let state = state_with_manifest(&home, manifest).await;
            let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();

            let (_, dto) = get_capabilities(&state).await;
            assert!(
                dto.get("composioCredentialSource").is_some(),
                "every response states the resolved tier: {dto}"
            );
            assert_eq!(
                dto["composioTokenConfigured"], false,
                "no BYO token pasted yet: {dto}"
            );

            composio::store_token(runtime.id(), runtime.secrets().as_ref(), "cmp_byo")
                .await
                .unwrap();
            let (_, dto) = get_capabilities(&state).await;
            assert_eq!(
                dto["composioTokenConfigured"], true,
                "the legacy field keeps answering its own narrow question: {dto}"
            );
            assert_eq!(
                dto["composioCredentialSource"], "static",
                "and a pasted token is the `static` tier: {dto}"
            );
        }
    }

    /// An unreadable secret store **omits** the field rather than reporting
    /// `none`.
    ///
    /// `none` is a verdict — "no credential resolves, no tools are wired" — and
    /// claiming it on a transient hiccup would send an operator to paste a token
    /// they already have. That is issue #886 in the other direction, so the only
    /// honest wire shape for "we do not know" is absence. The rest of the
    /// response still serves: budgets and tiers have nothing to do with Composio.
    #[tokio::test]
    async fn an_unreadable_store_omits_the_tier_rather_than_claiming_none() {
        let home_dir = home();
        let home = home_dir.path().to_path_buf();
        let state = state_with(
            &home,
            GRANTS_COMPOSIO,
            Some(std::sync::Arc::new(BrokenSecrets)),
        )
        .await;

        let (status, dto) = get_capabilities(&state).await;
        assert_eq!(status, StatusCode::OK, "the response still serves: {dto}");
        assert!(
            dto.get("composioCredentialSource").is_none(),
            "an unknown tier is omitted, never rendered as a confident `none`: {dto}"
        );
        assert_eq!(
            dto["composioGranted"], true,
            "the manifest-derived flags are unaffected by the store: {dto}"
        );
    }
}
