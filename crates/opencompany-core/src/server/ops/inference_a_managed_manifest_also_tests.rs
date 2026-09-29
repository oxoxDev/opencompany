use axum::http::StatusCode;
use serde_json::json;

use super::inference_test_support::*;
use super::*;
use crate::ports::types::CompanyId;
use crate::runtime::RuntimeBuilder;
use crate::{AppConfig, AppState};

#[tokio::test]
async fn a_managed_manifest_also_inherits_the_platform_url() {
    // The half of #597 the report did not cover: `resolve_endpoint` falls
    // back to the built-in constant for *any* `managed` config that names no
    // base URL of its own, so a tenant with `[inference] provider =
    // "managed"` printed the production URL too — under a `manifest` badge
    // rather than the `managed` one the report reproduced.
    let home_dir = home();
    let runtime = runtime_with(home_dir.path(), MANAGED_MANIFEST).await;

    let dto = effective_status_with(&runtime, None, false).await.unwrap();
    assert_eq!(dto.base_url, inference::PLATFORM_BASE_URL);

    let dto = effective_status_with(&runtime, Some(&staging_platform()), false)
        .await
        .unwrap();
    assert_eq!(dto.base_url, STAGING_URL);
    assert_eq!(dto.source, "manifest");
    assert!(
        !dto.key_configured,
        "the platform token must not read as a tenant key on a managed manifest"
    );
}

/// Three inputs converge on `keyConfigured`, and exactly one of them must
/// never feed it. Today the tenant sources are a manifest `api_key_secret`
/// and a console `PUT`; #634 makes the console path the ordinary way an
/// admin sets the key on `managed`, which is precisely the provider that
/// inherits the platform credential. So the field has to keep answering
/// "did the *tenant* store a credential" and never "is there a credential",
/// on the one company where both are true at once.
///
/// Pinned here rather than left to review: without it the distinction this
/// PR's two-resolve split exists to preserve is enforced by nothing.
#[tokio::test]
async fn a_platform_token_never_reads_as_a_console_set_key() {
    let home_dir = home();
    let runtime = runtime_with(home_dir.path(), MANAGED_MANIFEST).await;
    let platform = staging_platform();

    // The platform credential is doing the outbound work, and the card still
    // says no key is configured — because none of it is the tenant's.
    let dto = effective_status_with(&runtime, Some(&platform), false)
        .await
        .unwrap();
    assert!(
        !dto.key_configured,
        "the platform token is not a stored tenant key"
    );
    assert_eq!(dto.base_url, STAGING_URL);

    // An admin sets one from the console — the write #634's screen performs.
    inference::store_key(runtime.id(), runtime.secrets().as_ref(), "sk-console-set")
        .await
        .unwrap();

    let dto = effective_status_with(&runtime, Some(&platform), false)
        .await
        .unwrap();
    assert!(
        dto.key_configured,
        "a console-set key must read as configured"
    );
    // Unlike plain `openrouter` (`keyless_openrouter_rides_the_subscription_
    // and_a_key_goes_direct`), a `managed` config never goes direct: its
    // whole reason to be a separate provider from `openrouter` is that its
    // credential is a TinyHumans account key, valid only against the
    // TinyHumans proxy, not a raw OpenRouter secret. `resolve_endpoint`'s
    // managed branch documents this ("the endpoint is always the
    // platform's") — the key changes which credential rides the request,
    // never the endpoint it rides to. The base URL must therefore stay the
    // platform's even once the tenant's own key is stored.
    assert_eq!(dto.base_url, STAGING_URL);
}

#[tokio::test]
async fn an_explicit_tenant_base_url_outranks_the_platform_default() {
    let home_dir = home();
    let runtime = runtime_with(
        home_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [inference]\nprovider = \"managed\"\nbase_url = \"https://byo.example/v1\"\n",
    )
    .await;

    let dto = effective_status_with(&runtime, Some(&staging_platform()), false)
        .await
        .unwrap();
    assert_eq!(dto.base_url, "https://byo.example/v1");
}

/// A third-party endpoint we hold no credential for uses its own URL
/// verbatim — the platform default is not a fallback for somewhere we cannot
/// authenticate anyway.
#[tokio::test]
async fn a_third_party_provider_ignores_the_platform_default() {
    let home_dir = home();
    let runtime = runtime_with(
        home_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [inference]\nprovider = \"ollama\"\nbase_url = \"http://localhost:11434/v1\"\n",
    )
    .await;

    let dto = effective_status_with(&runtime, Some(&staging_platform()), false)
        .await
        .unwrap();
    assert_eq!(dto.base_url, "http://localhost:11434/v1");
    assert_eq!(dto.source, "manifest");
}

