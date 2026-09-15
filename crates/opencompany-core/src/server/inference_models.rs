//! OpenAI-compatible model catalog discovery, cached **per endpoint**.
//!
//! Both first-run setup and the inference settings picker consume the standard
//! `{ "data": [{ "id": ... }] }` model-list shape. Keeping the fetch and parser
//! here prevents setup from knowing only about the first entry while the picker
//! grows a second interpretation of the same provider response.
//!
//! The cache used to be a single process-wide slot holding OpenRouter's public
//! registry, because the picker route asked for that registry unconditionally —
//! whatever endpoint the company had actually been pointed at. Discovery now
//! follows the configured base URL, so the cache is a registry keyed on it: one
//! entry per endpoint, each with its own single-flight lock, so two tenants on
//! two providers neither share a catalog nor queue behind each other.
//!
//! An **authenticated** read is additionally partitioned by the company it was
//! made for, because an endpoint may publish an entitlement-scoped catalog and a
//! base-URL-only key would then hand one company's model list to the next. A
//! keyless read stays shared: it is a public property of the endpoint. Neither
//! path ever puts the credential, or anything derived from it, in the key. See
//! [`catalog_registry`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as TokioMutex;

use crate::company::inference::catalogue::{self, AuthStyle, CatalogShape};
use crate::company::inference::{paged_catalog, probe};

/// How long a successful catalog stays fresh in this process.
pub(crate) const MODEL_CATALOG_TTL: Duration = Duration::from_secs(60 * 60);

/// How long a *failed* catalog read is remembered.
///
/// Much shorter than the success TTL, and it exists for a different reason: a
/// failure that stored nothing meant every caller retried, so an unreachable
/// provider cost a fresh [`MODEL_CATALOG_TIMEOUT`] on every status read and
/// every turn that consulted the vocabulary. Remembering "this endpoint did not
/// answer, a minute ago" turns that into one attempt a minute while staying
/// short enough that a provider coming back up is picked up promptly.
pub(crate) const MODEL_CATALOG_FAILURE_TTL: Duration = Duration::from_secs(60);

/// Maximum time a console page-load waits for the registry on a cache miss.
const MODEL_CATALOG_TIMEOUT: Duration = Duration::from_secs(10);

/// How many redirects a catalog read will follow.
///
/// Three rather than `reqwest`'s default ten, matching the connect-time probe:
/// a model catalog is a leaf document, and a chain longer than a vendor's
/// http→https plus a host move is not one. Every hop is re-checked against
/// `probe::check_endpoint` — see [`discover_models`].
const CATALOG_MAX_REDIRECTS: usize = 3;

/// One model exposed to the operator console.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InferenceModel {
    /// Provider model id written unchanged into the tier mapping.
    pub(crate) id: String,
    /// Provider display name, when published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// Maximum context window, when published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) context_length: Option<u64>,
}

/// Parsed leniently: `data` is decoded as raw JSON values first, so one
/// malformed entry (a non-string `id`, a string-valued `context_length`, …)
/// only drops that entry — or, for a malformed *optional* field, just that
/// field — in [`parse_models`] instead of failing the whole response and
/// hiding every valid model the endpoint actually returned.
#[derive(Deserialize)]
struct RegistryResponse {
    #[serde(default)]
    data: Vec<serde_json::Value>,
}

/// Parse every concrete model in a standard OpenAI-compatible catalog, in the
/// order the provider listed them.
///
/// Each `data` entry is read field-by-field rather than decoded in one shot
/// into a struct. `id` is the only field an entry cannot survive without —
/// everything downstream keys the tier mapping on it — so a missing or
/// non-string `id` drops the entry. `name` and `context_length` are read
/// leniently instead: a malformed optional field (a string-valued
/// `context_length`, say) is treated as absent rather than failing, so it
/// costs the entry that one field, not the id underneath it (issue #1838
/// follow-up — decoding the whole entry into a struct in one shot used to let
/// a bad *optional* field discard an otherwise-good `id` right along with it,
/// same shape as the whole-response failure `RegistryResponse` above already
/// guards against, one level deeper).
///
/// Order is preserved rather than sorted here: `src/server/setup.rs`'s probe
/// path takes `.next()` off this list to pick a local/custom endpoint's
/// leading model, the same thing the pre-catalog `discover_local_model` did
/// by taking the provider's first array entry. Sorting only matters for the
/// operator-facing OpenRouter catalog, so [`openrouter_models`] sorts its own
/// copy before caching it rather than this shared parser reordering every
/// caller's result.
fn parse_models(payload: RegistryResponse) -> Vec<InferenceModel> {
    let mut seen = std::collections::HashSet::new();
    let mut models = Vec::new();
    for entry in payload.data {
        let Some(id) = entry.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        let name = entry
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty());
        let context_length = entry
            .get("context_length")
            .and_then(serde_json::Value::as_u64);
        models.push(InferenceModel {
            id: id.to_string(),
            name,
            context_length,
        });
    }
    models
}

/// Why a catalog read failed, and — the part that matters to the cache —
/// whether the answer was about the **endpoint** or about the **credential**.
///
/// The negative memo in [`catalog_models`] is keyed on the endpoint alone, the
/// same as the positive one. That is right for "this endpoint did not answer":
/// every caller reaching it gets the same result, and remembering it turns an
/// outage into one attempt a minute instead of one per request. It is *wrong*
/// for a `401`/`403`, which is a fact about the key that was presented and not
/// about the endpoint — on a multi-company host, memoizing one company's bad
/// key would make a second company on the same endpoint read the first's
/// rejection back out of the cache and fall to the pre-discovery guess without
/// ever presenting its own valid credential. It would also make a company that
/// has just rotated a bad key wait out the memo before its good one is tried.
///
/// So credential-specific failures are reported and **not** remembered. The
/// cost of not memoizing them is small in exactly the way that matters: an
/// auth rejection is a fast round trip, not the [`MODEL_CATALOG_TIMEOUT`] hang
/// the memo exists to stop paying for repeatedly.
#[derive(Debug)]
pub(crate) struct DiscoveryError {
    message: String,
    /// `401`/`403` when the answer is about the presented key.
    credential_status: Option<u16>,
    /// `true` for `404` — the endpoint does not serve this path at all, which is
    /// what lets the account-scoped read fall back to the public one.
    not_found: bool,
}

impl DiscoveryError {
    fn endpoint(message: String) -> Self {
        Self {
            message,
            credential_status: None,
            not_found: false,
        }
    }

    fn credential(status: reqwest::StatusCode, message: String) -> Self {
        Self {
            message,
            credential_status: Some(status.as_u16()),
            not_found: false,
        }
    }

    fn missing(message: String) -> Self {
        Self {
            message,
            credential_status: None,
            not_found: true,
        }
    }

