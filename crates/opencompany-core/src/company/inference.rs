//! Per-tenant Bring-Your-Own-Key inference (issue #56): the inert data model
//! plus the async secret-resolution used to materialize a company's *effective*
//! inference configuration.
//!
//! A company's effective inference config is the highest-precedence of three
//! sources:
//!
//! 1. **Runtime** — a config the operator sets through the console, persisted as
//!    a single JSON blob in the [`SecretStore`](crate::ports::SecretStore) under
//!    [`RUNTIME_CONFIG_KEY`]. Highest precedence, so a console switch takes
//!    effect on the agents' next turn with no rebuild.
//! 2. **Manifest** — the `[inference]` section committed in `company.toml`
//!    ([`Inference`]). Declarative intent; never a credential.
//! 3. **Default** — the platform-injected managed brain
//!    (`TINYHUMANS_API_KEY` / `OPENCOMPANY_INFERENCE_*`), passed in as an
//!    [`EnvDefault`]. Lowest precedence.
//!
//! Credentials live apart from the declarations. The outbound key is written to
//! its own [`KEY_KEY`] secret (write-only via the console) — never inline in the
//! runtime config blob or the manifest — and is attached to the
//! [`InferenceDecl`] as a [`Credential`] by [`resolve_effective`], then read on
//! the request path by [`InferenceDecl::bearer`]. Deferring the read is what lets
//! the managed tier be a *rotating* platform token rather than a value captured
//! once at boot. Nothing here ever serializes a credential into an API response,
//! log line, or agent-visible output: [`InferenceDecl`] derives no `Serialize`
//! and its `Debug` redacts the credential.

pub mod catalogue;
pub mod copy;
pub mod dialect;
pub(crate) mod legacy_tiers;
pub mod paged_catalog;
pub mod probe;
pub mod resolve;
pub mod store;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::company::credentials::Credential;
use crate::company::types::{INFERENCE_PROVIDERS, Inference};
use crate::error::OpenCompanyError;
use crate::ports::SecretStore;
use crate::ports::types::{CompanyId, SecretValue};

use self::store::provider_key_key;

/// The [`SecretStore`](crate::ports::SecretStore) key holding the JSON runtime
/// inference override (a [`RuntimeInference`] the console writes).
pub const RUNTIME_CONFIG_KEY: &str = "inference/config";

/// The canonical per-company inference credential key. The outbound token is
/// stored here (write-only via the console); the value is the raw token string.
pub const KEY_KEY: &str = "inference/key";

/// The [`SecretStore`](crate::ports::SecretStore) key holding the runtime
/// inference override for `harness_id`.
///
/// The **default** harness keeps the flat legacy key. That is not cosmetic: a
/// tenant's stored console override and credential already live at
/// [`RUNTIME_CONFIG_KEY`] / [`KEY_KEY`], and the store has no rename — so
/// namespacing every harness would silently orphan the config of every company
/// already running, which is the one migration this design cannot afford.
///
/// Non-default harnesses namespace under `harness/<id>/`, so two `built_in`
/// harnesses can hold two different OpenRouter accounts.
pub fn runtime_config_key(harness_id: &str, is_default: bool) -> String {
    if is_default {
        return RUNTIME_CONFIG_KEY.to_string();
    }
    format!("harness/{harness_id}/{RUNTIME_CONFIG_KEY}")
}

/// The credential key for `harness_id`. Same default-harness rule as
/// [`runtime_config_key`].
pub fn harness_key_key(harness_id: &str, is_default: bool) -> String {
    if is_default {
        return KEY_KEY.to_string();
    }
    format!("harness/{harness_id}/{KEY_KEY}")
}

/// Which harness's secrets a resolution reads, and whether that harness is the
/// company default (which keeps the flat legacy keys).
///
/// Passed as one value rather than two loose arguments because the pair is only
/// ever meaningful together — an id without the default flag cannot name a key.
#[derive(Clone, Debug)]
pub struct HarnessScope {
    /// The harness id.
    pub id: String,
    /// Whether it is the company's default harness.
    pub is_default: bool,
    /// Whether this harness declared `[harness.inference]` of its own.
    ///
    /// **Only the harness knows.** `built_in_lane` hands the resolver its own
    /// section where it has one and the company's `[inference]` where it does
    /// not, so by the time the value arrives the two are indistinguishable —
    /// and the difference decides whether the company's provider list outranks
    /// it. See [`resolve_effective_scoped`].
    pub declares_own_inference: bool,
}

impl HarnessScope {
    /// The scope for a company's default harness — what every pre-existing
    /// caller means, and what keeps them reading the flat keys.
    pub fn default_harness(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            is_default: true,
            declares_own_inference: false,
        }
    }

    /// A named, non-default harness.
    pub fn named(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            is_default: false,
            declares_own_inference: false,
        }
    }

    /// Records that this harness declared `[harness.inference]` of its own.
    pub fn declaring_own_inference(mut self, declares: bool) -> Self {
        self.declares_own_inference = declares;
        self
    }

    /// This scope's runtime-config secret key.
    pub fn config_key(&self) -> String {
        runtime_config_key(&self.id, self.is_default)
    }

    /// This scope's credential secret key.
    pub fn key_key(&self) -> String {
        harness_key_key(&self.id, self.is_default)
    }
}

impl Default for HarnessScope {
    fn default() -> Self {
        Self::default_harness(crate::company::types::IMPLICIT_HARNESS_ID)
    }
}

/// The platform's OpenAI-compatible endpoint — the TinyHumans OpenRouter proxy
/// that the legacy managed chain and an `openrouter` company with **no** key of
/// its own resolve against. The production value; see [`platform_base_url`] for
/// the one this instance actually uses.
///
/// Keys rework (#2306), slice 2a "commit 5": this used to be
/// `https://api.tinyhumans.ai/openai/v1`, the curated surface that takes tier
/// names (`chat-v1`). Every path that resolves here now sends a real model id
/// (2d), and the first-run wizard's `managed` provider probes and stores the id
/// it discovered — so the proxy, which lists real ids and rejects tier names, is
/// the right endpoint for all of them, and the same one the `tinyhumans`
/// provider row uses. The proxy fronts OpenRouter upstream and meters the spend
/// against the tenant's subscription, so from the workload's point of view this
/// and [`OPENROUTER_BASE_URL`] serve the same catalogue; only who pays differs.
pub const PLATFORM_BASE_URL: &str = "https://api.tinyhumans.ai/agent-integrations/openrouter";

/// The production TinyHumans OpenRouter proxy base used only when a resolver
/// has no per-runtime managed default. AppState carries its own `api_url` into
/// every attached runtime as that default, so separate AppStates cannot affect
/// one another's inference endpoint.
pub fn platform_base_url() -> String {
    PLATFORM_BASE_URL.to_string()
}

/// The provider kind removed when OpenCompany stopped exposing its own model
/// SKUs. A manifest or stored runtime blob still naming it aliases to
/// [`DEFAULT_PROVIDER`] rather than failing: a runtime blob is data an operator
/// cannot hand-edit, so hard-failing on it would strand a tenant whose console
/// wrote a value that used to be valid.
pub const LEGACY_MANAGED: &str = "managed";

/// The provider a company gets when nothing names one.
pub const DEFAULT_PROVIDER: &str = "openrouter";

/// Normalizes a provider kind: blank and the legacy `managed` both become
/// [`DEFAULT_PROVIDER`]; anything else passes through for validation to judge.
pub fn normalize_provider(provider: &str) -> &str {
    match provider.trim() {
        "" | LEGACY_MANAGED => DEFAULT_PROVIDER,
        other => other,
    }
}

/// The setup wizard's "TinyHumans" (managed) card, before [`normalize_provider`]
/// folds it into `openrouter`.
///
/// The managed choice must resolve to the platform endpoint and the injected
/// managed credential, never to a `base_url` the operator never typed — the card
/// has no URL field. Once normalized it is indistinguishable from a real
/// `openrouter`, so the managed probe branch keys on the raw kind instead. Only
/// [`decl_for_probe`] passes the raw kind here; [`resolve_effective_scoped`]
/// normalizes first, so runtime resolution of a legacy `managed` blob is
/// unaffected.
pub fn is_managed_choice(provider: &str) -> bool {
    matches!(provider.trim(), LEGACY_MANAGED | "tinyhumans")
}

/// The provider kind **as the operator chose it**, before [`normalize_provider`]
/// folds the managed alias into `openrouter`.
///
/// [`normalize_provider`] answers "where does this config resolve to", which is
/// the right question on every request path and the wrong one for the console:
/// the managed route and a plain `openrouter` route resolve identically, so
/// normalizing on the way *out* made "Managed (TinyHumans)" unselectable —
/// saving it stored `managed`, reading it back reported `openrouter`, and the
/// card's provider select (seeded from that answer verbatim) snapped straight
/// back to OpenRouter along with the Connect-TinyHumans button that only the
/// managed route offers. This is the read-back half of that pair: the operator's
/// own word for the route, canonicalized to [`LEGACY_MANAGED`] so the console has
/// exactly one spelling to render, and never used to decide an endpoint.
pub fn selected_kind(provider: &str) -> &str {
    if is_managed_choice(provider) {
        return LEGACY_MANAGED;
    }
    normalize_provider(provider)
}

/// The slug the managed/TinyHumans provider's credential is keyed on.
///
/// `tinyhumans`, not `openrouter`, even though [`normalize_provider`] folds the
/// managed kind onto `openrouter` for endpoint resolution. The two answer
/// different questions: the kind says *what shape of API this is*, the slug says
/// *whose account this is*. Keying the managed credential on `openrouter` would
/// put a TinyHumans key in the slot a real OpenRouter account belongs in, and a
/// company that had both would have one.
///
/// Not to be confused with [`company_key::KEY_KEY`](crate::company::company_key)
/// (`tinyhumans/key`), which is the company's **identity**. This is a slot for a
/// key pasted specifically for inference; that is the account the company signs
/// in as. They are consulted in that order and they are not the same thing.
pub const MANAGED_SLUG: &str = "tinyhumans";

