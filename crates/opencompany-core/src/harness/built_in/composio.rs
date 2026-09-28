//! Per-tenant Composio tools (issue #110, epic #26 Cell D): the Gmail / Slack /
//! GitHub surface OpenHuman exposes through its backend-proxied Composio routes,
//! bridged into a company agent's toolbelt behind the opt-in `composio` grant.
//!
//! ## Two routes: managed, and the company's own account
//!
//! Everything below describes the **managed** route, which is the default and
//! the one that needs no configuration. A company that holds its own Composio
//! account can instead store a Composio API key and have every call go straight
//! to `backend.composio.dev` — no proxy, no platform identity, no platform bill.
//! That route is [`composio_direct`](crate::harness::composio_direct); the two
//! meet at [`LiveClient`], which is the only place in this module that knows
//! which one answered. [`TenantComposio::mode`] is what says which is in force,
//! and it is folded into the roster fingerprint so switching reaches the agents
//! on their next turn.
//!
//! Under BYOK the tiers below do not apply at all: the company's own key is the
//! only credential, and its absence means no tools rather than a fallback — see
//! [`resolve_access`](crate::company::composio::resolve_access) for why
//! borrowing the platform identity there would be the wrong kind of helpful.
//!
//! ## Tenant isolation (the security spine)
//!
//! In OpenHuman's **backend mode**, no Composio call carries an `entity_id`: the
//! backend derives the Composio entity from the **bearer JWT** on the request.
//! So the *only* isolation lever is **which credential the call is made with**,
//! and that resolution happens **server-side** here — never from agent input and
//! never from manifest free-text.
//!
//! Three sources, in strict precedence:
//!
//! 1. **The company's own Composio token**, stored in its [`SecretStore`] under
//!    [`TINYHUMANS_KEY_KEY`] by the console. A company that brings its own Composio
//!    identity keeps it, always. This is the self-hosting escape hatch, not a
//!    deployment mode.
//! 2. **The company's own TinyHumans credential** —
//!    [`company_key`](crate::company::company_key), the one key its admin set on
//!    this tenant. The backend derives the Composio entity from whatever bearer
//!    it is handed, and a TinyHumans key is a bearer it recognises, so no
//!    separate Composio token and no per-tenant provider app is needed to
//!    connect a provider (issue #586).
//! 3. **This instance's platform identity** — the
//!    [`TinyhumansTokenSource`](crate::company::credentials::TinyhumansTokenSource)
//!    the runtime authenticates with everywhere else. On the hosted platform that
//!    is a projected, audience-bound token the cluster rotates in place, so it is
//!    read **per call**, never captured when the roster is built.
//!
//! Tiers 2 and 3 are not resolved here: they are
//! [`company_key::resolve`](crate::company::company_key::resolve), the one seam
//! a brokered surface resolves a company identity through. That is what makes
//! rotating the company key reach every surface wired to it rather than
//! whichever remembered to re-read — Composio today, with inference and
//! embeddings still on the environment until #585.
//!
//! With none of the three, resolution yields `None` and no tools are wired (fail
//! closed) — an absent credential must mean "no tools", never a borrowed
//! identity. Two companies pasting the *same* token would share one entity; that
//! cannot be prevented client-side and is documented as a deployment caveat.
//!
//! ## One connection, every agent
//!
//! Nothing here is scoped to the member who connected a provider. The credential
//! is resolved from the *company's* store, every agent in the company resolves
//! the same one, and the backend derives one entity from it — so a provider
//! connected once is usable by every agent in the company, which is the
//! behaviour issue #586 asks for.
//!
//! ## Rotation must not churn the roster
//!
//! The roster fingerprint hashes the credential's **identity**, not its bytes:
//! for the projected tier that is the tier + path (see
//! [`Credential::hash_identity`]). Hashing the value would rebuild every agent's
//! tool roster on the platform's rotation schedule — every few minutes, forever.
//! A pasted per-company token still fingerprints by value, because there a new
//! value really is a new identity.
//!
//! ## Write-only credential
//!
//! The token is write-only. Whatever value a call resolves is fed to
//! [`redact`](crate::harness::mcp_probe::redact) (successes) or
//! [`scrub`](crate::harness::mcp_probe::scrub) (errors) as a known secret, so it
//! cannot survive into **any** `ToolResult`; it is absent from every tracing
//! line and from the [`Debug`] impl. Only non-secret status (backend URL,
//! toolkit allowlist) is ever surfaced.
//!
//! ## Result sizing (issue #410)
//!
//! Successes go through `redact` + a **body** budget, never through `scrub`.
//! `scrub` caps at 300 bytes — correct for the one-line MCP failure sentence it
//! was built for, and the reason every Composio result used to arrive as a
//! silent fragment. See `scrubbed_ok` below and
//! [`composio_catalog`](crate::harness::composio_catalog).

use std::sync::Arc;

use crate::company::credentials::{Credential, TinyhumansTokenSource};
use crate::ports::SecretStore;
use crate::ports::types::CompanyId;

// The credential key + backend routing live in the always-compiled
// `company::composio` module (so the console read/write plane can manage the
// token in the default build); re-exported here for the harness call sites.
pub use crate::company::composio::{
    BYOK_KEY_KEY, ComposioMode, DIRECT_BASE_URL, TINYHUMANS_API_URL_ENV, TINYHUMANS_KEY_KEY,
    backend_url_or_default, resolve_access, resolve_credential,
};

/// A per-tenant Composio configuration: the backend URL, how the outbound bearer
/// is obtained, and the toolkit allowlist.
///
/// **Security invariant**: the credential decides which Composio entity the
/// backend resolves, so a company can only ever reach its own connected
/// accounts. It is never logged, returned, or `Debug`-printed.
///
/// Always compiled (so the [`HarnessDeps`](crate::harness::HarnessDeps) field
/// exists in every `openhuman` build and every construction site fails closed
/// with `None`); the live tool constructors in [`composio_tools`] are gated
/// behind the `composio` feature.
#[derive(Clone, Debug)]
pub struct TenantComposio {
    /// The Composio backend base URL (e.g. `https://api.tinyhumans.ai`).
    pub backend_url: String,
    /// How the outbound bearer is obtained: the company's own stored token, or
    /// this instance's platform identity. Resolved per call — see the module docs.
    ///
    /// Under [`ComposioMode::Byok`] this holds the company's own **Composio API
    /// key** instead, which is presented as `x-api-key` to Composio itself
    /// rather than as a bearer to the OpenHuman backend. One field because it is
    /// one thing — the single secret this config authenticates with — and
    /// [`Self::mode`] is what says which host it is presented to.
    credential: Credential,
    /// Which host the calls go to: the OpenHuman-managed backend, or the
    /// company's own Composio account (issue: BYOK Composio).
    ///
    /// Part of the config rather than re-read per call for the same reason
    /// [`Self::toolkits`] is: it is a company decision that must reach the
    /// agents through a roster rebuild, not a value a tool re-derives while it
    /// runs.
    mode: ComposioMode,
    /// The **managed-chain** credential, kept alongside the BYOK one for a
    /// single purpose: fetching OpenHuman's curated toolkit list.
    ///
    /// ## Why a BYOK company still asks OpenHuman what to offer
    ///
    /// Because neither list a BYOK company can reach on its own describes what
    /// it should be shown. Composio's directory is 1501 entries — the whole
    /// integration catalogue, most of which this harness has no curated tool
    /// surface for — and the compiled-in shortlist is 31. OpenHuman's backend
    /// publishes the middle answer, 123 providers, and it is the same list a
    /// managed company is offered, which is the point: switching a company to
    /// its own Composio account should change *who it acts as*, not what the
    /// console lets it browse.
    ///
    /// This is a **non-secret list**, fetched once per
    /// [`CATALOG_TTL`](crate::server::ops::composio_toolkits::CATALOG_TTL). No
    /// Composio traffic is proxied through it and nothing is billed by it —
    /// authorize and execute still go straight to the company's own account.
    ///
    /// [`Credential::None`] when no managed tier resolves (a standalone host
    /// with no TinyHumans identity at all), in which case the catalog falls back
    /// to the company's own Composio directory — see
    /// [`LiveClient::list_toolkits`].
    catalog: Credential,
    /// The toolkit allowlist (Gmail / Slack / GitHub, …). Empty defers to the
    /// backend's server-enforced allowlist (open mode); non-empty narrows
    /// strictly, client-side, before any network round-trip.
    pub toolkits: Vec<String>,
    /// Which connected account this company means, per toolkit (issue #820).
    ///
    /// Read from the company's own store by [`Self::resolve`], never from agent
    /// input: the id decides which Gmail an agent sends as, so it must be a
    /// company decision the same way the credential is.
    ///
    /// Empty — the ordinary case — means the company has expressed no intent and
    /// `composio_execute` sends no connection id, leaving the account to
    /// Composio's own resolution exactly as before.
    defaults: crate::company::composio::ComposioDefaults,
    #[cfg(feature = "composio")]
    authorizations: Arc<tokio::sync::Mutex<live::AuthorizeCache>>,
}

impl TenantComposio {
    /// A **managed** config over an explicit bearer — the constructor tests and
    /// callers outside the resolver use.
    ///
    /// The bearer is presented to the OpenHuman backend. A caller holding an
    /// answer from [`resolve_access`] wants [`Self::from_access`] instead,
    /// which cannot lose the route.
    pub fn new(
        backend_url: impl Into<String>,
        credential: Credential,
        toolkits: Vec<String>,
    ) -> Self {
        Self {
            backend_url: backend_url.into(),
            credential,
            mode: ComposioMode::Managed,
            catalog: Credential::None,
            toolkits,
            defaults: Default::default(),
            #[cfg(feature = "composio")]
            authorizations: Default::default(),
        }
    }

    /// The same config with the managed-chain credential attached, for the
    /// curated toolkit list only. Meaningless under
    /// [`ComposioMode::Managed`], where [`Self::credential`] already is it.
    pub fn with_catalog_credential(mut self, catalog: Credential) -> Self {
        self.catalog = catalog;
        self
    }

