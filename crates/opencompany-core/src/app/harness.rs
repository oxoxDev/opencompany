//! Attaching the embedded agent harness to a company runtime.
//!
//! Every process that builds a [`RuntimeBuilder`] and expects its companies to
//! be able to reach a model has to make the same four decisions: attach the
//! pool, and then wire whichever of the managed media, search and inference
//! backends the environment supplies. `serve` in `src/bin/opencompany.rs` used
//! to hold the only copy of that sequence, which meant the desktop shell —
//! which links this crate rather than spawning the binary — built companies
//! with no harness at all even in a build that compiled one in.
//!
//! So the sequence lives here, in [`attach`], for the same reason
//! [`prepare_instance`](crate::app::prepare_instance) does: one copy, shared by
//! the command line and by every embedder, rather than two that drift.
//!
//! Without the `openhuman` feature this is the identity function, so a default
//! build is byte-for-byte unaffected.

use crate::{app::types::AppConfig, runtime::RuntimeBuilder};

/// Attaches the harness pool and every managed backend the environment offers.
///
/// The pool is attached **unconditionally**, so cognition routes through a live
/// company agent whenever *any* inference source is configured — the managed
/// env default (`TINYHUMANS_API_KEY` / `OPENCOMPANY_INFERENCE_*`), a manifest
/// `[inference]` section, or a runtime console override (issue #56 — BYOK).
/// That is what unblocks a BYOK-only tenant with no platform credential: the
/// builder still constructs the harness brain from its manifest/runtime config.
/// With no source at all, the runtime keeps its hosted/echo brain.
///
/// Call this on any builder whose companies should be able to think.
#[cfg(not(feature = "openhuman"))]
pub fn attach(builder: RuntimeBuilder, config: &AppConfig) -> RuntimeBuilder {
    // No harness to think with, but the platform this host is on is still a
    // fact every managed surface reports — the LLM page's endpoint, the
    // managed probe — and it follows `api_url` here as it does with the
    // feature on.
    builder.with_api_url(config.api_url.clone())
}

#[cfg(feature = "openhuman")]
pub fn attach(builder: RuntimeBuilder, config: &AppConfig) -> RuntimeBuilder {
    use std::sync::Arc;

    use crate::app::config::ProcessEnv;
    use crate::harness::HarnessPool;
    use crate::harness::provider::{
        PlatformCredentialStatus, media_backend_from_env, platform_inference_default_at,
        search_backend_handle_from_env,
    };

    // Issue #879: every managed surface below fails closed and says nothing at
    // boot, so a tenant provisioned without its platform token comes up looking
    // healthy and only reveals the gap when an agent is built or a workflow node
    // 500s. Say it once, here, where an operator reading the first lines of the
    // log will see it.
    if let Some(warning) =
        PlatformCredentialStatus::resolve_at(&ProcessEnv, Some(&config.api_url)).boot_warning()
    {
        tracing::warn!("[boot] {warning}");
    }

    let builder = builder.with_harness(Arc::new(HarnessPool::new()));
    // Issue #109: the MANAGED media-generation backend, resolved from the
    // environment only (never a tenant secret). Absent ⇒ media tools stay unwired
    // even for a company that grants `media` (fail-closed).
    let builder = match media_backend_from_env(&ProcessEnv) {
        Some(media_backend) => builder.with_media_backend(media_backend),
        None => builder,
    };
    // Issue #238/#2342: one process-wide MANAGED web-search handle. It may have
    // no deployment credential: company credentials decorate clones at runtime,
    // while neither credential still leaves `web_search` unwired. Keeping the
    // base handle here makes its ledger shared by every harness lane and by
    // workflow tool calls.
    let builder = builder.with_search_backend(search_backend_handle_from_env(&ProcessEnv));
    // The managed default is the lowest-precedence source, and it is always
    // present: the *endpoint* follows this host's `api_url` whether or not the
    // environment holds an instance credential. A BYOK-only tenant, or a
    // desktop whose identity is the company's own account key, gets a default
    // whose credential is `Credential::None` — which every managed gate reads
    // as "cannot think on the instance identity", exactly as an absent default
    // did — but whose endpoint is the platform this host was configured for,
    // so a company key minted on staging is presented to staging. Before this
    // an absent credential meant an absent default, and an absent default
    // meant the production constant.
    let (inference, model_override) =
        platform_inference_default_at(&ProcessEnv, Some(&config.api_url));
    builder
        .with_api_url(config.api_url.clone())
        .with_harness_inference(inference, model_override)
}