/// Which `provider/<slug>/key` slot a provider kind's credential lives in.
///
/// One rule, used by the resolver and by the store's entry-zero reader, so the
/// address the turn path reads and the address the console writes cannot drift.
pub fn credential_slug(provider_raw: &str) -> &str {
    if is_managed_choice(provider_raw) {
        MANAGED_SLUG
    } else {
        normalize_provider(provider_raw)
    }
}

/// OpenRouter's OpenAI-compatible base URL — used when the `openrouter`
/// provider names no explicit `base_url`.
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// A local Ollama server's OpenAI-compatible surface — the convenience default
/// for the `ollama` provider (validation still requires an explicit `base_url`
/// in the manifest; this only backstops an empty resolved value).
pub const OLLAMA_DEFAULT_BASE_URL: &str = "http://localhost:11434/v1";

/// Where an effective inference config came from — drives the console's source
/// badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceSource {
    /// The platform-injected managed default (`env`).
    Default,
    /// Declared in `company.toml`'s `[inference]`.
    Manifest,
    /// Set at runtime through the console.
    Runtime,
}

/// The platform-injected managed default (from `harness_inference_from_env`):
/// the base URL + credential the manager supplies. Passed to
/// [`resolve_effective`] as the lowest-precedence source.
///
/// The credential is a [`Credential`], not a `String`: on the hosted platform it
/// is a projected token that rotates in place, so it is resolved per request
/// rather than captured here.
#[derive(Clone, Debug)]
pub struct EnvDefault {
    /// Managed base URL (env `OPENCOMPANY_INFERENCE_URL` or the default).
    pub base_url: String,
    /// Managed credential — a projected platform token source, or the static
    /// `OPENCOMPANY_INFERENCE_KEY` / `TINYHUMANS_API_KEY` value.
    pub credential: Credential,
}

/// The on-disk runtime inference override stored under [`RUNTIME_CONFIG_KEY`].
/// Carries no credential — the token lives apart under [`KEY_KEY`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeInference {
    /// Provider kind — one of [`INFERENCE_PROVIDERS`].
    pub provider: String,
    /// Optional OpenAI-compatible base URL override.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Abstract-tier → concrete model id.
    #[serde(default)]
    pub models: BTreeMap<String, String>,
}

/// One company's *effective* inference configuration — the highest-precedence
/// of runtime / manifest / env, carrying the credential as a resolvable
/// [`Credential`] rather than a captured value.
///
/// Derives **no** `Serialize` (the key must never cross a wire) and its `Debug`
/// redacts the credential.
#[derive(Clone, Debug)]
pub struct InferenceDecl {
    /// Provider slug — one of [`INFERENCE_PROVIDERS`]. Normalized: the managed
    /// alias has already been folded into `openrouter` here, because this is
    /// the field every resolution and attribution path reads.
    pub provider: String,
    /// The kind the operator actually selected, before normalization — see
    /// [`selected_kind`]. Differs from [`Self::provider`] only for the managed
    /// route, and exists so the console can render and re-offer the choice that
    /// was made rather than the one it resolves to.
    selected_provider: String,
    /// Resolved OpenAI-compatible base URL (never empty for a valid config).
    pub base_url: String,
    /// Abstract-tier → concrete model id. Empty means every tier passes
    /// through to the provider verbatim.
    pub models: BTreeMap<String, String>,
    /// Provenance badge for the console.
    pub source: InferenceSource,
    /// The outbound credential. Private — read only through
    /// [`bearer`](Self::bearer); never serialized.
    credential: Credential,
    /// Whether this rides the platform's subscription proxy. Read through
    /// [`is_proxied`](Self::is_proxied).
    proxied: bool,
    /// The model the new resolution path chose (keys rework, issue #2306,
    /// slice 2b): a full company default's (or, from slice 3a, an agent
    /// pair's). `None` on every legacy arm. Set only through
    /// [`with_chosen_model`](Self::with_chosen_model).
    chosen_model: Option<String>,
}

impl InferenceDecl {
    /// The provider kind as the operator selected it — [`Self::provider`] for
    /// every route but the managed one, which reports `managed`.
    ///
    /// For display and for re-offering the choice only. Anything deciding an
    /// endpoint, a credential or an attribution wants [`Self::provider`].
    pub fn selected_provider(&self) -> &str {
        &self.selected_provider
    }

    /// The outbound credential, unresolved. Callers on the request path want
    /// [`bearer`](Self::bearer); this is for status and fingerprinting.
    pub fn credential(&self) -> &Credential {
        &self.credential
    }

    /// The bearer to present on **this** request, or `None` to omit the header
    /// (the keyless Ollama case).
    ///
    /// Resolved per call rather than captured at build time: a hosted tenant's
    /// managed credential is a projected token the platform rotates in place, so
    /// a value captured once would go stale within minutes.
    pub async fn bearer(&self) -> Result<Option<String>> {
        self.credential.current().await
    }

    /// Whether an outbound credential is configured — the non-secret status the
    /// read APIs surface. Never returns the value, and never reads the token.
    pub fn key_configured(&self) -> bool {
        self.credential.configured()
    }

    /// Whether this config rides the platform's subscription proxy rather than
    /// a credential the tenant supplied.
    ///
    /// True only for `openrouter` with no tenant key — the default a company
    /// starts on. It is what separates "the subscription pays" from "the tenant
    /// pays", which is why it is recorded here rather than re-derived from the
    /// base URL by every caller that cares.
    pub fn is_proxied(&self) -> bool {
        self.proxied
    }

    /// The model a turn sends, when the new path (2b: a full company
    /// default; 3a: an agent pair) chose one.
    pub fn chosen_model(&self) -> Option<&str> {
        self.chosen_model.as_deref()
    }

    /// Attaches the chosen model (keys rework, issue #2306, slice 2b). Never
    /// call this on a legacy arm — [`decl_for_choice`] is the one place that
    /// does.
    #[must_use]
    pub fn with_chosen_model(mut self, model: String) -> Self {
        self.chosen_model = Some(model);
        self
    }

    /// The stable telemetry slug for this config
    /// (`subscription` / `openrouter` / `byok` / `ollama`).
    ///
    /// Distinguishes proxied from direct OpenRouter, because those are two
    /// different payers and a Usage view that merged them would be telling the
    /// operator nothing.
    pub fn telemetry_slug(&self) -> &'static str {
        if self.proxied {
            return "subscription";
        }
        provider_slug(&self.provider)
    }
}

/// The stable telemetry slug for a provider kind.
///
/// An unknown kind reports `unknown` rather than being folded into a real
/// provider's attribution: [`resolve_effective`] rejects one outright, so
/// reaching here with one means something upstream is wrong, and quietly
/// billing it to a provider that was never called would hide that.
///
/// This answers on the *kind* alone. Proxied OpenRouter slugs as
/// `subscription`, which needs the credential too — see
/// [`InferenceDecl::telemetry_slug`].
pub fn provider_slug(provider: &str) -> &'static str {
    match normalize_provider(provider) {
        "openrouter" => "openrouter",
        "ollama" => "ollama",
        "openai_compatible" => "byok",
        _ => "unknown",
    }
}

/// Resolves the effective `(base_url, credential, proxied)` for a provider.
///
/// **`openrouter` is dual-mode**, and that is the whole shape of the product's
/// first-run story:
///
/// * **No tenant key** — the config inherits the platform endpoint and the
///   platform credential, and the subscription pays. This is where a company
///   starts, with nothing configured and nobody asked for a card.
/// * **A tenant `sk-or-…`** — the config goes direct to OpenRouter on the
///   tenant's own account.
///
/// The inheritance branch is the one `managed` used to own, and it moved here
/// rather than being deleted for the same reason it existed: a config that named
/// a provider but dropped the platform key would 401 rather than fall back.
///
/// The one exception is a keyless `openrouter` that also sets its own
/// `base_url`: that endpoint is not the platform's, so the platform credential
/// is withheld and the config goes direct (keyless) instead — sending the
/// platform token to an arbitrary override would leak it.
///
/// Every other kind uses its own configured base URL and key verbatim — those
/// are third-party endpoints we hold no credential for.
fn resolve_endpoint(
    provider: &str,
    base_url_override: Option<&str>,
    key: String,
    env_default: Option<&EnvDefault>,
) -> (String, Credential, bool) {
    let base_url_override = base_url_override.map(str::trim).filter(|s| !s.is_empty());
    let has_key = !key.trim().is_empty();

    if is_managed_choice(provider) {
        // The managed card carries no endpoint field, so a base URL left in the
        // form by a previously-picked provider is stale, not a chosen endpoint:
        // it never redirects the managed probe. The endpoint is always the
        // platform's, and the credential is the operator's own key when given,
        // else the injected managed one.
        let base_url = env_default
            .map(|e| e.base_url.clone())
            .unwrap_or_else(platform_base_url);
        let credential = if has_key {
            Credential::from_value(key)
        } else {
            env_default
                .map(|e| e.credential.clone())
                .unwrap_or(Credential::None)
        };
        return (base_url, credential, true);
    }

    if normalize_provider(provider) == "openrouter" && !has_key {
        // The platform credential rides only the platform's own endpoint. A
        // tenant-supplied base URL override with no key is a direct (keyless)
        // config, not the inheritance branch: pairing the platform token with
        // an arbitrary endpoint would leak it to wherever the override points.
        if let Some(base_url) = base_url_override {
            return (base_url.to_string(), Credential::None, false);
        }
        let base_url = env_default
            .map(|e| e.base_url.clone())
            .unwrap_or_else(platform_base_url);
        let credential = env_default
            .map(|e| e.credential.clone())
            .unwrap_or(Credential::None);
        return (base_url, credential, true);
    }

    (
        effective_base_url(provider, base_url_override),
        Credential::from_value(key),
        false,
    )
}