    /// A config over a resolved
    /// [`ComposioAccess`](crate::company::composio::ComposioAccess) — the
    /// **only** way to build a BYOK config, and the only constructor a caller
    /// that resolved a credential should use.
    ///
    /// ## Why this is not a `with_mode(…)` builder
    ///
    /// Because a builder would make the dangerous thing representable. Under
    /// BYOK the credential is a **Composio** API key; under managed it is a
    /// bearer the **TinyHumans backend** recognises. A call site that resolved
    /// the first and then forgot to say so would build a managed config around
    /// it, and [`live_call`] would send that company's `ak_…` to
    /// `api.tinyhumans.ai` as a bearer — a credential delivered to a host that
    /// has no business holding it, on a path where nothing would look wrong.
    ///
    /// Taking the pair that [`resolve_access`] returns, as one value, means the
    /// route cannot be dropped on the floor between resolving a credential and
    /// presenting it. [`Self::new`] stays for the callers that genuinely mean
    /// "managed, with this bearer" — every existing one, and the tests.
    pub fn from_access(
        backend_url: impl Into<String>,
        access: crate::company::composio::ComposioAccess,
        toolkits: Vec<String>,
    ) -> Self {
        Self {
            backend_url: backend_url.into(),
            credential: access.credential,
            mode: access.mode,
            catalog: Credential::None,
            toolkits,
            defaults: Default::default(),
            #[cfg(feature = "composio")]
            authorizations: Default::default(),
        }
    }

    /// Which host this company's Composio calls go to.
    pub fn mode(&self) -> ComposioMode {
        self.mode
    }

    /// The non-secret endpoint the calls actually reach — Composio's own API
    /// host under BYOK, the managed backend otherwise.
    ///
    /// The console reports this rather than [`Self::backend_url`] so that
    /// switching to BYOK visibly changes where the traffic goes; a status line
    /// still naming `api.tinyhumans.ai` after the switch would read as though
    /// nothing had happened.
    pub fn endpoint(&self) -> &str {
        match self.mode {
            ComposioMode::Byok => DIRECT_BASE_URL,
            ComposioMode::Managed => &self.backend_url,
        }
    }

    /// The same config with this company's per-toolkit connection pins attached
    /// (issue #820).
    ///
    /// A builder rather than a fourth parameter on [`Self::new`]: every existing
    /// call site means "no pins", and the honest way to say that is to not say
    /// it.
    pub fn with_defaults(mut self, defaults: crate::company::composio::ComposioDefaults) -> Self {
        self.defaults = defaults;
        self
    }

    /// The connection id this company pinned for `toolkit`, if any.
    ///
    /// `toolkit` is matched as [`slug_toolkit`] produces it — lowercased — which
    /// is what [`crate::company::composio::set_default`] normalizes to on the way
    /// in.
    pub fn default_connection(&self, toolkit: &str) -> Option<&str> {
        self.defaults.get(toolkit).map(String::as_str)
    }

    /// Resolve a per-tenant Composio config, or `None` (fail closed) when no
    /// credential can be obtained at all.
    ///
    /// A company in **BYOK** mode short-circuits everything below: its own
    /// Composio API key is the credential, and if it is missing this yields
    /// `None` rather than falling back to a managed tier. An operator who asked
    /// to act through their own Composio account must never silently act
    /// through the platform's.
    ///
    /// Managed precedence: the company's **own Composio** token under [`TINYHUMANS_KEY_KEY`] wins
    /// — a company that pasted one keeps it even on the hosted platform. Failing
    /// that, the shared brokered-credential seam
    /// [`company_key::resolve`](crate::company::company_key::resolve) answers:
    /// the company's own TinyHumans key, else this instance's platform identity.
    /// A company that pasted nothing borrows no one else's identity — it presents
    /// its own key if it has one, otherwise the identity of the instance it runs
    /// in, which the backend resolves to that instance's owner.
    ///
    /// The URL resolves from the tenant API base [`TINYHUMANS_API_URL_ENV`],
    /// then [`DEFAULT_BACKEND_URL`] — see [`backend_url_or_default`]. The
    /// explicit per-surface override (`OPENCOMPANY_COMPOSIO_BACKEND_URL`) was
    /// removed in phase 6a (issue #2306). `toolkits` is the manifest allowlist,
    /// threaded through unchanged.
    ///
    /// A secret-store read error yields `None` — **fail closed, no tools this
    /// cycle** — rather than falling through to the instance identity. This is
    /// the roster path, so it must not bubble and brick a build; but an *unknown*
    /// credential must no more mean a borrowed identity than an absent one does.
    /// A company whose store hiccups loses its Composio tools for a cycle and
    /// gets them back on the next; it never quietly acts as somebody else. See
    /// [`company_key::resolve`](crate::company::company_key::resolve).
    pub async fn resolve(
        company: &CompanyId,
        secrets: &dyn SecretStore,
        toolkits: Vec<String>,
        api_url_env: Option<String>,
        token_source: Option<Arc<TinyhumansTokenSource>>,
    ) -> Option<Self> {
        // Cloned because both resolutions below want it: the access one, and the
        // curated-catalog one under BYOK.
        let catalog_source = token_source.clone();
        let access = match resolve_access(company, secrets, token_source).await {
            Ok(access) => access,
            Err(err) => {
                tracing::warn!(
                    company = %company,
                    error = %err,
                    "[composio] could not read this company's credential; withholding tools \
                     for this cycle rather than presenting another identity"
                );
                return None;
            }
        };
        let mode = access.mode;
        match access.credential {
            Credential::None => None,
            _ => {
                // Which account the company means, per toolkit (issue #820).
                // Read here rather than per call so it lands in the fingerprint
                // below: changing the pin then rebuilds the roster on the next
                // turn, the same way a rotated token does, and no tool holds a
                // stale answer. A store hiccup on *this* read means "no
                // preference" — degrading to Composio's own resolution is the
                // behaviour that existed before the pin did, so it cannot
                // reroute anything.
                let defaults = crate::company::composio::load_defaults(company, secrets)
                    .await
                    .unwrap_or_default();
                // Under BYOK, resolve the managed chain *as well* — not to act
                // through, only to ask OpenHuman which providers to offer. A
                // failure here is not a failure of the config: the company's own
                // Composio key is what its agents present, and an unavailable
                // curated list degrades the catalog, never the credential.
                let catalog = if mode.is_byok() {
                    crate::company::composio::resolve_credential(company, secrets, catalog_source)
                        .await
                        .unwrap_or(Credential::None)
                } else {
                    Credential::None
                };
                Some(
                    Self::from_access(backend_url_or_default(api_url_env), access, toolkits)
                        .with_catalog_credential(catalog)
                        .with_defaults(defaults),
                )
            }
        }
    }

    /// The credential this config presents. Status and fingerprinting only —
    /// callers on the request path want [`Self::current_token`].
    pub fn credential(&self) -> &Credential {
        &self.credential
    }

    /// The bearer to present on **this** Composio call.
    ///
    /// Resolved per call so a platform token the cluster rotated in place is
    /// picked up without rebuilding the roster. `None` would mean no credential
    /// at all, which [`Self::resolve`] already rules out; the tools refuse the
    /// call rather than dialling the backend unauthenticated.
    pub async fn current_token(&self) -> crate::Result<Option<String>> {
        self.credential.current().await
    }

    /// The bearer for the **curated toolkit list**, or `None` when no managed
    /// tier resolved.
    ///
    /// Only ever presented to the OpenHuman backend, and only for a non-secret
    /// list. Never to Composio, and never in place of
    /// [`Self::current_token`] — the two authenticate different hosts, and
    /// confusing them is the failure [`from_access`](Self::from_access) exists
    /// to make unrepresentable one level up.
    pub async fn catalog_token(&self) -> crate::Result<Option<String>> {
        self.catalog.current().await
    }

