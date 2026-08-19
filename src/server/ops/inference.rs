//! Per-tenant Bring-Your-Own-Key inference management (issue #56): read the
//! company's effective inference status, set a runtime provider override, revert
//! it, and live-probe the configured provider.
//!
//! The effective config is the highest-precedence of a runtime override (a JSON
//! blob the console writes under `inference/config`), the committed manifest
//! `[inference]` section, and the platform managed default. The outbound
//! credential lives apart under `inference/key` and is **write-only** over the
//! API: it is set through the `key` field, stored in the secret store, and never
//! echoed — the read shape carries only a `keyConfigured` bool.
//!
//! A runtime switch takes effect on the agents' **next turn** with no restart:
//! the per-tenant provider re-resolves this config every turn — *once the
//! company is already on the harness cognition path*. Which brain a company runs
//! is chosen once, at build time, so a company that resolved **no** inference
//! source at boot is on the offline echo brain until its runtime is rebuilt in
//! place (issue #290) or the process restarts. That transition is reported as
//! [`InferenceStatusDto::restart_required`] rather than papered over with a
//! "next turn" promise the runtime cannot keep (issue #266).
//!
//! Setting the key on the `managed` provider is an ordinary case, not a BYOK
//! edge (issue #585): it keeps the platform endpoint and swaps only the
//! credential, so the company pays for its own agents on the TinyHumans brain.
//! `resolve_endpoint` has always preferred a stored key over the env default
//! here; what was missing was anywhere for an admin to type one.

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, response::Response};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::app::config::EnvSource;
use crate::company::Inference;
use crate::company::inference::{
    self, EnvDefault, InferenceSource, RuntimeInference, clear_runtime_config, resolve_effective,
    save_runtime_config, store_key, validate_runtime,
};
use crate::company::runtime::CompanyRuntime;
use crate::error::OpenCompanyError;
use crate::ports::UsageMetering;
use crate::server::error::ApiError;
use crate::server::ops::{AdminScopedCompany, ScopedCompany, scoped};

/// The reminder attached to a mutating response that the running brain can act
/// on: a per-tenant provider re-resolves its config each turn, so a switch
/// reaches agents on the next turn with no restart.
const SWITCH_NOTE: &str =
    "Agents use the new inference provider on their next turn — no restart needed.";

/// The reminder attached instead when [`restart_pending`] holds — the save
/// landed, but the running brain predates it (issue #266). Says the thing the
/// operator has to do, and names the two surfaces that stay broken until they do
/// it, because "agents still echo" and "scheduled workflows never fire" are how
/// this is actually noticed.
const RESTART_NOTE: &str = "Saved — but this company started with no inference source, so it is \
     running the offline echo brain and its workflow runner is unwired. The brain is chosen at \
     startup: restart the company for agents to think with this provider and for scheduled \
     workflows to fire.";

/// The reminder attached when [`restart_pending`] held and the runtime was
/// rebuilt in place to clear it (issue #290). The restart the operator was
/// previously told to perform has already happened, for this company only.
const REBUILT_NOTE: &str = "Saved, and this company's runtime was rebuilt so the new provider is \
     live now. Agents think with it from their next turn and scheduled workflows fire again — no \
     restart needed.";

/// Builds the inference management route fragment.
pub fn router() -> Router<AppState> {
    scoped(
        "/inference",
        get(get_status).put(set_config).delete(revert_config),
    )
    .merge(scoped("/inference/test", post(test_config)))
    .merge(scoped("/inference/restart", post(restart_runtime)))
}

/// The company's effective inference status as the console renders it. **Never**
/// carries a credential — only a non-secret `keyConfigured` flag.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InferenceStatusDto {
    /// Provider kind (`managed` / `openrouter` / `openai_compatible` / `ollama`).
    provider: String,
    /// The stable telemetry slug (`managed` / `openrouter` / `byok` / `ollama`).
    slug: String,
    /// Resolved OpenAI-compatible base URL — the endpoint requests actually
    /// travel to, including the platform default this deployment was pointed at
    /// with `OPENCOMPANY_INFERENCE_URL`. A `managed` provider inherits that
    /// endpoint, so resolving this without the platform default printed the
    /// built-in production constant on every staging tenant (issue #597).
    base_url: String,
    /// Abstract-tier → concrete model id.
    models: BTreeMap<String, String>,
    /// Where the effective config came from: `default` / `manifest` / `runtime`,
    /// or `managed` when nothing tenant-specific is configured.
    source: String,
    /// Whether an outbound credential is stored — never the credential itself.
    key_configured: bool,
    /// The cognition path this company actually booted onto: `harness` (live
    /// local inference), `hosted` (Medulla), `sidecar`, `echo` (offline — no
    /// inference at all), or `custom`.
    ///
    /// Config resolving to a provider does **not** guarantee the harness path: a
    /// build without the `openhuman` feature, or a config that fails to resolve at
    /// boot, silently falls back to the hosted/echo brain. Reporting what the
    /// runtime actually holds is the only honest answer (issue #174).
    cognition: String,
    /// Where this path's inference usage is metered: `perTurn` (the harness meters
    /// each turn), `perCycle` (the runtime meters what the cycle reports), or
    /// `none` (nothing to meter — the echo path runs no model, so a zero Usage
    /// reading is the truth rather than a missing hook).
    usage_metering: UsageMetering,
    /// Whether a stored inference config resolves but the **running** brain
    /// predates it, so only a restart puts it to work (issue #266).
    ///
    /// See [`restart_pending`] for the exact predicate. `false` covers both "the
    /// config is already live" and "no restart would help either" — the console
    /// tells the second apart from `cognition` on its own, and this flag never
    /// promises a restart that would change nothing.
    restart_required: bool,
}

/// A mutating response: the resulting status plus the switch reminder.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MutationResponse {
    status: InferenceStatusDto,
    note: String,
}

/// Set-config body. `key` is write-only intake (never returned).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetInference {
    provider: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    models: Option<BTreeMap<String, String>>,
    /// The outbound credential, stored write-only. Omit to leave it unchanged;
    /// send an empty string to clear it.
    #[serde(default)]
    key: Option<String>,
}

/// Loads the company's committed `[inference]` section from its record.
async fn manifest_inference(runtime: &CompanyRuntime) -> Result<Inference, ApiError> {
    let record = runtime.store().load(runtime.id()).await.map_err(ApiError)?;
    Ok(record.map(|r| r.manifest.inference).unwrap_or_default())
}

/// The console-facing source label for a resolved source badge.
fn source_label(source: InferenceSource) -> &'static str {
    match source {
        InferenceSource::Default => "default",
        InferenceSource::Manifest => "manifest",
        InferenceSource::Runtime => "runtime",
    }
}