/// `openrouter` is dual-mode, and which mode it is in depends only on
/// whether the tenant holds a key. With none it rides the subscription on
/// the platform endpoint; that is the config a company starts on, and it
/// must work with nothing configured.
#[tokio::test]
async fn keyless_openrouter_rides_the_subscription_and_a_key_goes_direct() {
    let home_dir = home();
    let runtime = runtime_with(
        home_dir.path(),
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [inference]\nprovider = \"openrouter\"\n",
    )
    .await;
    let platform = staging_platform();

    let dto = effective_status_with(&runtime, Some(&platform), false)
        .await
        .unwrap();
    assert_eq!(dto.base_url, STAGING_URL, "proxied");
    assert_eq!(dto.slug, "subscription");
    assert!(!dto.key_configured);

    inference::store_key(runtime.id(), runtime.secrets().as_ref(), "sk-or-tenant")
        .await
        .unwrap();

    let dto = effective_status_with(&runtime, Some(&platform), false)
        .await
        .unwrap();
    assert_eq!(dto.base_url, inference::OPENROUTER_BASE_URL, "direct");
    assert_eq!(dto.slug, "openrouter");
    assert!(dto.key_configured);
}

/// The probe's gate stays keyed on *tenant* config. Pointing a deployment at
/// a platform endpoint gives the probe somewhere real to aim, but it must not
/// turn "nothing configured" into a live probe of the platform brain — that
/// 409 is the honest answer to "test my provider" from a company that has
/// not named one.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn probing_an_unconfigured_company_stays_not_configured() {
    let home_dir = home();
    let state = state_with_company(home_dir.path()).await;

    let (status, body, _) = send(&state, "POST", "/api/v1/company/inference/test", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "not_configured");
}

/// A company whose only inference lives in `[harness.inference]` resolves
/// the harness's provider for both status and probe — the same
/// default-harness fallback [`RuntimeBuilder::build`] applies at boot.
///
/// Before the fix `manifest_inference` read only the company-level
/// `[inference]`, so such a company reported `managed`, rejected
/// `/inference/test` as `not_configured`, and mislabeled its status while
/// turns ran on the harness configuration the same record holds.
#[cfg(feature = "openhuman")]
#[tokio::test]
async fn harness_only_inference_reports_the_harness_provider_for_status_and_probe() {
    let home_dir = home();
    let state = state_with_harness_inference(home_dir.path()).await;

    // Status resolves the default harness's `[harness.inference]`, not the
    // absent company-level section: the operator sees the provider their
    // turns actually run on.
    let (status, dto, _) = send(&state, "GET", "/api/v1/company/inference", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["provider"], "openai_compatible");
    assert_eq!(dto["source"], "manifest");
    assert_ne!(dto["provider"], "managed");

    // The probe resolves the same inference, so it is *not* rejected as
    // `not_configured`; it reaches the (unreachable) host and reports that
    // failure instead.
    let (status, body, _) = send(&state, "POST", "/api/v1/company/inference/test", None).await;
    assert_ne!(status, StatusCode::CONFLICT);
    assert_ne!(body["code"], "not_configured");
}