/// A declaration for the **first-run** connection test, before any company
/// exists to resolve one from.
///
/// [`resolve_effective`] cannot serve the wizard: it reads a company's secret
/// store and manifest, and during first-run setup there is neither. This builds
/// the same shape from what the operator has just typed, through the *same*
/// [`resolve_endpoint`] — so a probe defaults its URL and treats a blank key
/// exactly as the running system will, rather than by a second set of rules that
/// agree today and drift later. A test that passes under different rules than
/// the runtime uses is worse than no test.
///
/// `env_default` is what the host already has. Passing it is what makes "leave
/// the key blank and press Test" mean *test the credential this host was given*
/// — the hosted case, where the operator has no key of their own and the control
/// plane injected one. A blank key with no env default resolves to
/// [`Credential::None`]: correct for keyless Ollama, and honestly
/// unauthenticated everywhere else, which is what the probe should then report.
///
/// [`InferenceSource::Runtime`] because that is what this is — a value an
/// operator supplied, with nothing persisted anywhere yet.
pub fn decl_for_probe(
    provider: &str,
    base_url: Option<&str>,
    key: Option<&str>,
    env_default: Option<&EnvDefault>,
) -> InferenceDecl {
    let provider = provider.trim().to_string();
    let selected_provider = selected_kind(&provider).to_string();
    let (base_url, credential, proxied) = resolve_endpoint(
        &provider,
        base_url,
        key.unwrap_or_default().trim().to_string(),
        env_default,
    );
    InferenceDecl {
        provider,
        selected_provider,
        base_url,
        models: BTreeMap::new(),
        source: InferenceSource::Runtime,
        credential,
        proxied,
        chosen_model: None,
    }
}

/// The effective base URL for a provider kind, given an optional override.
///
/// This is the **direct** endpoint for each kind. Proxied `openrouter` does not
/// come through here — it inherits the platform endpoint in
/// [`resolve_endpoint`], which is the only place that distinction is made.
///
/// `ollama` backstops to a local default; every other local runtime, and
/// `openai_compatible`, has no default (validation requires an explicit URL).
///
/// ## Why the fallback is empty rather than OpenRouter
///
/// This used to end `_ => override_url.unwrap_or(OPENROUTER_BASE_URL)`, and
/// `lmstudio` and `omlx` have no arm — so a decl for either that resolved
/// without a base URL was handed **openrouter.ai**, carrying whatever key the
/// operator typed for the machine on their desk. A local runtime's turns would
/// have gone to a third party along with its credential.
///
/// The console's add path always writes a URL, so no traced path reached it. But
/// a `_ =>` arm that defaults to a third-party endpoint is the wrong shape
/// whatever today's callers happen to do: the blast radius is a credential
/// leaving the host, and the next caller is one refactor away. An empty string
/// fails loudly and locally instead — the same answer `openai_compatible`
/// already gave, for the same reason.
///
/// `openrouter` keeps its default by naming itself, which is also what stops an
/// unknown kind inheriting it by accident.
pub fn effective_base_url(provider: &str, override_url: Option<&str>) -> String {
    let override_url = override_url.map(str::trim).filter(|s| !s.is_empty());
    match normalize_provider(provider) {
        "ollama" => override_url.unwrap_or(OLLAMA_DEFAULT_BASE_URL).to_string(),
        "openrouter" => override_url.unwrap_or(OPENROUTER_BASE_URL).to_string(),
        // `openai_compatible`, `lmstudio`, `omlx`, and any unknown kind. None of
        // them has a guessable endpoint, and guessing is how a local runtime's
        // credential reached OpenRouter.
        _ => override_url.unwrap_or_default().to_string(),
    }
}

/// Normalizes an endpoint typed by a person during setup.
///
/// Local model applications commonly advertise themselves as `localhost:1234`
/// even though the OpenAI-compatible client needs
/// `http://localhost:1234/v1`. Accept that familiar spelling while preserving
/// explicit schemes and non-root paths.
pub fn normalize_setup_base_url(provider: &str, raw: Option<&str>) -> Option<String> {
    let raw = raw.map(str::trim).filter(|value| !value.is_empty())?;
    if !matches!(normalize_provider(provider), "ollama" | "openai_compatible") {
        return Some(raw.trim_end_matches('/').to_string());
    }

    // An `http:` or `https:` the operator typed is the scheme, in any case (RFC
    // 3986 §3.1) and with however many slashes they typed after it — URL parsing
    // reads `http:/host` and `http:///host` as `http://host`. Only a value with no
    // scheme at all gets one. Prefixing a second scheme onto `HTTP://host` or
    // `http:/alice:pw@host` produced `http://HTTP://…` and `http://http:/…`,
    // whose credential no longer sat in the first authority (Codex review on
    // #2281).
    let lower = raw.to_ascii_lowercase();
    let typed_scheme = ["https:", "http:"]
        .into_iter()
        .find(|scheme| lower.starts_with(scheme))
        .map(str::len);
    let mut url = match typed_scheme {
        Some(len) => format!(
            "{}//{}",
            &raw[..len],
            raw[len..].trim_start_matches(['/', '\\'])
        ),
        None => format!("http://{raw}"),
    };
    url = url.trim_end_matches('/').to_string();
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or("");
    if !after_scheme.contains('/') {
        url.push_str("/v1");
    }
    Some(url)
}

/// Loads the runtime inference override, or `None` when unset/blank. A malformed
/// blob is a store error (surfaced, not silently dropped).
pub async fn load_runtime_config(
    company: &CompanyId,
    secrets: &dyn SecretStore,
) -> Result<Option<RuntimeInference>> {
    load_runtime_config_scoped(company, secrets, &HarnessScope::default()).await
}

/// [`load_runtime_config`] for one harness's own slot.
pub async fn load_runtime_config_scoped(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    scope: &HarnessScope,
) -> Result<Option<RuntimeInference>> {
    let Some(SecretValue(raw)) = secrets.get(company, &scope.config_key()).await? else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let config: RuntimeInference = serde_json::from_str(&raw).map_err(|e| {
        OpenCompanyError::Store(format!("inference runtime config is not valid JSON: {e}"))
    })?;
    if config.provider.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(config))
}

/// Persists the runtime inference override (console `PUT`).
pub async fn save_runtime_config(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    config: &RuntimeInference,
) -> Result<()> {
    save_runtime_config_scoped(company, secrets, config, &HarnessScope::default()).await
}

/// [`save_runtime_config`] for one harness's own slot.
pub async fn save_runtime_config_scoped(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    config: &RuntimeInference,
    scope: &HarnessScope,
) -> Result<()> {
    let raw = serde_json::to_string(config)
        .map_err(|e| OpenCompanyError::Store(format!("serializing inference config: {e}")))?;
    secrets
        .set(company, &scope.config_key(), SecretValue(raw))
        .await
}

/// Clears the runtime inference override (console `DELETE` → revert to
/// manifest/managed). Best-effort — the store has no delete, so an empty value
/// reads back as unset.
pub async fn clear_runtime_config(company: &CompanyId, secrets: &dyn SecretStore) -> Result<()> {
    clear_runtime_config_scoped(company, secrets, &HarnessScope::default()).await
}

/// [`clear_runtime_config`] for one harness's own slot.
pub async fn clear_runtime_config_scoped(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    scope: &HarnessScope,
) -> Result<()> {
    secrets
        .set(company, &scope.config_key(), SecretValue(String::new()))
        .await
}

/// Reads the effective outbound credential.
///
/// The canonical [`KEY_KEY`] (`inference/key`) is tried first — the console
/// writes rotated tokens there. When it is empty/missing, `override_key` (a
/// manifest section's `api_key_secret`) is the fallback for a commit-time key.
/// Returns an empty string when neither holds a value.
pub async fn load_key(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    override_key: Option<&str>,
) -> Result<String> {
    load_key_scoped(company, secrets, override_key, &HarnessScope::default()).await
}

/// [`load_key`] for one harness's own credential slot.
pub async fn load_key_scoped(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    override_key: Option<&str>,
    scope: &HarnessScope,
) -> Result<String> {
    if let Some(SecretValue(raw)) = secrets.get(company, &scope.key_key()).await?
        && !raw.trim().is_empty()
    {
        return Ok(raw);
    }
    if let Some(key) = override_key.map(str::trim).filter(|s| !s.is_empty())
        && let Some(SecretValue(raw)) = secrets.get(company, key).await?
        && !raw.trim().is_empty()
    {
        return Ok(raw);
    }
    Ok(String::new())
}

/// Reads **managed's** credential, and only managed's.
///
/// Same two addresses as [`load_inference_key_scoped`] — `provider/tinyhumans/key`
/// then the legacy flat slot — with the gate the flat slot needs and the general
/// reader cannot have: `inference/key` is one address two different rows read
/// through, and for an upgraded company whose entry zero is a vendor account it
/// holds that vendor's key. Without the gate, an explicit Managed route sent a
/// BYOK credential to the platform URL, and the status and Test Managed made the
/// same ownership mistake.
///
/// The write path has always gated on this; see
/// [`store::legacy_slot_is_managed`].
pub async fn load_managed_key(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    scope: &HarnessScope,
) -> Result<String> {
    if let Some(SecretValue(raw)) = secrets
        .get(company, &provider_key_key(MANAGED_SLUG))
        .await?
        && !raw.trim().is_empty()
    {
        return Ok(raw);
    }
    // **The fallback is the *flat* slot, and only the default harness reads
    // that.** For a named scope `load_key_scoped` answers
    // `harness/<id>/inference/key`, which is that harness's own credential for
    // whatever *it* declared — presenting it to the platform endpoint is the
    // same ownership mistake as the BYOK one, a scope along. Managed still
    // resolves for a named harness; it just resolves through the company
    // account or the instance identity, which is what `managed_identity` does
    // with an empty key.
    if !scope.is_default {
        return Ok(String::new());
    }
    if !store::legacy_slot_is_managed(company, secrets).await? {
        return Ok(String::new());
    }
    load_key_scoped(company, secrets, None, scope).await
}

/// Reads the outbound inference credential for one provider slug.
///
/// ```text
///   1. provider/<slug>/key    the address every provider's credential lives at
///   2. inference/key          the legacy flat slot, read-only
///   3. <manifest secret>      a commit-time key named by `[inference].api_key_secret`
/// ```
///
/// Steps 1 and 2 are **the same meaning at two addresses**. Nothing writes step 2
/// any more ([`store_provider_key`](super::inference::store::store_provider_key)
/// clears it on the next save of that provider), so the fallback retires itself
/// company by company and can be deleted outright once nothing reads it. That is
/// lazy convergence rather than a migration: no flag day, and no half-migrated
/// state on a store with no transaction.
///
/// **A named harness reads its own slot first.** Step 1 is company-wide, so for
/// a harness with inference of its own it is a *different owner's* credential
/// wearing the same slug: a company that connects OpenRouter would otherwise
/// have its key substituted for the harness's, and a harness with its own
/// `base_url` would present it to a different gateway. The convergence order is
/// right for the company's own resolution and wrong one scope in, so the scope
/// decides which of the two comes first. A named harness holding no key of its
/// own still inherits the company's, which is what it did before harness scopes
/// existed and what a harness that declared only a model expects.
pub async fn load_inference_key_scoped(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    slug: &str,
    override_key: Option<&str>,
    scope: &HarnessScope,
) -> Result<String> {
    load_inference_key_for(company, secrets, slug, override_key, scope, true).await
}