/// Whether the harness cognition path is reachable on this host at all: the
/// `openhuman` feature compiled in **and** a harness pool attached at boot.
///
/// Without both, no restart can move this company off the echo/hosted brain, so
/// telling the operator to restart would just be a second false promise. The
/// pool is attached whenever the serve path ran `attach_harness`, independently
/// of which brain arm won — which is exactly the "this host could have run the
/// harness, and didn't" signal we need.
#[cfg(feature = "openhuman")]
fn harness_reachable(runtime: &CompanyRuntime) -> bool {
    runtime.harness().is_some()
}

#[cfg(not(feature = "openhuman"))]
fn harness_reachable(_runtime: &CompanyRuntime) -> bool {
    false
}

/// Whether a saved inference config is stranded behind a boot-time decision.
///
/// Brain selection happens once, in `RuntimeBuilder::build`: a company whose
/// inference resolved to nothing at boot gets the offline echo brain **and** an
/// unwired workflow runner, and a later credential write reaches neither. The
/// per-tenant provider does re-resolve every turn, so swapping a model or
/// rotating a key on a company already on the harness path is genuinely live —
/// this is only about the not-configured → configured transition (issue #266).
///
/// True requires all three:
/// 1. a tenant config resolves *now* (the same predicate `build` tests),
/// 2. the company is **not** on the harness path, and
/// 3. the harness path is reachable here, so a restart would actually change it.
fn restart_pending(runtime: &CompanyRuntime, configured: bool) -> bool {
    configured
        && runtime.cognition().path != crate::ports::brain::HARNESS_PATH
        && harness_reachable(runtime)
}

/// The three distinct reasons a company's `workflow_runner()` is `None`. All
/// three look identical from `workflow_runner() == None`, but the operator's
/// next step differs for each, so the run route classifies before answering
/// (issues #266, #514).
pub(crate) enum RunnerGap {
    /// A saved inference config is stranded behind a boot-time decision: a
    /// restart would rebuild the runner from it. → `restart_required` (issue
    /// #266).
    RestartPending,
    /// Nothing is configured, but this host *could* run the harness — so the
    /// operator has to configure an inference source first (which triggers
    /// #290's rebuild-in-place). → `inference_required` (issue #514).
    InferenceRequired,
    /// This build/deployment genuinely has no workflow execution, or no restart
    /// or config here would produce one (default build, resolve error, or a
    /// company already on the harness path). → `not_wired`.
    NotWired,
}

/// Classifies *why* a company has no workflow runner, resolving inference from
/// scratch in one manifest read/resolve pass.
///
/// The workflow-run route uses it to pick between three responses a bare
/// `workflow_runner() == None` cannot tell apart (issues #266, #514):
///
/// - [`RunnerGap::RestartPending`] — a config resolves *now*, the company is off
///   the harness path, and the harness is reachable here, so a restart would
///   wire the runner (the same predicate `build` tests, via [`restart_pending`]).
/// - [`RunnerGap::InferenceRequired`] — nothing resolves, but the harness is
///   reachable and the company is off the harness path, so *configuring* an
///   inference source is what wires the runner. Telling this operator the
///   deployment is "not wired" is a lie: the deployment is fine; the company
///   just has no brain yet.
/// - [`RunnerGap::NotWired`] — everything else: the default build with no
///   harness, a resolve error (a config we cannot read is not evidence a restart
///   or a save would help — the #266 doctrine), or a company already on the
///   harness path.
pub(crate) async fn runner_gap_for(runtime: &CompanyRuntime) -> RunnerGap {
    let Ok(manifest) = manifest_inference(runtime).await else {
        return RunnerGap::NotWired;
    };
    // A resolve *error* is not the same as a clean resolve to nothing. `Err`
    // means the config could not be read at all — which the #266 doctrine says
    // is not evidence that a restart or a save would help, so it degrades to
    // `NotWired`. Only `Ok(None)` — inference resolved, and nothing is set — can
    // make configuring inference the honest next step. Folding the two together
    // (the old `is_ok_and`) would tell an operator whose config we cannot read
    // to "configure inference", a 409 promising a fix that may not exist.
    let (configured, resolved_to_nothing) =
        match resolve_effective(runtime.id(), &manifest, None, runtime.secrets().as_ref()).await {
            Ok(Some(_)) => (true, false),
            Ok(None) => (false, true),
            Err(_) => (false, false),
        };
    if restart_pending(runtime, configured) {
        return RunnerGap::RestartPending;
    }
    if resolved_to_nothing
        && harness_reachable(runtime)
        && runtime.cognition().path != crate::ports::brain::HARNESS_PATH
    {
        return RunnerGap::InferenceRequired;
    }
    RunnerGap::NotWired
}

/// This deployment's platform-injected managed default — the same
/// `(base_url, credential)` pair the harness routes on
/// ([`harness_inference_from_env`](crate::harness::provider::harness_inference_from_env)),
/// read from the environment and never from a tenant secret.
///
/// `None` when no managed credential resolves — which is exactly when the
/// harness resolves no env default either. Deriving it from that one function
/// rather than reading `OPENCOMPANY_INFERENCE_URL` directly is what keeps
/// display and routing honest in both directions: a bare URL with no credential
/// routes nowhere, so the card must not advertise it as the endpoint in use.
///
/// Also `None` off the `openhuman` feature, which is a shipped configuration
/// (`Dockerfile`'s `FEATURES` arg defaults to empty, and `deploy/README.md`
/// suggests sets like `medulla tinyplace sqlite`) — but not a gap. Nothing
/// outside `src/harness/` reads `OPENCOMPANY_INFERENCE_URL`, so in such a build
/// there is no injected endpoint for the card to misreport: the built-in
/// constant is the only inference endpoint that exists, and the company is on
/// the echo or hosted brain, which `cognition` reports on its own. The medulla
/// hosted path talks to `DEFAULT_API_URL` over its own socket protocol rather
/// than an OpenAI-compatible surface, so it is not an endpoint `baseUrl`
/// describes either.
fn platform_default(env: &dyn EnvSource) -> Option<EnvDefault> {
    #[cfg(feature = "openhuman")]
    {
        crate::harness::provider::harness_inference_from_env(env).map(|(config, _)| EnvDefault {
            base_url: config.base_url,
            credential: config.credential,
        })
    }
    #[cfg(not(feature = "openhuman"))]
    {
        let _ = env;
        None
    }
}

/// Resolves the effective status DTO against the real process environment.
async fn effective_status(runtime: &CompanyRuntime) -> Result<InferenceStatusDto, ApiError> {
    effective_status_with(
        runtime,
        platform_default(&crate::app::config::ProcessEnv).as_ref(),
    )
    .await
}

