use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use super::tests_a_company_on_the::{BrokenSecrets, GRANTS_COMPOSIO, platform_identity};
use super::{CredentialSource, TinyhumansTokenSource};
use crate::company::CompanyManifest;
use crate::ports::types::{CompanyId, CompanyRecord};
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

/// The whole of issue #886 in one test: the panel's Composio verdict must
/// walk **all three** credential tiers, not just the BYO slot.
///
/// The hosted case is the one that was wrong. Nobody pastes a
/// `composio/tinyhumans/key` on a hosted tenant — the pod's platform identity
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

/// Storage addresses and the legacy fallback (#2306): a token pasted before
/// the rename, still sitting only at the legacy address, must keep reading
/// as configured.
#[tokio::test]
async fn a_legacy_only_token_still_reads_as_configured() {
    use crate::company::composio;

    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(&home, GRANTS_COMPOSIO).await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();
    let secrets = runtime.secrets().clone();

    secrets
        .set(
            runtime.id(),
            composio::LEGACY_TOKEN_KEY,
            crate::ports::types::SecretValue("th-not-a-real-key".into()),
        )
        .await
        .unwrap();

    assert!(
        composio::token_configured(runtime.id(), secrets.as_ref())
            .await
            .unwrap(),
        "a legacy-only token must still read as configured"
    );
    assert_eq!(
        super::composio_credential_source(runtime.as_ref(), None).await,
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
            "company key" => company_key::store_key(runtime.id(), secrets.as_ref(), "th_company")
                .await
                .unwrap(),
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

/// Both DTO construction sites report which provider a company's searches
/// actually reach, and it tracks what the Search settings page stored.
///
/// Same #567 precedent as the two tests above: a field wired into one branch
/// alone tells the truth to a company with no plan and lies to every company
/// that has one.
#[tokio::test]
async fn both_response_paths_carry_the_effective_search_provider() {
    use crate::company::search::{API_KEY_SECRET, PROVIDER_SECRET};
    use crate::ports::types::SecretValue;

    let grants_search =
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"search\"]\n";
    for manifest in [
        grants_search.to_string(),
        format!("{grants_search}[plan]\nname = \"starter\"\n"),
    ] {
        let home_dir = home();
        let home = home_dir.path().to_path_buf();
        let state = state_with_manifest(&home, &manifest).await;
        let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();

        let (_, dto) = get_capabilities(&state).await;
        assert_eq!(
            dto["searchProvider"], "managed",
            "nothing configured, so the platform's account answers: {dto}"
        );

        // A provider selected with no key is NOT a connection — the panel
        // must not report one, because the agents are still on managed.
        runtime
            .secrets()
            .set(runtime.id(), PROVIDER_SECRET, SecretValue("exa".into()))
            .await
            .unwrap();
        let (_, dto) = get_capabilities(&state).await;
        assert_eq!(dto["searchProvider"], "managed", "{dto}");

        runtime
            .secrets()
            .set(runtime.id(), API_KEY_SECRET, SecretValue("exa_key".into()))
            .await
            .unwrap();
        let (_, dto) = get_capabilities(&state).await;
        assert_eq!(
            dto["searchProvider"], "exa",
            "a finished connection is what the company searches through: {dto}"
        );
        // And never the key itself, on either path.
        assert!(!dto.to_string().contains("exa_key"), "{dto}");
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

/// **Issue #1192, test 1.** Publishing is reported on **both** DTO paths.
///
/// The DTO is built in two places — `unconfigured` and the configured tail
/// of `effective_status` — and a field wired into one of them alone reports
/// honestly for a company with no `[plan]` and is silently absent for every
/// company that has one. That is the failure mode the `OptInFlags` note
/// names, and it is worth a test rather than a convention because the two
/// literals are 200 lines apart.
#[tokio::test]
async fn publish_capability_is_reported_on_both_dto_paths() {
    // No `[plan]` → the `unconfigured` literal.
    let home_a_dir = home();
    let state = state_with_manifest(
        home_a_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"files\"]\n",
    )
    .await;
    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["configured"], false, "{dto}");
    assert_eq!(dto["publishGranted"], true, "{dto}");
    assert!(
        dto.get("publishInBuild").is_some(),
        "the build flag is always present so the console can render every state: {dto}"
    );

    // A `[plan]` → the configured literal, the one a field is most easily
    // forgotten in.
    let home_b_dir = home();
    let state2 = state_with_manifest(
        home_b_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"files\"]\n\
         [plan]\nname = \"starter\"\n",
    )
    .await;
    let (status2, dto2) = get_capabilities(&state2).await;
    assert_eq!(status2, StatusCode::OK);
    assert_eq!(dto2["configured"], true, "{dto2}");
    assert_eq!(
        dto2["publishGranted"], true,
        "a company with a plan must get the same publishing verdict: {dto2}"
    );
    assert_eq!(
        dto2["publishInBuild"], dto["publishInBuild"],
        "the build flag cannot depend on whether a plan is configured: {dto2}"
    );
}

/// **Issue #1192, test 2.** A company whose grants confer no file family
/// reads as ungranted — using the *shipped* `companies/e2e_harness` allow
/// list rather than an invented one, so the negative is a real manifest
/// somebody runs rather than a string chosen to make the assertion pass.
#[tokio::test]
async fn publish_is_ungranted_without_a_file_or_docs_grant() {
    let home_dir = home();
    let state = state_with_manifest(
        home_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\n\
         allow = [\"composio\", \"mcp:*\", \"workspace\", \"workspace.*\", \"web\"]\n",
    )
    .await;
    let (status, dto) = get_capabilities(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        dto["publishGranted"], false,
        "no files/docs grant means no publish_artifact on the belt: {dto}"
    );

    // …and the wildcard the majority of manifests actually ship DOES grant
    // it. Asserted here, beside the negative, so the two shapes read
    // against each other.
    let wildcard_dir = home();
    let state2 = state_with_manifest(
        wildcard_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n[tools]\nallow = [\"*\"]\n",
    )
    .await;
    let (_, dto2) = get_capabilities(&state2).await;
    assert_eq!(
        dto2["publishGranted"], true,
        "a bare `*` confers publishing — this is the shape most manifests ship: {dto2}"
    );
}