/// [`load_inference_key_scoped`], with the company-wide step made optional.
///
/// **A named harness that names its own endpoint must not borrow the company's
/// credential.** The company slot holds a key for the company's own gateway;
/// presenting it to a different one is the same mistake as managed reading a
/// vendor's. Inheriting is right only where the harness changed something that
/// is not the destination — a model, say — so the caller that knows which of
/// those it is holding passes the answer in.
pub async fn load_inference_key_for(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    slug: &str,
    override_key: Option<&str>,
    scope: &HarnessScope,
    may_inherit_company_key: bool,
) -> Result<String> {
    if !scope.is_default {
        let own = load_key_scoped(company, secrets, override_key, scope).await?;
        if !own.trim().is_empty() {
            return Ok(own);
        }
        if !may_inherit_company_key {
            return Ok(String::new());
        }
    }
    if let Some(SecretValue(raw)) = secrets.get(company, &provider_key_key(slug)).await?
        && !raw.trim().is_empty()
    {
        return Ok(raw);
    }
    load_key_scoped(company, secrets, override_key, scope).await
}

/// Which step of the managed chain a request would actually resolve at.
///
/// The managed row on the console has to say this, and it has to say it
/// honestly. The design this is ported from renders a permanent `Always on`
/// badge, which is true **there** — they run the managed backend — and is a lie
/// here: our managed tier needs a credential and can resolve to nothing. A row
/// claiming availability while agents cannot think is the failure
/// `CognitionState`'s five states exist to prevent.
///
/// Steps 3 and 4 are kept apart because they answer different questions for the
/// operator: one bills the company's own account, the other bills whoever runs
/// the server. Collapsing them into "on" hides the decision they would make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedSource {
    /// A key pasted for inference — `provider/tinyhumans/key`, or the legacy
    /// `inference/key`. These are two addresses for one meaning.
    ProviderKey,
    /// The company's own TinyHumans account.
    CompanyAccount,
    /// This instance's identity — so the server's account pays.
    Instance,
    /// Nothing resolves. The managed brain is **not set up**.
    None,
}