    /// A stable, credential-safe fingerprint of the resolved config, folded into
    /// the harness roster fingerprint so a console token set/rotate (or a
    /// toolkit-allowlist change) rebuilds the roster on the next turn without a
    /// restart.
    ///
    /// The credential contributes its **identity**, not its bytes: a projected
    /// platform token rotates every few minutes, and hashing the value would
    /// rebuild the whole roster on that schedule. A pasted per-company token —
    /// and the company's own TinyHumans key — does contribute its value, since
    /// an admin changing either is a real identity change and must reach the
    /// agents on the next cycle.
    ///
    /// **Internal only — never log, serialize, or journal this.** Because it is
    /// value-derived over a live credential, anyone who can read it can confirm
    /// a guessed key against it: cheap to check, expensive to discover has been
    /// leaking. It is compared for equality inside [`HarnessPool`] and goes
    /// nowhere else. If a rebuild needs explaining, say that the identity
    /// changed — not what it hashes to.
    pub fn fingerprint(config: &Option<TenantComposio>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        match config {
            None => 0u8.hash(&mut hasher),
            Some(c) => {
                1u8.hash(&mut hasher);
                c.backend_url.hash(&mut hasher);
                // Switching between managed and BYOK changes which Composio
                // account the agents act through, so it has to rebuild the
                // roster on the next turn exactly as a rotated token does — and
                // it can happen without the credential's *bytes* changing at
                // all, when a company clears a BYOK key it never used.
                c.mode.hash(&mut hasher);
                c.credential.hash_identity(&mut hasher);
                // Under BYOK this is a *second* live credential — the
                // managed-chain bearer `list_toolkits` fetches OpenHuman's
                // curated catalog with, captured at resolve time and re-read
                // per call through `catalog_token`. Missing it here means
                // rotating the company's TinyHumans key while BYOK is active
                // moves neither `mode` nor `credential`, so the roster keeps
                // presenting the stale bearer: the curated fetch then fails on
                // it and `LiveClient::list_toolkits` silently widens the
                // provider grid to the account's own Composio directory,
                // staying that way until some unrelated change happens to
                // rebuild the roster. `Credential::None` under managed hashes
                // to a fixed tag, so this is a no-op there.
                c.catalog.hash_identity(&mut hasher);
                c.toolkits.hash(&mut hasher);
                // The pins are part of what the tools do, so a console change
                // to one has to reach the agents the same cycle a token change
                // does (issue #820). Safe to hash by value: a connection id is
                // not a credential.
                c.defaults.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

/// Whether a toolkit is admitted by an allowlist. An **empty** allowlist defers
/// to the backend's server-enforced allowlist (open mode) and admits every
/// toolkit; a non-empty allowlist admits only its members (case-insensitive).
///
/// Every call site is inside the `composio`-gated [`live`] module, so this is
/// genuinely dead in an `openhuman`-without-`composio` build and is gated to
/// match. A blanket `#[allow(dead_code)]` would say the same thing to the
/// compiler while also hiding the day a real call site disappears.
#[cfg(feature = "composio")]
fn toolkit_allowed(allowlist: &[String], toolkit: &str) -> bool {
    allowlist.is_empty()
        || allowlist
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(toolkit))
}

/// The toolkit a Composio action slug belongs to: the segment before the first
/// `_`, lowercased (`GMAIL_SEND_EMAIL` → `gmail`). Empty slug → empty string.
///
/// Gated for the same reason as [`toolkit_allowed`]: both call sites live in
/// the `composio`-gated [`live`] module.
#[cfg(feature = "composio")]
fn slug_toolkit(slug: &str) -> String {
    slug.split('_').next().unwrap_or("").to_ascii_lowercase()
}

/// One connected Composio account, projected for the console (issue #404).
///
/// Composio models a connection as an **account**, not as a boolean: a company
/// can hold two Gmail connections, and telling them apart is the entire point of
/// a detail view. [`list_connection_states`] deliberately folds this down to one
/// `(toolkit, connected)` pair per toolkit for the tile grid and the
/// reconciliation probe, both of which only ever ask "is this provider wired".
/// Everything that needs to *manage* a connection reads these rows instead.
///
/// **Non-secret projection.** Composio returns no token material on this route,
/// and nothing here is derived from the tenant bearer. The [`id`](Self::id) is a
/// Composio-side handle, not a credential: it is the argument
/// [`delete_connection`] takes, and it is useless without the bearer that scopes
/// it to this company.
///
/// Always compiled, so the console DTO that mirrors it (`ops::composio`) can be
/// defined in a build without the `composio` feature — only the functions that
/// produce these rows are gated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposioConnectionRow {
    /// Composio's connection id — what [`delete_connection`] revokes.
    pub id: String,
    /// Toolkit slug, normalized (`gmail`, `googlecalendar`).
    pub toolkit: String,
    /// Composio's raw status string (`ACTIVE`, `INITIATED`, `EXPIRED`, …).
    ///
    /// Forwarded verbatim rather than reduced to [`connected`](Self::connected),
    /// because "connected: false" reads as *not set up* while an expired
    /// connection was set up and needs re-authorizing — a different sentence and
    /// a different button. The console maps the vocabulary; the host does not
    /// pretend to know every value Composio may add.
    pub status: String,
    /// Whether this connection is usable — `ACTIVE` or `CONNECTED`,
    /// case-insensitively, matching the vendored client's own `is_active`.
    pub connected: bool,
    /// When Composio recorded the connection, ISO-8601, when it says.
    pub created_at: Option<String>,
    /// The account label this connection acts as, when the provider published
    /// one: the account email, else a workspace/team name, else a handle.
    ///
    /// Derived here rather than in the console so the precedence is stated once
    /// and tested once — it mirrors OpenHuman's `deriveConnectionLabel`, which
    /// is the experience this issue ports. `None` is honest: plenty of toolkits
    /// publish no identity at all, and inventing one from the slug would render
    /// as a fact the operator cannot check.
    pub account: Option<String>,
}

/// Why a disconnect did not happen.
///
/// Two variants because they are two different sentences to an operator, and —
/// caught by running the route rather than by a test — two different HTTP
/// statuses. Collapsing both into one error type reported a refused id as
/// `502 Bad Gateway`: "the provider is down", about a call that was never made,
/// for an id the guard rejected locally. The caller cannot re-derive the
/// distinction from an error string, so the type carries it.
#[derive(Debug)]
pub enum DisconnectError {
    /// The id names nothing this company can see, so there is nothing to
    /// revoke. A client mistake, not an outage.
    NotFound(String),
    /// The call reached Composio, and Composio failed or declined it. Already
    /// scrubbed of the tenant bearer.
    Upstream(anyhow::Error),
}

impl std::fmt::Display for DisconnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(message) => f.write_str(message),
            Self::Upstream(err) => write!(f, "{err}"),
        }
    }
}

#[cfg(feature = "composio")]
pub use live::{
    ComposioMetering, authorize_connect_url, composio_tools, delete_connection,
    list_catalog_toolkits, list_connection_states, list_connections_detailed,
    set_default_connection,
};

#[cfg(feature = "composio")]
mod live {
    use super::*;

    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::{Value, json};

    use crate::company::composio::CatalogEntry;
    use crate::harness::composio_catalog as catalog;
    use crate::harness::mcp_probe::{redact, scrub};
    use crate::metering::record_oauth_call;
    use crate::ports::UsageMeter;
    use crate::ports::now_millis;

    use oh::integrations::IntegrationClient;
    use oh::integrations::composio::ComposioClient;
    use oh::integrations::composio::types::{
        ComposioAuthorizeResponse, ComposioConnectionsResponse, ComposioDeleteResponse,
        ComposioExecuteResponse, ComposioToolkitsResponse, ComposioToolsResponse,
    };
    use openhuman_core as oh;
    use tinytools::{PermissionLevel, Tool, ToolResult};

    use crate::harness::built_in::composio_direct::DirectComposio;

    /// What `composio_execute` needs to meter a call it just made: the company
    /// the sample belongs to, the agent that made it, and the meter to write to.
    ///
    /// Bundled because these three travel together and are individually
    /// meaningless — and because [`composio_tools`] would otherwise grow four
    /// positional parameters at a call site that already has many.
    #[derive(Clone)]
    pub struct ComposioMetering {
        /// The company the sample is scoped to.
        pub company: CompanyId,
        /// The agent whose turn made the call.
        pub agent: String,
        /// The usage meter. `None` leaves metering off entirely (the harness
        /// wires no meter in some embeddings) — the tools still work.
        pub meter: Option<Arc<dyn UsageMeter>>,
    }

    /// Build the five per-tenant Composio tools over the tenant's credential.
    ///
    /// Each tool holds the shared [`TenantComposio`] and the toolkit allowlist,
    /// and builds its [`ComposioClient`] **when it runs** via [`live_call`] — the
    /// bearer is resolved then, not now, so a platform token that rotated since
    /// the roster was built still authenticates. The read tools are `ReadOnly`;
    /// the `authorize` / `execute` tools are `Execute` and additionally park for
    /// operator approval through the harness [`ApprovalPolicy`](crate::harness::policy).
    ///
    /// `metering` lets `composio_execute` record a
    /// [`SampleKind::OauthCall`](crate::ports::usage::SampleKind) sample per
    /// call it completes, which is what puts numbers in the Usage view's
    /// calls-by-provider chart (issue #152).
    ///
    /// Gated on the `composio` feature; the default/`openhuman` build never
    /// compiles this.
    pub fn composio_tools(
        config: &TenantComposio,
        metering: ComposioMetering,
    ) -> Vec<Box<dyn Tool>> {
        let config = Arc::new(config.clone());
        let toolkits = Arc::new(config.toolkits.clone());
        vec![
            Box::new(ComposioListToolkitsTool {
                config: Arc::clone(&config),
                toolkits: Arc::clone(&toolkits),
            }),
            Box::new(ComposioListConnectionsTool {
                config: Arc::clone(&config),
                toolkits: Arc::clone(&toolkits),
            }),
            Box::new(ComposioListToolsTool {
                config: Arc::clone(&config),
                toolkits: Arc::clone(&toolkits),
            }),
            Box::new(ComposioAuthorizeTool {
                config: Arc::clone(&config),
                toolkits: Arc::clone(&toolkits),
                company: metering.company.clone(),
            }),
            Box::new(ComposioExecuteTool {
                config,
                toolkits,
                metering,
            }),
        ]
    }

    /// A client for one call plus the known-secret vector that call's output must
    /// be scrubbed against.
    ///
    /// Built per call on purpose. The Config-free seam
    /// `IntegrationClient::new(backend_url, auth_token)` takes the credential
    /// directly (no OpenHuman global `Config`), and the bearer it is handed is the
    /// ONLY isolation lever — see the module docs — so it must be the value the
    /// credential yields *now*. A per-call client costs one HTTP-client
    /// construction on a path that is already a network round-trip; capturing the
    /// token once instead would leave a hosted tenant presenting a bearer the
    /// cluster rotated away from minutes ago.
    ///
    /// The scrub vector carries exactly the token that went out, so it cannot
    /// survive into agent-visible output even if the backend reflects it.
    async fn live_call(config: &TenantComposio) -> Result<(LiveClient, Vec<String>)> {
        let secret = config
            .current_token()
            .await
            .map_err(|e| anyhow::anyhow!("resolving this company's Composio credential: {e}"))?
            .ok_or_else(|| anyhow::anyhow!("no Composio credential is configured"))?;
        let mut secrets = vec![secret.clone()];
        crate::harness::backend_transport::ensure_installed();
        let client = match config.mode() {
            ComposioMode::Managed => LiveClient::Managed(ComposioClient::new(Arc::new(
                IntegrationClient::new(config.backend_url.clone(), secret.clone()),
            ))),
            ComposioMode::Byok => {
                // The curated-list client, when a managed tier resolves. Built
                // here rather than inside `list_toolkits` so its bearer joins
                // the scrub vector: it is a second live credential on this call,
                // and it must no more survive into agent-visible output than the
                // Composio key does.
                let catalog = match config.catalog_token().await {
                    Ok(Some(token)) => {
                        secrets.push(token.clone());
                        Some(ComposioClient::new(Arc::new(IntegrationClient::new(
                            config.backend_url.clone(),
                            token,
                        ))))
                    }
                    Ok(None) => None,
                    Err(err) => {
                        // Degrades the catalog, never the call: the company's
                        // own key is unaffected by this.
                        tracing::warn!(
                            error = %err,
                            "[composio-byok] could not resolve the managed credential for the \
                             curated toolkit list; falling back to this account's own catalogue"
                        );
                        None
                    }
                };
                LiveClient::Byok {
                    direct: DirectComposio::new(&secret),
                    catalog,
                }
            }
        };
        Ok((client, secrets))
    }