/// [`effective_status`] against an explicit platform default, so the resolution
/// is testable without touching the process environment.
///
/// Two resolves, deliberately, because the card answers two different questions
/// (issue #597):
///
/// - **What did the tenant configure?** — resolved with no env default, and the
///   only thing `source`, `keyConfigured` and `restartRequired` may see. A
///   platform endpoint is not tenant config: reporting it as `source: "default"`
///   would move the badge, reporting the platform token as `keyConfigured` would
///   tell an operator a key they never set is stored (and that blanking the
///   field "keeps" it), and feeding it to [`restart_pending`] would strand a
///   company behind a restart that changes nothing. That is the intent the old
///   `None` argument was protecting, and it survives here intact.
/// - **Where do requests actually go?** — resolved *with* the platform default,
///   and used for `baseUrl` alone. `managed` inherits the platform endpoint, so
///   without it every non-production tenant rendered the built-in production
///   constant: both the `None` arm below and any tenant whose own config names
///   `managed` without its own `base_url`.
async fn effective_status_with(
    runtime: &CompanyRuntime,
    platform: Option<&EnvDefault>,
) -> Result<InferenceStatusDto, ApiError> {
    let manifest = manifest_inference(runtime).await?;
    let secrets = runtime.secrets().as_ref();
    let decl = resolve_effective(runtime.id(), &manifest, None, secrets)
        .await
        .map_err(ApiError)?;
    let base_url = match platform {
        // Re-resolve with the platform default in place rather than
        // reconstructing `resolve_endpoint`'s precedence here — the tenant's own
        // `base_url` still outranks it, and only the `managed` kind inherits it.
        Some(platform) => resolve_effective(runtime.id(), &manifest, Some(platform), secrets)
            .await
            .map_err(ApiError)?
            .map_or_else(|| inference::MANAGED_BASE_URL.to_string(), |d| d.base_url),
        // No platform endpoint on this deployment: nothing to inherit, so the
        // tenant resolve already holds the whole answer and the second read is
        // skipped.
        None => decl.as_ref().map_or_else(
            || inference::MANAGED_BASE_URL.to_string(),
            |d| d.base_url.clone(),
        ),
    };
    // What the company actually booted onto, not what the config implies.
    let cognition = runtime.cognition();
    let restart_required = restart_pending(runtime, decl.is_some());
    Ok(match decl {
        Some(d) => InferenceStatusDto {
            provider: d.provider.clone(),
            slug: d.telemetry_slug().to_string(),
            base_url,
            models: d.models.clone(),
            source: source_label(d.source).to_string(),
            key_configured: d.key_configured(),
            cognition: cognition.path.to_string(),
            usage_metering: cognition.metering,
            restart_required,
        },
        None => InferenceStatusDto {
            provider: "managed".to_string(),
            slug: "managed".to_string(),
            base_url,
            models: BTreeMap::new(),
            source: "managed".to_string(),
            key_configured: false,
            cognition: cognition.path.to_string(),
            usage_metering: cognition.metering,
            // `decl` is `None`, so `restart_pending` is `false` here by
            // construction — nothing tenant-specific is configured to be
            // stranded. Threaded rather than hardcoded so the two arms cannot
            // drift apart.
            restart_required,
        },
    })
}

/// `GET …/inference` — the company's effective inference status.
async fn get_status(company: ScopedCompany) -> Result<Json<InferenceStatusDto>, ApiError> {
    Ok(Json(effective_status(company.runtime.as_ref()).await?))
}

/// `PUT …/inference` — set (or replace) the runtime provider override, and
/// optionally rotate the write-only outbound credential.
///
/// Requires authority over the company (issue #403). This route sets the base
/// URL every agent's prompts and completions travel to, and the key they are
/// billed against; deciding that is deciding how the company thinks. `POST
/// …/inference/test` stays open — it probes the config as already stored,
/// naming no destination of its own.
async fn set_config(
    State(state): State<AppState>,
    company: AdminScopedCompany,
    Json(body): Json<SetInference>,
) -> Result<Json<MutationResponse>, ApiError> {
    let runtime = company.runtime.as_ref();

    let config = RuntimeInference {
        provider: body.provider.trim().to_string(),
        base_url: body
            .base_url
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty()),
        models: body.models.unwrap_or_default(),
    };
    let problems = validate_runtime(&config);
    if !problems.is_empty() {
        return Err(ApiError(OpenCompanyError::InvalidRequest(
            problems.join(" "),
        )));
    }

    save_runtime_config(runtime.id(), runtime.secrets().as_ref(), &config)
        .await
        .map_err(ApiError)?;

    // The key is write-only: a non-empty value rotates it, an explicit empty
    // string clears it, and an omitted field leaves it untouched.
    if let Some(key) = body.key {
        store_key(runtime.id(), runtime.secrets().as_ref(), key.trim())
            .await
            .map_err(ApiError)?;
    }

    let status = effective_status(runtime).await?;
    // Issue #290: the not-configured → configured transition is the one a save
    // alone cannot deliver, because the brain was chosen at build time. Rather
    // than telling the operator to restart a container they may have no access
    // to, rebuild this company's runtime in place and re-read the status from
    // the successor.
    if status.restart_required {
        match crate::runtime::rebuild_company(&state, runtime.id()).await {
            Ok(successor) => {
                let status = effective_status(successor.as_ref()).await?;
                return Ok(Json(MutationResponse {
                    // Read off the *successor*, so a rebuild that somehow landed
                    // on the same brain still reports honestly rather than
                    // claiming a success the runtime cannot back up.
                    note: if status.restart_required {
                        RESTART_NOTE
                    } else {
                        REBUILT_NOTE
                    }
                    .to_string(),
                    status,
                }));
            }
            Err(err) => {
                // The save landed and the company is still running its old
                // brain, which is exactly what RESTART_NOTE describes. Falling
                // through is therefore the honest answer, not a swallowed error.
                tracing::warn!(
                    company = %runtime.id(),
                    error = %err,
                    "inference saved but the runtime could not be rebuilt; a restart is still required",
                );
            }
        }
    }
    Ok(Json(MutationResponse {
        // The note follows the *resulting* status, so the response can never
        // promise "next turn" to a company whose brain cannot honour it.
        note: if status.restart_required {
            RESTART_NOTE
        } else {
            SWITCH_NOTE
        }
        .to_string(),
        status,
    }))
}

/// `DELETE …/inference` — clear the runtime override, reverting to the committed
/// manifest `[inference]` (or the managed default). The stored credential is
/// left in place (harmless for the managed default; still resolves for a
/// manifest provider) — clear it explicitly with `PUT { key: "" }`.
/// Requires authority over the company (issue #403) — same reasoning as the
/// set: reverting decides which model the company thinks with.
async fn revert_config(company: AdminScopedCompany) -> Result<Json<MutationResponse>, ApiError> {
    let runtime = company.runtime.as_ref();
    clear_runtime_config(runtime.id(), runtime.secrets().as_ref())
        .await
        .map_err(ApiError)?;
    Ok(Json(MutationResponse {
        status: effective_status(runtime).await?,
        note: "Reverted to the committed manifest (or managed) configuration.".to_string(),
    }))
}