impl ManagedSource {
    /// The stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderKey => "provider_key",
            Self::CompanyAccount => "company_account",
            Self::Instance => "instance",
            Self::None => "none",
        }
    }

    /// Whether the managed brain can be reached at all.
    pub fn resolves(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// [`ManagedSource`] from the three facts that decide it.
///
/// Pure, because it is a decision with four branches and every one of them is a
/// different sentence on screen. The inputs are read where a store is available;
/// the reasoning is here, where it can be tested with three booleans.
pub fn managed_source(
    inference_key_set: bool,
    company_account: &Credential,
    env_default: Option<&EnvDefault>,
) -> ManagedSource {
    if inference_key_set {
        return ManagedSource::ProviderKey;
    }
    if matches!(company_account, Credential::Company(_)) {
        return ManagedSource::CompanyAccount;
    }
    match env_default {
        // `configured()` rather than presence: a projected-token source reports
        // itself configured while its file can still yield nothing, and what
        // decides availability is whether a value would reach the wire.
        Some(env) if env.credential.configured() => ManagedSource::Instance,
        _ => ManagedSource::None,
    }
}

/// Steps 3 and 4 of the managed chain: the company's account identity, then this
/// instance's.
///
/// **An identity flows to a surface only when the vendor at the other end is the
/// identity's own vendor.** That is the whole safety property, and `proxied` is
/// what enforces it: it is true exactly when the resolved endpoint is the
/// platform's own, and false for OpenRouter, Anthropic, a custom endpoint or any
/// other vendor. A `th_…` key presented as a bearer to `openrouter.ai` is a live
/// bug in the credential-link path today, and this is the line that stops it
/// being reproduced here.
///
/// `had_key` is the second gate: a key pasted for inference is a more specific
/// answer than an identity, so it wins and this is not consulted at all.
///
/// A store read error **propagates**. An unreadable store means we do not know
/// who this company is, and resolving that to the instance's identity would bill
/// the company's thinking to the server's account, invisibly.
async fn managed_identity(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    resolved: Credential,
    proxied: bool,
    had_key: bool,
) -> Result<Credential> {
    if !proxied || had_key {
        return Ok(resolved);
    }
    Ok(
        match crate::company::company_key::load(company, secrets).await? {
            // The company's own TinyHumans account. Setting it used to move only
            // the app connections and leave every agent turn on whoever runs the
            // server — the expensive half, with nothing on screen saying so.
            company_key @ Credential::Company(_) => company_key,
            // Nothing of the company's own: the instance identity that
            // `resolve_endpoint` already put here, or nothing at all.
            _ => resolved,
        },
    )
}

/// Writes the company's outbound inference credential (write-only intake).
pub async fn store_key(company: &CompanyId, secrets: &dyn SecretStore, key: &str) -> Result<()> {
    store_key_scoped(company, secrets, key, &HarnessScope::default()).await
}

/// [`store_key`] for one harness's own credential slot.
pub async fn store_key_scoped(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    key: &str,
    scope: &HarnessScope,
) -> Result<()> {
    secrets
        .set(company, &scope.key_key(), SecretValue(key.to_string()))
        .await
}

/// Clears the stored credential (best-effort — the store has no delete, so an
/// empty value reads back as "not configured").
pub async fn clear_key(company: &CompanyId, secrets: &dyn SecretStore) -> Result<()> {
    secrets
        .set(company, KEY_KEY, SecretValue(String::new()))
        .await
}

/// Whether the company currently has an outbound inference credential — the
/// non-secret status surfaced by the read APIs. Never returns the value.
pub async fn key_configured(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    override_key: Option<&str>,
) -> Result<bool> {
    Ok(!load_key(company, secrets, override_key)
        .await?
        .trim()
        .is_empty())
}

/// Resolves a company's *effective* inference configuration.
///
/// Precedence is **full default (2b) > provider list > runtime > manifest >
/// env-default > a routing table that names `managed`**. Returns `None` when
/// no source configures inference at all — the caller then keeps the
/// managed/echo brain. The single seam the harness builder and the ops route
/// both use so the agent-facing resolution and the console's status view
/// stay identical.
///
/// This re-reads the secret store on every call, which is what makes a console
/// switch take effect on the agents' next turn with no rebuild.
pub async fn resolve_effective(
    company: &CompanyId,
    manifest: &Inference,
    env_default: Option<&EnvDefault>,
    secrets: &dyn SecretStore,
) -> Result<Option<InferenceDecl>> {
    resolve_effective_scoped(
        company,
        manifest,
        env_default,
        secrets,
        &HarnessScope::default(),
    )
    .await
}

/// [`resolve_effective`] against one harness's own config and credential slots.
///
/// `manifest` is that harness's `[harness.inference]`, or the company-level
/// `[inference]` when it declares none — the caller picks, because only it knows
/// which fallback applies.
///
/// The precedence within a harness is unchanged (runtime > manifest > env
/// default); what differs is only *which* secret keys the runtime and credential
/// tiers read. Two `built_in` harnesses therefore resolve independently, which
/// is what lets one run on the subscription while the other runs on a key.
pub async fn resolve_effective_scoped(
    company: &CompanyId,
    manifest: &Inference,
    env_default: Option<&EnvDefault>,
    secrets: &dyn SecretStore,
    scope: &HarnessScope,
) -> Result<Option<InferenceDecl>> {
    // 0. The provider list — what the console's Connected rows actually hold.
    //
    // **This is the seam the whole feature hung off and nobody connected.** The
    // write routes populated `inference/providers`, the status route rendered
    // it, and the resolver began at `inference/config` — so a company that added
    // a provider through the console had configured its *display*, not itself,
    // and the chat pane's "no model configured" was telling the truth.
    //
    // Entry zero is why this sits ABOVE the legacy read rather than replacing
    // it: `inference/config` is the first element of this list, so a company
    // that predates the list resolves through the same branch it always did —
    // which is exactly what entry zero was designed to make true without a
    // migration. The `EntryZero` arm below therefore falls through to step 1
    // deliberately, so the legacy path keeps every rule it has (the proxy
    // inheritance, the managed chain, `reject_unknown_provider`) rather than a
    // reimplementation of them here.
    //
    // **Skipped for a named harness that configured itself.** The provider list
    // is a company-level statement, and a `[harness.inference]` section or a
    // `harness/<id>/inference/config` blob is a narrower one that predates it:
    // `docs/spec/runtime/providers.md` has said runtime-then-manifest-then-
    // default *within a harness* all along. Putting the list unconditionally on
    // top inverted that, so connecting the company's first provider in the
    // console silently re-pointed a harness that had its own account at the
    // company's — and charged the wrong one, with nothing on any screen saying
    // the harness's own section had stopped applying.
    if scope.is_default || !harness_configures_itself(company, secrets, scope).await? {
        // Keys rework (#2306), slice 2b: a full company default outranks the
        // provider list's own positional/marked-primary rule — an operator
        // who chose `{provider, model}` explicitly said more than "this row
        // is my default slug", and that choice's model is what a status read
        // and a turn both send. A default naming a missing or switched-off
        // provider falls through here (see `full_default_decl`'s own doc);
        // the turn path's `resolve_for_turn` is where that fails closed.
        if let Some(decl) =
            full_default_decl(company, manifest, env_default, secrets, scope).await?
        {
            return Ok(Some(decl));
        }
        let providers = store::list_providers(company, secrets).await?;
        if let Some(decl) = decl_for_primary(company, secrets, &providers).await? {
            return Ok(Some(decl));
        }
    }

    // The switched-off-Managed refusal used to sit here and now sits on the turn
    // path alone: a *read* has to be able to describe the state that refuses a
    // turn, and erroring here left a company with no page and therefore no
    // switch to turn Managed back on with.
    let legacy = resolve_legacy_scoped(company, manifest, env_default, secrets, scope).await?;
    if legacy.is_some() {
        return Ok(legacy);
    }

    // 4. The routing table naming `managed` — after the legacy chain's steps
    //    1-3, and last of everything.
    //
    // **The branch whose absence put a working company on the echo brain.**
    // Managed is the one inference source with no record in either place the two
    // branches above read: it is not a row in `inference/providers` (it resolves
    // through a credential chain rather than from a record), and a company that
    // configured it through the console's Managed row wrote neither the legacy
    // runtime blob nor a manifest `[inference]` block. Its credential is at
    // `provider/tinyhumans/key` and its *choice* is in `inference/routes`.
    //
    // So on the reported company — one disabled provider, all four tiers routed
    // to `managed`, a non-empty managed key — both branches above returned
    // `None`, `RuntimeBuilder::build` read that as "nothing configured" and
    // selected the offline echo brain. Restarting the host did not help, because
    // a fresh boot ran this same computation and got the same answer. Meanwhile
    // [`resolve_effective_for_tier`] resolved those rows to
    // [`managed_decl`] perfectly well — the turn-time path knew, and the
    // boot-time path had no way to ask.
    //
    // Tried **last**, so every company that resolves today resolves exactly
    // where it did: this branch only turns a `None` into a `Some`.
    //
    // Gated on the Managed switch for the same reason it goes through
    // `managed_decl`: [`resolve_effective_for_tier`] *refuses* an explicit
    // `managed` route while the switch is off, so a boot that selected the
    // harness brain on the strength of those rows would hand every turn to a
    // resolver that errors. Off means off on both paths, or this branch
    // reintroduces the drift it exists to close.
    //
    // A gate and not a refusal: this answers `Ok(None)`, so the status read
    // still renders and still offers the switch. That distinction is the one
    // the read-path guard got wrong.
    let routes = store::load_routes(company, secrets).await?;
    if resolve::any_route_is_managed(&routes) && store::managed_enabled(company, secrets).await? {
        // Through `managed_decl` rather than a second opinion about the managed
        // chain. It is the same function the routed turn path calls, so "does
        // managed resolve for boot" and "what does a managed route resolve to"
        // cannot drift — which is the failure mode that produced this bug and
        // three of its siblings.
        //
        // The predicate is the resolved declaration's **own** credential, not a
        // re-derivation: `managed_decl` always returns a decl (the platform
        // endpoint exists regardless), and what separates a company that can
        // think from one that cannot is whether a credential reached it through
        // `managed_identity` — the pasted key, the company's TinyHumans account,
        // or the instance identity. `Credential::None` means the operator picked
        // managed and put nothing behind it, and that company belongs on the
        // echo brain exactly as before.
        let decl = managed_decl(company, secrets, env_default, scope).await?;
        if decl.credential.configured() {
            return Ok(Some(decl));
        }
    }

    Ok(None)
}

/// Refuses a fallback that rides the managed chain once Managed is switched off.
///
/// The switch is honoured on the explicit `managed` route in
/// [`resolve_effective_for_tier`], but an **unset** row does not take that
/// branch: it falls through here, and for a company whose legacy config or
/// environment resolves to the platform it kept spending. The console says
/// "Managed is switched off, so it is not a fallback. Its credential is
/// untouched" — a sentence about exactly this path — so the page was making a
/// promise the resolver did not keep.
///
/// **On the turn path only, never on a read.** It lived in
/// [`resolve_effective_scoped`] for one commit, which every status read also
/// goes through — so switching Managed off on a company with nothing else made
/// `GET …/inference` fail, and the console could no longer render the switch
/// needed to turn it back on. A refusal is a statement about a turn; a status
/// read has to be able to *describe* the state that refuses one.
///
/// `is_proxied` is the marker because that is what riding the platform's
/// endpoint on the platform's credential *is*; a company on its own key is not
/// proxied whatever its provider is called.
async fn refuse_a_managed_fallback_that_is_switched_off(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    decl: Option<InferenceDecl>,
) -> Result<Option<InferenceDecl>> {
    let Some(decl) = decl else {
        return Ok(None);
    };
    if decl.is_proxied() && !store::managed_enabled(company, secrets).await? {
        return Err(OpenCompanyError::Config(
            "Managed is switched off and nothing else is connected, so there is \
             nothing to think with. Switch it back on, or connect a provider in \
             Settings → Inference."
                .to_string(),
        ));
    }
    Ok(Some(decl))
}

/// Whether a **named** harness holds inference configuration of its own.
///
/// Two tiers count, and they are the two `resolve_legacy_scoped` reads first: a
/// blob in this harness's own `harness/<id>/inference/config` slot, and a
/// `[harness.inference]` section in the manifest — which only the caller can
/// report, because the section reaches the resolver already merged with the
/// company's. Never true for the default harness: its "scoped" keys *are* the
/// flat ones, and entry zero already carries them into the provider list.
async fn harness_configures_itself(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    scope: &HarnessScope,
) -> Result<bool> {
    if scope.is_default {
        return Ok(false);
    }
    if scope.declares_own_inference {
        return Ok(true);
    }
    Ok(load_runtime_config_scoped(company, secrets, scope)
        .await?
        .is_some())
}

/// The declaration the company's **primary** provider resolves to, if the list
/// settles the question at all.
///
/// `None` means it does not, and the caller falls through to
/// [`resolve_legacy_scoped`]: either the list is empty, or its primary is entry
/// zero — which is the legacy blob wearing a provider record's clothes and has
/// to resolve through the chain that owns it.
///
/// Takes the list rather than reading it, because both callers already hold one
/// and a second read per turn buys nothing but a round trip.
async fn decl_for_primary(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    providers: &[store::Provider],
) -> Result<Option<InferenceDecl>> {
    let marked = store::load_default_slug(company, secrets).await?;
    match resolve::primary(providers, marked.as_deref()) {
        Some(provider) if provider.origin == store::ProviderOrigin::Indexed => {
            Ok(Some(decl_for_indexed(company, secrets, provider).await?))
        }
        _ => Ok(None),
    }
}

/// The declaration one **indexed** provider record resolves to.
///
/// Extracted because two callers need it and must not drift: the unrouted path
/// above, which reaches it through the primary, and a routing row that names
/// this provider by slug. A second copy of these six lines is a second opinion
/// about which credential an added provider presents.
async fn decl_for_indexed(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    provider: &store::Provider,
) -> Result<InferenceDecl> {
    let key = store::load_provider_key(company, secrets, provider).await?;
    let had_key = !key.trim().is_empty();
    // A provider the operator added names its own endpoint. It is a vendor
    // account, never the platform proxy, so `proxied` is false — and that is
    // what denies it both the instance identity and the company's, which is the
    // safety property the credential chain is built on.
    //
    // **Stated, not derived.** This used to read `is_managed_choice(&kind)`,
    // which is `false` for every kind an indexed record can hold — the add
    // route only ever writes a catalogue slug or `custom`, and the edit route
    // carries the kind across unchanged — so the two lines agreed by accident
    // rather than by construction. Saying it outright means a future kind
    // cannot quietly hand an operator-typed endpoint the platform's identity.
    let credential = Credential::from_value(key);
    let proxied = false;
    let credential = managed_identity(company, secrets, credential, proxied, had_key).await?;
    Ok(InferenceDecl {
        provider: normalize_provider(&provider.kind).to_string(),
        selected_provider: selected_kind(&provider.kind).to_string(),
        base_url: provider.base_url.clone(),
        models: provider.models.clone(),
        source: InferenceSource::Runtime,
        credential,
        proxied,
        chosen_model: None,
    })
}

/// What a company resolves to **before** the provider list existed: the runtime
/// blob, then the manifest, then the platform default.
///
/// Split out of [`resolve_effective_scoped`] rather than inlined because a
/// routing row naming **entry zero** has to reach exactly this chain — entry
/// zero *is* the legacy blob wearing a provider record's clothes, so resolving
/// it through the record would drop the proxy inheritance, the managed chain and
/// `reject_unknown_provider` that only live here.
async fn resolve_legacy_scoped(
    company: &CompanyId,
    manifest: &Inference,
    env_default: Option<&EnvDefault>,
    secrets: &dyn SecretStore,
    scope: &HarnessScope,
) -> Result<Option<InferenceDecl>> {
    // 1. Runtime override (console) wins.
    if let Some(runtime) = load_runtime_config_scoped(company, secrets, scope).await? {
        let selected_provider = selected_kind(&runtime.provider).to_string();
        let provider = normalize_provider(&runtime.provider).to_string();
        reject_unknown_provider(&provider, "the stored runtime inference config")?;
        // Trimmed and blank-filtered for the same reason the manifest arm
        // below normalizes its own `base_url` before asking "did this name an
        // endpoint": a stored `base_url: Some(String::new())` is not an
        // endpoint anybody named (tinysweeper/CodeRabbit review, same root
        // cause as the manifest arm's blank-`base_url` finding).
        let runtime_base_url = runtime
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty());
        // `managed` is an alias for the platform endpoint, not a credential
        // type an arbitrary gateway can receive.  Once this runtime row names
        // its own endpoint it resolves as a direct OpenRouter-compatible
        // provider, so it must not read the managed slot: account-key fan-out
        // deliberately places the write-only TinyHumans account key there.
        // There is no runtime field for a gateway credential; operators that
        // need one select the actual gateway provider, whose distinct slot the
        // provider route writes.  Failing closed here prevents a `managed` +
        // `base_url` row from disclosing a TinyHumans key to that endpoint.
        let key = if runtime_base_url.is_some() && is_managed_choice(&runtime.provider) {
            String::new()
        } else {
            load_inference_key_for(
                company,
                secrets,
                credential_slug(&runtime.provider),
                None,
                scope,
                // It named its own endpoint, so the company's key is for
                // somewhere else. See `load_inference_key_for`.
                runtime_base_url.is_none(),
            )
            .await?
        };
        let had_key = !key.trim().is_empty();
        // Which spelling reaches `resolve_endpoint` depends on whether this
        // runtime config also names an endpoint — the same split the manifest
        // arm below makes, and for the same reason: `validate_runtime`
        // accepts `provider: "managed"` with a valid non-blank `base_url`
        // (normalizing to `openrouter` for the provider-allowlist check
        // only), so a console `PUT` naming a gateway in front of the platform
        // is exactly as valid a runtime config as a manifest one, and must be
        // honoured the same way rather than having `resolve_endpoint`'s
        // managed branch silently discard it for the platform's own endpoint
        // (CodeRabbit review). Without a `base_url` the **raw** word must go
        // in: `normalize_provider` folds `managed` onto `openrouter`, and
        // resolving through the normalized value skipped both managed
        // branches — so a company that declared `managed` and stored a key
        // had its requests sent to `openrouter.ai` carrying a TinyHumans
        // token. `resolve_endpoint` consults `is_managed_choice` first and
        // needs the word the operator chose.
        let endpoint_kind = if runtime_base_url.is_some() {
            provider.as_str()
        } else {
            runtime.provider.as_str()
        };
        let (base_url, credential, proxied) =
            resolve_endpoint(endpoint_kind, runtime.base_url.as_deref(), key, env_default);
        let credential = managed_identity(company, secrets, credential, proxied, had_key).await?;
        return Ok(Some(InferenceDecl {
            provider,
            selected_provider,
            base_url,
            models: runtime.models,
            source: InferenceSource::Runtime,
            credential,
            proxied,
            chosen_model: None,
        }));
    }

    // 2. Manifest `[inference]`.
    if manifest.is_set() {
        let declared = manifest.provider.as_deref().unwrap_or_default();
        let selected_provider = selected_kind(declared).to_string();
        let provider = normalize_provider(declared).to_string();
        reject_unknown_provider(&provider, "`[inference].provider`")?;
        let raw = manifest.provider.as_deref().unwrap_or_default();
        // Trimmed and blank-filtered once, up front: `resolve_endpoint` itself
        // already treats a blank override as absent, but the two decisions
        // below (whether the row/default may inherit the company-wide key, and
        // which spelling of the provider reaches `resolve_endpoint`) used to
        // check `manifest.base_url.is_some()` directly — a `base_url = ""` or
        // whitespace-only manifest value read as "an endpoint was named" to
        // both, which sent `credential_slug`'s lookup to `tinyhumans` while the
        // endpoint side used the *normalized* `openrouter` spelling and skipped
        // the managed branch entirely: a manifest `managed` with a blank
        // `base_url` and a stored key resolved straight to `openrouter.ai`,
        // reproducing the same 401 the raw/normalized split below exists to
        // prevent (tinysweeper/CodeRabbit review).
        let manifest_base_url = manifest
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty());
        // An explicit endpoint is a direct provider.  In particular, the
        // `managed` spelling must not make its TinyHumans account key travel
        // to an operator-selected gateway.
        let key = if manifest_base_url.is_some()
            && is_managed_choice(raw)
            && manifest
                .api_key_secret
                .as_deref()
                .map(str::trim)
                .filter(|secret| !secret.is_empty())
                .is_none()
        {
            String::new()
        } else {
            load_inference_key_for(
                company,
                secrets,
                credential_slug(raw),
                manifest.api_key_secret.as_deref(),
                scope,
                manifest_base_url.is_none(),
            )
            .await?
        };
        let had_key = !key.trim().is_empty();
        // Which spelling reaches `resolve_endpoint` depends on whether the
        // manifest also names an endpoint. A manifest is hand-authored and
        // committed: `provider = "managed"` **with** a `base_url` is a sentence
        // somebody typed on purpose, usually a gateway in front of the platform,
        // and `resolve_endpoint`'s managed branch would silently discard that
        // URL — so the normalized kind goes in and the gateway is honoured as a
        // vendor (`proxied = false`, no platform credential, no company
        // identity). Without a `base_url` the **raw** word must go in, exactly
        // as the runtime branch above does: the normalized `openrouter` skips
        // the managed branch, and a manifest `managed` with a stored key — the
        // first-run wizard's TinyHumans card writes precisely that — resolved to
        // `openrouter.ai` carrying the TinyHumans key. The e2e symptom was a
        // wizard-built company answering every turn with a 401 from OpenRouter.
        let endpoint_kind = if manifest_base_url.is_some() {
            provider.as_str()
        } else {
            declared
        };
        let (base_url, credential, proxied) = resolve_endpoint(
            endpoint_kind,
            manifest.base_url.as_deref(),
            key,
            env_default,
        );
        let credential = managed_identity(company, secrets, credential, proxied, had_key).await?;
        return Ok(Some(InferenceDecl {
            provider,
            selected_provider,
            base_url,
            models: manifest.models.clone(),
            source: InferenceSource::Manifest,
            credential,
            proxied,
            chosen_model: None,
        }));
    }

    // 3. The platform-injected default: OpenRouter, proxied on the subscription.
    //    A company that has configured nothing lands here, which is why it must
    //    be a working config and not a prompt for a credential.
    //
    //    It still runs through `resolve_endpoint` rather than assuming proxied,
    //    because a console-set key is a configuration act even when the operator
    //    never named a provider: the console's key field on a fresh company
    //    writes `inference/key` and nothing else. Assuming proxied here would
    //    take that key, store it, report it as configured — and then never send
    //    it anywhere.
    if let Some(env) = env_default {
        // **Managed's own slot first.** This branch *is* the managed path — a
        // company that configured nothing, landing on the platform endpoint —
        // and the console's Managed row writes `provider/tinyhumans/key`. Read
        // only through `DEFAULT_PROVIDER`, that key was stored, reported as the
        // step that answers, and then never sent anywhere: turns kept riding
        // the instance identity while the page said they were billed to it.
        //
        // The old lookup stays as the fallback, because a company that has a
        // key at the default provider's slot or the flat one is a company this
        // already served and must keep serving.
        let managed_key = load_managed_key(company, secrets, scope).await?;
        let key = if managed_key.trim().is_empty() {
            load_inference_key_scoped(company, secrets, DEFAULT_PROVIDER, None, scope).await?
        } else {
            managed_key
        };
        let had_key = !key.trim().is_empty();
        let (base_url, credential, proxied) =
            resolve_endpoint(DEFAULT_PROVIDER, None, key, Some(env));
        // A company that has configured nothing still lands on the platform's
        // own endpoint, so its account key is the right credential for it — and
        // this is the case where the silent billing split hurt most: an operator
        // set a company key, watched Composio move onto their account, and left
        // every agent turn on the server's.
        let credential = managed_identity(company, secrets, credential, proxied, had_key).await?;
        // The default is always present now — its endpoint is the platform
        // this host is on, credential or not — so presence no longer means a
        // credential. What separates a company that can think from one that
        // cannot is the same predicate `managed_decl`'s caller applies: did a
        // credential reach the declaration through `managed_identity` (a
        // pasted key, the company's account, the instance identity)? None of
        // them means the operator configured nothing and the deployment holds
        // no identity, which is the echo brain, exactly as an absent default
        // was before the endpoint and the credential were split.
        if !credential.configured() {
            return Ok(None);
        }
        return Ok(Some(InferenceDecl {
            provider: DEFAULT_PROVIDER.to_string(),
            // Nothing is declared and the platform's own endpoint is answering:
            // that *is* the managed route, and it is what the console's
            // "Managed (TinyHumans)" means. Saying `openrouter` here is the
            // second way the managed choice used to vanish — the console sends a
            // keyless managed save as a revert (a managed brain with no key of
            // its own is the platform default, not an override), so the operator
            // pressed Save on Managed and this arm answered with the name of the
            // provider underneath it. The arm below, for a host with no platform
            // default at all, has always reported `managed` for the same reason;
            // these two now agree.
            selected_provider: LEGACY_MANAGED.to_string(),
            base_url,
            models: BTreeMap::new(),
            source: InferenceSource::Default,
            credential,
            proxied,
            chosen_model: None,
        }));
    }

    Ok(None)
}