    /// The two routes a Composio call can take, behind one surface.
    ///
    /// Every caller in this module speaks these six operations and none of them
    /// branches on the route: a BYOK answer arrives in the same envelope a
    /// managed one does, so the allowlist filtering, the scrubbing, the
    /// rendering and the metering downstream of here are shared rather than
    /// duplicated. The branch lives once, in the impl below, which is also the
    /// only place that has to state what BYOK cannot do.
    enum LiveClient {
        /// Proxied through the OpenHuman backend — the default route.
        Managed(ComposioClient),
        /// Straight to the company's own Composio account.
        Byok {
            /// The company's own Composio account — every call but the toolkit
            /// catalogue.
            direct: DirectComposio,
            /// OpenHuman's curated toolkit list, when a managed tier resolved to
            /// fetch it with. `None` on a host with no TinyHumans identity at
            /// all, where the company's own directory is the only list there is.
            catalog: Option<ComposioClient>,
        },
    }

    impl LiveClient {
        /// The catalog of toolkits this company may connect.
        ///
        /// Managed: the backend's server-enforced allowlist. BYOK: whatever the
        /// company's own Composio account lists, every entry connectable —
        /// there is no gate between a company and its own account.
        async fn list_toolkits(&self) -> Result<ComposioToolkitsResponse> {
            match self {
                Self::Managed(client) => client.list_toolkits().await,
                // The curated list first, so a BYOK company browses the same
                // 123 providers a managed one does. Switching a company to its
                // own Composio account changes who it acts as; it should not
                // also change what the console lets it look at.
                Self::Byok {
                    catalog: Some(catalog),
                    direct,
                } => match catalog.list_toolkits().await {
                    Ok(resp) if !resp.toolkits.is_empty() || !resp.catalog.is_empty() => Ok(resp),
                    // An empty or failed curated list is not worth failing the
                    // catalogue over — the company's own directory is a real
                    // answer, just a longer and less curated one.
                    other => {
                        if let Err(ref err) = other {
                            tracing::warn!(
                                error = %format!("{err:#}"),
                                "[composio-byok] the curated toolkit list could not be read; \
                                 falling back to this account's own catalogue"
                            );
                        }
                        direct.list_toolkits().await
                    }
                },
                Self::Byok {
                    catalog: None,
                    direct,
                } => direct.list_toolkits().await,
            }
        }

        /// The connected accounts this company holds.
        async fn list_connections(&self) -> Result<ComposioConnectionsResponse> {
            match self {
                Self::Managed(client) => client.list_connections().await,
                Self::Byok { direct, .. } => direct.list_connections().await,
            }
        }

        /// The action schemas for `toolkits` (all of them when `None`),
        /// optionally narrowed server-side by `search` and `tags`.
        ///
        /// `search` is new (and `tags` newly honoured on the BYOK route). The
        /// tool surface has taken a search term all along and applied it
        /// **client-side**, over whatever survived the page budget — so a
        /// narrowing the caller asked for could not reach an action that was
        /// dropped before it ever arrived. Composio filters both server-side on
        /// `/tools`; `tinyhumansai/backend` already threads `tags` for the same
        /// reason.
        async fn list_tools(
            &self,
            toolkits: Option<&[String]>,
            tags: Option<&[String]>,
            search: Option<&str>,
        ) -> Result<(ComposioToolsResponse, bool)> {
            // The `bool` is whether the listing was **curated** — the BYOK route
            // asks Composio for featured actions only when nothing narrows the
            // call, and the renderer has to say so or it reports ~50 featured
            // rows as the toolkit's whole catalogue (codex on
            // tinyhumansai/opencompany#2153). The response type is vendored, so
            // the flag rides beside it rather than on it.
            let curated = matches!(self, Self::Byok { .. })
                && !search.is_some_and(|term| !term.trim().is_empty())
                && !tags.is_some_and(|tags| !tags.is_empty());
            match self {
                Self::Managed(client) => {
                    if search.is_some_and(|term| !term.trim().is_empty()) {
                        // The managed backend's own endpoint takes no search
                        // parameter, so the term stays a client-side filter
                        // there. Said out loud rather than dropped silently —
                        // that silence is what made the BYOK route's truncation
                        // so hard to see.
                        tracing::debug!(
                            "[composio] list_tools: managed route has no server-side search; \
                             the term is applied client-side"
                        );
                    }
                    client
                        .list_tools(toolkits, tags)
                        .await
                        .map(|resp| (resp, curated))
                }
                Self::Byok { direct, .. } => direct
                    .list_tools(toolkits.unwrap_or(&[]), search, tags)
                    .await
                    .map(|resp| (resp, curated)),
            }
        }

        /// Begin an OAuth handoff and return the hosted connect URL.
        async fn authorize(
            &self,
            toolkit: &str,
            extra: Option<Value>,
        ) -> Result<ComposioAuthorizeResponse> {
            match self {
                Self::Managed(client) => client.authorize(toolkit, extra).await,
                Self::Byok { direct, .. } => {
                    if extra.is_some() {
                        // The v3 link call takes no per-toolkit extras; a
                        // BYOK operator configures them on the auth config in
                        // their own Composio dashboard. Same answer OpenHuman's
                        // direct branch gives, and it is logged rather than
                        // failed so a toolkit that does not need them still
                        // connects.
                        tracing::warn!(
                            toolkit = %toolkit,
                            "[composio-byok] authorize: extra_params are not forwarded on the                              BYOK route — set them on the toolkit's auth config at app.composio.dev"
                        );
                    }
                    direct.authorize(toolkit).await
                }
            }
        }

        /// Run one action, optionally as a named connected account.
        async fn execute(
            &self,
            tool: &str,
            arguments: Option<Value>,
            connection_id: Option<&str>,
            metering: &ComposioMetering,
        ) -> Result<ComposioExecuteResponse> {
            match self {
                Self::Managed(client) => {
                    execute_managed(client, tool, arguments, connection_id, metering).await
                }
                Self::Byok { direct, .. } => direct.execute(tool, arguments, connection_id).await,
            }
        }

        /// Revoke one connected account — the backend's route under managed,
        /// Composio's own `DELETE /connected_accounts/{id}` under BYOK.
        async fn delete_connection(&self, connection_id: &str) -> Result<ComposioDeleteResponse> {
            match self {
                Self::Managed(client) => client.delete_connection(connection_id).await,
                Self::Byok { direct, .. } => direct.delete_connection(connection_id).await,
            }
        }
    }

    /// Identical normalized actions from one company agent share a backend key.
    async fn execute_managed(
        client: &ComposioClient,
        tool: &str,
        arguments: Option<Value>,
        connection_id: Option<&str>,
        metering: &ComposioMetering,
    ) -> Result<oh::integrations::composio::types::ComposioExecuteResponse> {
        use oh::security::egress::{EgressDescriptor, emit_external_transfer, enforce_egress};

        let egress = EgressDescriptor::composio(tool);
        enforce_egress(&egress)?;
        emit_external_transfer(egress);

        let arguments =
            oh::integrations::composio::execute_prepare::prepare_execute_arguments(tool, arguments)
                .map_err(anyhow::Error::msg)?;
        let mut body = json!({
            "tool": tool,
            "arguments": arguments,
        });
        if let Some(connection_id) = connection_id {
            body["connectionId"] = json!(connection_id);
        }
        let key = execute_idempotency_key(metering, &body)?;
        let http = managed_execute_client()?;

        let post =
            async |body: &Value| post_managed_execute(client.inner(), http, body, &key).await;

        let mut resp = post(&body).await?;
        if is_post_oauth_auth_error(&resp) {
            tracing::debug!(
                tool = %tool,
                "[composio] execute hit the post-OAuth readiness gap; retrying once"
            );
            tokio::time::sleep(POST_OAUTH_RETRY_DELAY).await;
            resp = post(&body).await?;
        }
        if !resp.successful
            && let Some(ref err) = resp.error
        {
            resp.error =
                Some(oh::integrations::composio::error_mapping::format_provider_error(tool, err));
        }
        Ok(resp)
    }

    fn execute_idempotency_key(metering: &ComposioMetering, body: &Value) -> Result<String> {
        use sha2::{Digest, Sha256};

        let mut identity = json!([&metering.company, &metering.agent, body]);
        identity.sort_all_objects();
        Ok(format!(
            "oc-composio-v1-{:x}",
            Sha256::digest(serde_json::to_vec(&identity)?)
        ))
    }