/// `POST …/inference/restart` — rebuild this company's runtime in place, now.
///
/// The action behind the console's "Restart required" notice. Saving inference
/// already attempts this rebuild ([`set_config`]), so this route exists for the
/// cases that attempt could not cover: a company that was already sitting in the
/// restart-required state before #290 landed, one whose rebuild failed
/// transiently, and one an operator arrives at without touching the form at all.
/// Without it the notice is a dead end — it names a restart the operator of a
/// hosted tenant has no way to perform, since the container is the unit of
/// restart and the control plane has no button for it.
///
/// Requires authority over the company, like the save it mirrors: rebuilding
/// swaps the brain every agent thinks with.
///
/// **Idempotent and safe to call when nothing is pending.** A rebuild of a
/// company that is already live is a no-op from the operator's point of view —
/// same journal, same parked approvals, same grants (see
/// [`rebuild`](crate::runtime::rebuild)) — so this does not gate on
/// `restart_required` first. Gating would introduce a race in which the check
/// and the rebuild disagree, and would refuse the one case most worth allowing:
/// an operator trying to recover a company whose state the console is reading
/// wrongly.
async fn restart_runtime(
    State(state): State<AppState>,
    company: AdminScopedCompany,
) -> Result<Json<MutationResponse>, ApiError> {
    let id = company.runtime.id().clone();
    let successor = crate::runtime::rebuild_company(&state, &id)
        .await
        .map_err(ApiError)?;
    // Read the status off the *successor*, never off the runtime we came in
    // with: a rebuild that landed on the same brain must still report honestly
    // rather than claim a success the runtime cannot back up.
    let status = effective_status(successor.as_ref()).await?;
    Ok(Json(MutationResponse {
        note: if status.restart_required {
            RESTART_NOTE
        } else {
            REBUILT_NOTE
        }
        .to_string(),
        status,
    }))
}

/// `POST …/inference/test` — a live one-message probe of the resolved provider.
///
/// Gated on the `openhuman` feature (the HTTP provider lives there); without it
/// the route reports `not_wired` so the console falls back gracefully. The probe
/// error is scrubbed of the credential by the provider layer.
#[cfg(feature = "openhuman")]
async fn test_config(company: ScopedCompany) -> Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    let runtime = company.runtime.as_ref();
    let manifest = match manifest_inference(runtime).await {
        Ok(m) => m,
        Err(err) => return err.into_response(),
    };
    let secrets = runtime.secrets().as_ref();
    // Gate on *tenant* config, with no platform default: the button probes what
    // the company configured, so "nothing configured" stays a 409 instead of
    // quietly probing the platform brain on the operator's behalf.
    let decl = match resolve_effective(runtime.id(), &manifest, None, secrets).await {
        Ok(d) => d,
        Err(err) => return ApiError(err).into_response(),
    };
    match decl {
        None => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "ok": false,
                "error": "No custom inference is configured — this company is on the managed brain.",
                "code": "not_configured",
            })),
        )
            .into_response(),
        Some(tenant) => {
            // Probe with the platform default in place. A `managed` config
            // inherits both the platform endpoint and its credential, so
            // resolving without it aimed the probe at the built-in production
            // URL carrying no bearer at all — a staging tenant whose routing is
            // perfectly fine would be told its provider is unreachable, on the
            // same card that was already misreporting the URL (issue #597).
            let decl = match platform_default(&crate::app::config::ProcessEnv) {
                None => tenant,
                Some(platform) => {
                    match resolve_effective(runtime.id(), &manifest, Some(&platform), secrets).await
                    {
                        // Adding the platform default can only add a source,
                        // never remove the one that just resolved — fall back to
                        // it rather than inventing an error path for a case that
                        // cannot happen.
                        Ok(d) => d.unwrap_or(tenant),
                        Err(err) => return ApiError(err).into_response(),
                    }
                }
            };
            match crate::harness::provider::probe(&decl).await {
                Ok(()) => Json(serde_json::json!({
                    "ok": true,
                    "provider": decl.provider,
                    "note": "Reached the provider and got a reply.",
                }))
                .into_response(),
                Err(err) => (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({
                        "ok": false,
                        "error": format!("Inference probe failed: {err}"),
                        "code": "probe_failed",
                    })),
                )
                    .into_response(),
            }
        }
    }
}