/// The desktop pointed at staging: no instance credential in the
/// environment, a company account key minted on staging, `api_url` on
/// staging. Every managed surface must say staging — the LLM page's
/// endpoint, the managed card, and the endpoint a turn would actually be
/// sent to. Before the platform default followed `api_url` on its own,
/// each of these fell back to the production constant the moment the
/// environment held no `TINYHUMANS_API_KEY`, so the key was presented to
/// a platform that had never issued it.
#[tokio::test]
async fn a_company_key_on_a_staging_host_is_presented_to_staging() {
    const STAGING_API: &str = "https://staging-api.tinyhumans.ai";
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let id = CompanyId::new("acme");
    save_record(&home, &id, &manifest()).await;
    let config = AppConfig {
        api_url: STAGING_API.to_string(),
        ..AppConfig::default()
    };
    // Through `attach`, as `serve` and the desktop build their runtimes —
    // that is where the host's `api_url` becomes the runtime's default.
    let builder = crate::app::harness::attach(
        RuntimeBuilder::new(home.clone(), manifest()).with_id(id.clone()),
        &config,
    );
    let runtime = builder.build().await.unwrap();
    let state = AppState::new(config);
    state
        .registry()
        .insert(id.clone(), std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;

    let expected = format!("{STAGING_API}/agent-integrations/openrouter");

    // Nothing configured yet: the managed card already names the platform
    // this host is on, not production.
    let (status, dto, _) = send(&state, "GET", "/api/v1/company/inference", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["managed"]["baseUrl"], expected, "{dto}");
    assert_eq!(dto["managed"]["configured"], false, "{dto}");

    // The company's own account key: what the desktop stores.
    let (status, _, raw) = send(
        &state,
        "PUT",
        "/api/v1/company/credential",
        Some(json!({ "key": "tiny_test_minted_on_staging" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");

    let (status, dto, _) = send(&state, "GET", "/api/v1/company/inference", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["baseUrl"], expected, "{dto}");
    assert_eq!(dto["managed"]["baseUrl"], expected, "{dto}");
    assert_eq!(dto["managed"]["configured"], true, "{dto}");

    // And the declaration a turn resolves — the same resolver the brain
    // was built on — carries the same endpoint and the company key.
    let runtime = state.registry().get(&id).unwrap();
    let (manifest, _) = manifest_inference(&runtime).await.unwrap();
    let decl = resolve_effective(
        runtime.id(),
        &manifest,
        runtime.platform_default(),
        runtime.secrets().as_ref(),
    )
    .await
    .unwrap()
    .expect("a company key resolves managed inference");
    assert_eq!(decl.base_url, expected);
    assert!(decl.credential().configured());
}

/// A host with no instance credential and no company key still has a
/// platform default — the endpoint — but it must not be mistaken for a
/// source: nothing resolves, the company stays on the echo brain, and the
/// card reports Managed unconfigured.
#[tokio::test]
async fn an_endpoint_without_a_credential_is_not_a_source() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let id = CompanyId::new("acme");
    save_record(&home, &id, &manifest()).await;
    let config = AppConfig::default();
    let runtime = crate::app::harness::attach(
        RuntimeBuilder::new(home.clone(), manifest()).with_id(id.clone()),
        &config,
    )
    .build()
    .await
    .unwrap();
    assert!(
        runtime.platform_default().is_some(),
        "every attached runtime carries the platform endpoint"
    );
    let (manifest, _) = manifest_inference(&runtime).await.unwrap();
    let decl = resolve_effective(
        runtime.id(),
        &manifest,
        runtime.platform_default(),
        runtime.secrets().as_ref(),
    )
    .await
    .unwrap();
    assert!(decl.is_none(), "an endpoint alone routes nowhere: {decl:?}");
}

#[tokio::test]
async fn status_defaults_to_managed_then_switches_to_runtime() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home).await;

    // A company with no manifest/runtime inference reports the managed default.
    let (status, dto, _) = send(&state, "GET", "/api/v1/company/inference", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dto["provider"], "managed");
    assert_eq!(dto["source"], "managed");
    assert_eq!(dto["keyConfigured"], false);
    assert!(dto.get("key").is_none(), "status DTO must not carry a key");
    // Keys rework (#2306), slice 2d: no shipped tier defaults are sent
    // any more — every kind asks for a model explicitly (2c), so there is
    // nothing left to prefill from a guessed vocabulary.
    assert!(dto.get("defaultTierModels").is_none(), "{dto}");

    // Switch to OpenRouter with a write-only key + a tier→model map.
    let (status, resp, raw) = send(
        &state,
        "PUT",
        "/api/v1/company/inference",
        Some(json!({
            "provider": "openrouter",
            "models": { "chat-v1": "deepseek/deepseek-chat", "reasoning-v1": "deepseek/deepseek-r1" },
            "key": TOKEN,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(resp["status"]["provider"], "openrouter");
    assert_eq!(resp["status"]["slug"], "openrouter");
    assert_eq!(resp["status"]["source"], "runtime");
    assert_eq!(resp["status"]["keyConfigured"], true);
    assert_eq!(
        resp["status"]["models"]["chat-v1"],
        "deepseek/deepseek-chat"
    );
    // The token must NEVER appear in the mutation response body.
    assert!(!raw.contains(TOKEN), "PUT response leaked the token: {raw}");

    // GET reflects the switch and still never carries the token.
    let (_, dto, raw) = send(&state, "GET", "/api/v1/company/inference", None).await;
    assert_eq!(dto["provider"], "openrouter");
    assert_eq!(dto["source"], "runtime");
    assert_eq!(dto["keyConfigured"], true);
    assert!(!raw.contains(TOKEN), "GET status leaked the token: {raw}");
    assert!(dto.get("defaultTierModels").is_none(), "{dto}");
}

/// Keys rework (#2306) slice 2a, decision Q3: exactly one TinyHumans row
/// is ever shown. A `tinyhumans` row in the index hides the legacy row.
#[tokio::test]
async fn a_listed_tinyhumans_row_hides_the_legacy_managed_row() {
    use crate::company::inference::store;

    let home_dir = home();
    let runtime = runtime_with(home_dir.path(), NO_INFERENCE).await;
    let secrets = runtime.secrets().as_ref();
    store::put_provider(
        runtime.id(),
        secrets,
        store::ProviderDraft {
            slug: inference::MANAGED_SLUG.to_string(),
            label: "TinyHumans".to_string(),
            kind: inference::MANAGED_SLUG.to_string(),
            base_url: "https://api.tinyhumans.ai/agent-integrations/openrouter".to_string(),
            models: crate::company::INFERENCE_TIERS
                .iter()
                .map(|t| ((*t).to_string(), "acme/test-model".to_string()))
                .collect(),
            enabled: true,
        },
    )
    .await
    .unwrap();
    secrets
        .set(
            runtime.id(),
            &store::provider_key_key(inference::MANAGED_SLUG),
            crate::ports::types::SecretValue("th-not-a-real-key".to_string()),
        )
        .await
        .unwrap();

    let dto = effective_status_with(&runtime, None, false).await.unwrap();
    assert_eq!(
        dto.providers
            .iter()
            .filter(|p| p.slug == "tinyhumans")
            .count(),
        1,
        "exactly one tinyhumans row: {:?}",
        dto.providers
    );
    assert!(
        dto.managed.configured,
        "the legacy chain still resolves through the same key slot"
    );
    assert!(
        !dto.managed.legacy_row,
        "a listed tinyhumans row must hide the legacy Managed row"
    );
    assert!(
        !dto.managed.needs_model,
        "needs_model is moot once the legacy row is hidden"
    );
}

/// A company whose only TinyHumans credential is the account key
/// (`tinyhumans/key`, no row) still shows the legacy Managed row — and it
/// never reads as fully configured (D-key-without-row / X5).
#[tokio::test]
async fn an_account_key_only_company_shows_the_legacy_managed_row() {
    let home_dir = home();
    let runtime = runtime_with(home_dir.path(), NO_INFERENCE).await;
    crate::company::company_key::store_key(
        runtime.id(),
        runtime.secrets().as_ref(),
        "th-not-a-real-key",
    )
    .await
    .unwrap();

    let dto = effective_status_with(&runtime, None, false).await.unwrap();
    assert!(
        !dto.providers.iter().any(|p| p.slug == "tinyhumans"),
        "no tinyhumans row exists yet: {:?}",
        dto.providers
    );
    assert_eq!(dto.managed.source, "company_account");
    assert!(dto.managed.configured);
    assert!(
        dto.managed.legacy_row,
        "with no tinyhumans row, the legacy row is the only TinyHumans row"
    );
    assert!(
        dto.managed.needs_model,
        "a key with no row must never read as fully configured (X5)"
    );
}

/// A company with nothing configured shows no legacy row at all.
#[tokio::test]
async fn a_company_with_nothing_shows_no_legacy_row() {
    let home_dir = home();
    let runtime = runtime_with(home_dir.path(), NO_INFERENCE).await;

    let dto = effective_status_with(&runtime, None, false).await.unwrap();
    assert!(!dto.managed.configured);
    assert!(!dto.managed.legacy_row);
    assert!(!dto.managed.needs_model);
}

#[tokio::test]
async fn revert_clears_the_runtime_override() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home).await;

    send(
        &state,
        "PUT",
        "/api/v1/company/inference",
        Some(json!({ "provider": "openrouter", "key": TOKEN })),
    )
    .await;

    let (status, resp, _) = send(&state, "DELETE", "/api/v1/company/inference", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp["status"]["provider"], "managed");
    assert_eq!(resp["status"]["source"], "managed");
    assert_eq!(
        resp["status"]["keyConfigured"], false,
        "the reset must clear a stored credential too, or keyConfigured lies"
    );
}

/// The reset is a *full* reset (issue #993): reverting also clears a stored
/// key. This is what keeps a keyless reconfiguration keyless — without it, a
/// stale secret would make the company resolve direct even though the console
/// shows no key, and `DELETE` would strand it with a credential it can never
/// see or clear.
#[tokio::test]
async fn revert_clears_the_key_so_a_keyless_save_rides_the_subscription() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home).await;

    // Store a key first, so the reset has something stale to clear.
    let (status, resp, _) = send(
        &state,
        "PUT",
        "/api/v1/company/inference",
        Some(json!({ "provider": "openrouter", "key": TOKEN })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp["status"]["slug"], "openrouter");
    assert_eq!(resp["status"]["keyConfigured"], true);

    let (status, resp, _) = send(&state, "DELETE", "/api/v1/company/inference", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp["status"]["keyConfigured"], false);

    // A keyless save afterwards must land on the subscription, not be flung
    // direct by a credential the reset was supposed to remove.
    let (status, resp, raw) = send(
        &state,
        "PUT",
        "/api/v1/company/inference",
        Some(json!({ "provider": "openrouter" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(resp["status"]["slug"], "subscription");
    assert_eq!(resp["status"]["keyConfigured"], false);

    let (_, dto, raw) = send(&state, "GET", "/api/v1/company/inference", None).await;
    assert_eq!(dto["slug"], "subscription");
    assert_eq!(dto["keyConfigured"], false);
    assert!(!raw.contains(TOKEN), "GET leaked the reset token: {raw}");
}

/// Issue #585: the company's own key — set, rotate, and clear — is the whole
/// point of the screen, and the route must never echo any of the three
/// tokens back.
///
/// Written against the legacy `managed` provider name on purpose: it is what
/// a console built before the rename still sends, and it must keep working.
#[tokio::test]
async fn a_legacy_managed_key_can_be_set_rotated_and_cleared() {
    const ROTATED: &str = "sk-rotated-inference-token-ABC";
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_company(&home).await;

    // Set: the company pays for its own agents on the platform endpoint.
    let (status, resp, raw) = send(
        &state,
        "PUT",
        "/api/v1/company/inference",
        Some(json!({ "provider": "managed", "key": TOKEN })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    // The choice is echoed back as it was made. This used to read
    // `"openrouter"` — the alias resolved — which is what made the managed
    // route unselectable from the console: the card seeds its provider
    // select straight from this field, so saving `managed` and reading back
    // `openrouter` snapped the select (and the managed-only Connect button)
    // back to OpenRouter every time. Where it resolves to is still reported,
    // on the two fields that answer that question.
    assert_eq!(resp["status"]["provider"], "managed");
    assert_eq!(resp["status"]["source"], "runtime");
    assert_eq!(resp["status"]["keyConfigured"], true);
    // Attribution follows the endpoint, not the label — and the endpoint
    // is the platform's (next assertion), so the company is still proxied
    // through it, on its own key rather than the subscription.
    assert_eq!(
        resp["status"]["proxied"], true,
        "a managed key still rides the platform endpoint"
    );
    // The **endpoint stays the platform's**, and this assertion is the
    // fix. `managed` used to normalize onto `openrouter` before the managed
    // branch was consulted, so a company that declared `managed` and stored
    // a key had its requests sent to `openrouter.ai` — carrying, in the
    // credential-link flow that writes exactly this blob, a TinyHumans
    // token. Declaring `managed` means the company pays for its own agents
    // on the TinyHumans brain, which is what this route's own header has
    // said since #585 and what the code now does.
    assert_eq!(
        resp["status"]["baseUrl"],
        crate::company::inference::PLATFORM_BASE_URL
    );
    assert_eq!(
        resp["status"]["slug"], "subscription",
        "the telemetry slug separates the platform endpoint from a direct OpenRouter account"
    );
    assert!(!raw.contains(TOKEN), "PUT leaked the token: {raw}");

    // Rotate: a second key replaces the first, still write-only.
    let (status, resp, raw) = send(
        &state,
        "PUT",
        "/api/v1/company/inference",
        Some(json!({ "provider": "managed", "key": ROTATED })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert_eq!(resp["status"]["keyConfigured"], true);
    assert!(!raw.contains(TOKEN), "PUT leaked the old token: {raw}");
    assert!(!raw.contains(ROTATED), "PUT leaked the new token: {raw}");

    // Clear: an explicit empty key removes it (the console's "Remove key").
    let (status, resp, _) = send(
        &state,
        "PUT",
        "/api/v1/company/inference",
        Some(json!({ "provider": "managed", "key": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp["status"]["keyConfigured"], false);

    let (_, dto, raw) = send(&state, "GET", "/api/v1/company/inference", None).await;
    assert_eq!(dto["keyConfigured"], false);
    // The selection outlives the key: clearing the credential is not a way
    // of un-choosing the route, and a plain read has to report the same
    // thing the write did or the console will drift from it on reload.
    assert_eq!(dto["provider"], "managed");
    assert_eq!(
        dto["proxied"], true,
        "with the key gone it is back on the subscription"
    );
    assert_eq!(dto["slug"], "subscription");
    for token in [TOKEN, ROTATED] {
        assert!(!raw.contains(token), "GET leaked a token: {raw}");
    }
}