    /// The HTTP client every managed execute posts through.
    ///
    /// One client for the process, not one per call: a `reqwest::Client` owns
    /// a connection pool, and building one per execute means a fresh TCP and
    /// TLS handshake on every tool call, with a burst paying for as many as it
    /// makes. The vendored client is not used here only because it offers no
    /// way to set the idempotency header; the pooling it provides is not
    /// something to give up along with it.
    fn managed_execute_client() -> Result<&'static reqwest::Client> {
        static CLIENT: std::sync::OnceLock<std::result::Result<reqwest::Client, String>> =
            std::sync::OnceLock::new();
        CLIENT
            .get_or_init(|| {
                oh::util::tls::tls_client_builder()
                    .http1_only()
                    .timeout(std::time::Duration::from_secs(60))
                    .connect_timeout(std::time::Duration::from_secs(15))
                    .default_headers(openhuman_tinyhumans::backend::product_identity_headers())
                    .build()
                    .map_err(|error| format!("{error}"))
            })
            .as_ref()
            .map_err(|error| anyhow::anyhow!("composio execute client: {error}"))
    }

    async fn post_managed_execute(
        client: &IntegrationClient,
        http: &reqwest::Client,
        body: &Value,
        key: &str,
    ) -> Result<ComposioExecuteResponse> {
        use openhuman_core::core::observability::report_error_or_expected;

        const PATH: &str = "/agent-integrations/composio/execute";
        let url = openhuman_core::util::url::join_url(&client.backend_url, PATH);
        let response = http
            .post(&url)
            .bearer_auth(&client.auth_token)
            .header("idempotency-key", key)
            .json(body)
            .send()
            .await
            .map_err(|error| {
                let error = anyhow::Error::new(error);
                report_error_or_expected(
                    &format!("{error:#}"),
                    "integrations",
                    "post",
                    &[("path", PATH), ("failure", "transport")],
                );
                error
            })?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await?;
            let parsed = serde_json::from_str::<Value>(&text).ok();
            let detail = parsed
                .as_ref()
                .and_then(|value| value.get("error"))
                .and_then(Value::as_str)
                .filter(|error| !error.trim().is_empty())
                .unwrap_or(&text);
            let detail = oh::util::truncate_at_byte_boundary(detail, 500);
            let message = if status == reqwest::StatusCode::UNAUTHORIZED {
                let message = format!(
                    "SESSION_EXPIRED: backend rejected session token on POST {PATH} \
                     (401 for {url}: {detail}) — sign in again to resume"
                );
                openhuman_core::core::bus::BUS.publish(
                    openhuman_core::core::events::DomainEvent::SessionExpired {
                        source: format!("integrations.POST:{PATH}"),
                        reason: tinyinference_core::sanitize::sanitize_api_error(&message),
                    },
                );
                message
            } else {
                format!("Backend returned {status} for POST {url}: {detail}")
            };
            report_error_or_expected(
                &message,
                "integrations",
                "post",
                &[("path", PATH), ("status", &status.as_u16().to_string())],
            );
            anyhow::bail!(message);
        }
        let envelope = response
            .json::<oh::integrations::types::BackendResponse<ComposioExecuteResponse>>()
            .await?;
        if !envelope.success {
            let message = envelope
                .error
                .unwrap_or_else(|| "unknown backend error".into());
            report_error_or_expected(
                &message,
                "integrations",
                "post",
                &[("path", PATH), ("failure", "envelope_error")],
            );
            anyhow::bail!("Backend error for POST {url}: {message}");
        }
        envelope
            .data
            .ok_or_else(|| anyhow::anyhow!("Backend returned success but no data for POST {url}"))
    }

    /// Composio's gateway string for the window between a connection reporting
    /// `ACTIVE` and its token being usable for actions. Matched
    /// case-insensitively as a substring, mirroring the vendored client.
    const POST_OAUTH_AUTH_ERROR: &str = "connection error, try to authenticate";

    /// How long to wait before the single post-OAuth retry — the vendored
    /// client's own delay.
    const POST_OAUTH_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(10);

    /// Whether a response is the post-OAuth readiness gap rather than a real
    /// refusal. Only the payload-level `successful:false` shape is eligible;
    /// transport errors have already propagated by this point.
    fn is_post_oauth_auth_error(
        resp: &oh::integrations::composio::types::ComposioExecuteResponse,
    ) -> bool {
        !resp.successful
            && resp
                .error
                .as_deref()
                .is_some_and(|err| err.to_ascii_lowercase().contains(POST_OAUTH_AUTH_ERROR))
    }

    /// Serialize a successful response to JSON, redact the tenant token out of
    /// it, and bound it to a *body* budget before it reaches the agent.
    /// Text-only output — the structured value is dropped so a credential the
    /// backend might reflect can never ride out in a JSON field.
    ///
    /// # Why not `scrub` (issue #410)
    ///
    /// This used to call [`scrub`], whose third pass caps its output at
    /// [`SCRUB_MAX_BYTES`](crate::harness::mcp_probe::SCRUB_MAX_BYTES) — 300
    /// bytes, the right size for the one-line MCP failure sentence it was built
    /// for and a catastrophe for a tool body. Every Composio result was cut to
    /// 300 bytes and terminated with a bare `…`: an action listing became the
    /// first action and half of its schema, and `composio_execute` returned 300
    /// bytes of whatever the provider actually said. Nothing in the result said
    /// it was a fragment, so the agent had no reason to ask differently and
    /// reissued the identical call until the repetition guard stopped the run.
    ///
    /// [`redact`] keeps the security half — the token replacement and the URL
    /// query strip — verbatim and unconditional; only the length decision moves
    /// here, where it can be sized for a body and describe its own cut.
    fn scrubbed_ok(value: Value, secrets: &[String]) -> ToolResult {
        // Project BEFORE serialising, and serialise before redacting.
        //
        // Order is the whole of it. `redact` strips URL query strings and
        // rewrites secret substrings inside the serialised text, which leaves a
        // string that is no longer valid JSON — so a projection attempted after
        // it (the first cut of this) could never parse the payload and declined
        // on every real response, silently, while the byte cut carried on
        // dropping whole records. Projecting the structured `Value` here means
        // the transform sees JSON, and `redact` still sees every byte that ends
        // up in front of the model.
        let value = catalog::project_records_value(value);
        let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
        // No `bound_body` here any more (issue #6014).
        //
        // It bounded the payload to 12 KiB **inside the tool**, chosen so that
        // "the harness's own anonymous cut never fires first" — a good trade
        // when the only thing downstream was a byte cut that said nothing.
        //
        // It is the wrong trade now. The same 12 KiB also fired ahead of the
        // per-result artifact store (so an oversized Composio result was
        // discarded rather than written to disk and pointed at) and ahead of the
        // task-aware extractor (whose threshold is 4000 tokens, which a
        // pre-bounded body can never reach). Bounding first made this the one
        // tool family whose large results could be handled by nothing but
        // truncation — measured on a live GitHub call as 3 of 30 records
        // surviving, and the agent correctly reporting 3.
        //
        // Handing the full payload downstream puts it back on the same ladder
        // every other tool's output takes: extract against the task, else
        // persist and hand back a path, else cut at the budget. Each of those
        // says what it did, which was the property the pre-bound was protecting
        // and is now protected by the mechanisms themselves.
        ToolResult::success(redact(&text, secrets))
    }

    /// A scrubbed error result — the tenant token is stripped from any error
    /// body (mirrors [`crate::harness::mcp`]'s failure handling).
    ///
    /// `{err:#}` renders the whole cause chain, not just its outermost layer.
    /// The managed client's errors are single-level so the two used to read
    /// alike; the BYOK client wraps its calls in `.context(…)`, and with plain
    /// `{err}` an agent was handed the bare call name — "Composio v3 /toolkits"
    /// — with the reason it failed silently discarded one frame below.
    fn scrubbed_err(context: &str, err: &anyhow::Error, secrets: &[String]) -> ToolResult {
        ToolResult::error(scrub(&format!("{context}: {err:#}"), secrets))
    }

    /// Pull a required, non-empty string argument.
    fn required_string_arg(args: &Value, key: &str) -> Result<String> {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("missing required `{key}` string argument"))
    }

    /// Begin an OAuth handoff for `toolkit` and return the Composio-hosted
    /// connect URL the operator opens in a browser. Backs the console's
    /// `POST …/composio/authorize` route (the same building block the
    /// `composio_authorize` agent tool wraps).
    ///
    /// Composio runs the OAuth itself — there is **no** local callback route.
    /// The console opens the returned URL in a new tab and polls
    /// [`list_connection_states`] until the toolkit reports connected.
    ///
    /// The tenant allowlist is enforced **before** any network call — a toolkit
    /// the company is not permitted to connect never reaches the backend. Any
    /// upstream error is scrubbed of the tenant bearer before it bubbles.
    pub async fn authorize_connect_url(config: &TenantComposio, toolkit: &str) -> Result<String> {
        let toolkit = toolkit.trim();
        if toolkit.is_empty() {
            anyhow::bail!("composio authorize: toolkit must not be empty");
        }
        if !toolkit_allowed(&config.toolkits, toolkit) {
            anyhow::bail!("toolkit `{toolkit}` is not in this company's Composio allowlist");
        }
        tracing::debug!(toolkit = %toolkit, "[composio] ops authorize");
        let (client, secrets) = live_call(config).await?;
        match client.authorize(toolkit, None).await {
            Ok(resp) => Ok(resp.connect_url),
            Err(err) => Err(anyhow::anyhow!(scrub(&format!("{err:#}"), &secrets))),
        }
    }

    /// Every connected account this company holds, one row per **connection**
    /// rather than per toolkit (issue #404). Backs the console's provider detail
    /// view and the disconnect it offers.
    ///
    /// Filtered to the tenant allowlist exactly as [`list_connection_states`] is
    /// — the two share this call, so a connection outside the company's grant is
    /// invisible to both, and cannot be reached by guessing its id either (see
    /// [`delete_connection`]). Sorted by `(toolkit, id)` for a stable render
    /// order. Any upstream error is scrubbed of the tenant bearer before it
    /// bubbles.
    pub async fn list_connections_detailed(
        config: &TenantComposio,
    ) -> Result<Vec<ComposioConnectionRow>> {
        tracing::debug!(allowlist = ?config.toolkits, "[composio] ops list_connections_detailed");
        let (client, secrets) = live_call(config).await?;
        let resp = match client.list_connections().await {
            Ok(resp) => resp,
            Err(err) => return Err(anyhow::anyhow!(scrub(&format!("{err:#}"), &secrets))),
        };
        let mut rows: Vec<ComposioConnectionRow> = resp
            .connections
            .into_iter()
            .filter_map(|conn| {
                let toolkit = conn.normalized_toolkit();
                if !toolkit_allowed(&config.toolkits, &toolkit) {
                    return None;
                }
                let connected = conn.is_active();
                Some(ComposioConnectionRow {
                    id: conn.id,
                    toolkit,
                    status: conn.status,
                    connected,
                    created_at: conn.created_at,
                    // Same precedence as OpenHuman's `deriveConnectionLabel`:
                    // email, then workspace, then handle. A field present but
                    // blank is treated as absent — a whitespace label would
                    // render as an empty parenthetical, which reads as a bug.
                    account: [conn.account_email, conn.workspace, conn.username]
                        .into_iter()
                        .flatten()
                        .map(|v| v.trim().to_string())
                        .find(|v| !v.is_empty()),
                })
            })
            .collect();
        rows.sort_by(|a, b| a.toolkit.cmp(&b.toolkit).then_with(|| a.id.cmp(&b.id)));
        Ok(rows)
    }

    /// The per-toolkit connected state the console renders as provider tiles: one
    /// `(toolkit, connected)` pair per toolkit that has at least one connection,
    /// with `connected == true` when **any** connection for that toolkit is
    /// active. Backs the console's `GET …/composio/connections` route and the
    /// reconciliation probe in `ops::connections_read`.
    ///
    /// A projection over [`list_connections_detailed`] rather than a second call
    /// shape: one network round-trip, one allowlist filter, one scrub. The fold
    /// is what the tile grid wants and all the probe can use — both ask only
    /// "is this provider wired" — but it is lossy, so anything that manages a
    /// connection reads the rows instead.
    pub async fn list_connection_states(config: &TenantComposio) -> Result<Vec<(String, bool)>> {
        let rows = list_connections_detailed(config).await?;
        let mut states: std::collections::BTreeMap<String, bool> =
            std::collections::BTreeMap::new();
        for row in rows {
            states
                .entry(row.toolkit)
                .and_modify(|c| *c = *c || row.connected)
                .or_insert(row.connected);
        }
        Ok(states.into_iter().collect())
    }

    /// Revoke one connected account by its Composio connection id (issue #404).
    /// Backs the console's `DELETE …/composio/connections/{id}`.
    ///
    /// **The id is checked against this tenant's own filtered list first**, and
    /// an unknown one fails before any delete is attempted. Two reasons, and the
    /// second is the load-bearing one:
    ///
    /// * The backend scopes a delete to the caller's bearer, so another
    ///   company's connection was never reachable — but a connection belonging
    ///   to *this* company under a toolkit its manifest does **not** allow is
    ///   reachable by that bearer, and is deliberately invisible to every read
    ///   here. Letting an id delete what no read will show would make the
    ///   allowlist a display filter rather than a boundary.
    /// * It turns "already disconnected" into a clear answer instead of whatever
    ///   the upstream returns for a stale id.
    ///
    /// Returns `Ok(())` on a completed revoke. A refusal (`deleted: false`) is an
    /// error rather than a silent success: the console's next line tells the
    /// operator the account is gone, and it must not say so on the strength of a
    /// call the backend declined.
    pub async fn delete_connection(
        config: &TenantComposio,
        connection_id: &str,
    ) -> std::result::Result<(), DisconnectError> {
        let connection_id = connection_id.trim();
        if connection_id.is_empty() {
            return Err(DisconnectError::NotFound(
                "a connection id is required".to_string(),
            ));
        }
        let known = list_connections_detailed(config)
            .await
            .map_err(DisconnectError::Upstream)?;
        if !known.iter().any(|row| row.id == connection_id) {
            return Err(DisconnectError::NotFound(
                "no such connection for this company".to_string(),
            ));
        }
        tracing::debug!(connection_id = %connection_id, "[composio] ops delete_connection");
        let (client, secrets) = live_call(config).await.map_err(DisconnectError::Upstream)?;
        match client.delete_connection(connection_id).await {
            Ok(resp) if resp.deleted => Ok(()),
            Ok(_) => Err(DisconnectError::Upstream(anyhow::anyhow!(
                "Composio declined to delete the connection"
            ))),
            Err(err) => Err(DisconnectError::Upstream(anyhow::anyhow!(scrub(
                &format!("{err:#}"),
                &secrets
            )))),
        }
    }

    /// Pin the toolkit of `connection_id` to that account, so every
    /// `composio_execute` for it acts as that account (issue #820). Backs the
    /// console's `PUT …/composio/connections/{id}/default`.
    ///
    /// **The id is checked against this tenant's own filtered list first**, for
    /// the same two reasons [`delete_connection`] checks it, and one more that
    /// only applies here: an unchecked id would be stored, and a stored id that
    /// names nothing is not an error the operator sees at write time — it is a
    /// toolkit that stops working at the next agent turn, for a reason nothing
    /// on screen explains. Failing the write is the only place the mistake is
    /// still legible.
    ///
    /// Returns the toolkit that was pinned, which is the one the console needs
    /// to re-render and never has to guess at.
    pub async fn set_default_connection(
        config: &TenantComposio,
        company: &CompanyId,
        secrets: &dyn SecretStore,
        connection_id: &str,
    ) -> std::result::Result<String, DisconnectError> {
        let connection_id = connection_id.trim();
        if connection_id.is_empty() {
            return Err(DisconnectError::NotFound(
                "a connection id is required".to_string(),
            ));
        }
        let known = list_connections_detailed(config)
            .await
            .map_err(DisconnectError::Upstream)?;
        let Some(row) = known.iter().find(|row| row.id == connection_id) else {
            return Err(DisconnectError::NotFound(
                "no such connection for this company".to_string(),
            ));
        };
        // An account that is not usable is refused rather than stored: pinning
        // an EXPIRED connection would route every send for the toolkit to an
        // account that cannot send, which is worse than the unpinned behaviour
        // it replaces. Re-authorize it first, then pin it.
        if !row.connected {
            return Err(DisconnectError::NotFound(format!(
                "that account is `{}`, not connected — re-authorize it before making it the default",
                row.status
            )));
        }
        let toolkit = row.toolkit.clone();
        tracing::debug!(connection_id = %connection_id, toolkit = %toolkit, "[composio] ops set_default_connection");
        crate::company::composio::set_default(company, secrets, &toolkit, connection_id)
            .await
            .map_err(|err| DisconnectError::Upstream(anyhow::anyhow!("{err}")))?;
        Ok(toolkit)
    }

    /// The backend's live Composio toolkit catalog — every slug it will let
    /// this tenant connect. Backs the console's open-mode provider list
    /// (issue #397).
    ///
    /// This is the same `GET /agent-integrations/composio/toolkits` call the
    /// `composio_list_toolkits` agent tool makes and the same one OpenHuman's
    /// Skills grid drives off, so the console offers what the backend actually
    /// permits instead of a list maintained by hand here.
    ///
    /// Deliberately **not** filtered by the tenant allowlist, unlike
    /// [`list_connection_states`]: the only caller is the open-mode path, where
    /// the allowlist is empty by definition. A company with a non-empty
    /// allowlist is offered its own list verbatim and never reaches this
    /// function — the catalog must not be able to widen a manifest that
    /// deliberately narrowed.
    ///
    /// Entries are the catalog's **connectable** slugs — `enabled == true`,
    /// mirroring the vendored runtime's own `connectable_toolkit_slugs`, since
    /// advertising a provider the backend gate will refuse only invites a failed
    /// sign-in. Backends predating the dynamic catalog send no `catalog[]` at
    /// all; their plain slug allowlist is used instead. Slugs are trimmed,
    /// lowercased, de-duplicated and sorted for a stable render order. Any
    /// upstream error is scrubbed of the tenant bearer before it bubbles.
    ///
    /// ## Why this returns entries rather than slugs (issue #600)
    ///
    /// It used to return `Vec<String>`, and that one `.map(|e| e.slug)` was the
    /// whole of #600. The backend publishes `name`, `logo`, `description` and
    /// `categories` on every entry and states plainly that it assembles them so
    /// the frontend can read them straight from there — and this function threw
    /// five of the six fields away one layer before the console, which then had
    /// nothing to group by, nothing to brand with, and nothing to search but the
    /// slug. A hundred-and-twenty-three-item flat list was the honest rendering
    /// of what it was handed.
    ///
    /// Nothing about *admission* changed. The agent-side gate is
    /// [`toolkit_allowed`], which takes slugs and never consulted this function;
    /// the console's slug list is still derived from these entries. This widens
    /// what is *described*, not what is permitted.
    pub async fn list_catalog_toolkits(config: &TenantComposio) -> Result<Vec<CatalogEntry>> {
        tracing::debug!("[composio] ops list_catalog_toolkits");
        let (client, secrets) = live_call(config).await?;
        let resp = match client.list_toolkits().await {
            Ok(resp) => resp,
            Err(err) => return Err(anyhow::anyhow!(scrub(&format!("{err:#}"), &secrets))),
        };
        let normalize = |slug: &str| slug.trim().to_ascii_lowercase();
        // A `BTreeMap` keyed on the normalized slug keeps the de-duplication and
        // the stable sort the slug set gave us, while carrying the metadata that
        // is the entire point of the widening.
        //
        // `or_insert_with`, NOT `collect()` into the map: collecting keeps the
        // LAST value for a repeated key, and a duplicate catalog entry is
        // typically the degenerate one — the mock's `Gmail (dup)` carries no
        // description, and collecting would let it silently blank the real
        // Gmail's. First entry wins, which is also what the `BTreeSet<String>`
        // this replaced effectively did.
        let mut entries: std::collections::BTreeMap<String, CatalogEntry> =
            std::collections::BTreeMap::new();
        for entry in resp.catalog.iter().filter(|e| e.enabled.unwrap_or(false)) {
            let slug = normalize(&entry.slug);
            if slug.is_empty() {
                continue;
            }
            entries.entry(slug.clone()).or_insert_with(|| CatalogEntry {
                slug,
                name: entry.name.trim().to_string(),
                description: entry
                    .description
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string(),
                logo: entry
                    .logo
                    .as_deref()
                    .map(str::trim)
                    .filter(|logo| !logo.is_empty())
                    .map(str::to_string),
                categories: entry
                    .categories
                    .iter()
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
                    .collect(),
            });
        }
        if entries.is_empty() {
            // A backend predating the dynamic catalog. Slugs are all it has, so
            // slugs are all the console gets — rendered with local typography
            // rather than dropped.
            entries = resp
                .toolkits
                .iter()
                .map(|slug| normalize(slug))
                .filter(|slug| !slug.is_empty())
                .map(|slug| (slug.clone(), CatalogEntry::from_slug(slug)))
                .collect();
        }
        Ok(entries.into_values().collect())
    }

    // ── composio_list_toolkits ──────────────────────────────────────────

    struct ComposioListToolkitsTool {
        config: Arc<TenantComposio>,
        toolkits: Arc<Vec<String>>,
    }

    #[async_trait]
    impl Tool for ComposioListToolkitsTool {
        fn name(&self) -> &str {
            "composio_list_toolkits"
        }

        fn description(&self) -> &str {
            catalog::list_toolkits_description()
        }

        fn parameters_schema(&self) -> Value {
            catalog::list_toolkits_parameters_schema()
        }

        fn permission_level(&self) -> PermissionLevel {
            PermissionLevel::ReadOnly
        }

        async fn execute(&self, args: Value) -> Result<ToolResult> {
            let request = catalog::ToolkitListRequest::parse(&args);
            tracing::debug!(
                allowlist = ?self.toolkits,
                search = ?request.search,
                limit = request.limit,
                "[composio] list_toolkits"
            );
            let (client, secrets) = match live_call(&self.config).await {
                Ok(live) => live,
                Err(err) => {
                    return Ok(ToolResult::error(format!(
                        "composio_list_toolkits failed: {err}"
                    )));
                }
            };
            match client.list_toolkits().await {
                Ok(mut resp) => {
                    if !self.toolkits.is_empty() {
                        resp.toolkits
                            .retain(|slug| toolkit_allowed(&self.toolkits, slug));
                        resp.catalog
                            .retain(|entry| toolkit_allowed(&self.toolkits, &entry.slug));
                    }
                    // Issue #410: bounded, self-describing rendering rather than
                    // the whole catalogue as pretty JSON. Composio publishes
                    // several hundred toolkits, each with prose and a categories
                    // array, so this listing is the same silent-cut class as the
                    // action listing one level down.
                    let catalogued: Vec<catalog::CatalogToolkit> = resp
                        .catalog
                        .iter()
                        .map(|entry| catalog::CatalogToolkit {
                            slug: entry.slug.clone(),
                            name: entry.name.clone(),
                            description: entry.description.clone().unwrap_or_default(),
                            connected: entry.enabled,
                        })
                        .collect();
                    // Backends predating the dynamic catalogue send only the
                    // slug allowlist; render those slugs rather than nothing.
                    let toolkits: Vec<catalog::CatalogToolkit> = if catalogued.is_empty() {
                        resp.toolkits
                            .iter()
                            .map(|slug| catalog::CatalogToolkit {
                                slug: slug.clone(),
                                name: String::new(),
                                description: String::new(),
                                connected: None,
                            })
                            .collect()
                    } else {
                        catalogued
                    };
                    let rendered = catalog::render_toolkits(&toolkits, &request);
                    Ok(ToolResult::success(redact(&rendered, &secrets)))
                }
                Err(err) => Ok(scrubbed_err(
                    "composio_list_toolkits failed",
                    &err,
                    &secrets,
                )),
            }
        }
    }

    // ── composio_list_connections ───────────────────────────────────────

    struct ComposioListConnectionsTool {
        config: Arc<TenantComposio>,
        toolkits: Arc<Vec<String>>,
    }

    #[async_trait]
    impl Tool for ComposioListConnectionsTool {
        fn name(&self) -> &str {
            "composio_list_connections"
        }

        fn description(&self) -> &str {
            "List this company's connected Composio accounts (which Gmail / Slack / GitHub integrations are authorized). Read-only."
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })
        }

        fn permission_level(&self) -> PermissionLevel {
            PermissionLevel::ReadOnly
        }

        async fn execute(&self, _args: Value) -> Result<ToolResult> {
            tracing::debug!(
                allowlist = ?self.toolkits,
                "[composio] list_connections"
            );
            let (client, secrets) = match live_call(&self.config).await {
                Ok(live) => live,
                Err(err) => {
                    return Ok(ToolResult::error(format!(
                        "composio_list_connections failed: {err}"
                    )));
                }
            };
            match client.list_connections().await {
                Ok(mut resp) => {
                    if !self.toolkits.is_empty() {
                        resp.connections.retain(|conn| {
                            toolkit_allowed(&self.toolkits, &conn.normalized_toolkit())
                        });
                    }
                    Ok(scrubbed_ok(
                        serde_json::to_value(&resp).unwrap_or(Value::Null),
                        &secrets,
                    ))
                }
                Err(err) => Ok(scrubbed_err(
                    "composio_list_connections failed",
                    &err,
                    &secrets,
                )),
            }
        }
    }

    // ── composio_list_tools ─────────────────────────────────────────────

    struct ComposioListToolsTool {
        config: Arc<TenantComposio>,
        toolkits: Arc<Vec<String>>,
    }

    #[async_trait]
    impl Tool for ComposioListToolsTool {
        fn name(&self) -> &str {
            "composio_list_tools"
        }

        fn description(&self) -> &str {
            catalog::list_tools_description()
        }

        fn parameters_schema(&self) -> Value {
            catalog::list_tools_parameters_schema()
        }

        fn permission_level(&self) -> PermissionLevel {
            PermissionLevel::ReadOnly
        }

        async fn execute(&self, args: Value) -> Result<ToolResult> {
            // Requested toolkits (if any) intersected with the allowlist; when
            // the request is empty and an allowlist is set, use the allowlist as
            // the query so the backend never returns a toolkit the tenant is not
            // permitted to see.
            let requested: Vec<String> = args
                .get("toolkits")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();

            let effective: Vec<String> = if self.toolkits.is_empty() {
                requested
            } else if requested.is_empty() {
                self.toolkits.as_ref().clone()
            } else {
                requested
                    .into_iter()
                    .filter(|t| toolkit_allowed(&self.toolkits, t))
                    .collect()
            };
            // The narrowing + rendering request (issue #410). Toolkit resolution
            // above is a security decision (allowlist intersection) and stays
            // here; `search` / `detail` / `limit` are presentation and live in
            // the pure catalogue module.
            let mut request = catalog::ListRequest::parse(&args, effective.clone());
            tracing::debug!(
                effective = ?effective,
                allowlist = ?self.toolkits,
                search = ?request.search,
                detail = ?request.detail,
                limit = request.limit,
                "[composio] list_tools"
            );

            let query = if effective.is_empty() {
                None
            } else {
                Some(effective.as_slice())
            };
            let (client, secrets) = match live_call(&self.config).await {
                Ok(live) => live,
                Err(err) => {
                    return Ok(ToolResult::error(format!(
                        "composio_list_tools failed: {err}"
                    )));
                }
            };
            // The search term now travels to Composio rather than being applied
            // only to what came back. `request.search` is the tool's own
            // already-parsed terms; joined because the API takes one free-text
            // string over name/slug/description.
            let search_term = request.search.join(" ");
            let search = Some(search_term.as_str()).filter(|term| !term.trim().is_empty());
            // The parsed tags, not `None`: without this the tag narrowing added
            // to the BYOK route was unreachable from the agent tool that is
            // supposed to use it (CodeRabbit on tinyhumansai/opencompany#2153).
            let tags: Option<&[String]> =
                Some(request.tags.as_slice()).filter(|tags| !tags.is_empty());
            match client.list_tools(query, tags, search).await {
                Ok((mut resp, curated)) => {
                    request.curated = curated;
                    if !self.toolkits.is_empty() {
                        resp.tools.retain(|schema| {
                            toolkit_allowed(&self.toolkits, &slug_toolkit(&schema.function.name))
                        });
                    }
                    // Issue #410: render through the bounded, self-describing
                    // catalogue view rather than dumping the whole response as
                    // pretty JSON. A hundred-action toolkit serialized whole is
                    // hundreds of kilobytes; the harness's shared tool-result
                    // budget then cut it on a byte boundary, leaving the agent a
                    // fragment with nothing in it saying so.
                    let actions: Vec<catalog::CatalogAction> = resp
                        .tools
                        .iter()
                        .map(|schema| catalog::CatalogAction {
                            toolkit: slug_toolkit(&schema.function.name),
                            slug: schema.function.name.clone(),
                            description: schema.function.description.clone().unwrap_or_default(),
                            parameters: schema.function.parameters.clone(),
                        })
                        .collect();
                    let rendered = catalog::render(&actions, &request);
                    Ok(ToolResult::success(redact(&rendered, &secrets)))
                }
                Err(err) => Ok(scrubbed_err("composio_list_tools failed", &err, &secrets)),
            }
        }
    }

    // ── composio_authorize ──────────────────────────────────────────────

    const AUTHORIZE_HANDOFF_LIFETIME: std::time::Duration = std::time::Duration::from_secs(600);
    const MAX_PENDING_AUTHORIZATIONS: usize = 64;

    #[derive(Default)]
    pub(super) struct AuthorizeCache {
        pending: std::collections::HashMap<String, PendingAuthorization>,
        in_flight: std::collections::HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>,
    }

    impl std::fmt::Debug for AuthorizeCache {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("AuthorizeCache")
                .finish_non_exhaustive()
        }
    }

    struct PendingAuthorization {
        started: tokio::time::Instant,
        response: ComposioAuthorizeResponse,
    }

    fn authorize_request_key(
        config: &TenantComposio,
        company: &CompanyId,
        credential: &str,
        toolkit: &str,
        extra: &Option<Value>,
    ) -> Result<String> {
        use sha2::{Digest, Sha256};

        let mut identity = json!([
            company,
            config.backend_url,
            config.mode() == ComposioMode::Byok,
            credential,
            toolkit.trim().to_ascii_lowercase(),
            extra.is_some(),
            extra
        ]);
        identity.sort_all_objects();
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&identity)?)
        ))
    }

    async fn authorize_pending(
        config: &TenantComposio,
        company: &CompanyId,
        client: &LiveClient,
        credential: &str,
        toolkit: &str,
        extra: Option<Value>,
    ) -> Result<ComposioAuthorizeResponse> {
        let key = authorize_request_key(config, company, credential, toolkit, &extra)?;
        let key_lock = {
            let mut cache = config.authorizations.lock().await;
            cache
                .pending
                .retain(|_, pending| pending.started.elapsed() < AUTHORIZE_HANDOFF_LIFETIME);
            cache.in_flight.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = cache.in_flight.get(&key).and_then(std::sync::Weak::upgrade) {
                lock
            } else {
                let occupied = cache.pending.len()
                    + cache
                        .in_flight
                        .keys()
                        .filter(|in_flight_key| !cache.pending.contains_key(*in_flight_key))
                        .count();
                if !cache.pending.contains_key(&key) && occupied >= MAX_PENDING_AUTHORIZATIONS {
                    anyhow::bail!(
                        "too many cached OAuth handoffs; wait for earlier handoffs to expire"
                    );
                }
                let lock = Arc::new(tokio::sync::Mutex::new(()));
                cache.in_flight.insert(key.clone(), Arc::downgrade(&lock));
                lock
            }
        };
        let _key_guard = key_lock.lock().await;
        let pending = {
            let mut cache = config.authorizations.lock().await;
            cache
                .pending
                .retain(|_, pending| pending.started.elapsed() < AUTHORIZE_HANDOFF_LIFETIME);
            cache
                .pending
                .get(&key)
                .map(|pending| (pending.started, pending.response.clone()))
        };
        if let Some((started, response)) = pending {
            let connections = client.list_connections().await?;
            if let Some(connection) = connections.connections.iter().find(|connection| {
                connection.id == response.connection_id
                    && connection
                        .normalized_toolkit()
                        .eq_ignore_ascii_case(toolkit)
            }) {
                let status = connection.status.trim().to_ascii_uppercase();
                match status.as_str() {
                    "PENDING" | "INITIATED" | "INITIALIZING" => {
                        if started.elapsed() < AUTHORIZE_HANDOFF_LIFETIME {
                            return Ok(response);
                        }
                    }
                    "ACTIVE" | "CONNECTED" | "EXPIRED" | "FAILED" | "ERROR" | "INACTIVE"
                    | "DISCONNECTED" => {}
                    _ => anyhow::bail!(
                        "cannot verify the existing OAuth handoff status; no new handoff was started"
                    ),
                }
            }
            let mut cache = config.authorizations.lock().await;
            cache.pending.remove(&key);
        }
        let started = tokio::time::Instant::now();
        let response = client.authorize(toolkit, extra).await?;
        let mut cache = config.authorizations.lock().await;
        cache.pending.insert(
            key,
            PendingAuthorization {
                started,
                response: response.clone(),
            },
        );
        Ok(response)
    }

    struct ComposioAuthorizeTool {
        config: Arc<TenantComposio>,
        toolkits: Arc<Vec<String>>,
        company: CompanyId,
    }

    #[async_trait]
    impl Tool for ComposioAuthorizeTool {
        fn name(&self) -> &str {
            "composio_authorize"
        }

        fn description(&self) -> &str {
            "Reuse a pending OAuth handoff or begin one for a Composio toolkit (e.g. `gmail`). Return the hosted connect URL the operator opens in a browser. Completed, failed, or expired handoffs are replaced when connecting again."
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "toolkit": {
                        "type": "string",
                        "description": "Toolkit slug to authorize (e.g. `gmail`, `slack`, `github`)."
                    },
                    "extra_params": {
                        "type": "object",
                        "description": "Optional extra fields some toolkits require during authorization."
                    }
                },
                "required": ["toolkit"],
                "additionalProperties": false
            })
        }

        fn permission_level(&self) -> PermissionLevel {
            PermissionLevel::Execute
        }

        async fn execute(&self, args: Value) -> Result<ToolResult> {
            let toolkit = match required_string_arg(&args, "toolkit") {
                Ok(t) => t,
                Err(err) => return Ok(ToolResult::error(format!("composio_authorize: {err}"))),
            };
            // Enforce the allowlist BEFORE any network call — a toolkit the
            // tenant is not permitted to connect never reaches the backend.
            if !toolkit_allowed(&self.toolkits, &toolkit) {
                return Ok(ToolResult::error(format!(
                    "toolkit `{toolkit}` is not in this company's Composio allowlist"
                )));
            }
            let extra = args.get("extra_params").cloned();
            tracing::debug!(toolkit = %toolkit, "[composio] authorize");
            let (client, secrets) = match live_call(&self.config).await {
                Ok(live) => live,
                Err(err) => {
                    return Ok(ToolResult::error(format!(
                        "composio_authorize failed: {err}"
                    )));
                }
            };
            match authorize_pending(
                &self.config,
                &self.company,
                &client,
                &secrets[0],
                &toolkit,
                extra,
            )
            .await
            {
                Ok(resp) => Ok(scrubbed_ok(
                    serde_json::to_value(&resp).unwrap_or(Value::Null),
                    &secrets,
                )),
                Err(err) => Ok(scrubbed_err("composio_authorize failed", &err, &secrets)),
            }
        }
    }

    // ── composio_execute ────────────────────────────────────────────────

    struct ComposioExecuteTool {
        config: Arc<TenantComposio>,
        toolkits: Arc<Vec<String>>,
        metering: ComposioMetering,
    }

    #[async_trait]
    impl Tool for ComposioExecuteTool {
        fn name(&self) -> &str {
            "composio_execute"
        }

        fn description(&self) -> &str {
            "Run a Composio action by its slug (e.g. `GMAIL_SEND_EMAIL`) with a JSON `arguments` object. Discover the slug first with `composio_list_tools({\"search\": \"<words>\"})`, then read its parameters with `composio_list_tools({\"search\": \"<SLUG>\", \"detail\": \"schemas\"})`. Never guess a slug that was not listed."
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "tool": {
                        "type": "string",
                        "description": "Composio action slug from `composio_list_tools` (e.g. `GMAIL_SEND_EMAIL`)."
                    },
                    "arguments": {
                        "type": "object",
                        "description": "Arguments object passed through to the Composio action."
                    }
                },
                "required": ["tool"],
                "additionalProperties": false
            })
        }

        fn permission_level(&self) -> PermissionLevel {
            PermissionLevel::Execute
        }

        async fn execute(&self, args: Value) -> Result<ToolResult> {
            let tool = match required_string_arg(&args, "tool") {
                Ok(t) => t,
                Err(err) => return Ok(ToolResult::error(format!("composio_execute: {err}"))),
            };
            // Enforce the allowlist on the slug's toolkit prefix BEFORE any
            // network call (e.g. `GMAIL_SEND_EMAIL` → `gmail`).
            let toolkit = slug_toolkit(&tool);
            if !toolkit_allowed(&self.toolkits, &toolkit) {
                return Ok(ToolResult::error(format!(
                    "action `{tool}` targets toolkit `{toolkit}`, which is not in this company's Composio allowlist"
                )));
            }
            let arguments = args.get("arguments").cloned();
            // Which account this company means for the toolkit, if it has said
            // (issue #820). Resolved from the company's own config — never from
            // `args` — because "send from billing@, not ops@" is a company
            // decision, and an agent that could name a connection could name one
            // the operator deliberately did not choose.
            let pinned = self.config.default_connection(&toolkit).map(str::to_string);
            // tracing carries the slug/toolkit only — NEVER arguments or bodies.
            tracing::debug!(tool = %tool, toolkit = %toolkit, pinned = ?pinned, "[composio] execute");
            let (client, secrets) = match live_call(&self.config).await {
                Ok(live) => live,
                Err(err) => {
                    return Ok(ToolResult::error(format!("composio_execute failed: {err}")));
                }
            };
            let call = client
                .execute(&tool, arguments, pinned.as_deref(), &self.metering)
                .await;
            match call {
                Ok(resp) => {
                    // Metered only on success — i.e. a call that actually
                    // reached the connected account. `connections` in the read
                    // model is the *count of providers seen*, so counting a
                    // failed call would report a connection for a provider this
                    // company may not even be connected to. Never fails the
                    // call: `record_oauth_call` logs and swallows.
                    if let Some(meter) = self.metering.meter.as_deref() {
                        record_oauth_call(
                            meter,
                            &self.metering.company,
                            &self.metering.agent,
                            &toolkit,
                            now_millis(),
                        )
                        .await;
                    }
                    Ok(scrubbed_ok(
                        serde_json::to_value(&resp).unwrap_or(Value::Null),
                        &secrets,
                    ))
                }
                Err(err) => Ok(scrubbed_err("composio_execute failed", &err, &secrets)),
            }
        }
    }

    #[cfg(test)]
    #[path = "composio_live_tests.rs"]
    mod live_tests;
}

/// The mandatory tenant-isolation test (issue #110): two per-tenant configs (A
/// and B) over a mock backend that records the `Authorization` header of each
/// request and answers with tenant-specific data. Proves the ONLY isolation
/// lever — which token the client is constructed with — actually holds: A's
/// request carries token A (never B), and A's result carries only A's account.
#[cfg(all(test, feature = "composio"))]
#[path = "composio_isolation_tests.rs"]
mod isolation_tests;
/// The console-facing ops helpers ([`authorize_connect_url`],
/// [`list_connection_states`]) over a mock Composio backend: proves the connect
/// URL is surfaced, the allowlist is enforced before any network call, and
/// connection rows aggregate to per-toolkit `connected` state filtered to the
/// tenant grant.
#[cfg(all(test, feature = "composio"))]
#[path = "composio_ops_helper_tests.rs"]
mod ops_helper_tests;

#[cfg(test)]
#[path = "composio_tests.rs"]
mod tests;