    #[cfg(feature = "openhuman")]
    pub(crate) fn credential_status(&self) -> Option<u16> {
        self.credential_status
    }
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Fetch every model from an OpenAI-compatible `{base_url}/models` endpoint.
///
/// `bearer` is the credential the endpoint expects — the company's stored key
/// for a tenant catalog read, `None` for a public registry (OpenRouter's) or a
/// keyless local server.
///
/// ## The endpoint is a tenant's to choose, so it gets the probe's guard
///
/// Every `base_url` reaching here is one an operator typed: the stored provider
/// record behind `GET …/providers/{slug}/models`, or the endpoint the setup
/// wizard was handed. A connect-time probe alone does not make it safe to fetch
/// later — the row survives a probe failure on purpose (that is the whole point
/// of keeping a key whose endpoint was merely unreachable), `add anyway` stores
/// one that was refused outright, and an edit can move the URL afterwards. So
/// the same [`probe::check_endpoint`] policy is applied **here**, on every hop,
/// rather than being trusted to have happened upstream. Without it a tenant
/// admin on a hosted instance can point a provider at `169.254.169.254` and
/// have this process read it for them, and a permitted host that redirects
/// there does it without even needing the URL stored.
pub(crate) async fn discover_models(
    base_url: &str,
    bearer: Option<&str>,
    auth: AuthStyle,
    shape: CatalogShape,
) -> Result<Vec<InferenceModel>, DiscoveryError> {
    let policy = probe::default_policy();
    let credentialed = bearer.is_some_and(|b| !b.trim().is_empty());
    let origin = base_url.trim().to_string();
    probe::check_endpoint_with_credential(
        base_url,
        policy,
        bearer.is_some_and(|b| !b.trim().is_empty()),
    )
    .map_err(|refusal| DiscoveryError::endpoint(refusal.to_string()))?;
    let base = base_url.trim_end_matches('/');
    // Bounded here, not left to each caller: reqwest's async client has no
    // default timeout, so an endpoint that accepts the connection but never
    // responds would otherwise hold this open indefinitely. `setup.rs`'s
    // local/custom probe calls this directly (no wrapping timeout of its
    // own), while `catalog_models` below also wraps its call in
    // `tokio::time::timeout` for a friendlier, endpoint-naming message.
    //
    // The redirect policy is the second half of the guard: `reqwest` resolves
    // and connects on our behalf, so the hop about to be made is the only thing
    // there is to inspect, and a check applied to the first URL alone waves the
    // interesting case straight through.
    let client = reqwest::Client::builder()
        .timeout(MODEL_CATALOG_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= CATALOG_MAX_REDIRECTS {
                return attempt.stop();
            }
            // A credentialed read stays on its origin: `reqwest` drops
            // `Authorization` across hosts but keeps a custom header, and the
            // catalogue's one non-bearer entry sends the key as `x-api-key`, so
            // a provider that can answer `302` could name any host to send it
            // to. See `probe::same_origin`.
            if credentialed && !probe::same_origin(&origin, attempt.url().as_str()) {
                return attempt.stop();
            }
            match probe::check_endpoint(attempt.url().as_str(), policy) {
                Ok(()) => attempt.follow(),
                // `stop`, not `error`: the caller then classifies the redirect's
                // own status as an endpoint problem, which is what it is. Either
                // way the next request is never sent.
                Err(_) => attempt.stop(),
            }
        }))
        .build()
        .map_err(|error| {
            DiscoveryError::endpoint(format!(
                "failed to build the model-discovery client: {error}"
            ))
        })?;

    // Keys rework (#2306), slice 2a: the TinyHumans proxy's catalog is a paged
    // envelope, not the single-response OpenAI shape the rest of this function
    // reads — a different fetch entirely, so it branches before the
    // account-scoped/public-registry logic below, which is OpenRouter-specific
    // and does not apply to it.
    if shape == CatalogShape::PagedEnvelope {
        return fetch_paged_catalog(&client, base, bearer, auth).await;
    }

    // The account-scoped catalogue first, where the endpoint has one — see
    // `catalogue::scoped_catalog_path` for why, and why it is one host's rule
    // rather than a general assumption.
    //
    // A `404` here is the look-alike case: a proxy or a self-hosted gateway that
    // answers `/models` on OpenRouter's own host, or OpenRouter withdrawing the
    // path. It degrades to the public registry rather than reporting the company
    // has no models at all — but loudly, because the picker is then offering
    // models the account may not be able to reach and nothing else would say so.
    if let Some(path) = catalogue::scoped_catalog_path(base_url, bearer.is_some()) {
        let url = format!("{base}{path}");
        match fetch_catalog(&client, &url, bearer, auth).await {
            Ok(models) => return Ok(models),
            // Redacted: this is a log, and a log is disk. The request above
            // still went to `url` itself.
            Err(error) if error.not_found => tracing::warn!(
                url = %catalogue::redact_endpoint(&url),
                "the account-scoped model catalogue answered 404; falling back to the public \
                 registry, which is not filtered by this key's provider permissions"
            ),
            Err(error) => return Err(error),
        }
    }

    // The public registry needs the same shape parameters the scoped path uses:
    // they are a property of OpenRouter's catalog API rather than of
    // `/models/user`, and taking the defaults here is why this fallback returned
    // a text-only, 500-entry view of a catalogue the caller believes is whole.
    let query = crate::company::inference::catalogue::catalog_query(base);
    fetch_catalog(&client, &format!("{base}/models{query}"), bearer, auth).await
}

/// One catalog read against one URL.
///
/// Split out so the account-scoped path and the public one cannot drift on auth,
/// status classification or parsing — the fallback is about *which URL*, and
/// nothing else.
async fn fetch_catalog(
    client: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
    auth: AuthStyle,
) -> Result<Vec<InferenceModel>, DiscoveryError> {
    let mut response = send_classified(client, url, bearer, auth).await?;
    let named = crate::company::inference::catalogue::redact_endpoint(url);
    // Capped and read chunk-by-chunk, matching `fetch_paged_catalog` below and
    // the connect-time probe's own `probe::CATALOG_BODY_CAP` (bug KR-L1-01,
    // keys rework issue #2306) — one shared limit, refused explicitly rather
    // than either buffered without bound (`response.json()`'s old behaviour
    // here) or silently truncated into invalid JSON.
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| {
        DiscoveryError::endpoint(format!(
            "reading the model catalog from {named} failed: {e}"
        ))
    })? {
        if body.len() + chunk.len() > crate::company::inference::probe::CATALOG_BODY_CAP {
            return Err(DiscoveryError::endpoint(format!(
                "the model list from {named} could not be read: it is larger than {} MiB",
                crate::company::inference::probe::CATALOG_BODY_CAP / (1024 * 1024)
            )));
        }
        body.extend_from_slice(&chunk);
    }
    let payload: RegistryResponse = serde_json::from_slice(&body).map_err(|error| {
        DiscoveryError::endpoint(format!("model catalog from {named} was invalid: {error}"))
    })?;
    Ok(parse_models(payload))
}

/// One GET, applying this provider's auth style and classifying an HTTP
/// failure the same way for every catalog reader — the OpenAI-shaped
/// [`fetch_catalog`] above and every page of [`fetch_paged_catalog`] below
/// (keys rework, issue #2306, slice 2a). Split out of `fetch_catalog` so the
/// paged reader is not a second, silently-divergent copy of this
/// request/classify logic.
///
/// **The provider's own style, not bearer-for-everyone.** This is a NATIVE
/// endpoint — `GET /v1/models` — and Anthropic's native API rejects a
/// bearer-authenticated request with no `anthropic-version` header as
/// malformed: a 400, not a 401. That 400 on a perfectly good key was the
/// reported symptom, and it appeared here and nowhere else precisely because
/// this is the one native call the console makes.
///
/// Verified against `platform.claude.com/docs/en/api/models/list`, whose own
/// curl example is `-H 'anthropic-version: 2023-06-01' -H "X-Api-Key: …"`.
async fn send_classified(
    client: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
    auth: AuthStyle,
) -> Result<reqwest::Response, DiscoveryError> {
    let request = crate::company::inference::probe::apply_auth(client.get(url), auth, bearer);
    // Every message below names the endpoint **redacted**. A URL may carry
    // userinfo, and `reqwest` already masks it in its own `Display` — so a
    // `format!` that interpolates our copy of the URL beside that error is
    // precisely how a credential that reqwest had already hidden gets put back
    // into a string the console renders.
    let named = crate::company::inference::catalogue::redact_endpoint(url);
    let response = request
        .send()
        .await
        .map_err(|error| DiscoveryError::endpoint(format!("request to {named} failed: {error}")))?;
    let status = response.status();
    response.error_for_status().map_err(|error| {
        let message = format!("request to {named} failed: {error}");
        match status {
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                DiscoveryError::credential(status, message)
            }
            reqwest::StatusCode::NOT_FOUND => DiscoveryError::missing(message),
            _ => DiscoveryError::endpoint(message),
        }
    })
}