/// Without the `openhuman` feature there is no HTTP provider, so the live probe
/// is "not wired" (the console falls back to the stored status).
#[cfg(not(feature = "openhuman"))]
async fn test_config(company: ScopedCompany) -> Response {
    let _ = company;
    crate::server::ops::not_wired("inference test")
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    use crate::company::CompanyManifest;
    use crate::ports::types::{CompanyId, CompanyRecord};
    use crate::runtime::RuntimeBuilder;
    use crate::server::router;
    use crate::store::FsCompanyStore;
    use crate::{AppConfig, AppState};

    const TOKEN: &str = "sk-super-secret-inference-token-XYZ";

    fn home() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("oc-inference-")
            .tempdir()
            .expect("tempdir")
    }

    fn manifest() -> CompanyManifest {
        toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap()
    }

    /// Commits `manifest` as `id`'s record — what `manifest_inference` reads.
    async fn save_record(home: &std::path::Path, id: &CompanyId, manifest: &CompanyManifest) {
        use crate::ports::CompanyStore;
        FsCompanyStore::new(home.to_path_buf())
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
            })
            .await
            .unwrap();
    }

    async fn state_with_company(home: &std::path::Path) -> AppState {
        let id = CompanyId::new("acme");
        save_record(home, &id, &manifest()).await;
        let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest())
            .with_id(id.clone())
            .build()
            .await
            .unwrap();
        let state = AppState::new(AppConfig::default());
        state.registry().insert(id, std::sync::Arc::new(runtime));
        crate::server::test_support::seed_fixed_admin(&state, "acme").await;
        state
    }

    /// A rebuilder that rebuilds over the handover, as the binary's does.
    struct Working {
        home: std::path::PathBuf,
    }

    #[async_trait::async_trait]
    impl crate::runtime::RuntimeRebuilder for Working {
        async fn rebuild(
            &self,
            _state: &AppState,
            request: crate::runtime::RebuildRequest,
        ) -> crate::Result<crate::company::runtime::CompanyRuntime> {
            RuntimeBuilder::new(self.home.clone(), request.manifest)
                .with_id(request.id)
                .with_handover(request.handover)
                .build()
                .await
        }
    }

    /// `POST …/inference/restart` rebuilds the runtime in place, so the console's
    /// "Restart required" notice has an action behind it rather than being a
    /// dead end pointing at a container the operator may not be able to touch.
    #[tokio::test]
    async fn restart_rebuilds_the_registered_runtime() {
        let home_dir = home();
        let home = home_dir.path();
        let id = CompanyId::new("acme");
        let state = state_with_company(home)
            .await
            .with_rebuilder(std::sync::Arc::new(Working {
                home: home.to_path_buf(),
            }));
        state.set_boot_inputs(id.clone(), crate::runtime::BootInputs::default());
        let before = state.registry().get(&id).expect("registered");

        let (status, resp, raw) =
            send(&state, "POST", "/api/v1/company/inference/restart", None).await;
        assert_eq!(status, StatusCode::OK, "{raw}");

        // A genuinely different runtime is registered — the point of the route.
        let after = state.registry().get(&id).expect("registered");
        assert!(
            !std::sync::Arc::ptr_eq(&before, &after),
            "the restart must actually swap the runtime, not report success and leave it"
        );
        // And it is taking work, rather than stuck in the quiesce window. A
        // company parked there refuses every cycle forever, which is worse than
        // the stale brain the rebuild was replacing.
        assert!(!after.is_quiesced());
        assert!(resp["status"].is_object(), "{raw}");
    }

    /// Calling it twice is not an error. The console offers the button off a
    /// status read, so it can always be a moment stale — refusing when nothing
    /// is pending would turn a harmless retry into a failure an operator has to
    /// interpret.
    #[tokio::test]
    async fn restarting_a_healthy_company_is_a_no_op_not_a_refusal() {
        let home_dir = home();
        let home = home_dir.path();
        let id = CompanyId::new("acme");
        let state = state_with_company(home)
            .await
            .with_rebuilder(std::sync::Arc::new(Working {
                home: home.to_path_buf(),
            }));
        state.set_boot_inputs(id, crate::runtime::BootInputs::default());

        for attempt in 1..=2 {
            let (status, _, raw) =
                send(&state, "POST", "/api/v1/company/inference/restart", None).await;
            assert_eq!(status, StatusCode::OK, "attempt {attempt}: {raw}");
        }
    }

    /// A host that wired no rebuilder cannot do this, and must say so rather
    /// than report a success that changed nothing. This is the pre-#290
    /// deployment, and the console keeps showing the restart notice.
    #[tokio::test]
    async fn a_host_that_cannot_rebuild_says_so() {
        let home_dir = home();
        let state = state_with_company(home_dir.path()).await;

        let (status, _, raw) =
            send(&state, "POST", "/api/v1/company/inference/restart", None).await;
        assert_ne!(status, StatusCode::OK, "{raw}");
        assert!(
            raw.contains("restart the process"),
            "the failure must tell the operator what will work instead: {raw}"
        );

        // Critically, the company is still serving. A failed rebuild that left
        // it quiesced would turn a cosmetic dead end into an outage.
        let (status, _, _) = send(&state, "GET", "/api/v1/company/inference", None).await;
        assert_eq!(status, StatusCode::OK);
    }

    async fn send(
        state: &AppState,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value, String) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("cookie", crate::server::test_support::fixed_cookie("acme"));
        let request = match body {
            Some(body) => request
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => request.body(Body::empty()).unwrap(),
        };
        let response = router(state.clone()).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let raw = String::from_utf8_lossy(&bytes).to_string();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value, raw)
    }

    // ---------------------------------------------------------------------
    // Issue #597 — the card must report the endpoint requests actually reach,
    // not the built-in production constant.
    // ---------------------------------------------------------------------

    /// Where a staging deployment is pointed with `OPENCOMPANY_INFERENCE_URL`.
    const STAGING_URL: &str = "https://staging-api.tinyhumans.ai/openai/v1";

    /// The platform default a staging tenant is injected with.
    fn staging_platform() -> EnvDefault {
        EnvDefault {
            base_url: STAGING_URL.to_string(),
            credential: crate::company::credentials::Credential::from_value(
                "platform-token".to_string(),
            ),
        }
    }

    /// A built runtime whose committed manifest is `manifest_toml`.
    async fn runtime_with(home: &std::path::Path, manifest_toml: &str) -> CompanyRuntime {
        let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
        let id = CompanyId::new("acme");
        save_record(home, &id, &manifest).await;
        RuntimeBuilder::new(home.to_path_buf(), manifest)
            .with_id(id)
            .build()
            .await
            .unwrap()
    }

    const NO_INFERENCE: &str = "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n";

    const MANAGED_MANIFEST: &str = "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [inference]\nprovider = \"managed\"\n";

    #[tokio::test]
    async fn unconfigured_company_reports_the_platform_url_not_the_built_in_default() {
        let home_dir = home();
        let runtime = runtime_with(home_dir.path(), NO_INFERENCE).await;

        // No platform endpoint on this deployment — the built-in constant is
        // still the only honest answer, and this arm must not regress.
        let dto = effective_status_with(&runtime, None).await.unwrap();
        assert_eq!(dto.base_url, inference::MANAGED_BASE_URL);

        // Pointed at staging, the card follows — and *only* the URL moves.
        let dto = effective_status_with(&runtime, Some(&staging_platform()))
            .await
            .unwrap();
        assert_eq!(dto.base_url, STAGING_URL);
        assert_eq!(dto.provider, "managed");
        assert_eq!(
            dto.source, "managed",
            "a platform endpoint is not tenant config"
        );
        assert!(
            !dto.key_configured,
            "the platform token is not a stored tenant key"
        );
        assert!(
            !dto.restart_required,
            "a platform endpoint strands no tenant config behind a restart"
        );
    }

    #[tokio::test]
    async fn a_managed_manifest_also_inherits_the_platform_url() {
        // The half of #597 the report did not cover: `resolve_endpoint` falls
        // back to the built-in constant for *any* `managed` config that names no
        // base URL of its own, so a tenant with `[inference] provider =
        // "managed"` printed the production URL too — under a `manifest` badge
        // rather than the `managed` one the report reproduced.
        let home_dir = home();
        let runtime = runtime_with(home_dir.path(), MANAGED_MANIFEST).await;

        let dto = effective_status_with(&runtime, None).await.unwrap();
        assert_eq!(dto.base_url, inference::MANAGED_BASE_URL);

        let dto = effective_status_with(&runtime, Some(&staging_platform()))
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
        let dto = effective_status_with(&runtime, Some(&platform))
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

        let dto = effective_status_with(&runtime, Some(&platform))
            .await
            .unwrap();
        assert!(
            dto.key_configured,
            "a console-set key must read as configured even on managed"
        );
        // Same company, same injected platform default, opposite answer — and the
        // endpoint is unmoved either way: paying for your own agents on the
        // managed brain does not take you off it.
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

        let dto = effective_status_with(&runtime, Some(&staging_platform()))
            .await
            .unwrap();
        assert_eq!(dto.base_url, "https://byo.example/v1");
    }

    #[tokio::test]
    async fn a_non_managed_provider_ignores_the_platform_default() {
        // Only `managed` inherits the platform endpoint; every other kind uses
        // its own resolved URL verbatim.
        let home_dir = home();
        let runtime = runtime_with(
            home_dir.path(),
            "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
             [inference]\nprovider = \"openrouter\"\n",
        )
        .await;

        let dto = effective_status_with(&runtime, Some(&staging_platform()))
            .await
            .unwrap();
        assert_eq!(dto.base_url, inference::OPENROUTER_BASE_URL);
        assert_eq!(dto.source, "manifest");
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

    #[cfg(feature = "openhuman")]
    #[test]
    fn platform_default_follows_the_injected_inference_url() {
        use crate::app::config::MapEnv;

        let env = MapEnv::new([
            ("TINYHUMANS_API_KEY", "platform-key"),
            ("OPENCOMPANY_INFERENCE_URL", STAGING_URL),
        ]);
        assert_eq!(
            platform_default(&env).map(|d| d.base_url),
            Some(STAGING_URL.to_string())
        );

        // A URL with no credential resolves to nothing — the same answer the
        // harness gives, so the card never advertises an endpoint that would
        // route nowhere.
        let bare = MapEnv::new([("OPENCOMPANY_INFERENCE_URL", STAGING_URL)]);
        assert!(platform_default(&bare).is_none());
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
    }

    /// Issue #585: the company's key on the *managed* brain — set, rotate, and
    /// clear — is the whole point of the screen, and the route must never echo
    /// any of the three tokens back.
    #[tokio::test]
    async fn managed_key_can_be_set_rotated_and_cleared() {
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
        assert_eq!(resp["status"]["provider"], "managed");
        assert_eq!(resp["status"]["slug"], "managed");
        assert_eq!(resp["status"]["source"], "runtime");
        assert_eq!(resp["status"]["keyConfigured"], true);
        // Setting only a key must not move the tenant off the managed endpoint.
        assert_eq!(
            resp["status"]["baseUrl"],
            crate::company::inference::MANAGED_BASE_URL
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
        for token in [TOKEN, ROTATED] {
            assert!(!raw.contains(token), "GET leaked a token: {raw}");
        }
    }

    #[tokio::test]
    async fn invalid_provider_config_is_rejected() {
        let home_dir = home();
        let home = home_dir.path().to_path_buf();
        let state = state_with_company(&home).await;

        // Ollama requires a base_url.
        let (status, err, _) = send(
            &state,
            "PUT",
            "/api/v1/company/inference",
            Some(json!({ "provider": "ollama" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            err["error"]
                .as_str()
                .unwrap_or_default()
                .contains("base_url"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn key_never_leaks_across_any_response() {
        let home_dir = home();
        let home = home_dir.path().to_path_buf();
        let state = state_with_company(&home).await;

        let (_, _, put_raw) = send(
            &state,
            "PUT",
            "/api/v1/company/inference",
            Some(json!({ "provider": "openai_compatible", "baseUrl": "https://byo.example/v1", "key": TOKEN })),
        )
        .await;
        let (_, _, get_raw) = send(&state, "GET", "/api/v1/company/inference", None).await;
        // The live probe path returns an error (unreachable host) — assert the
        // scrubbed error body still never contains the token.
        let (_, _, test_raw) = send(&state, "POST", "/api/v1/company/inference/test", None).await;

        for raw in [put_raw, get_raw, test_raw] {
            assert!(!raw.contains(TOKEN), "a response leaked the token: {raw}");
        }
    }

    // --- Issue #266: a save the running brain cannot honour --------------------

    /// On a host with no harness reachable, a saved config is *never* "restart
    /// pending" — the echo brain is where this build ends up no matter how many
    /// times it is restarted, and telling the operator otherwise would send them
    /// bouncing a process for nothing.
    ///
    /// This is the default build, so it is also the guard that keeps the flag
    /// from firing on every self-hosted instance that simply has no local
    /// inference compiled in.
    #[tokio::test]
    async fn a_save_is_not_restart_pending_when_no_restart_would_help() {
        let home_dir = home();
        let home = home_dir.path().to_path_buf();
        let state = state_with_company(&home).await;

        let (status, resp, _) = send(
            &state,
            "PUT",
            "/api/v1/company/inference",
            Some(json!({ "provider": "openrouter", "key": TOKEN })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // The config landed...
        assert_eq!(resp["status"]["source"], "runtime");
        assert_eq!(resp["status"]["keyConfigured"], true);
        // ...and the runtime is on the echo brain, but no harness is reachable
        // here, so there is nothing a restart would change.
        assert_eq!(resp["status"]["cognition"], "echo");
        assert_eq!(resp["status"]["restartRequired"], false);
        assert!(
            resp["note"]
                .as_str()
                .unwrap_or_default()
                .contains("no restart needed"),
            "{}",
            resp["note"]
        );

        let (_, dto, _) = send(&state, "GET", "/api/v1/company/inference", None).await;
        assert_eq!(dto["restartRequired"], false);
    }

    /// A company with nothing configured has nothing stranded, so the flag is
    /// off even before any save.
    #[tokio::test]
    async fn an_unconfigured_company_is_not_restart_pending() {
        let home_dir = home();
        let home = home_dir.path().to_path_buf();
        let state = state_with_company(&home).await;

        let (_, dto, _) = send(&state, "GET", "/api/v1/company/inference", None).await;
        assert_eq!(dto["source"], "managed");
        assert_eq!(dto["restartRequired"], false);
    }

    /// Issue #266, reproduced at the route: a company built with a harness pool
    /// but **no** inference source boots onto the echo brain with an unwired
    /// workflow runner. Storing a credential afterwards updates the secret store
    /// and nothing else — the brain is chosen in `RuntimeBuilder::build` and this
    /// one already ran.
    ///
    /// So the save must report `restartRequired`, the note must say restart
    /// rather than "next turn", and `POST …/workflows/{id}/run` must stop
    /// claiming the deployment lacks workflow execution when what it lacks is a
    /// restart.
    #[cfg(feature = "openhuman")]
    #[tokio::test]
    async fn configuring_inference_after_boot_reports_restart_required() {
        use crate::harness::HarnessPool;

        let home_dir = home();
        let home = home_dir.path().to_path_buf();

        // Build the company the way the serve path does — with a harness pool
        // attached — but with no inference source of any kind. That is the boot
        // this issue is about.
        let id = CompanyId::new("acme");
        let runtime = RuntimeBuilder::new(home.clone(), manifest())
            .with_id(id.clone())
            .with_harness(std::sync::Arc::new(HarnessPool::new()))
            .build()
            .await
            .unwrap();
        // The bug's precondition, asserted rather than assumed: the harness was
        // available and the company still landed on the offline brain.
        assert_eq!(
            runtime.cognition().path,
            "echo",
            "expected the no-inference boot to select the echo brain"
        );
        assert!(
            runtime.workflow_runner().is_none(),
            "expected the no-inference boot to leave the workflow runner unwired"
        );

        let state = AppState::new(AppConfig::default());
        state.registry().insert(id, std::sync::Arc::new(runtime));
        crate::server::test_support::seed_fixed_admin(&state, "acme").await;

        // Configure inference, exactly as the console does.
        let (status, resp, raw) = send(
            &state,
            "PUT",
            "/api/v1/company/inference",
            Some(json!({
                "provider": "openai_compatible",
                "baseUrl": "https://stub.invalid/v1",
                "key": TOKEN,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{raw}");
        assert_eq!(resp["status"]["source"], "runtime");
        assert_eq!(resp["status"]["keyConfigured"], true);
        // The config resolves now; the running brain still does not know it.
        assert_eq!(resp["status"]["cognition"], "echo");
        assert_eq!(resp["status"]["restartRequired"], true, "{raw}");

        let note = resp["note"].as_str().unwrap_or_default();
        assert!(note.contains("restart"), "note must say restart: {note}");
        assert!(
            !note.contains("next turn"),
            "note must not promise the next turn: {note}"
        );
        assert!(!raw.contains(TOKEN), "PUT response leaked the token: {raw}");

        // The flag is a property of the runtime, not of the mutation, so a plain
        // read reports it too — this is what keeps the warning on screen after
        // the toast is gone.
        let (_, dto, _) = send(&state, "GET", "/api/v1/company/inference", None).await;
        assert_eq!(dto["restartRequired"], true);

        // Second surface: the run route no longer blames the deployment.
        let (status, err, _) = send(
            &state,
            "POST",
            "/api/v1/company/workflows/daily/run",
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(err["code"], "restart_required");
        assert!(
            err["error"]
                .as_str()
                .unwrap_or_default()
                .contains("Restart"),
            "{err}"
        );

        // Reverting to managed un-strands the company: there is no longer a
        // saved config waiting on a restart, so the flag clears.
        let (_, resp, _) = send(&state, "DELETE", "/api/v1/company/inference", None).await;
        assert_eq!(resp["status"]["restartRequired"], false);

        // #514: ...and with nothing pending BUT the harness reachable here,
        // the run route no longer 404s "no workflow execution" — it names the
        // real dead end, that this company never configured inference. (Before
        // #514 this asserted 404 `not_wired`; that was the third, unhandled
        // cause the console read as a permanent gap and degraded to read-only.)
        let (status, err, raw) = send(
            &state,
            "POST",
            "/api/v1/company/workflows/daily/run",
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{raw}");
        assert_eq!(err["code"], "inference_required");
    }

    /// A brain standing in for the one a rebuild puts a configured company on,
    /// so the route's behaviour is pinned without constructing a real harness
    /// brain (which would resolve MCP servers, Composio and an agent roster
    /// inside a unit test).
    ///
    /// Only its [`Cognition`](crate::ports::Cognition) matters here: reporting
    /// the harness path is exactly what makes `restart_pending` false, which is
    /// the observable the console reads.
    #[cfg(feature = "openhuman")]
    struct RebuiltBrain;

    #[cfg(feature = "openhuman")]
    #[async_trait::async_trait]
    impl crate::ports::brain::Brain for RebuiltBrain {
        async fn run_cycle(
            &self,
            _req: crate::ports::types::CycleRequest,
            _host: &dyn crate::ports::brain::CycleHost,
        ) -> crate::Result<crate::ports::types::CycleResult> {
            Ok(crate::ports::types::CycleResult {
                channel_responses: Vec::new(),
                new_traces: Vec::new(),
                ledger_deltas: Vec::new(),
                token_usage: crate::ports::types::TokenUsage::default(),
            })
        }

        fn cognition(&self) -> crate::ports::Cognition {
            crate::ports::Cognition {
                path: crate::ports::brain::HARNESS_PATH,
                // A stub brain meters per turn and reports zero usage, so
                // nothing is double-counted (see `Brain::cognition`).
                provider: "stub",
                metering: crate::ports::UsageMetering::PerTurn,
            }
        }
    }

    /// A rebuilder that runs the real builder over the real handover, and puts
    /// the successor on a brain that reports the harness path — the shape a live
    /// rebuild produces once inference resolves.
    #[cfg(feature = "openhuman")]
    struct StubRebuilder {
        home: std::path::PathBuf,
    }

    #[cfg(feature = "openhuman")]
    #[async_trait::async_trait]
    impl crate::runtime::RuntimeRebuilder for StubRebuilder {
        async fn rebuild(
            &self,
            _state: &AppState,
            request: crate::runtime::RebuildRequest,
        ) -> crate::Result<crate::company::runtime::CompanyRuntime> {
            RuntimeBuilder::new(self.home.clone(), request.manifest)
                .with_id(request.id)
                .with_harness(std::sync::Arc::new(crate::harness::HarnessPool::new()))
                .with_brain(std::sync::Arc::new(RebuiltBrain))
                .with_handover(request.handover)
                .build()
                .await
        }
    }

    /// Issue #290, the half #266 did not ship: with a rebuilder wired, the save
    /// that *would* have set `restartRequired` rebuilds the company instead, and
    /// the response reports the successor rather than the runtime that is being
    /// replaced.
    #[cfg(feature = "openhuman")]
    #[tokio::test]
    async fn configuring_inference_after_boot_rebuilds_instead_of_asking_for_a_restart() {
        use crate::harness::HarnessPool;

        let home_dir = home();
        let home = home_dir.path().to_path_buf();
        let id = CompanyId::new("acme");
        let runtime = RuntimeBuilder::new(home.clone(), manifest())
            .with_id(id.clone())
            .with_harness(std::sync::Arc::new(HarnessPool::new()))
            .build()
            .await
            .unwrap();
        assert_eq!(runtime.cognition().path, "echo");

        let outgoing = std::sync::Arc::new(runtime);
        let state = AppState::new(AppConfig::default())
            .with_rebuilder(std::sync::Arc::new(StubRebuilder { home: home.clone() }));
        state.registry().insert(id.clone(), outgoing.clone());
        crate::server::test_support::seed_fixed_admin(&state, "acme").await;

        let (status, resp, raw) = send(
            &state,
            "PUT",
            "/api/v1/company/inference",
            Some(json!({
                "provider": "openai_compatible",
                "baseUrl": "https://stub.invalid/v1",
                "key": TOKEN,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{raw}");
        // The save landed, and the company is no longer stranded behind a boot
        // decision: it reports the live cognition path, not "restart me".
        assert_eq!(resp["status"]["source"], "runtime");
        assert_eq!(resp["status"]["keyConfigured"], true);
        assert_eq!(resp["status"]["cognition"], "harness", "{raw}");
        assert_eq!(resp["status"]["restartRequired"], false, "{raw}");
        let note = resp["note"].as_str().unwrap_or_default();
        assert!(!note.contains("restart the company"), "{note}");
        assert!(!raw.contains(TOKEN), "PUT response leaked the token: {raw}");

        // The registry really was swapped, and the replaced runtime is left
        // quiesced so nothing still holding it keeps driving a dead company.
        let registered = state.registry().get(&id).expect("still registered");
        assert!(!std::sync::Arc::ptr_eq(&registered, &outgoing));
        assert!(outgoing.is_quiesced());
        assert!(!registered.is_quiesced());

        // A plain read agrees: the banner clears itself rather than needing the
        // console to remember what the mutation said.
        let (_, dto, _) = send(&state, "GET", "/api/v1/company/inference", None).await;
        assert_eq!(dto["restartRequired"], false);
        assert_eq!(dto["cognition"], "harness");
    }

    /// Issue #514, case (c): a company built with a harness pool that never
    /// configured any inference source. `workflow_runner()` is `None` — but not
    /// because the deployment lacks execution (the harness is right here) and
    /// not because a saved config is stranded behind a boot decision (nothing
    /// was ever saved). The run route must name the real dead end:
    /// `inference_required` (409), pointing the operator at Settings — NOT the
    /// `not_wired` 404 the console reads as a permanent gap and degrades to a
    /// read-only view on.
    #[cfg(feature = "openhuman")]
    #[tokio::test]
    async fn running_a_workflow_with_no_inference_configured_asks_to_configure_inference() {
        use crate::harness::HarnessPool;

        let home_dir = home();
        let home = home_dir.path().to_path_buf();

        let id = CompanyId::new("acme");
        let runtime = RuntimeBuilder::new(home.clone(), manifest())
            .with_id(id.clone())
            .with_harness(std::sync::Arc::new(HarnessPool::new()))
            .build()
            .await
            .unwrap();
        // Precondition (case c): the harness was reachable, nothing is
        // configured, and the company still landed with no runner — the exact
        // shape that used to 404 `not_wired`.
        assert_eq!(runtime.cognition().path, "echo");
        assert!(runtime.workflow_runner().is_none());

        let state = AppState::new(AppConfig::default());
        state.registry().insert(id, std::sync::Arc::new(runtime));
        crate::server::test_support::seed_fixed_admin(&state, "acme").await;

        // Nothing configured — the status read agrees.
        let (_, dto, _) = send(&state, "GET", "/api/v1/company/inference", None).await;
        assert_eq!(dto["source"], "managed");
        assert_eq!(dto["restartRequired"], false);

        // The run route names the fix instead of blaming the deployment.
        let (status, err, raw) = send(
            &state,
            "POST",
            "/api/v1/company/workflows/daily/run",
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{raw}");
        assert_eq!(err["code"], "inference_required");
        let msg = err["error"].as_str().unwrap_or_default();
        assert!(
            msg.contains("inference"),
            "message must name inference: {msg}"
        );
        assert!(
            msg.contains("Settings"),
            "message must point at Settings: {msg}"
        );
        assert!(
            !msg.contains("not wired"),
            "message must not say 'not wired': {msg}"
        );
        assert!(
            !msg.contains("deployment"),
            "message must not blame the deployment: {msg}"
        );

        // The dead end left nothing behind — run history is still empty.
        let (status, runs, raw) = send(&state, "GET", "/api/v1/company/workflows/runs", None).await;
        assert_eq!(status, StatusCode::OK, "{raw}");
        // Issue #1012: the runs read is now a `{ runs, hasMore, … }` page.
        assert_eq!(runs["runs"].as_array().map(Vec::len), Some(0), "{runs}");
    }

    /// Issue #514 (review): a config that cannot be *read* is not evidence to
    /// tell the operator to configure inference. When `resolve_effective`
    /// returns `Err` — the secret store is unreachable — the classifier must
    /// degrade to `NotWired` (404), never `inference_required` (409), even with
    /// a harness right here. A 409 would promise a fix ("configure inference")
    /// that a resolve *failure* gives no reason to expect; the #266 doctrine is
    /// that an unreadable config is not evidence a save would help. Guards the
    /// `Err(_) => (false, false)` arm of `runner_gap_for` against a regression
    /// back to the old `is_ok_and`, which folded `Err` into "not configured".
    #[cfg(feature = "openhuman")]
    #[tokio::test]
    async fn a_resolve_error_stays_not_wired_even_with_a_harness() {
        use crate::harness::HarnessPool;
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
        let id = CompanyId::new("acme");
        let runtime = RuntimeBuilder::new(home.clone(), manifest())
            .with_id(id.clone())
            .with_harness(std::sync::Arc::new(HarnessPool::new()))
            .with_secrets(std::sync::Arc::new(FailingSecrets))
            .build()
            .await
            .unwrap();
        assert!(runtime.workflow_runner().is_none());

        // The classifier degrades an unreadable config to NotWired...
        assert!(
            matches!(
                super::runner_gap_for(&runtime).await,
                super::RunnerGap::NotWired
            ),
            "a resolve error must classify as NotWired, not InferenceRequired",
        );

        // ...and the run route answers 404 `not_wired`, not 409 `inference_required`.
        let state = AppState::new(AppConfig::default());
        state.registry().insert(id, std::sync::Arc::new(runtime));
        crate::server::test_support::seed_fixed_admin(&state, "acme").await;
        let (status, err, raw) = send(
            &state,
            "POST",
            "/api/v1/company/workflows/daily/run",
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{raw}");
        assert_eq!(err["code"], "not_wired");
    }

    /// Issue #514 negative control: on the default build (no `openhuman`
    /// feature, so no harness is reachable), an unconfigured company still gets
    /// the honest `not_wired` 404. The `inference_required` arm must NOT fire
    /// where configuring inference would be a false promise — there is no
    /// harness here to run it, so a restart or a save changes nothing.
    #[cfg(not(feature = "openhuman"))]
    #[tokio::test]
    async fn running_a_workflow_on_the_default_build_stays_not_wired() {
        let home_dir = home();
        let home = home_dir.path().to_path_buf();
        let state = state_with_company(&home).await;

        let (status, err, raw) = send(
            &state,
            "POST",
            "/api/v1/company/workflows/daily/run",
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{raw}");
        assert_eq!(err["code"], "not_wired");
    }

    /// Issue #514 negative control: a company that *has* configured inference
    /// but runs on a build with no harness reachable still gets `not_wired`.
    /// Neither the `restart_pending` arm (no harness → a restart changes
    /// nothing) nor the `inference_required` arm (a config is already present)
    /// applies.
    #[cfg(not(feature = "openhuman"))]
    #[tokio::test]
    async fn running_a_workflow_configured_without_a_harness_stays_not_wired() {
        let home_dir = home();
        let home = home_dir.path().to_path_buf();
        let state = state_with_company(&home).await;

        let (status, _, _) = send(
            &state,
            "PUT",
            "/api/v1/company/inference",
            Some(json!({ "provider": "openrouter", "key": TOKEN })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, err, raw) = send(
            &state,
            "POST",
            "/api/v1/company/workflows/daily/run",
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{raw}");
        assert_eq!(err["code"], "not_wired");
    }
}