/// The turn-path refusal when neither a pin, a full default, nor the legacy
/// chain gives a model (keys rework, issue #2306, slices 2b/2d). Superseded
/// as the *sentence itself* by decision D-copy (X9, 2026-09-15,
/// `docs/key-reworks/README.md`) — see
/// [`copy::nothing_resolved_for_company`] for the company-wide wording this
/// constant now delegates to, and [`copy::nothing_resolved`] for the
/// per-agent one. Kept as a `pub const` because slice names outside this
/// module still match error text against it.
pub const NO_MODEL_CHOSEN: &str = copy::COMPANY_NO_MODEL_CHOSEN;

/// The model id a request carries. **Never a tier name** (keys rework, issue
/// #2306, slice 2d). Order:
///
/// 1. [`InferenceDecl::chosen_model`] — an agent pair (3a) or a full company
///    default (2b). Refused if it is somehow a tier name (never sent).
/// 2. `requested`, when it is a real id — this is
///    [`crate::harness::HarnessDeps::model_override`]
///    (`OPENCOMPANY_INFERENCE_MODEL`), or an id a caller named directly.
/// 3. The legacy per-tier map's value for the tier `requested` names —
///    [`legacy_tiers::configured_model_for_tier`] — for a company on none of
///    the above (entry zero, a manifest `[inference].models`, or a routed
///    tier until slice 5b).
/// 4. [`NO_MODEL_CHOSEN`] — nothing resolves, so nothing is sent.
pub fn model_on_the_wire(decl: &InferenceDecl, requested: &str) -> Result<String> {
    let refuse = || OpenCompanyError::Config(NO_MODEL_CHOSEN.to_string());
    if let Some(chosen) = decl.chosen_model() {
        let chosen = chosen.trim();
        if chosen.is_empty() || legacy_tiers::is_tier_name(chosen) {
            return Err(refuse());
        }
        return Ok(chosen.to_string());
    }
    let requested = requested.trim();
    if !requested.is_empty() && !legacy_tiers::is_tier_name(requested) {
        return Ok(requested.to_string());
    }
    legacy_tiers::configured_model_for_tier(requested, &decl.models).ok_or_else(refuse)
}