/// Reads the TinyHumans proxy's paged catalog to `total` (keys rework, issue
/// #2306, slice 2a): `GET {base}/models?limit=500&offset=N`, following
/// [`paged_catalog::NextPage::At`] until the envelope says there is no more,
/// or [`paged_catalog::MAX_PAGES`] is reached — at which point this reader
/// stops rather than loop, and logs what it read.
async fn fetch_paged_catalog(
    client: &reqwest::Client,
    base: &str,
    bearer: Option<&str>,
    auth: AuthStyle,
) -> Result<Vec<InferenceModel>, DiscoveryError> {
    let mut collector = paged_catalog::Collector::default();
    loop {
        let url = format!("{base}{}", paged_catalog::page_path(collector.offset()));
        let named = crate::company::inference::catalogue::redact_endpoint(&url);
        let mut response = send_classified(client, &url, bearer, auth).await?;
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            DiscoveryError::endpoint(format!(
                "reading the model catalog from {named} failed: {e}"
            ))
        })? {
            if body.len() + chunk.len() > paged_catalog::PAGE_BODY_CAP {
                return Err(DiscoveryError::endpoint(format!(
                    "a model catalog page from {named} is larger than 4 MiB"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        let body = String::from_utf8(body).map_err(|e| {
            DiscoveryError::endpoint(format!("model catalog from {named} was not UTF-8: {e}"))
        })?;
        let page = paged_catalog::parse_page(&body).map_err(|e| {
            DiscoveryError::endpoint(format!("model catalog from {named} was invalid: {e}"))
        })?;
        match collector.push(page) {
            paged_catalog::NextPage::At(_) => {}
            paged_catalog::NextPage::Done => break,
            paged_catalog::NextPage::Truncated { read, total } => {
                tracing::warn!(
                    base = %crate::company::inference::catalogue::redact_endpoint(base),
                    read,
                    total,
                    "model catalog has more pages than one read follows"
                );
                break;
            }
        }
    }
    Ok(collector
        .finish()
        .into_iter()
        .map(|e| InferenceModel {
            id: e.id,
            name: e.name,
            context_length: e.context_length,
        })
        .collect())
}

struct CacheEntry {
    at: Instant,
    models: Vec<InferenceModel>,
}

/// One endpoint's catalog cache.
#[derive(Default)]
pub(crate) struct ModelCatalogCache {
    entry: Mutex<Option<CacheEntry>>,
    /// The last failure and when it happened — see [`MODEL_CATALOG_FAILURE_TTL`].
    failure: Mutex<Option<(Instant, String)>>,
    /// Serializes cache-miss fetches (issue #1838 follow-up). Held across the
    /// whole `discover_models` await, not just the cache write: without it,
    /// every console request that lands after startup or a TTL expiry sees
    /// the same empty/stale entry and fires its own upstream fetch, so a
    /// multi-tenant host can burst several identical registry calls at once —
    /// and any of them that gets rate-limited fails even though a sibling
    /// fetch is about to populate the cache. A `tokio` mutex, not `std`: the
    /// guard needs to survive the `.await` inside [`catalog_models`].
    fetch_lock: TokioMutex<()>,
}

impl ModelCatalogCache {
    pub(crate) fn lookup(&self, now: Instant) -> Option<Vec<InferenceModel>> {
        let entry = self.entry.lock().ok()?;
        let entry = entry.as_ref()?;
        (now.saturating_duration_since(entry.at) < MODEL_CATALOG_TTL).then(|| entry.models.clone())
    }

    pub(crate) fn store(&self, models: Vec<InferenceModel>, at: Instant) {
        if let Ok(mut entry) = self.entry.lock() {
            *entry = Some(CacheEntry { at, models });
        }
        // A success clears the failure memo: the endpoint is answering again,
        // and leaving a stale "unreachable" behind would keep reporting it.
        if let Ok(mut failure) = self.failure.lock() {
            *failure = None;
        }
    }

    /// The remembered failure, while it is still fresh.
    pub(crate) fn lookup_failure(&self, now: Instant) -> Option<String> {
        let failure = self.failure.lock().ok()?;
        let (at, message) = failure.as_ref()?;
        (now.saturating_duration_since(*at) < MODEL_CATALOG_FAILURE_TTL).then(|| message.clone())
    }

    pub(crate) fn store_failure(&self, message: String, at: Instant) {
        if let Ok(mut failure) = self.failure.lock() {
            *failure = Some((at, message));
        }
    }
}

/// The catalog cache registry.
///
/// **Never keyed on the credential.** A credential must not become a map key:
/// hashing one to key a cache would put a derivative of it in process memory
/// next to the data it guards.
///
/// It *is* keyed on who asked, whenever a credential was presented. An
/// unauthenticated read is a public property of the endpoint and is shared by
/// every caller reaching it. An **authenticated** read is not: an endpoint may
/// publish an entitlement-scoped catalog, in which case a base-URL-only key
/// hands one company's model list to the next company on the same endpoint for
/// the rest of the hour (CodeRabbit security review on #2045). That only ever
/// happens inside a single process serving several companies — a local
/// multi-company host, or hosted shared-single-DB mode; database-per-tenant
/// gives each tenant its own container and so its own registry — but it is a
/// real cross-company disclosure in a supported mode, so the partition is the
/// safe side to err on.
///
/// The scope is the **company id**: already non-secret, already the unit of
/// isolation everywhere else, and it changes when the answer should change. The
/// cost is one catalog fetch per company per endpoint per hour rather than one
/// per endpoint — a bounded trade for not sharing an authenticated answer across
/// a trust boundary.
fn catalog_registry() -> &'static Mutex<HashMap<String, Arc<ModelCatalogCache>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<ModelCatalogCache>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Trailing slashes and surrounding space do not make a different endpoint.
fn cache_key(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

/// The cache key for `base_url`, read in `shape` (keys rework, issue #2306,
/// slice 2a).
///
/// One URL can answer two different bodies depending which shape it is read
/// as — 2a's own gated commit can point the legacy managed constants at the
/// same host the `tinyhumans` row uses, and until then `OPENCOMPANY_INFERENCE_URL`
/// can already alias the two. Folding both reads into one [`cache_key`] slot
/// would serve a paged envelope to an OpenAI-shaped reader or vice versa,
/// whichever fetched second. The separator is the same control character
/// [`catalog_cache_scoped`] already uses between scope and endpoint, so no
/// endpoint string can forge a shape suffix.
fn shaped_endpoint(base_url: &str, shape: CatalogShape) -> String {
    match shape {
        CatalogShape::OpenAi => cache_key(base_url),
        CatalogShape::PagedEnvelope => format!("{}\u{2}paged", cache_key(base_url)),
    }
}

/// The cache slot for an endpoint read within `scope`.
///
/// `scope` is `None` for a read that presented no credential — a public catalog,
/// shared by everyone — and `Some(company_id)` for an authenticated one. The
/// separator is a control character no company id or URL can contain, so no
/// scope-plus-endpoint pair can be spelled two ways.
pub(crate) fn catalog_cache_scoped(base_url: &str, scope: Option<&str>) -> Arc<ModelCatalogCache> {
    let endpoint = cache_key(base_url);
    let key = match scope {
        Some(scope) => format!("{scope}\u{1}{endpoint}"),
        None => endpoint,
    };
    let mut registry = match catalog_registry().lock() {
        Ok(registry) => registry,
        // A poisoned registry must not take the catalog offline for the rest of
        // the process: hand back an unshared cache, which costs this caller a
        // fetch and nothing else.
        Err(_) => return Arc::new(ModelCatalogCache::default()),
    };
    Arc::clone(registry.entry(key).or_default())
}

/// Drop every **authenticated** catalog entry read on `company`'s behalf.
///
/// Called when that company's inference credential is written, because a
/// rotation changes what the endpoint will answer without changing anything in
/// the cache key — which is made of non-secret ids on purpose, and must stay
/// that way (see [`catalog_registry`]). Without this, a company that rotated to
/// a key with different entitlements would keep reading the previous
/// credential's catalog for the rest of [`MODEL_CATALOG_TTL`], so the new bearer
/// would never be presented to `/models` at all (Codex review on #2045).
///
/// Matches on the `company\u{1}` prefix, which covers both shapes the scope
/// takes: the console route's `(company, endpoint)` and the turn path's
/// `(company, harness, endpoint)`. Keyless entries are keyed on the bare
/// endpoint and are deliberately left alone — an unauthenticated catalog is a
/// public property of the endpoint and no credential change can alter it. A URL
/// cannot contain the separator, so the prefix cannot match one by accident.
pub(crate) fn evict_company_catalogs(company: &str) {
    let prefix = format!("{company}\u{1}");
    if let Ok(mut registry) = catalog_registry().lock() {
        registry.retain(|key, _| !key.starts_with(&prefix));
    }
    // A poisoned registry needs no handling here: `catalog_cache_scoped` already
    // hands out an unshared cache in that state, so nothing stale can be served.
}

/// The unscoped (public, keyless) cache for an endpoint.
#[cfg(test)]
pub(crate) fn catalog_cache(base_url: &str) -> Arc<ModelCatalogCache> {
    catalog_cache_scoped(base_url, None)
}

/// Return the cached catalog for `base_url`, fetching it on a miss.
///
/// Single-flight per endpoint (issue #1838 follow-up): every caller for one
/// endpoint queues on its [`ModelCatalogCache::fetch_lock`] rather than racing
/// its own request, and re-checks the cache after acquiring it, so only the
/// first caller through actually fetches — everyone behind it reads what that
/// fetch just stored instead of duplicating the upstream call.
///
/// Bounded across the *whole* queue-wait-plus-fetch, not just the fetch
/// itself (issue #1838 follow-up): during an outage each queued caller would
/// otherwise acquire the lock in turn and run its own fresh
/// `MODEL_CATALOG_TIMEOUT`-bounded attempt — the Nth caller through the queue
/// waiting roughly `N * MODEL_CATALOG_TIMEOUT` before ever finding out,
/// breaking the "a console page-load waits at most [`MODEL_CATALOG_TIMEOUT`]"
/// contract this module documents (`docs/spec/runtime/providers.md`). Wrapping
/// the lock acquisition and the fetch in one `tokio::time::timeout` keeps every
/// individual caller's own wall-clock budget fixed, however many callers are
/// already ahead of it in the queue.
///
/// A failure is remembered for [`MODEL_CATALOG_FAILURE_TTL`] and replayed to
/// callers within it, so an unreachable provider costs one attempt a minute
/// rather than one per request.
///
/// `scope` is the company this read is on behalf of. It partitions the cache
/// whenever a `bearer` is presented, so an authenticated answer is never handed
/// to a different company — see [`catalog_registry`]. A keyless read carries
/// `None` and is shared, because an unauthenticated catalog is a public property
/// of the endpoint.
pub(crate) async fn catalog_models(
    base_url: &str,
    bearer: Option<&str>,
    scope: Option<&str>,
    auth: AuthStyle,
    shape: CatalogShape,
) -> Result<Vec<InferenceModel>, String> {
    // The partition follows the credential, not the caller: a read that presents
    // nothing has nothing company-specific to leak, and sharing it keeps one
    // fetch serving every company on a public endpoint.
    let authenticated_scope = bearer
        .filter(|bearer| !bearer.trim().is_empty())
        .and(scope)
        .filter(|scope| !scope.trim().is_empty());
    let cache = catalog_cache_scoped(&shaped_endpoint(base_url, shape), authenticated_scope);
    let now = Instant::now();
    if let Some(models) = cache.lookup(now) {
        return Ok(models);
    }
    if let Some(failure) = cache.lookup_failure(now) {
        return Err(failure);
    }

    // Only ever *said*, never used to key or reach anything — so it is the
    // redacted form. Both messages below are cached and replayed, and the
    // console route formats them into a banner every reader of the company
    // sees; interpolating the raw endpoint there put a stored userinfo
    // credential straight back into text that had redacted it once already
    // (Codex review on #2281).
    let endpoint = catalogue::redact_endpoint(&cache_key(base_url));
    let outcome = tokio::time::timeout(MODEL_CATALOG_TIMEOUT, async {
        let _fetch_guard = cache.fetch_lock.lock().await;
        // Another caller may have already refilled the cache while we waited
        // for the lock — re-check before fetching again.
        let now = Instant::now();
        if let Some(models) = cache.lookup(now) {
            return Ok(models);
        }
        if let Some(failure) = cache.lookup_failure(now) {
            return Err(FetchError::Failed(failure));
        }

        let mut models = discover_models(base_url, bearer, auth, shape)
            .await
            .map_err(|error| {
                if error.credential_status.is_some() {
                    FetchError::Credential(error.to_string())
                } else {
                    FetchError::Failed(error.to_string())
                }
            })?;
        if models.is_empty() {
            return Err(FetchError::Failed(format!(
                "{endpoint} published an empty model catalog"
            )));
        }
        // Sorted here, not in `parse_models`: this is the operator-facing
        // catalog picker's own copy, while `parse_models` also serves
        // `setup.rs`'s local/custom probe, which relies on provider order.
        models.sort_by(|a, b| a.id.cmp(&b.id));
        cache.store(models.clone(), now);
        Ok(models)
    })
    .await;

    let result = match outcome {
        Ok(Ok(models)) => return Ok(models),
        // An answer about the key that was presented, not about the endpoint.
        // Reported, never remembered — see [`DiscoveryError`]: memoizing it on
        // an endpoint key would hand one company's rejection to the next
        // company reaching the same endpoint with a different credential, and
        // would make a company that has just rotated a bad key wait the memo
        // out before its good one is ever tried.
        Ok(Err(FetchError::Credential(message))) => return Err(message),
        Ok(Err(FetchError::Failed(message))) => message,
        Err(_elapsed) => format!(
            "{endpoint} did not answer within {} seconds",
            MODEL_CATALOG_TIMEOUT.as_secs()
        ),
    };
    cache.store_failure(result.clone(), Instant::now());
    Err(result)
}

/// Distinguishes "the fetch itself failed" from the outer
/// [`tokio::time::timeout`] elapsing in [`catalog_models`], since both
/// have to report through the same `Result` and the outer timeout's own
/// message must win regardless of which inner step it interrupted.
enum FetchError {
    Failed(String),
    /// A `401`/`403` — about the credential presented, not the endpoint, so it
    /// is reported to this caller and never written to the endpoint's memo.
    Credential(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str) -> InferenceModel {
        InferenceModel {
            id: id.to_string(),
            name: None,
            context_length: None,
        }
    }

    /// A catalog read is refused at an address a model endpoint is never on.
    ///
    /// The connect-time probe is not the guard for this call. A provider row
    /// outlives a failed probe on purpose, `add anyway` stores one that was
    /// refused outright, and an edit can move the URL afterwards — so by the
    /// time the picker asks this endpoint what it serves, "it passed once" is
    /// not a statement about the URL in hand. On a hosted instance the address
    /// below is where the container's credentials live.
    #[tokio::test]
    async fn a_catalog_read_is_refused_at_a_link_local_address() {
        let refused = discover_models(
            "http://169.254.169.254/latest/meta-data",
            Some("pw-not-a-real-key"),
            AuthStyle::Bearer,
            CatalogShape::OpenAi,
        )
        .await
        .expect_err("a link-local endpoint must not be fetched");

        assert!(
            refused.to_string().contains("link-local"),
            "the refusal should name the reason, said: {refused}"
        );
        assert!(
            refused.credential_status.is_none(),
            "a policy refusal is not evidence about the credential, and must not \
             be classified as one"
        );
    }

    /// A failure the catalogue cache writes itself names the endpoint redacted.
    ///
    /// The fetch errors were already redacted where they are built; the empty
    /// catalogue and the timeout are sentences `catalog_models` composes on its
    /// own, and they are cached and replayed to every later reader. An endpoint
    /// stored before the credential refusal existed must not come back out of
    /// either one.
    #[tokio::test]
    async fn a_cache_written_catalog_failure_never_names_the_endpoint_credential() {
        let app = axum::Router::new().route(
            "/v1/models",
            axum::routing::get(|| async { axum::Json(serde_json::json!({ "data": [] })) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let endpoint = format!("http://alice:hunter2@{address}/v1");

        let error = catalog_models(
            &endpoint,
            None,
            None,
            AuthStyle::Bearer,
            CatalogShape::OpenAi,
        )
        .await
        .expect_err("an empty catalogue is reported as a failure");
        server.abort();

        assert!(
            error.contains("empty model catalog"),
            "expected the cache's own sentence, got: {error}"
        );
        assert!(
            !error.contains("hunter2") && !error.contains("alice"),
            "the cached failure must not carry the endpoint's userinfo: {error}"
        );
    }

    // ---- the paged catalog (keys rework, issue #2306, slice 2a) ------------

    /// Serves `/agent-integrations/openrouter/models`, calling `respond(offset)`
    /// for each request to build the `(status, body)` it answers, and recording
    /// every query string it was called with.
    fn spawn_proxy_catalog(
        respond: impl Fn(usize) -> (u16, String) + Clone + Send + Sync + 'static,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use axum::extract::Query;
        use axum::http::StatusCode;
        use axum::response::{IntoResponse, Response};

        let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_for_route = seen.clone();
        let app = axum::Router::new().route(
            "/agent-integrations/openrouter/models",
            axum::routing::get(move |Query(params): Query<HashMap<String, String>>| {
                let seen = seen_for_route.clone();
                let respond = respond.clone();
                async move {
                    let query = params
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join("&");
                    seen.lock().unwrap().push(query);
                    let offset: usize = params
                        .get("offset")
                        .and_then(|o| o.parse().ok())
                        .unwrap_or(0);
                    let (status, body) = respond(offset);
                    let status = StatusCode::from_u16(status).unwrap();
                    (status, [("content-type", "application/json")], body).into_response()
                        as Response
                }
            }),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            format!("http://{address}/agent-integrations/openrouter"),
            seen,
        )
    }

    #[tokio::test]
    async fn the_paged_catalog_is_read_to_total_with_the_bearer() {
        let (base, seen) = spawn_proxy_catalog(|offset| {
            let body = if offset == 0 {
                serde_json::json!({
                    "success": true,
                    "data": {"data": [{"id": "acme/test-model", "name": "Test", "context_length": 8192}], "total": 2},
                })
            } else {
                serde_json::json!({
                    "success": true,
                    "data": {"data": [{"id": "acme/other-model"}], "total": 2},
                })
            };
            (200, body.to_string())
        });

        let models = discover_models(
            &base,
            Some("th-not-a-real-key"),
            AuthStyle::Bearer,
            CatalogShape::PagedEnvelope,
        )
        .await
        .expect("the paged catalog reads");

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "acme/test-model");
        assert_eq!(models[0].name.as_deref(), Some("Test"));
        assert_eq!(models[0].context_length, Some(8192));
        assert_eq!(models[1].id, "acme/other-model");
        assert_eq!(seen.lock().unwrap().len(), 2, "one request per page");
    }

    #[tokio::test]
    async fn a_large_catalog_is_read_in_pages_of_500() {
        const TOTAL: usize = 1_200;
        let (base, seen) = spawn_proxy_catalog(|offset| {
            let end = (offset + 500).min(TOTAL);
            let data: Vec<serde_json::Value> = (offset..end)
                .map(|i| serde_json::json!({"id": format!("acme/model-{i:04}")}))
                .collect();
            (
                200,
                serde_json::json!({
                    "success": true,
                    "data": {"data": data, "total": TOTAL},
                })
                .to_string(),
            )
        });

        let models = discover_models(
            &base,
            Some("th-not-a-real-key"),
            AuthStyle::Bearer,
            CatalogShape::PagedEnvelope,
        )
        .await
        .expect("the paged catalog reads");

        assert_eq!(models.len(), TOTAL);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3, "offsets 0, 500, 1000: {seen:?}");
        for id in [0usize, 500, 1199] {
            assert!(
                models.iter().any(|m| m.id == format!("acme/model-{id:04}")),
                "missing model-{id:04}"
            );
        }
    }

    #[tokio::test]
    async fn a_503_before_the_snapshot_loads_is_memoized_not_empty() {
        let (base, _seen) = spawn_proxy_catalog(|_offset| {
            (
                503,
                serde_json::json!({"success": false, "error": "loading"}).to_string(),
            )
        });

        let scope = "paged-503-scope";
        let error = catalog_models(
            &base,
            Some("th-not-a-real-key"),
            Some(scope),
            AuthStyle::Bearer,
            CatalogShape::PagedEnvelope,
        )
        .await
        .expect_err("a 503 is a failure");
        assert!(error.contains("503"), "{error}");

        let now = Instant::now();
        assert!(
            catalog_cache_scoped(
                &shaped_endpoint(&base, CatalogShape::PagedEnvelope),
                Some(scope)
            )
            .lookup_failure(now)
            .is_some(),
            "a 503 must be memoized"
        );
    }

    #[tokio::test]
    async fn a_401_or_403_on_the_paged_catalog_is_not_memoized() {
        for status in [401u16, 403] {
            let (base, _seen) = spawn_proxy_catalog(move |_offset| {
                (status, serde_json::json!({"error": "no"}).to_string())
            });
            let scope = format!("paged-{status}-scope");
            catalog_models(
                &base,
                Some("th-not-a-real-key"),
                Some(scope.as_str()),
                AuthStyle::Bearer,
                CatalogShape::PagedEnvelope,
            )
            .await
            .expect_err("a credential failure");
            let now = Instant::now();
            assert!(
                catalog_cache_scoped(
                    &shaped_endpoint(&base, CatalogShape::PagedEnvelope),
                    Some(scope.as_str())
                )
                .lookup_failure(now)
                .is_none(),
                "a {status} must not be memoized"
            );
        }
    }

    #[tokio::test]
    async fn an_openai_shaped_answer_is_not_read_as_the_paged_catalog() {
        let (base, _seen) = spawn_proxy_catalog(|_offset| {
            (
                200,
                serde_json::json!({"data": [{"id": "acme/test-model"}]}).to_string(),
            )
        });

        let error = discover_models(
            &base,
            Some("th-not-a-real-key"),
            AuthStyle::Bearer,
            CatalogShape::PagedEnvelope,
        )
        .await
        .expect_err("an OpenAI-shaped body is not the envelope");
        assert!(error.to_string().contains("envelope"), "{error}");
    }

    #[tokio::test]
    async fn a_paged_failure_never_names_the_endpoint_credential() {
        let (base, _seen) = spawn_proxy_catalog(|_offset| {
            (
                503,
                serde_json::json!({"success": false, "error": "loading"}).to_string(),
            )
        });
        let base = base.replacen("http://", "http://alice:hunter2@", 1);

        let error = discover_models(
            &base,
            Some("th-not-a-real-key"),
            AuthStyle::Bearer,
            CatalogShape::PagedEnvelope,
        )
        .await
        .expect_err("a 503 is a failure");
        let text = error.to_string();
        assert!(!text.contains("hunter2"), "{text}");
        assert!(!text.contains("alice"), "{text}");
    }

    #[tokio::test]
    async fn an_oversized_page_is_refused_not_buffered() {
        let oversized = "x".repeat(paged_catalog::PAGE_BODY_CAP + 1);
        let (base, _seen) = spawn_proxy_catalog(move |_offset| (200, oversized.clone()));

        let error = discover_models(
            &base,
            Some("th-not-a-real-key"),
            AuthStyle::Bearer,
            CatalogShape::PagedEnvelope,
        )
        .await
        .expect_err("an oversized page is refused");
        assert!(error.to_string().contains("larger than"), "{error}");
    }

    #[test]
    fn one_url_read_in_two_shapes_is_two_cache_slots() {
        const ENDPOINT: &str = "https://shape-split.example/v1";
        assert_eq!(
            shaped_endpoint(ENDPOINT, CatalogShape::OpenAi),
            cache_key(ENDPOINT)
        );
        assert_ne!(
            shaped_endpoint(ENDPOINT, CatalogShape::OpenAi),
            shaped_endpoint(ENDPOINT, CatalogShape::PagedEnvelope)
        );
    }

    #[test]
    fn catalog_parser_returns_every_non_empty_unique_model_in_provider_order() {
        let parsed = parse_models(RegistryResponse {
            data: vec![
                serde_json::json!({"id": " vendor/zeta ", "name": " Zeta ", "context_length": 128_000}),
                serde_json::json!({"id": "vendor/alpha"}),
                serde_json::json!({"id": "vendor/zeta", "name": "duplicate"}),
                serde_json::json!({"id": "   "}),
            ],
        });

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "vendor/zeta");
        assert_eq!(parsed[0].name.as_deref(), Some("Zeta"));
        assert_eq!(parsed[0].context_length, Some(128_000));
        assert_eq!(parsed[1], model("vendor/alpha"));
    }

    /// Regression for a Minor review finding on #1838: the previous
    /// `BTreeMap`-keyed parser returned ids in lexicographic order, so
    /// `src/server/setup.rs` taking `.next()` off the result silently swapped
    /// from "the provider's first-listed model" (what the deleted
    /// `discover_local_model` returned) to "the alphabetically first model" —
    /// an arbitrary pick on any multi-model host whose ids don't already
    /// sort first-to-preferred.
    #[test]
    fn catalog_parser_does_not_alphabetize_a_local_hosts_leading_model() {
        let parsed = parse_models(RegistryResponse {
            data: vec![
                serde_json::json!({"id": "zephyr-preferred"}),
                serde_json::json!({"id": "alpaca-not-preferred"}),
            ],
        });

        assert_eq!(
            parsed.first().map(|m| m.id.as_str()),
            Some("zephyr-preferred"),
            "the provider's leading model must survive `.next()` in setup.rs, not lose to sort order"
        );
    }

    /// Regression for a P2 review finding on #1838: a single malformed
    /// record (here, a numeric `id`) used to fail `RegistryResponse`
    /// deserialization outright — `discover_models` never reached
    /// `parse_models` at all, so a valid model earlier or later in the same
    /// `data` array was lost with it. `data` is now decoded as raw JSON
    /// first, so only the bad entry drops out.
    #[test]
    fn catalog_parser_skips_malformed_entries_instead_of_rejecting_the_response() {
        let payload: RegistryResponse = serde_json::from_str(
            r#"{"data": [
                {"id": "vendor/good-one"},
                {"id": 12345},
                {"id": "vendor/good-three"}
            ]}"#,
        )
        .expect("RegistryResponse itself must still deserialize leniently");

        let parsed = parse_models(payload);

        let ids: Vec<&str> = parsed.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["vendor/good-one", "vendor/good-three"]);
    }

    /// Regression for a P2 review finding on #1838's follow-up round: an
    /// entry whose `id` is fine but whose *optional* `context_length` is not
    /// used to lose the id right along with it. The old parser decoded each
    /// `data` entry into `RegistryModel` in one shot, and serde fails that
    /// whole decode on a type-mismatched field even when it is
    /// `Option<u64>` — a string-valued `context_length` isn't "field
    /// absent", it's "field present with the wrong type", and `Option`
    /// deserialization does not paper over that. So this id used to vanish
    /// from the catalog entirely instead of just losing its context length.
    #[test]
    fn catalog_parser_preserves_a_valid_id_with_malformed_optional_metadata() {
        let payload: RegistryResponse = serde_json::from_str(
            r#"{"data": [
                {"id": "vendor/good-one"},
                {"id": "vendor/good-two", "context_length": "not-a-number"},
                {"id": "vendor/good-three", "name": 42}
            ]}"#,
        )
        .expect("RegistryResponse itself must still deserialize leniently");

        let parsed = parse_models(payload);

        let ids: Vec<&str> = parsed.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["vendor/good-one", "vendor/good-two", "vendor/good-three"],
            "a bad optional field must not discard the id sitting next to it"
        );
        let good_two = parsed
            .iter()
            .find(|m| m.id == "vendor/good-two")
            .expect("vendor/good-two must survive");
        assert_eq!(good_two.context_length, None);
        let good_three = parsed
            .iter()
            .find(|m| m.id == "vendor/good-three")
            .expect("vendor/good-three must survive");
        assert_eq!(good_three.name, None);
    }

    #[test]
    fn cache_serves_only_fresh_catalogs() {
        let cache = ModelCatalogCache::default();
        let stored_at = Instant::now();
        cache.store(vec![model("vendor/model")], stored_at);

        assert_eq!(
            cache.lookup(stored_at + MODEL_CATALOG_TTL - Duration::from_secs(1)),
            Some(vec![model("vendor/model")])
        );
        assert_eq!(cache.lookup(stored_at + MODEL_CATALOG_TTL), None);
    }

    /// A failure used to store nothing, so an unreachable provider cost a fresh
    /// `MODEL_CATALOG_TIMEOUT` on every request that consulted it — once per
    /// status read and, now that the turn path consults the vocabulary, once
    /// per turn. Remembering it briefly turns that into one attempt a minute.
    #[test]
    fn a_failure_is_remembered_briefly_and_cleared_by_the_next_success() {
        let cache = ModelCatalogCache::default();
        let failed_at = Instant::now();
        cache.store_failure("provider.example did not answer".to_string(), failed_at);

        assert_eq!(
            cache.lookup_failure(failed_at + MODEL_CATALOG_FAILURE_TTL - Duration::from_secs(1)),
            Some("provider.example did not answer".to_string()),
        );
        assert_eq!(
            cache.lookup_failure(failed_at + MODEL_CATALOG_FAILURE_TTL),
            None,
            "the memo expires far sooner than a success, so a provider coming back up is \
             picked up promptly"
        );

        cache.store_failure("still down".to_string(), Instant::now());
        let recovered_at = Instant::now();
        cache.store(vec![model("vendor/model")], recovered_at);
        assert_eq!(
            cache.lookup_failure(recovered_at),
            None,
            "a success clears the memo — leaving it would keep reporting an outage that ended"
        );
    }

    /// An authenticated catalog is not shared across companies.
    ///
    /// The positive cache is keyed on the endpoint, which is right for a public
    /// catalog and wrong for one read with a company's own credential: an
    /// endpoint may publish an entitlement-scoped list, and a base-URL-only key
    /// would serve one company's answer to the next for the rest of the hour
    /// (CodeRabbit security review on #2045). A keyless read stays shared,
    /// because there is nothing company-specific in it to leak.
    #[test]
    fn an_authenticated_catalog_is_partitioned_per_company_and_a_keyless_one_is_not() {
        const ENDPOINT: &str = "https://shared-gateway.example/v1";
        // Company ids nothing else uses. The registry is process-global and
        // `evict_company_catalogs` clears a whole company, so a scope named
        // `acme` would be wiped by any route test in another module that saves a
        // key for the company of that name, mid-assertion and at random.
        const ONE: &str = "partition-one";
        const TWO: &str = "partition-two";
        let now = Instant::now();

        let acme = catalog_cache_scoped(ENDPOINT, Some(ONE));
        let other = catalog_cache_scoped(ENDPOINT, Some(TWO));
        acme.store(vec![model("acme/entitled-only")], now);

        assert_eq!(acme.lookup(now), Some(vec![model("acme/entitled-only")]));
        assert_eq!(
            other.lookup(now),
            None,
            "one company's authenticated catalog must not answer for another on the same endpoint"
        );
        assert_eq!(
            catalog_cache_scoped(ENDPOINT, None).lookup(now),
            None,
            "nor must it answer a keyless read of the same endpoint"
        );

        // The same company reaching the same endpoint does reuse its own entry,
        // so the partition costs one fetch per company rather than one per call.
        assert_eq!(
            catalog_cache_scoped(ENDPOINT, Some(ONE)).lookup(now),
            Some(vec![model("acme/entitled-only")])
        );

        // A keyless catalog is a public property of the endpoint, and stays
        // shared by everyone reading it that way.
        const PUBLIC: &str = "https://public-registry.example/v1";
        catalog_cache_scoped(PUBLIC, None).store(vec![model("vendor/public")], now);
        assert_eq!(
            catalog_cache_scoped(PUBLIC, None).lookup(now),
            Some(vec![model("vendor/public")])
        );
    }

    /// The partition goes one level finer than the company: per **harness**.
    ///
    /// `resolve_effective_scoped` resolves config and credentials per
    /// `HarnessScope`, which is what lets one `built_in` harness ride the
    /// subscription while another runs on a key of its own. Two harnesses in one
    /// company can therefore present different credentials to the same endpoint,
    /// and a company-only key reused the first one's entitlement-scoped catalog
    /// for the second without its credential ever being presented (Codex review
    /// on #2045).
    ///
    /// This asserts the property at the cache level, on the exact scope-string
    /// shape a per-harness catalog read builds — company and harness joined by
    /// the same control character `catalog_cache_scoped` uses, so a
    /// three-field key cannot be spelled two ways.
    #[test]
    fn two_harnesses_in_one_company_do_not_share_an_authenticated_catalog() {
        const ENDPOINT: &str = "https://gateway.example/v1";
        // A company id nothing else uses — see the note in the test above.
        const COMPANY: &str = "harness-partition-co";
        let now = Instant::now();
        let subscription = format!("{COMPANY}\u{1}{}", "default");
        let own_key = format!("{COMPANY}\u{1}{}", "research");

        catalog_cache_scoped(ENDPOINT, Some(&subscription))
            .store(vec![model("gateway/subscription-tier")], now);

        assert_eq!(
            catalog_cache_scoped(ENDPOINT, Some(&own_key)).lookup(now),
            None,
            "a second harness's key may reach a different entitlement, so it must read for itself"
        );
        assert_eq!(
            catalog_cache_scoped(ENDPOINT, Some(&subscription)).lookup(now),
            Some(vec![model("gateway/subscription-tier")]),
            "the harness that did the read still reuses its own entry"
        );
        // The company-only key is a third, distinct slot — proof the harness
        // half genuinely participates rather than being absorbed into the id.
        assert_eq!(
            catalog_cache_scoped(ENDPOINT, Some(COMPANY)).lookup(now),
            None
        );
    }

    /// Rotating a credential drops that company's authenticated catalogs, and
    /// nobody else's.
    ///
    /// The cache key holds non-secret ids only, so a rotation is invisible to it
    /// — the previous credential's catalog would otherwise answer for the rest
    /// of [`MODEL_CATALOG_TTL`] and the new bearer would never reach `/models`
    /// (Codex review on #2045). Eviction on the write is what keeps that
    /// invariant affordable.
    #[test]
    fn rotating_a_credential_evicts_only_that_companys_authenticated_catalogs() {
        const ENDPOINT: &str = "https://rotating-gateway.example/v1";
        let now = Instant::now();
        let acme_console = "rot-acme".to_string();
        let acme_harness = format!("rot-acme\u{1}{}", "research");

        catalog_cache_scoped(ENDPOINT, Some(&acme_console))
            .store(vec![model("old/entitlement")], now);
        catalog_cache_scoped(ENDPOINT, Some(&acme_harness))
            .store(vec![model("old/entitlement")], now);
        catalog_cache_scoped(ENDPOINT, Some("rot-other"))
            .store(vec![model("other/entitlement")], now);
        catalog_cache_scoped(ENDPOINT, None).store(vec![model("public/model")], now);

        evict_company_catalogs("rot-acme");

        assert_eq!(
            catalog_cache_scoped(ENDPOINT, Some(&acme_console)).lookup(now),
            None,
            "the console's own scoped read must be re-fetched with the new credential"
        );
        assert_eq!(
            catalog_cache_scoped(ENDPOINT, Some(&acme_harness)).lookup(now),
            None,
            "and so must every harness scope beneath that company"
        );
        assert_eq!(
            catalog_cache_scoped(ENDPOINT, Some("rot-other")).lookup(now),
            Some(vec![model("other/entitlement")]),
            "another company's credential did not change, so its catalog stands"
        );
        assert_eq!(
            catalog_cache_scoped(ENDPOINT, None).lookup(now),
            Some(vec![model("public/model")]),
            "a keyless catalog is a public property of the endpoint and no \
             credential change can alter it"
        );
    }

    /// A credential-specific rejection is reported and **not** remembered.
    ///
    /// The negative memo is keyed on the endpoint, which is right for "this
    /// endpoint did not answer" and wrong for "this key was rejected". On a
    /// multi-company host the second would let one company's bad key answer for
    /// the next company reaching the same endpoint with a valid one, and would
    /// make a company that has just rotated a bad key wait the memo out before
    /// its good key is ever presented (Codex review on #2045).
    ///
    /// Asserted at the seam rather than over the network: the classification
    /// lives in [`DiscoveryError`], and [`catalog_models`] is what must not
    /// write a `Credential` failure into the endpoint's memo.
    #[test]
    fn a_credential_rejection_is_not_written_to_the_endpoints_failure_memo() {
        const ENDPOINT: &str = "https://rejects-one-key.example/v1";
        let cache = catalog_cache(ENDPOINT);
        let now = Instant::now();
        assert_eq!(cache.lookup_failure(now), None, "nothing remembered yet");

        // What an endpoint-level failure does: it is remembered, so an outage
        // costs one attempt a minute rather than one per request.
        cache.store_failure(format!("{ENDPOINT} did not answer within 10 seconds"), now);
        assert!(cache.lookup_failure(now).is_some());

        // And the classification that keeps a 401 out of that path.
        let rejection = DiscoveryError::credential(
            reqwest::StatusCode::UNAUTHORIZED,
            "401 Unauthorized".to_string(),
        );
        assert!(
            rejection.credential_status.is_some(),
            "a 401 is an answer about the key, not about the endpoint"
        );
        assert_eq!(rejection.credential_status, Some(401));
        assert!(
            DiscoveryError::endpoint("connection refused".to_string())
                .credential_status
                .is_none(),
            "a transport failure is an answer about the endpoint, and is memoized"
        );
    }

    /// Two endpoints are two caches. A single process-wide slot is what let one
    /// company's catalog answer for another's endpoint in the first place.
    #[test]
    fn each_endpoint_gets_its_own_cache_and_trailing_slashes_do_not_split_one() {
        let a = catalog_cache("https://a.example/v1");
        let b = catalog_cache("https://b.example/v1");
        let now = Instant::now();
        a.store(vec![model("a-only")], now);

        assert_eq!(a.lookup(now), Some(vec![model("a-only")]));
        assert_eq!(b.lookup(now), None, "b must not inherit a's catalog");
        assert_eq!(
            catalog_cache("https://a.example/v1/").lookup(now),
            Some(vec![model("a-only")]),
            "a trailing slash is the same endpoint"
        );
    }

    /// Regression for a P2 review finding on #1838's follow-up round: without
    /// `fetch_lock`, every concurrent caller that observed the same
    /// empty/stale entry would independently "fetch" — a multi-tenant host
    /// bursting several identical upstream calls at once. This exercises the
    /// exact lock-then-recheck sequence [`catalog_models`] runs (acquire
    /// `fetch_lock`, re-`lookup`, only then do the (here, simulated) fetch),
    /// against the real `ModelCatalogCache`, so a regression that drops the
    /// lock or the re-check fails this test rather than only showing up as
    /// upstream rate-limit noise in production.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_misses_coalesce_into_a_single_fetch() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = Arc::new(ModelCatalogCache::default());
        let fetch_count = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let fetch_count = Arc::clone(&fetch_count);
                tokio::spawn(async move {
                    let now = Instant::now();
                    if let Some(models) = cache.lookup(now) {
                        return models;
                    }
                    let _fetch_guard = cache.fetch_lock.lock().await;
                    let now = Instant::now();
                    if let Some(models) = cache.lookup(now) {
                        return models;
                    }
                    fetch_count.fetch_add(1, Ordering::SeqCst);
                    // Hold the lock across a slow "upstream" call so every
                    // other task is still queued on `fetch_lock` — the same
                    // shape a real registry round-trip has.
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let models = vec![model("vendor/single-flight")];
                    cache.store(models.clone(), Instant::now());
                    models
                })
            })
            .collect();

        for handle in handles {
            let models = handle.await.expect("fetch task must not panic");
            assert_eq!(models, vec![model("vendor/single-flight")]);
        }

        assert_eq!(
            fetch_count.load(Ordering::SeqCst),
            1,
            "only the first caller through fetch_lock should fetch; the rest must reuse its result"
        );
    }

    /// Regression for a P2 review finding on #1838's follow-up round, filed
    /// against the single-flight fix directly above: `fetch_lock` serializes
    /// misses, but a *failed* fetch stores nothing, so during a registry
    /// outage every queued caller would previously run its own fresh
    /// `bound`-length attempt after acquiring the lock — the Nth caller
    /// through the queue waiting roughly `N * bound` before ever finding out,
    /// which is exactly the docs/spec/runtime/providers.md "at most
    /// `MODEL_CATALOG_TIMEOUT` seconds" promise this test defends. Mirrors
    /// `catalog_models`'s real composition (`tokio::time::timeout` wrapped
    /// around lock-acquire + recheck + fetch) against the real
    /// `ModelCatalogCache`, with a simulated fetch standing in for
    /// `discover_models` so the assertion is deterministic instead of racing
    /// a real HTTP timeout.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_queued_caller_never_waits_longer_than_the_catalog_timeout() {
        use std::sync::Arc;

        let cache = Arc::new(ModelCatalogCache::default());
        // Short enough to keep the suite fast, long enough that three tasks
        // queuing on one real `tokio::sync::Mutex` stay well-ordered.
        let bound = Duration::from_millis(120);
        let fetch_delay = Duration::from_millis(80);

        async fn bounded_miss(
            cache: Arc<ModelCatalogCache>,
            bound: Duration,
            fetch_delay: Duration,
        ) {
            let _ = tokio::time::timeout(bound, async {
                let _fetch_guard = cache.fetch_lock.lock().await;
                let now = Instant::now();
                if cache.lookup(now).is_some() {
                    return;
                }
                // Simulates a registry that is down: takes real time, then
                // fails without storing anything — so the next caller through
                // the lock faces the same empty cache this one did.
                tokio::time::sleep(fetch_delay).await;
            })
            .await;
        }

        let handles: Vec<_> = (0..3)
            .map(|_| {
                let cache = Arc::clone(&cache);
                tokio::spawn(async move {
                    let started = Instant::now();
                    bounded_miss(cache, bound, fetch_delay).await;
                    started.elapsed()
                })
            })
            .collect();

        for handle in handles {
            let elapsed = handle.await.expect("task must not panic");
            assert!(
                elapsed <= bound + Duration::from_millis(40),
                "a caller waited {elapsed:?}, which exceeds its own {bound:?} budget by more \
                 than scheduling slack — queue position must not multiply the wait"
            );
        }
    }
}