/// Which explicit choice is being resolved; only the refusal wording differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChoiceSource {
    /// An agent pair (3a).
    Pin,
    /// The company's full default.
    Default,
}

/// The decl one chosen provider resolves to, carrying the chosen model (keys
/// rework, issue #2306, slice 2b).
///
/// `Indexed` ⇒ [`decl_for_indexed`]. `EntryZero` ⇒ [`resolve_legacy_scoped`]
/// at the flat scope, the same rule as the entry-zero route arm, so the
/// proxy inheritance, the managed chain and `reject_unknown_provider` still
/// apply to a default (or pin) that happens to name entry zero.
async fn decl_for_choice(
    company: &CompanyId,
    manifest: &Inference,
    env_default: Option<&EnvDefault>,
    secrets: &dyn SecretStore,
    scope: &HarnessScope,
    provider: &store::Provider,
    model: &str,
) -> Result<Option<InferenceDecl>> {
    let decl = match provider.origin {
        store::ProviderOrigin::Indexed => Some(decl_for_indexed(company, secrets, provider).await?),
        store::ProviderOrigin::EntryZero => {
            let flat = HarnessScope::default_harness(&scope.id);
            resolve_legacy_scoped(company, manifest, env_default, secrets, &flat).await?
        }
    };
    Ok(decl.map(|d| d.with_chosen_model(model.to_string())))
}

/// A full default, for **read** paths (boot and status) (keys rework, issue
/// #2306, slice 2b).
///
/// A default naming a missing or switched-off provider answers `None`, and
/// the caller falls through to today's steps: a read must be able to
/// describe the state that refuses a turn (see
/// `refuse_a_managed_fallback_that_is_switched_off`). The refusal itself
/// lives in [`resolve_for_turn`], not here.
async fn full_default_decl(
    company: &CompanyId,
    manifest: &Inference,
    env_default: Option<&EnvDefault>,
    secrets: &dyn SecretStore,
    scope: &HarnessScope,
) -> Result<Option<InferenceDecl>> {
    let store::DefaultChoice::Full(choice) = store::load_default(company, secrets).await? else {
        return Ok(None);
    };
    let Some(provider) = store::get_provider(company, secrets, &choice.provider).await? else {
        return Ok(None);
    };
    if !provider.enabled {
        return Ok(None);
    }
    decl_for_choice(
        company,
        manifest,
        env_default,
        secrets,
        scope,
        &provider,
        &choice.model,
    )
    .await
}

/// Resolves an explicit choice, failing closed (F6) (keys rework, issue
/// #2306, slice 2b): a missing or switched-off provider is an error, never a
/// fall-through to the default or to legacy.
async fn resolve_choice(
    company: &CompanyId,
    manifest: &Inference,
    env_default: Option<&EnvDefault>,
    secrets: &dyn SecretStore,
    scope: &HarnessScope,
    choice: &store::ModelChoice,
    source: ChoiceSource,
) -> Result<InferenceDecl> {
    let slug = choice.provider.trim();
    let Some(provider) = store::get_provider(company, secrets, slug).await? else {
        return Err(OpenCompanyError::Config(match source {
            // Deliberately agent-less: this is the pin's OWN fallback, reached
            // only if a caller resolves a pin without going through 3a's
            // `TenantProvider::resolve` pin check, which runs first there and
            // produces `copy::pair_broken` (with the real agent name) before
            // this branch is ever reached in practice. Round-3a review P1-1:
            // still routed through `copy::pair_broken` rather than a
            // hand-written sentence, with a neutral display name in its
            // place — the shared X9 wording and settings path, just without
            // an agent to name.
            ChoiceSource::Pin => copy::pair_broken("this agent", slug, copy::ProviderGone::Removed),
            ChoiceSource::Default => copy::default_broken(slug, copy::ProviderGone::Removed),
        }));
    };
    if !provider.enabled {
        let label = provider.label.as_str();
        return Err(OpenCompanyError::Config(match source {
            ChoiceSource::Pin => {
                copy::pair_broken("this agent", label, copy::ProviderGone::TurnedOff)
            }
            ChoiceSource::Default => copy::default_broken(label, copy::ProviderGone::TurnedOff),
        }));
    }
    decl_for_choice(
        company,
        manifest,
        env_default,
        secrets,
        scope,
        &provider,
        &choice.model,
    )
    .await?
    .ok_or_else(|| OpenCompanyError::Config(copy::nothing_resolved_for_company()))
}

/// The turn resolver (keys rework, issue #2306, slice 2b; 3a adds `pin`).
/// Order, and nothing else:
///
/// 1. `pin` (an agent pair; always `None` until 3a) ⇒ [`resolve_choice`].
/// 2. A named, non-default harness that configures itself skips step 3.
/// 3. A [`store::DefaultChoice::Full`] default ⇒ [`resolve_choice`].
/// 4. Otherwise ⇒ [`resolve_effective_for_tier`] with `legacy_hint` as the
///    tier, byte for byte; `None` becomes the company-wide "no model chosen"
///    refusal.
pub async fn resolve_for_turn(
    company: &CompanyId,
    manifest: &Inference,
    env_default: Option<&EnvDefault>,
    secrets: &dyn SecretStore,
    scope: &HarnessScope,
    pin: Option<store::ModelChoice>,
    legacy_hint: &str,
) -> Result<InferenceDecl> {
    if let Some(pin) = pin {
        return resolve_choice(
            company,
            manifest,
            env_default,
            secrets,
            scope,
            &pin,
            ChoiceSource::Pin,
        )
        .await;
    }
    let harness_owns_its_inference =
        !scope.is_default && harness_configures_itself(company, secrets, scope).await?;
    if !harness_owns_its_inference
        && let store::DefaultChoice::Full(choice) = store::load_default(company, secrets).await?
    {
        return resolve_choice(
            company,
            manifest,
            env_default,
            secrets,
            scope,
            &choice,
            ChoiceSource::Default,
        )
        .await;
    }
    resolve_effective_for_tier(company, manifest, env_default, secrets, scope, legacy_hint)
        .await?
        .ok_or_else(|| OpenCompanyError::Config(copy::nothing_resolved_for_company()))
}

/// [`resolve_effective_scoped`] for the workload one turn is actually for.
///
/// **The routing table's only caller on the turn path.** Without it the Routing
/// tab is a screen that persists choices and changes nothing: every row wrote
/// `inference/routes`, the status route read it back, and the turn resolved
/// through the primary regardless — so the value stuck across a reload while the
/// turn kept reaching the provider and the model the operator had just moved off.
/// A control that visibly fails is a bug; one that reports success and is inert
/// is worse, because nothing about it looks wrong.
///
/// `tier` is the abstract tier the turn carries (`chat-v1`, …). A tier with no
/// row of its own — anything outside
/// [`ROUTABLE_WORKLOADS`](resolve::ROUTABLE_WORKLOADS) — resolves exactly as it
/// did before routes existed, rather than acquiring a route by accident or
/// failing closed for want of one.
///
/// ## Why a route fails closed and an unset row does not
///
/// [`Resolution::Missing`](resolve::Resolution::Missing) and
/// [`Disabled`](resolve::Resolution::Disabled) become errors here. An unset
/// workload falls back to the primary because nobody chose anything for it; a
/// route is a choice with a workload attached, and silently spending it on a
/// different account is the failure the explicit default marker exists to
/// prevent, wearing a different hat.
pub async fn resolve_effective_for_tier(
    company: &CompanyId,
    manifest: &Inference,
    env_default: Option<&EnvDefault>,
    secrets: &dyn SecretStore,
    scope: &HarnessScope,
    tier: &str,
) -> Result<Option<InferenceDecl>> {
    let Some(workload) = resolve::Workload::from_tier(tier) else {
        // A tier with no row of its own still resolves for a *turn*, so the
        // switch applies to it as much as to a routable one.
        let decl = resolve_effective_scoped(company, manifest, env_default, secrets, scope).await?;
        return refuse_a_managed_fallback_that_is_switched_off(company, secrets, decl).await;
    };
    let providers = store::list_providers(company, secrets).await?;
    let routes = store::load_routes(company, secrets).await?;

    match resolve::provider_for_workload(workload, &routes, &providers) {
        // Unset. The whole existing chain, unchanged — which is what keeps a
        // company that has never opened the Routing tab resolving exactly where
        // it always did.
        //
        // Same carve-out as `resolve_effective_scoped` step 0, for the same
        // reason: an unset row is not a choice, so it must not outrank a named
        // harness's own `[harness.inference]` or scoped runtime blob. A row
        // that *is* set outranks both — that one is a choice, made here.
        resolve::Resolution::Primary => {
            let primary =
                if scope.is_default || !harness_configures_itself(company, secrets, scope).await? {
                    // Round-3a review P1-4: a bare-slug default (the only shape
                    // still standing here — a `Full` default is caught in
                    // `resolve_for_turn` step 3 whether or not it is broken, so
                    // it never reaches this function) names a provider on
                    // purpose. Failing it closed, rather than handing the turn
                    // to `resolve::primary`'s "first enabled" fallback, is F6:
                    // an explicit choice that cannot be honoured is an error,
                    // never a silent substitution for a different account.
                    // `Unset` (nobody has named a default at all) is the one
                    // case that legitimately still falls through to
                    // `decl_for_primary`'s positional pick.
                    if let store::DefaultChoice::ProviderOnly(slug) =
                        store::load_default(company, secrets).await?
                    {
                        let slug = slug.trim();
                        if !slug.is_empty() {
                            match providers.iter().find(|p| p.slug == slug) {
                                None => {
                                    return Err(OpenCompanyError::Config(copy::default_broken(
                                        slug,
                                        copy::ProviderGone::Removed,
                                    )));
                                }
                                Some(p) if !p.enabled => {
                                    return Err(OpenCompanyError::Config(copy::default_broken(
                                        p.label.as_str(),
                                        copy::ProviderGone::TurnedOff,
                                    )));
                                }
                                Some(_) => {}
                            }
                        }
                    }
                    decl_for_primary(company, secrets, &providers).await?
                } else {
                    None
                };
            match primary {
                Some(decl) => Ok(Some(decl)),
                None => {
                    let fallback =
                        resolve_legacy_scoped(company, manifest, env_default, secrets, scope)
                            .await?;
                    refuse_a_managed_fallback_that_is_switched_off(company, secrets, fallback).await
                }
            }
        }
        // `managed` is a word in the route grammar, not a provider slug — it is
        // what the Managed mode button writes into every row. Read as a slug it
        // names nothing and the workload would fail closed against a provider
        // the operator never had.
        //
        // Its switch is honoured **here**, on the turn path, and not only in the
        // status the console renders. A row that is switched off and still
        // billed is the same defect the routing table itself was added to fix,
        // one provider along: the operator's statement was "stop spending on
        // this", the page agreed, and the spend continued. It refuses in the
        // same words a disabled provider does, because it is the same act.
        resolve::Resolution::Managed => {
            if !store::managed_enabled(company, secrets).await? {
                return Err(OpenCompanyError::Config(format!(
                    "the {} workload is routed to Managed, which is switched off. \
                     Switch it back on, or point that workload somewhere else in \
                     Settings → Inference → Routing.",
                    workload.as_str()
                )));
            }
            Ok(Some(
                managed_decl(company, secrets, env_default, scope).await?,
            ))
        }
        resolve::Resolution::Missing { workload, slug } => Err(OpenCompanyError::Config(format!(
            "the {} workload is routed to `{slug}`, which this company does not have. \
             Point it somewhere else in Settings → Inference → Routing.",
            workload.as_str()
        ))),
        resolve::Resolution::Disabled { workload, slug } => Err(OpenCompanyError::Config(format!(
            "the {} workload is routed to `{slug}`, which is switched off. \
             Switch it back on, or point that workload somewhere else in \
             Settings → Inference → Routing.",
            workload.as_str()
        ))),
        resolve::Resolution::Resolved { provider, model } => {
            let mut decl = match provider.origin {
                store::ProviderOrigin::Indexed => {
                    decl_for_indexed(company, secrets, provider).await?
                }
                // See [`resolve_legacy_scoped`]: entry zero is the legacy blob,
                // and resolving it through the synthesized record would drop the
                // rules only that chain holds.
                //
                // **At the company scope, whatever scope asked.** Entry zero
                // *is* the flat `inference/config` — that is what the row is
                // synthesised from and what its label names. Resolving it under
                // a named harness read `harness/<id>/inference/config` instead,
                // so a route shown as OpenRouter could run against that
                // harness's Anthropic configuration and its credential: the
                // explicit choice defeated, and the bill sent elsewhere.
                store::ProviderOrigin::EntryZero => {
                    let flat = HarnessScope::default_harness(&scope.id);
                    match resolve_legacy_scoped(company, manifest, env_default, secrets, &flat)
                        .await?
                    {
                        Some(decl) => decl,
                        None => return Ok(None),
                    }
                }
            };
            if let Some(model) = model {
                // The route's pinned model beats the provider's own tier map,
                // and deliberately: the map is that provider's default for every
                // workload, the route is this workload's choice. `model_for_tier`
                // reads `models` first, so writing it here is the whole of it.
                decl.models.insert(tier.to_string(), model);
            }
            Ok(Some(decl))
        }
    }
}

/// The declaration a route naming `managed` resolves to.
///
/// The platform endpoint and the managed credential chain — the company's own
/// TinyHumans key when it has one, else the instance identity — reached through
/// the same [`resolve_endpoint`] and [`managed_identity`] every other managed
/// path uses, rather than by assembling the endpoint here.
async fn managed_decl(
    company: &CompanyId,
    secrets: &dyn SecretStore,
    env_default: Option<&EnvDefault>,
    scope: &HarnessScope,
) -> Result<InferenceDecl> {
    let key = load_managed_key(company, secrets, scope).await?;
    let had_key = !key.trim().is_empty();
    let (base_url, credential, proxied) = resolve_endpoint(LEGACY_MANAGED, None, key, env_default);
    let credential = managed_identity(company, secrets, credential, proxied, had_key).await?;
    Ok(InferenceDecl {
        provider: normalize_provider(LEGACY_MANAGED).to_string(),
        selected_provider: LEGACY_MANAGED.to_string(),
        base_url,
        models: BTreeMap::new(),
        source: InferenceSource::Runtime,
        credential,
        proxied,
        chosen_model: None,
    })
}

/// Fails a provider kind that is not in [`INFERENCE_PROVIDERS`].
///
/// The manifest validator already rejects one, but a **stored runtime blob**
/// never passes through it: the console wrote it, possibly under an older build
/// whose vocabulary differed. Resolving one silently would attribute its spend to
/// whatever the fallback happened to be, so it fails loudly here instead — the
/// one place both sources converge.
fn reject_unknown_provider(provider: &str, whence: &str) -> Result<()> {
    if crate::company::types::INFERENCE_PROVIDERS.contains(&provider) {
        return Ok(());
    }
    Err(OpenCompanyError::Config(format!(
        "{whence} names an unknown inference provider `{provider}` — expected one of {}.",
        crate::company::types::INFERENCE_PROVIDERS.join(", ")
    )))
}

/// Validates the manifest `[inference]` section, returning every problem in
/// prosumer language. An absent section (`provider = None`) is inert. Shared by
/// manifest validation and the ops `PUT` route (via [`validate_runtime`]).
pub fn validate_inference(inference: &Inference) -> Vec<String> {
    let Some(provider_raw) = inference.provider.as_deref() else {
        return Vec::new();
    };
    let provider = provider_raw.trim();
    if provider.is_empty() {
        return Vec::new();
    }
    validate_parts(
        provider,
        inference.base_url.as_deref(),
        inference.api_key_secret.as_deref(),
    )
}

/// Validates a runtime override (console `PUT`) — same rules as the manifest,
/// but a runtime override never names a secret key (the console writes the
/// canonical `inference/key`), so `api_key_secret` is not part of the shape.
pub fn validate_runtime(config: &RuntimeInference) -> Vec<String> {
    validate_parts(config.provider.trim(), config.base_url.as_deref(), None)
}

/// The shared validation rules for an inference declaration.
fn validate_parts(
    provider: &str,
    base_url: Option<&str>,
    api_key_secret: Option<&str>,
) -> Vec<String> {
    let mut problems = Vec::new();

    // `managed` aliases rather than failing. It named a real thing until
    // OpenCompany stopped exposing its own SKUs, and a committed manifest that
    // still says it means "the platform's brain" — which is now proxied
    // OpenRouter. Rejecting it would break bundles that were valid when written,
    // to no purpose: the intent still resolves.
    let provider = normalize_provider(provider);

    if !INFERENCE_PROVIDERS.contains(&provider) {
        problems.push(format!(
            "`[inference].provider` must be one of {} — you wrote `{provider}`.",
            INFERENCE_PROVIDERS.join(", ")
        ));
    }

    let base_url = base_url.map(str::trim).filter(|s| !s.is_empty());
    // Every echo of the typed URL below is redacted. A `base_url` is quoted back
    // in a rejection the console renders, and a rejection is the one moment a
    // malformed URL — the kind most likely to have been typed by hand with a
    // password in it — is guaranteed to be shown to somebody.
    match provider {
        "ollama" | "openai_compatible" => match base_url {
            None => problems.push(format!(
                "`[inference].base_url` is required for provider `{provider}` — give the OpenAI-compatible endpoint URL."
            )),
            Some(url) if !is_http_url(url) => problems.push(format!(
                "`[inference].base_url` must be an `http://` or `https://` URL — you wrote `{}`.",
                catalogue::redact_endpoint(url)
            )),
            _ => {}
        },
        _ => {
            if let Some(url) = base_url
                && !is_http_url(url)
            {
                problems.push(format!(
                    "`[inference].base_url` must be an `http://` or `https://` URL — you wrote `{}`.",
                    catalogue::redact_endpoint(url)
                ));
            }
        }
    }

    // A credential in the endpoint, refused for the same reason
    // `api_key_secret` refuses a pasted token just below: a `base_url` is stored
    // as written, returned to every console reader on the company status read,
    // and interpolated into operator-facing failure text. The console's own
    // endpoint fields refuse this before anything is written
    // (`catalogue::normalize_local_endpoint`); this is the manifest and
    // console-`PUT` half of the same rule, so the two ways to set an endpoint
    // cannot disagree about it.
    if let Some(url) = base_url
        && catalogue::endpoint_has_credentials(url)
    {
        problems.push(format!(
            "`[inference].base_url` carries a username or password in the URL — you wrote `{}`. Remove them and store the credential in the key slot instead; an endpoint is readable by everyone who can see this company's settings.",
            catalogue::redact_endpoint(url)
        ));
    }

    // The credential must be a *key name*, not the token itself. Reject values
    // that look like a pasted credential so a secret never lands in the manifest.
    if let Some(secret) = api_key_secret.map(str::trim).filter(|s| !s.is_empty())
        && looks_like_inline_credential(secret)
    {
        problems.push(
            "`[inference].api_key_secret` names a secret-store key, not the secret itself — you appear to have pasted a credential. Set the token through the console instead.".to_string(),
        );
    }

    problems
}

/// True when `url` is an absolute `http://` or `https://` URL.
fn is_http_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Heuristic: does this string look like a pasted credential rather than a
/// secret-store *key name*? Catches the common provider token prefixes and any
/// long, opaque, single-token value.
fn looks_like_inline_credential(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    const PREFIXES: &[&str] = &[
        "sk-", "sk_", "pk-", "pk_", "rk-", "or-v1-", "xai-", "gsk_", "bearer ",
    ];
    if PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    // A long, opaque token with no path separator or whitespace — key names are
    // short and structured (`inference/openrouter`), tokens are long and dense.
    value.len() >= 40 && !value.contains('/') && !value.contains(char::is_whitespace)
}

#[cfg(test)]
#[path = "inference_tests_support.rs"]
mod inference_tests_support;
#[cfg(test)]
#[path = "inference_tests_managed.rs"]
mod tests_managed;
#[cfg(test)]
#[path = "inference_tests_precedence.rs"]
mod tests_precedence;
#[cfg(test)]
#[path = "inference_tests_routed_company.rs"]
mod tests_routed_company;
#[cfg(test)]
#[path = "inference_tests_routing.rs"]
mod tests_routing;
#[cfg(test)]
#[path = "inference_tests_validation.rs"]
mod tests_validation;
