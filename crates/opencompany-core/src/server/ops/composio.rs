//! Per-tenant Composio connection management (issue #110, epic #26 Cell D): read
//! the company's Composio status and set a per-company OAuth bearer token.
//!
//! ## Where the credential normally comes from
//!
//! On the hosted platform a company needs to paste nothing. The instance already
//! authenticates with a platform-minted, audience-bound identity, and Composio
//! calls present that — the backend derives the Composio entity from it. The read
//! shape reports this as `credentialSource: "attested"`, and there is nothing
//! stored on the instance to leak, rotate, or lose.
//!
//! ## `PUT …/composio/token` — the BYO override
//!
//! The write route stays, and it is **not** the hosted path. It is:
//!
//! * the **per-company BYO override** — a company that has its own Composio
//!   account/token can set it here, and it wins over the instance identity for
//!   that company only; and
//! * the escape hatch for running this repo **standalone**. This repository is
//!   public and people do run it that way, but self-hosting is **unsupported**:
//!   there is no platform identity to present, so pasting a token is the only way
//!   to reach Composio at all. Do not read this as parity with the hosted
//!   product — it is a hatch, not a deployment mode, and nothing else about
//!   standalone operation is supported this milestone.
//!
//! ## Who a connection belongs to (issue #403)
//!
//! **A connection belongs to the company, and is admin-managed.** The account
//! reached through it is the account the company's *agents* act through, so it
//! is company property in the same sense the roster and the manifest are — not
//! the personal property of whichever member happened to click Connect.
//!
//! This is not a new position; it is the one the code already took and did not
//! enforce. The native OAuth plane's connect-time identity check
//! ([`account_mismatch`](super::connections)) compares the connected account
//! against `bootstrap_admins` — the company's *admin* addresses — which only
//! makes sense if a connection is the company's. Issue #316 settled "one
//! operator, one connected account"; this is the layer above it, and it
//! resolves the same way.
//!
//! Two consequences, both deliberate:
//!
//! * **Writes require an admin.** `PUT …/composio/token` and
//!   `POST …/composio/authorize` take [`AdminScopedCompany`]. A member is
//!   refused with a message that says why.
//! * **Reads stay open to any member.** `GET …/composio` and
//!   `GET …/composio/connections` carry no credential — only a tier name,
//!   non-secret routing, and which providers are connected. Knowing *that*
//!   Gmail is connected is what lets a member understand why an agent can read
//!   mail; being able to change it is the part that needed an owner. The split
//!   mirrors the roster, where `GET …/team` is open and the budget writes are
//!   not.
//!
//! Either way the token is **write-only** over the API: set through the `token`
//! field, stored in the secret store under
//! [`TINYHUMANS_KEY_KEY`](crate::company::composio::TINYHUMANS_KEY_KEY), and
//! **never** echoed. The read shape carries only `credentialSource` plus
//! non-secret routing (backend URL, toolkit allowlist) — never a token, and
//! never a file path. A set / rotate / clear takes effect on the agents'
//! **next turn** with no restart (the harness re-resolves the credential
//! each turn and rebuilds the roster when the *identity* behind it changes).

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::routing::{delete, get, post, put};
use serde::{Deserialize, Serialize};

use crate::AppState;
// `token_configured` is gone from this list deliberately: the status route no
// longer re-derives the credential tier from booleans, it asks the resolver
// (`resolve_credential`) — see `credential_source_for` below.
use crate::company::composio::{
    CatalogEntry, ComposioMode, backend_url_or_default, load_mode, resolve_access,
    resolve_credential, store_api_key, store_token,
};
use crate::company::composio_probe::{ComposioProbeClass, classify, describe, describe_verdict};
use crate::company::credentials::{CredentialSource, TinyhumansTokenSource};
use crate::company::runtime::CompanyRuntime;
// Keys rework (#2306), slice 4c: the Composio half of the account-key reuse
// banner. `company_key::copy_account_key_to_composio` is 4c's own function;
// the rest of this surface never otherwise reaches into `company_key`.
use crate::company::company_key::{self, SlotOutcome};
use crate::ports::types::CompanyEvent;
use crate::server::error::ApiError;
use crate::server::ops::composio_toolkits::{self, CatalogSource, OpenModeToolkits};
use crate::server::ops::slot_report::SlotReportDto;
use crate::server::ops::{AdminScopedCompany, ScopedCompany, scoped};

/// The reminder attached to a set / rotate response.
const SWITCH_NOTE: &str =
    "Agents pick up the new Composio token on their next turn — no restart needed.";

/// The reminder attached to a clear response.
///
/// A clear does not hand agents a new token — it withdraws the BYO override, so
/// what they present next is whatever tier remains (a company key, the instance
/// identity, or none). Reusing [`SWITCH_NOTE`] here would tell the operator that
/// a new token exists when the effective credential has fallen back to nothing
/// (issue #1471).
const CLEAR_NOTE: &str =
    "Composio token cleared. Agents use whatever credential remains on their next turn.";

/// The reminder attached to storing a managed-route token for a company that is
/// **on BYOK**.
///
/// [`SWITCH_NOTE`] would be a lie here, in the way that matters most: it says
/// agents pick the token up on their next turn, and they do not. `resolve_access`
/// reads the BYOK key while the company is on that route, so a token stored for
/// the managed route sits there doing nothing until somebody chooses that route.
///
/// The state is reachable on purpose. A BYOK company whose managed chain
/// resolves to nothing cannot be offered "Use this" — that would be switching
/// into an outage — so the console offers the token first and the switch after,
/// which is the only order that works. Saying "in effect" at the end of the
/// first step would be reporting the second one as already done.
const INACTIVE_TOKEN_NOTE: &str = "Composio token saved for the managed route. This company is \
     still on its own Composio account, so agents keep using that key until you switch routes.";

/// The reminder attached to switching a company onto its own Composio account.
///
/// It names the consequence the radio button cannot: the providers connected
/// through the managed route live in the platform's Composio tenant and are not
/// visible from the company's own account, so the grid will look empty until
/// they are connected again there.
const BYOK_NOTE: &str = "This company now reaches Composio through its own account. Agents pick \
     that up on their next turn. Providers connected through TinyHumans-managed Composio stay in \
     that account — connect them again here, or clear the key to switch back.";

/// The reminder attached to giving the managed route back.
const MANAGED_NOTE: &str = "Composio API key cleared. This company is back on TinyHumans-managed \
     Composio from the agents' next turn, with the providers it had connected there.";

/// The toolkits the console should offer for a company, whether that answer
/// came from open mode, and where the list came from.
///
/// A non-empty manifest allowlist is authoritative and is offered **verbatim**:
/// a company that deliberately narrowed its belt sees exactly what it chose, the
/// backend catalog is not consulted, and nothing can widen it. That is not a
/// performance shortcut — it is the boundary. Widening a restrictive manifest
/// from a catalog fetch would silently hand a company providers it decided
/// against.
///
/// Empty means **open mode**, where the manifest is deferring to the backend's
/// own server-enforced allowlist. The honest answer there is the backend's live
/// catalog ([`composio_toolkits`]), fetched once per
/// [`CATALOG_TTL`](composio_toolkits::CATALOG_TTL) and marked
/// [`CatalogSource::Fallback`] with a reason if it cannot be had.
///
/// Returning the triple (rather than letting the console infer any of it) is the
/// whole point of the fix: the console must never have to guess which of two
/// opposite meanings an empty list carries, nor whether the list it is rendering
/// is the real catalog.
///
/// This is a **console affordance only**. Agent-side toolkit admission is
/// [`toolkit_allowed`](crate::harness::composio), which still treats an empty
/// manifest list as "allow every toolkit" — unchanged by any of this.
async fn effective_toolkits(
    runtime: &CompanyRuntime,
    manifest: &[String],
) -> (bool, OpenModeToolkits) {
    if manifest.is_empty() {
        (true, open_mode_toolkits(runtime).await)
    } else {
        (
            false,
            OpenModeToolkits {
                // A manifest allowlist is slugs and nothing else — the company
                // wrote it by hand, and the catalog is deliberately not
                // consulted here (it must not be able to widen a list that
                // narrowed on purpose). So these entries carry no metadata, and
                // the console falls back to its own typography for them.
                toolkits: manifest.iter().map(CatalogEntry::from_slug).collect(),
                source: CatalogSource::Manifest,
                notice: None,
            },
        )
    }
}

/// Open mode's provider list: the backend's live catalog, cached, or an
/// honestly-marked fallback.
///
/// The cache is consulted before the (cfg-split) fetch and records the outcome
/// either way, so a status poll during an outage costs a map lookup rather than
/// another fetch timeout of waiting. Concurrent callers on a cold key share one
/// fetch rather than each starting their own.
async fn open_mode_toolkits(runtime: &CompanyRuntime) -> OpenModeToolkits {
    let key = catalog_cache_key(runtime);
    let outcome = composio_toolkits::cache()
        .get_or_fetch(&key, || fetch_catalog(runtime))
        .await;
    OpenModeToolkits::from_outcome(outcome)
}

/// Drop this company's cached catalog.
///
/// Exposed because the catalog is a property of the **credential**, not of the
/// Composio token alone: changing the company's TinyHumans key
/// ([`ops::company_key`](super::company_key)) can change which account the
/// backend resolves and therefore which catalog it serves. Both writes evict
/// through here rather than each reaching into the cache with its own key
/// derivation.
pub(crate) fn evict_catalog_cache(runtime: &CompanyRuntime) {
    composio_toolkits::cache().evict(&catalog_cache_key(runtime));
}

/// This company's catalog cache key. The backend URL is resolved the same way
/// the status DTO resolves it, so a company repointed at a different backend
/// does not read the old backend's catalog.
fn catalog_cache_key(runtime: &CompanyRuntime) -> String {
    use crate::app::config::EnvSource;
    let env = crate::app::config::ProcessEnv;
    let backend_url =
        backend_url_or_default(env.get(crate::company::composio::TINYHUMANS_API_URL_ENV));
    composio_toolkits::cache_key(runtime.id(), &backend_url)
}

/// Fetch the backend's toolkit catalog for this company, bounded by
/// `composio_toolkits::FETCH_TIMEOUT`.
///
/// `Err` is a plain-language reason the console can show — never a bare
/// fall-through to a short list that would look authoritative.
#[cfg(feature = "composio")]
async fn fetch_catalog(runtime: &CompanyRuntime) -> Result<Vec<CatalogEntry>, String> {
    // No credential of any tier means there is nothing to dial the backend
    // with. Say that, rather than spending the timeout to discover it.
    //
    // But say only that when it is what happened. `resolve_tenant` answers
    // `Conflict` for "nothing configured" and propagates anything else — a
    // secret-store read failure among them. Collapsing every error into "no
    // credential yet" would tell an operator whose store hiccupped that they
    // never set a key, which is the confident-wrong-answer this credential work
    // exists to remove (see `company_key::resolve`).
    let config = resolve_tenant(runtime).await.map_err(|err| {
        if matches!(err.0, crate::error::OpenCompanyError::NotConfigured(_)) {
            "this company has no Composio credential yet, so the catalog cannot be read".to_string()
        } else {
            format!(
                "this company's Composio credential could not be resolved: {}",
                err.0
            )
        }
    })?;
    let fetch = crate::harness::composio::list_catalog_toolkits(&config);
    match tokio::time::timeout(composio_toolkits::FETCH_TIMEOUT, fetch).await {
        Err(_) => Err(format!(
            "the Composio backend did not answer within {}s",
            composio_toolkits::FETCH_TIMEOUT.as_secs()
        )),
        // `{err:#}`, not `to_string()`: the latter renders only the outermost
        // context and drops the cause chain behind it, so a catalog that failed
        // on a rejected API key reported the bare call name — "Composio v3
        // /toolkits" — with nothing saying what went wrong. The operator-facing
        // notice is the only place this surfaces, so it has to carry the reason.
        Ok(Err(err)) => Err(format!("{err:#}")),
        Ok(Ok(toolkits)) if toolkits.is_empty() => {
            Err("the Composio backend returned an empty catalog".to_string())
        }
        Ok(Ok(toolkits)) => Ok(toolkits),
    }
}

/// Without the `composio` feature there is no client to fetch a catalog with.
/// The status route still answers (reporting `inBuild:false`), and it says so
/// rather than presenting the fallback as the backend's list.
#[cfg(not(feature = "composio"))]
async fn fetch_catalog(_runtime: &CompanyRuntime) -> Result<Vec<CatalogEntry>, String> {
    Err("Composio is not compiled into this build".to_string())
}

/// Builds the Composio management route fragment.
///
/// The read/write-token plane (`GET …/composio`, `PUT …/composio/token`) is
/// always present. The per-provider OAuth sign-in plane (`POST
/// …/composio/authorize`, `GET …/composio/connections`, `DELETE
/// …/composio/connections/{connection_id}`) is **also** always present in the
/// route table — mirroring how `get_status` stays wired and reports
/// `inBuild:false` rather than `#[cfg]`-ing itself out — but its handlers only
/// reach the live Composio client under the `composio` feature; otherwise they
/// answer `409 not_in_build` "not in this build".
pub fn router() -> Router<AppState> {
    scoped("/composio", get(get_status))
        .merge(scoped("/composio/token", put(set_token)))
        .merge(scoped("/composio/api-key", put(set_api_key)))
        .merge(scoped("/composio/api-key/test", post(test_api_key)))
        .merge(scoped(
            "/composio/tinyhumans/key/from-account",
            post(copy_account_key),
        ))
        .merge(scoped("/composio/authorize", post(authorize)))
        .merge(scoped("/composio/connections", get(connections)))
        .merge(scoped(
            "/composio/connections/{connection_id}",
            delete(disconnect),
        ))
        .merge(scoped(
            "/composio/connections/{connection_id}/default",
            put(set_default).delete(clear_default),
        ))
}

/// The company's Composio status as the console renders it. **Never** carries the
/// token — only the non-secret `credentialSource` plus routing.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ComposioStatusDto {
    /// Whether the `composio` feature is compiled into this build at all (the
    /// tools only exist under it). `false` lets the console show a "not in this
    /// build" state rather than implying a missing credential.
    in_build: bool,
    /// Whether the company **explicitly** grants the `composio` namespace (a `*`
    /// wildcard does NOT count).
    granted: bool,
    /// Where this company's Composio credential comes from:
    ///
    /// * `attested` — the instance's platform identity (nothing is stored here);
    /// * `static` — a token this company pasted, or a static instance key;
    /// * `none` — no credential can be obtained, so no tools are wired.
    ///
    /// A tier name, never a credential and never a path.
    credential_source: CredentialSource,
    /// Which tier the **managed** chain resolves to, whatever [`Self::mode`]
    /// says — the company's own backend token, then its TinyHumans key, then
    /// this instance's platform identity, then nothing.
    ///
    /// Under `managed` this is [`Self::credential_source`] by construction:
    /// both come from the same resolver, so they cannot disagree. Under `byok`
    /// they differ on purpose. `credentialSource` there names the **Composio
    /// API key** the calls actually present, because that is what the agents
    /// hold; this field names what the managed route *would* answer with if the
    /// key were cleared. Without it the console can show a BYOK company the
    /// consequence of going back to managed only by making it go back and look.
    ///
    /// A **tier name**. Never a credential, never a path, and — the part worth
    /// saying out loud — **never a boolean about a secret slot**. A
    /// `tokenConfigured`-shaped field was on this DTO once and was removed by
    /// issue #886: it answered "did somebody paste something into
    /// `composio/tinyhumans/key` (then `composio/token`)", which is one tier of
    /// the chain, and it read `false` for companies whose agents were calling
    /// `GITHUB_*` tools successfully in the same session. Do not reintroduce
    /// one under any name. The question it looked like it answered is
    /// answered here, by the tier that actually resolves.
    managed_credential_source: CredentialSource,
    /// Which host this company's Composio calls go to — `managed` (proxied
    /// through the OpenHuman backend, the default) or `byok` (straight to this
    /// company's own Composio account).
    ///
    /// Orthogonal to [`Self::credential_source`], which names *whose identity*
    /// a call presents rather than *which host* it is presented to. A BYOK
    /// company reports `byok` + `static`; a company that pasted a backend token
    /// override reports `managed` + `static`. Collapsing them into one field
    /// would leave the console unable to say which of the two an operator is
    /// looking at.
    mode: ComposioMode,
    /// The effective Composio backend URL (env override or default), or
    /// Composio's own API host under BYOK. Non-secret, and the address the
    /// calls really go to — a status line still naming the managed backend
    /// after a switch to BYOK would read as though nothing had happened.
    backend_url: String,
    /// The manifest toolkit allowlist verbatim (empty = defer to the backend
    /// allowlist, i.e. open mode). Kept as-is so an operator can still see what
    /// the manifest literally says; the console renders
    /// [`Self::effective_toolkits`] instead.
    toolkits: Vec<String>,
    /// Whether this company is in **open mode** — an empty manifest allowlist,
    /// meaning the backend's own allowlist governs and every toolkit it permits
    /// is reachable (issue #397).
    ///
    /// The console needs this told to it rather than inferred: an empty
    /// `toolkits` means *allow everything*, which is the opposite of what an
    /// empty list reads as.
    open_mode: bool,
    /// The toolkits the console offers as provider rows — the manifest list when
    /// it is non-empty, else the backend's live catalog (or, when that cannot be
    /// fetched, a built-in fallback flagged by [`Self::catalog_source`]). In open
    /// mode this is still not a hard limit: any slug the backend permits can be
    /// authorized by typing it.
    effective_toolkits: Vec<String>,
    /// The same providers as [`Self::effective_toolkits`], in the same order,
    /// carrying whatever display metadata the backend published for each —
    /// name, description, logo URL, and Composio's own category names (issue
    /// #600).
    ///
    /// **Additive, and deliberately so.** The slug list above is the contract
    /// every existing consumer reads and the only thing an authorize call
    /// needs; this is the render model beside it. Replacing the slug list with
    /// this one would have bought nothing and broken that contract.
    ///
    /// Empty metadata is a real state, not a bug: a manifest allowlist, a
    /// fallback list, and a backend predating the dynamic catalog all yield
    /// slug-only entries, and the console renders those with its own
    /// typography.
    ///
    /// The categories are forwarded **verbatim**, uninterpreted. The console
    /// buckets them by substring, which is what lets a Composio integration
    /// added tomorrow land in the right group with no change on either side of
    /// this wire.
    effective_catalog: Vec<CatalogEntry>,
    /// Where [`Self::effective_toolkits`] came from — `manifest`, `backend`, or
    /// `fallback` (issue #397).
    ///
    /// The console must be able to distinguish "these are the hundred providers
    /// the backend permits" from "the catalog could not be read, here are eight
    /// we know of". Both render as a list of slugs; only one of them is
    /// authoritative, and a console that could not tell them apart would present
    /// the second as though it were the first.
    catalog_source: CatalogSource,
    /// Why the list is a fallback, in plain language for the operator. `None`
    /// unless [`Self::catalog_source`] is `fallback`.
    catalog_notice: Option<String>,
}

/// A mutating response: the resulting status plus the switch reminder.
///
/// [`Self::advisory`] and [`Self::probe_class`] were added **additively** for
/// issue #2275 and are omitted entirely when there is nothing to say, so every
/// existing consumer of `status` + `note` reads the same body it always did.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MutationResponse {
    status: ComposioStatusDto,
    note: String,
    /// What the credential check said, when the write landed anyway.
    ///
    /// Present only when a probe failed **non-destructively** — the key is
    /// stored and in use, and only its reachability is in question. Always
    /// [`describe`]'s fixed copy, never the upstream error text: that can echo
    /// request headers or a key fragment, and this lands in a banner somebody
    /// screenshots.
    #[serde(skip_serializing_if = "Option::is_none")]
    advisory: Option<String>,
    /// The class behind [`Self::advisory`], so the console can decide how to
    /// render it (and whether to offer "add anyway") without parsing prose.
    #[serde(skip_serializing_if = "Option::is_none")]
    probe_class: Option<ComposioProbeClass>,
    /// The `usedBy` this mutation would have refused with, echoed back on a
    /// **confirmed** clear/switch (`docs/key-reworks/in-use-guards.md` §3) —
    /// the shape computed *before* the mutation applied, so the console can
    /// show what it just broke without re-deriving it. `None` on every
    /// mutation that is not a guarded clear/switch, and on a guarded one that
    /// had nothing to warn about.
    #[serde(skip_serializing_if = "Option::is_none")]
    used_by: Option<crate::error::UsedBy>,
    /// What [`copy_account_key`] did to the Composio slot (keys rework #2306,
    /// slice 4c) — always exactly one entry, `Slot::Composio`. Empty (and
    /// omitted) on every other mutation on this surface, which touches this
    /// company's own credential directly rather than copying the account key.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    slots: Vec<SlotReportDto>,
}

/// Set-token body. `token` is write-only intake (never returned): a non-empty
/// value rotates the company's BYO override, an explicit empty string clears it
/// (reverting to the instance identity, where there is one).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetToken {
    token: String,
    /// Confirms a clear that the in-use guard would otherwise refuse
    /// (`docs/key-reworks/in-use-guards.md` §2). Ignored on a set/rotate —
    /// only a clear is guarded — and ignored when there is nothing to
    /// confirm. Defaults to `false`, so a caller that has never heard of this
    /// field gets the guarded path.
    #[serde(default)]
    confirm_in_use: bool,
}

/// Set-API-key body. `apiKey` is write-only intake (never returned): a non-empty
/// value stores this company's own Composio API key and switches it to BYOK, an
/// explicit empty string clears the key and returns it to OpenHuman-managed
/// Composio.
///
/// One field, not two. A `mode` an operator could set independently of the key
/// would let them select BYOK with nothing stored — a company with no Composio
/// tools and no obvious reason why — so the mode is a consequence of the key
/// rather than a separate control. See
/// [`store_api_key`](crate::company::composio::store_api_key).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetApiKey {
    api_key: String,
    /// Store the key without checking it first.
    ///
    /// The escape hatch for a key that is valid but whose check cannot
    /// complete — a proxy in the way, a network the host cannot see past.
    /// Defaults to **false**, so a caller that has never heard of this field
    /// gets the checked path; nothing opts in by omission.
    ///
    /// The route simply honours it. Gating the *offer* on a probe having
    /// already failed is the console's job (see `connect-flow.md`: "add anyway"
    /// is unlocked by a typed probe failure and cleared on every retry), and it
    /// has to be, because a route cannot tell a considered override from a
    /// client that sends `true` always. What the route can guarantee is the
    /// default, which is what it guarantees.
    #[serde(default)]
    skip_verify: bool,
    /// Confirms a route switch (or a BYOK key clear back to managed) that the
    /// in-use guard would otherwise refuse (`docs/key-reworks/in-use-guards.md`
    /// §2). Ignored when the write does not change the company's route —
    /// rotating a key already in use is never guarded. Defaults to `false`.
    #[serde(default)]
    confirm_in_use: bool,
}

/// `POST …/composio/authorize` body: the toolkit slug (`gmail` / `slack` /
/// `github` / …) to begin an OAuth handoff for.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizeBody {
    toolkit: String,
}

/// `POST …/composio/authorize` response: the Composio-hosted connect URL the
/// operator opens in a browser tab. Composio runs the OAuth itself; there is no
/// local callback — the console polls [`ConnectionDto`] until the toolkit
/// reports connected.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizeDto {
    connect_url: String,
}

/// One per-toolkit connected state in the `GET …/composio/connections`
/// response: `connected` is true when the company has at least one active
/// connection for that toolkit.
///
/// [`accounts`](Self::accounts) was added for the provider detail view (issue
/// #404) **additively**: `toolkit` and `connected` keep their exact previous
/// meaning, so the tile grid and the post-authorize poll that read only those
/// two are untouched by it. A detail view therefore needs no second route and
/// no second round-trip — the call the page already makes now carries enough to
/// open a provider.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionDto {
    toolkit: String,
    connected: bool,
    /// Every connection this company holds for the toolkit, oldest id first.
    ///
    /// Usually one. Composio permits several accounts per toolkit, and a company
    /// that connected Gmail twice needs to see which is which before revoking
    /// one — the concrete reason `connected: bool` alone could not back a
    /// disconnect.
    accounts: Vec<ConnectedAccountDto>,
    /// The account this company chose for the toolkit, when it has chosen one
    /// (issue #820) — the id `composio_execute` sends as `connectionId`.
    ///
    /// **Omitted, not defaulted.** Absent means the company has expressed no
    /// intent and Composio resolves the account itself; there is no implicit
    /// default here to report, and inventing one (the oldest, the first in the
    /// sort) would be a claim the console makes and the harness does not honour.
    /// That absence is the honest state and stays the ordinary one.
    #[serde(skip_serializing_if = "Option::is_none")]
    default_connection_id: Option<String>,
}

/// One connected account inside a [`ConnectionDto`] (issue #404).
///
/// A non-secret projection of
/// [`ComposioConnectionRow`](crate::harness::composio::ComposioConnectionRow) —
/// see its docs for why the id is safe to hand the console.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectedAccountDto {
    /// Composio's connection id — the path segment `DELETE …/connections/{id}`
    /// takes.
    id: String,
    /// Composio's raw status string, forwarded verbatim so the console can tell
    /// "never set up" from "set up and expired".
    status: String,
    /// Whether this individual account is usable.
    connected: bool,
    /// When Composio recorded the connection, when it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    /// The account label, when the provider published one. Omitted rather than
    /// guessed — see the row type's docs.
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<String>,
    /// Whether this is the account the company chose for the toolkit (issue
    /// #820). At most one account per toolkit carries it, and none does until
    /// somebody says so.
    is_default: bool,
}

/// How a company reaches Composio — the route, and which tier its credential
/// comes from.
///
/// Asks the resolver itself rather than restating its precedence. The console
/// must never be able to name a tier the agents are not on, and a second copy of
/// the rule is a second place to forget to update — which is how a status route
/// ends up confidently reporting a credential that no longer resolves.
///
/// Takes the instance identity already resolved, rather than an `&dyn
/// EnvSource`: a trait object with no `Send + Sync` bound held across the await
/// below makes the whole handler future non-`Send`, which axum rejects. Callers
/// resolve it from whichever environment they mean, so the matrix stays testable
/// without mutating the process environment.
async fn access_for(
    runtime: &CompanyRuntime,
    token_source: Option<std::sync::Arc<TinyhumansTokenSource>>,
) -> Result<(ComposioMode, CredentialSource), ApiError> {
    let access = resolve_access(runtime.id(), runtime.secrets().as_ref(), token_source)
        .await
        .map_err(ApiError)?;
    Ok((access.mode, access.credential.source()))
}

/// Resolves the Composio status DTO for a company.
async fn effective_status(runtime: &CompanyRuntime) -> Result<ComposioStatusDto, ApiError> {
    let record = runtime.store().load(runtime.id()).await.map_err(ApiError)?;
    let (granted, toolkits) = match record {
        Some(record) => (
            crate::company::grants_composio_explicit(&record.manifest.tools.allow),
            record.manifest.tools.composio.toolkits.clone(),
        ),
        None => (false, Vec::new()),
    };
    let env = crate::app::config::ProcessEnv;
    let api_url = {
        use crate::app::config::EnvSource;
        env.get(crate::company::composio::TINYHUMANS_API_URL_ENV)
    };
    let token_source = TinyhumansTokenSource::from_env(&env).map(std::sync::Arc::new);
    let (mode, credential_source) = access_for(runtime, token_source.clone()).await?;
    // The managed chain, asked unconditionally.
    //
    // `access_for` short-circuits under BYOK — `resolve_access` reads the
    // company's Composio API key and stops — so `credential_source` there names
    // the BYOK key and says nothing about the route the company would return
    // to. Nor is the managed chain resolved anywhere else on this path: the
    // catalog fetch goes through `resolve_tenant`, which calls the same
    // `resolve_access` and therefore dials `backend.composio.dev` with the BYOK
    // key. So the managed tier has to be resolved here, explicitly, or not at
    // all.
    //
    // The cost is secret-store reads and no network — the same reads the
    // managed path already makes on every status poll — which is why this is
    // unconditional rather than `if mode.is_byok()`. One derivation, one code
    // path, and no branch where the field could come from somewhere else.
    let managed_credential_source =
        resolve_credential(runtime.id(), runtime.secrets().as_ref(), token_source)
            .await
            .map_err(ApiError)?
            .source();
    let (open_mode, effective) = effective_toolkits(runtime, &toolkits).await;
    // Report the host the calls actually reach. Under BYOK the managed backend
    // URL is resolved and then not used, so echoing it would be misdirection.
    let backend_url = match mode {
        ComposioMode::Byok => crate::company::composio::DIRECT_BASE_URL.to_string(),
        ComposioMode::Managed => backend_url_or_default(api_url),
    };
    Ok(ComposioStatusDto {
        in_build: cfg!(feature = "composio"),
        granted,
        credential_source,
        managed_credential_source,
        mode,
        backend_url,
        toolkits,
        open_mode,
        effective_toolkits: effective.slugs(),
        effective_catalog: effective.toolkits,
        catalog_source: effective.source,
        catalog_notice: effective.notice,
    })
}

/// `GET …/composio` — the company's Composio status.
async fn get_status(company: ScopedCompany) -> Result<Json<ComposioStatusDto>, ApiError> {
    Ok(Json(effective_status(company.runtime.as_ref()).await?))
}

/// The `usedBy` a Composio credential clear or mode switch would carry, per
/// `docs/key-reworks/in-use-guards.md` §2's `surfaces` table: `surfaces:
/// [Composio]` iff `composio/mode` currently selects `slot` — the route the
/// credential being cleared/switched belongs to — else `None`, the whole
/// field omitted rather than emitted empty, matching every other producer of
/// this shape.
///
/// `current_mode` is the mode the caller already read, passed in rather than
/// re-read here: [`set_token`] reads it purely to call this, and
/// [`set_api_key`] already needed it to decide whether the route is
/// switching at all, so a second read here could only disagree with the
/// caller's own under a concurrent write between them. `slot` names which
/// route the touched credential belongs to —
/// [`ComposioMode::Managed`] for `composio/tinyhumans/key`
/// ([`TINYHUMANS_KEY_KEY`](crate::company::composio::TINYHUMANS_KEY_KEY)),
/// [`ComposioMode::Byok`] for `composio/byok/key`
/// ([`BYOK_KEY_KEY`](crate::company::composio::BYOK_KEY_KEY)) — so `mode ==
/// slot` is exactly "a workload would actually lose tool access": the
/// credential being touched is the one calls are currently resolving
/// through.
///
/// This replaces the previous signal (`has_connected_integrations` reading
/// `composio/defaults`, this company's pinned toolkit connections): cheap
/// but imprecise, since a pin can exist under a route this mutation does not
/// even touch, and it could not tell a clear on the *active* slot from one
/// on the inactive one. The mode match is both cheaper (no secret-store read
/// beyond the one the caller already made) and exact — it is the literal
/// condition in-use-guards.md §2 states.
///
/// Composio never populates `default` or `agents`: it has no default/pair
/// concept of its own (§1: "only `surfaces`, since Composio has no
/// default/agent-pair concept").
fn composio_used_by(
    current_mode: ComposioMode,
    slot: ComposioMode,
) -> Option<crate::error::UsedBy> {
    (current_mode == slot).then(|| crate::error::UsedBy {
        surfaces: vec![crate::error::UsedBySurface::Composio],
        ..Default::default()
    })
}

/// §2's fixed sentence for "a key clear/disable with only `surfaces`":
/// `"<Label>'s key is used by <surfaces, comma-joined>."`.
///
/// This is the only shape a Composio-native guard ever produces (Composio has
/// no `default`/`agents` — see [`composio_used_by`]), and applying the
/// template here reads self-referentially: "Composio's key is used by
/// Composio." That is not a mistake. The `surfaces` vocabulary
/// (`llm`/`composio`/`search`) exists to name *other* product areas that
/// share one credential — e.g. the TinyHumans account key also brokering
/// Composio — and Composio clearing its *own* token or key has exactly one
/// surface that can depend on it: Composio itself, via the toolkit
/// connections pinned under it. There is no better noun in the shared
/// vocabulary for "this company's connected integrations", so the literal
/// application of the template is what this function does; a human reading
/// the sentence still gets the right idea (something Composio-shaped breaks),
/// which is what the message is for.
fn composio_in_use_message() -> String {
    "Composio's key is used by Composio.".to_string()
}

/// `PUT …/composio/token` — set / rotate / clear this company's BYO token. See
/// the module docs: the hosted path needs no token, and standalone operation
/// (where this is the only option) is unsupported.
///
/// **Admin-only** (issue #403). This is the sharpest write in the module: the
/// token it stores is the identity every one of the company's agents presents
/// to Composio, so whoever sets it decides which account those agents act
/// through. That is a decision made *for* the company, not a member's own, and
/// [`AdminScopedCompany`] is what says so in the signature.
async fn set_token(
    company: AdminScopedCompany,
    Json(body): Json<SetToken>,
) -> Result<Json<MutationResponse>, ApiError> {
    let runtime = company.runtime.as_ref();
    // Keys rework (#2306), slice 4a: this route writes the same
    // `composio/tinyhumans/key` slot the account-key fan-out
    // (`company_key::fan_out`) copies into, under the same lock — so a
    // concurrent paste here and account-key save cannot interleave and leave
    // the two disagreeing about which value is current.
    let _fan_out_guard = crate::company::company_key::slot_guard(runtime.id()).await;
    let clearing = body.token.trim().is_empty();
    // Only a clear is guarded (in-use-guards.md §1/§6): setting or rotating a
    // non-empty token cannot strand anything this company already had — the
    // credential it presents only gets more likely to resolve. Computed
    // before the write, per §3, so a confirmed clear echoes exactly what it
    // would have refused with. `composio/tinyhumans/key` only feeds a live
    // call while the route is `managed` (§2's surfaces table) — under `byok`
    // this clear touches nothing the agents are currently resolving through.
    let used_by = if clearing {
        let mode = load_mode(runtime.id(), runtime.secrets().as_ref())
            .await
            .map_err(ApiError)?;
        composio_used_by(mode, ComposioMode::Managed)
    } else {
        None
    };
    if clearing
        && !body.confirm_in_use
        && let Some(used_by) = used_by.clone()
    {
        return Err(ApiError(crate::error::OpenCompanyError::InUse {
            message: composio_in_use_message(),
            used_by,
        }));
    }
    store_token(runtime.id(), runtime.secrets().as_ref(), &body.token)
        .await
        .map_err(ApiError)?;
    // The credential decides which Composio entity the backend resolves, so a
    // set / rotate / clear can change which catalog this company gets. Drop the
    // cached one rather than serving the previous account's answer for up to
    // `CATALOG_TTL` — the response below re-reads the status, so the operator
    // sees the new list immediately.
    evict_catalog_cache(runtime);
    // After the store, so the journal records a completed change. An empty
    // value is a clear, not a set — the two are worth telling apart in an
    // audit trail, since one grants access and the other withdraws it.
    let change = if clearing {
        "credential_cleared"
    } else {
        "credential_set"
    };
    journal(&company, change, None).await?;
    // Read once and answered from, rather than read twice: the note below is a
    // statement about the same status this response carries, and deriving the
    // two from separate reads is how a page comes to show a sentence that
    // disagrees with the row underneath it.
    let status = effective_status(runtime).await?;
    let note = if clearing {
        CLEAR_NOTE.to_string()
    } else if matches!(status.mode, ComposioMode::Managed) {
        SWITCH_NOTE.to_string()
    } else {
        // Stored for a route this company is not on. See `INACTIVE_TOKEN_NOTE`.
        INACTIVE_TOKEN_NOTE.to_string()
    };
    Ok(Json(MutationResponse {
        status,
        note,
        // Not probed. This route sets a bearer the *TinyHumans backend*
        // recognises, and there is no cheap call here that distinguishes a bad
        // bearer from a backend that is down — which is the distinction the
        // whole classifier exists to make. Checking it badly would be worse
        // than not checking it.
        advisory: None,
        probe_class: None,
        used_by,
        slots: Vec::new(),
    }))
}

/// `PUT …/composio/api-key` — bring this company's **own** Composio account, or
/// give it back.
///
/// A non-empty `apiKey` stores the key and routes every Composio call straight
/// to `backend.composio.dev` with it; an empty one clears the key and returns
/// the company to the OpenHuman-managed backend. This is the BYOK half of the
/// surface OpenHuman calls `direct` mode.
///
/// **Admin-only**, for the same reason [`set_token`] is: the key decides which
/// Composio account every one of the company's agents acts through, and — since
/// a BYOK account is billed to whoever owns it — who pays for the calls.
///
/// A switch in either direction is a **different tenant**: the providers
/// connected under the old route are not the ones connected under the new one,
/// so the console's cached catalog is dropped here exactly as a token change
/// drops it, and the response carries the freshly-read status so the operator
/// sees the new answer rather than the previous account's.
async fn set_api_key(
    company: AdminScopedCompany,
    Json(body): Json<SetApiKey>,
) -> Result<Json<MutationResponse>, ApiError> {
    let runtime = company.runtime.as_ref();
    let api_key = body.api_key.trim();

    // The mode this write would select, mirroring `store_api_key`'s own rule:
    // a non-empty key means BYOK, an empty one gives the managed route back.
    let requested_mode = if api_key.is_empty() {
        ComposioMode::Managed
    } else {
        ComposioMode::Byok
    };

    // This route writes `composio/mode` — the same fact the account-key
    // fan-out's in-use check (`company_key::account_key_used_by`) reads to
    // decide whether an UNCONFIRMED account-key clear may touch
    // `composio/tinyhumans/key` (round-2 review comment 4012457339, keys
    // rework #2306). Without sharing the fan-out's `slot_guard`, a mode
    // switch landing here and a concurrent account-key clear could each see
    // the OTHER's pre-image: the clear sees the OLD mode and decides the
    // managed slot is inactive so an unconfirmed clear is safe, the switch
    // then lands and makes the managed slot active — and now it is keyless,
    // with neither request ever having confirmed that outcome. Taking the
    // same guard here closes the window the same way `set_token` already
    // does for `composio/tinyhumans/key` itself.
    //
    // Held only around the reads/decision below and the final write, never
    // across the network probe: a probe can take seconds, and blocking every
    // other account-key/Composio route on this company for that long — to
    // say nothing of holding a lock across a call to a third party — is the
    // wrong trade. (Lock order, for any future caller that also needs
    // `inference_store::index_lock`: this guard first, never the reverse.)
    let guard = crate::company::company_key::slot_guard(runtime.id()).await;
    let before_mode = load_mode(runtime.id(), runtime.secrets().as_ref())
        .await
        .map_err(ApiError)?;
    // Guarded only when the ROUTE actually changes (in-use-guards.md §2/§6):
    // a rotate (byok → byok, or an empty-clear on a company that was already
    // managed) is never a switch, so `switching` alone decides whether this
    // is a guarded action at all. Separately, `composio_used_by` decides
    // whether that switch has anything to report: `composio/byok/key` only
    // feeds a live call while `before_mode` already reads `byok` — a switch
    // FROM managed TO byok (`before_mode == Managed`) sets a key nothing was
    // resolving through yet, so it reports nothing, matching §2's rule that
    // `surfaces` fires only when the mode already selects the slot being
    // touched.
    let switching = before_mode != requested_mode;
    let used_by = if switching {
        composio_used_by(before_mode, ComposioMode::Byok)
    } else {
        None
    };
    if switching
        && !body.confirm_in_use
        && let Some(used_by) = used_by.clone()
    {
        return Err(ApiError(crate::error::OpenCompanyError::InUse {
            message: composio_in_use_message(),
            used_by,
        }));
    }
    drop(guard);

    // Probe the DRAFT, before anything is written. The clear path is never
    // probed — withdrawing a credential is always allowed, and there would be
    // nothing to check — and `skipVerify` is the operator's explicit opt-out.
    // Unguarded (see above): this is the network round trip the lock must
    // never be held across.
    let probe = if api_key.is_empty() || body.skip_verify {
        None
    } else {
        classified_probe(runtime, api_key).await
    };
    // Composio rejected the key: nothing is stored, so the company's mode, its
    // existing key and its cached catalog are all exactly as they were — there
    // is no rollback here because there was no write. Not journaled either:
    // nothing about what this company connects through changed.
    if let Some(class) = probe
        && class.is_destructive()
    {
        return Err(ApiError(crate::error::OpenCompanyError::InvalidRequest(
            describe(class).to_string(),
        )));
    }

    // Re-acquire, and re-check `composio/mode` before writing: the probe ran
    // unguarded, so a concurrent write to `composio/mode` — another
    // `set_api_key` call — could have landed in that window. `before_mode`
    // above is what this request's `switching`/`used_by`/confirmation
    // decision was computed against; writing over a mode that has since moved
    // would silently apply that stale decision to a different transition than
    // the one actually confirmed. Refuse and ask the caller to retry rather
    // than guess.
    let guard = crate::company::company_key::slot_guard(runtime.id()).await;
    let mode_now = load_mode(runtime.id(), runtime.secrets().as_ref())
        .await
        .map_err(ApiError)?;
    if mode_now != before_mode {
        return Err(ApiError(crate::error::OpenCompanyError::Conflict(
            "Composio's mode changed while this key was being checked; reload and try again."
                .to_string(),
        )));
    }
    let mode = store_api_key(runtime.id(), runtime.secrets().as_ref(), api_key)
        .await
        .map_err(ApiError)?;
    drop(guard);
    evict_catalog_cache(runtime);
    journal(
        &company,
        match mode {
            ComposioMode::Byok => "composio_byok_set",
            ComposioMode::Managed => "composio_byok_cleared",
        },
        None,
    )
    .await?;
    Ok(Json(MutationResponse {
        status: effective_status(runtime).await?,
        note: match mode {
            ComposioMode::Byok => BYOK_NOTE.to_string(),
            ComposioMode::Managed => MANAGED_NOTE.to_string(),
        },
        // A non-destructive class rode through the store: the key is live, and
        // only the check is in doubt.
        advisory: probe.map(|class| describe(class).to_string()),
        probe_class: probe,
        used_by,
        slots: Vec::new(),
    }))
}

/// Ask Composio whether it recognises an API key, and classify the answer.
/// `None` means the check came back clean.
///
/// Two callers, one derivation: [`set_api_key`] checks a **draft** before
/// storing it, and [`test_api_key`] checks the one already **stored**. Both
/// hand the key in directly, so neither needs the key to be resolvable from
/// anywhere, and a second copy of this would be a second place for the two
/// routes' verdicts to drift apart.
///
/// The handlers are auth, shape and delegation; every branch worth a test is
/// either here (one `match`) or in
/// [`composio_probe`](crate::company::composio_probe), which is pure. What this
/// function owns is the one rule that cannot live in either: **the raw upstream
/// reason does not leave this function at all — not to the response, and not
/// to a log.** It can carry a proxy's HTML, a response header, or a key
/// fragment, and the only string that leaves this function is the class.
///
/// It used to go to `debug`, on the theory that a debug log is private. It is
/// not: a debug stream reaches log aggregation, exporters and anyone who can
/// read the host's logs, which is a wider audience than the admin who pasted
/// the key. The class is what diagnosis needs, and it is still logged.
async fn classified_probe(runtime: &CompanyRuntime, api_key: &str) -> Option<ComposioProbeClass> {
    classify_key(runtime.id().as_ref(), api_key).await
}

/// The same check, named by whoever asked rather than by a company.
///
/// First-run setup has no company and still has to answer "is this key any
/// good" before the operator finds out from an empty tool belt. `scope` is the
/// caller's name, used for the log line and for the test-only transport
/// override — the check itself dials Composio's own fixed URL and reads no
/// company at all, which is why a second caller is possible.
pub(crate) async fn classify_key(scope: &str, api_key: &str) -> Option<ComposioProbeClass> {
    #[cfg(test)]
    let outcome = match probe_override::get(scope) {
        Some(forced) => forced,
        None => probe_transport(api_key).await,
    };
    #[cfg(not(test))]
    let outcome = probe_transport(api_key).await;

    match outcome {
        Ok(()) => None,
        Err(raw) => {
            let class = classify(&raw);
            tracing::debug!(
                company = %scope,
                class = %class,
                "[composio] a draft API key did not check out"
            );
            Some(class)
        }
    }
}

/// The probe's network call, split on the feature exactly as [`fetch_catalog`]
/// is.
#[cfg(feature = "composio")]
async fn probe_transport(api_key: &str) -> Result<(), String> {
    crate::harness::composio_direct::probe_api_key(api_key).await
}

/// Without the `composio` feature there is no client to check a key with.
///
/// This reports the key as **un-probeable**, which classifies `unknown` — the
/// non-destructive class — so the key is stored with an advisory. That is the
/// same shape [`fetch_catalog`] takes (an honest `Err` that the caller degrades
/// gracefully from) and it is the only safe direction: refusing the write would
/// make a default build unable to configure BYOK at all, and treating an
/// absent client as a rejected credential would throw away a key nothing ever
/// looked at.
#[cfg(not(feature = "composio"))]
async fn probe_transport(_api_key: &str) -> Result<(), String> {
    Err("Composio is not compiled into this build, so the key could not be checked".to_string())
}

/// Test-only: force [`classified_probe`]'s transport answer for one company.
///
/// The route's three outcomes — rejected, clean, advisory — are the whole point
/// of the change, and two of them cannot be reached from a default build (which
/// has no Composio client) nor from a feature build (which would dial
/// `backend.composio.dev` from a unit test). Keyed by company id rather than
/// held as one global, for the reason `composio_toolkits::cache` is: the test
/// binary runs these concurrently in one process, and a single slot would make
/// them race. `#[cfg(test)]` throughout — there is no seam here in a shipped
/// build.
#[cfg(test)]
#[path = "composio_probe_override.rs"]
pub(crate) mod probe_override;

/// `POST …/composio/api-key/test` — check the **stored** Composio API key and
/// report the verdict. Changes nothing.
///
/// ## It never writes, on any path — including `auth`
///
/// Not even a rejected key is cleared here, and this is the comment that exists
/// so a later edit does not "tidy" this into reuse of [`set_api_key`]'s path.
/// Testing a credential and committing to (or withdrawing from) it are separate
/// acts — the same rule `POST /api/v1/setup/inference/test` already states — and
/// a Test button that deleted a company's key on a bad afternoon would be the
/// worst control on the page: the operator pressed the one thing that promised
/// to be safe. A key an operator wants gone goes through `PUT …/composio/api-key`
/// with an empty value, deliberately, which is also the only path that journals
/// the change.
///
/// Consequently there is no journal line here either. Nothing about what this
/// company connects through changed, and an audit trail that records reads
/// dilutes the one thing it is for.
///
/// ## No body, and there never will be one
///
/// The key is read from the company's own store and the endpoint is the
/// compile-time [`DIRECT_BASE_URL`](crate::company::composio::DIRECT_BASE_URL).
/// A body carrying a key — or worse, an endpoint — would turn an authenticated
/// console route into "send this credential to that host", which is the
/// SSRF-shaped primitive the constant destination currently rules out. A draft
/// key is checked by [`set_api_key`], which has to be handed one anyway.
///
/// ## Admin-only
///
/// It performs no write, so this is not the usual write/read split. It spends
/// the company's own credential against a third party and reports whether that
/// credential is good — an action taken on the company's behalf, and one whose
/// answer (`this company's Composio account is rejected`) is about the
/// company's standing with a vendor rather than about what its agents can do.
/// Reads stay open to members because they carry no credential and trigger no
/// outbound call; this one does the second of those.
async fn test_api_key(company: AdminScopedCompany) -> Result<Json<ApiKeyTestDto>, ApiError> {
    let runtime = company.runtime.as_ref();
    let api_key = stored_api_key(runtime).await?;
    // `NotConfigured`, not the catch-all conflict: "there is nothing here to
    // test" is a permanent state the console reads off the code and renders as
    // a disabled control, and it must stay distinguishable from a check that
    // ran and failed. Same code `resolve_tenant` answers with, for the same
    // distinction.
    let Some(api_key) = api_key else {
        return Err(ApiError(crate::error::OpenCompanyError::NotConfigured(
            "this company is on TinyHumans-managed Composio, so there is no API key to test — \
             paste one to bring its own Composio account"
                .to_string(),
        )));
    };
    Ok(Json(match classified_probe(runtime, &api_key).await {
        None => ApiKeyTestDto {
            ok: true,
            probe_class: None,
            message: None,
        },
        Some(class) => ApiKeyTestDto {
            ok: false,
            probe_class: Some(class),
            // `describe_verdict`, not `describe`: this route stored nothing, and
            // the latter's copy says `Saved, …`.
            message: Some(describe_verdict(class).to_string()),
        },
    }))
}

/// The company's own Composio API key, or `None` when there is nothing to
/// check.
///
/// `None` covers both ways that happens — the company is on the managed route,
/// or it is on BYOK with a blank slot — because the answer the caller gives is
/// the same either way and a second variant would only invite a second
/// sentence. The BYOK-with-no-key case is already a state
/// [`resolve_access`] warns about on the agent path.
async fn stored_api_key(runtime: &CompanyRuntime) -> Result<Option<String>, ApiError> {
    use crate::company::composio::{load_byok_key, load_mode};

    if !load_mode(runtime.id(), runtime.secrets().as_ref())
        .await
        .map_err(ApiError)?
        .is_byok()
    {
        return Ok(None);
    }
    let stored = load_byok_key(runtime.id(), runtime.secrets().as_ref())
        .await
        .map_err(ApiError)?;
    Ok(stored
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty()))
}

/// `POST …/composio/api-key/test` response — a verdict, and never a credential.
///
/// [`Self::ok`] alone answers the console's control. The other two are present
/// only on a failure: the class so the page can decide how to render it, and
/// the fixed copy for the class so it does not have to hold its own table of
/// sentences. Both are omitted rather than nulled, matching every other
/// optional field on this surface.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiKeyTestDto {
    /// Whether Composio answered the check.
    pub(crate) ok: bool,
    /// Why it did not, when it did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) probe_class: Option<ComposioProbeClass>,
    /// The operator-facing sentence for [`Self::probe_class`] — always
    /// [`describe_verdict`]'s fixed copy, never the upstream error text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
}

/// `POST …/composio/tinyhumans/key/from-account` — copy this company's
/// TinyHumans account key (`company_key::KEY_KEY`, set on the Account page)
/// into `composio/tinyhumans/key`, for the reuse banner offered once Composio's
/// own copy is gone but the account key still exists
/// (`docs/key-reworks/phase-4c-reuse-banner.md`, keys rework #2306).
///
/// ## No body, and there never will be one
///
/// The key is read from this company's own store; there is nothing for a
/// request body to name. See [`test_api_key`]'s own "no body" note for the
/// same shape of reasoning against a route that could otherwise be tempted to
/// take one.
///
/// ## Admin-only
///
/// The same boundary [`set_token`] and [`set_api_key`] carry: this decides
/// which account the company's Composio tool calls present.
///
/// Refuses with `400 invalid_request` before any write when there is no
/// account key to copy, or when the Composio slot already holds a different,
/// non-empty key of its own
/// ([`company_key::copy_account_key_to_composio`]'s own refusal table).
/// Never touches `composio/mode` or `composio/byok/key`.
async fn copy_account_key(company: AdminScopedCompany) -> Result<Json<MutationResponse>, ApiError> {
    let runtime = company.runtime.as_ref();
    let report =
        company_key::copy_account_key_to_composio(runtime.id(), runtime.secrets().as_ref())
            .await
            .map_err(ApiError)?;

    // The account key can change which Composio entity the backend resolves,
    // exactly as a direct token/API-key write does — drop the cached catalog
    // so this response (and every read after it) reflects the new credential
    // rather than the previous one's.
    evict_catalog_cache(runtime);

    let filled = report
        .slots
        .iter()
        .any(|s| matches!(s.outcome, SlotOutcome::Filled));
    // P3-3 (keys rework #2306 review): journal only when the copy actually
    // changed stored state, matching 4a §3.5's convention ("one entry per
    // slot whose outcome changed stored state ... no entry for kept, skipped,
    // failed"). `copy_account_key_to_composio`'s own doc comment says its
    // successful report only ever carries `Filled` or `Kept(AlreadyCurrent)`
    // — a `CustomKey` conflict returns `Err` before any `FanOutReport` exists,
    // and this call never rotates or clears — so `filled` is exactly the
    // right and only test: a `Kept` outcome changed nothing, and an audit
    // line for it would misreport "this admin changed something" for an
    // action that did not.
    if filled {
        journal(&company, "company_key_composio_filled", None).await?;
    }

    let note = if filled {
        "Composio now uses your account key. A key you created by hand may lack the \
         connections permission Composio needs."
            .to_string()
    } else {
        "Composio already uses your account key.".to_string()
    };

    Ok(Json(MutationResponse {
        status: effective_status(runtime).await?,
        note,
        advisory: None,
        probe_class: None,
        used_by: None,
        slots: report.slots.iter().map(SlotReportDto::from).collect(),
    }))
}

/// Records who changed the company's tool access (issue #403).
///
/// Propagates a journal failure rather than swallowing it, unlike the
/// best-effort workflow journaling. The point of this record is that a change
/// to what the company's agents connect through is never invisible; an audit
/// line that quietly fails to be written is the one failure mode that would
/// defeat it. `MemoryFactDeleted` — the other write journaled *for* the audit
/// trail — propagates for the same reason.
async fn journal(
    company: &AdminScopedCompany,
    change: &str,
    toolkit: Option<String>,
) -> Result<(), ApiError> {
    company
        .runtime
        .events()
        .append(
            company.id(),
            CompanyEvent::ToolAccessChanged {
                change: change.to_string(),
                toolkit,
                by: Some(company.actor()),
            },
        )
        .await
        .map_err(ApiError)?;
    Ok(())
}

/// `POST …/composio/authorize` — begin a per-provider OAuth handoff and return
/// the Composio-hosted connect URL the operator opens in a browser tab.
///
/// Composio runs the OAuth flow itself — there is **no** local callback route.
/// The console opens the URL and polls [`connections`] until the toolkit
/// reports connected. Under a non-`composio` build this answers `409
/// not_in_build` (see [`router`]).
///
/// **Admin-only** (issue #403). The connection this begins belongs to the
/// company — it is the account its agents will act through from then on — so
/// the person who chooses it has to be someone entitled to choose on the
/// company's behalf. Note that nothing downstream can re-check this: Composio
/// runs its own OAuth with no callback here, so unlike the native connections
/// plane there is no later point at which the connecting identity could be
/// compared against the company's operators. Connect time is the only boundary
/// there is.
async fn authorize(
    company: AdminScopedCompany,
    Json(body): Json<AuthorizeBody>,
) -> Result<Json<AuthorizeDto>, ApiError> {
    let dto = authorize_impl(company.runtime.as_ref(), body.toolkit.clone()).await?;
    // Only once a connect URL was actually minted — a refused or unbuildable
    // authorize changed nothing and does not belong in the trail.
    journal(
        &company,
        "provider_authorization_started",
        Some(body.toolkit),
    )
    .await?;
    Ok(dto)
}

/// `GET …/composio/connections` — the company's per-toolkit connected state,
/// one [`ConnectionDto`] per toolkit that has at least one connection. The
/// console cross-references this against the granted `toolkits` to render each
/// provider row's connected/sign-in state. `409 not_in_build` on a
/// non-`composio` build, and `409 not_configured` when this company has no
/// Composio credential — two permanent states the console reads off the code,
/// because neither is cleared by trying the read again.
async fn connections(company: ScopedCompany) -> Result<Json<Vec<ConnectionDto>>, ApiError> {
    connections_impl(company.runtime.as_ref()).await
}

/// Resolve the per-tenant Composio config (bearer + backend URL + toolkit
/// allowlist) the way the harness roster build does, so the console dials the
/// backend with the exact same tenant identity. A `409 not_configured` when no
/// credential of any tier can be resolved — neither this company's own stored
/// token nor a platform identity — in which case the operator must paste a token
/// before OAuth. Its own code, not the catch-all `conflict`: a caller has to be
/// able to tell "nothing is set up here" from a write that lost a race.
#[cfg(feature = "composio")]
pub(crate) async fn resolve_tenant(
    runtime: &CompanyRuntime,
) -> Result<crate::harness::composio::TenantComposio, ApiError> {
    use crate::app::config::EnvSource;

    let record = runtime.store().load(runtime.id()).await.map_err(ApiError)?;
    let toolkits = record
        .map(|record| record.manifest.tools.composio.toolkits.clone())
        .unwrap_or_default();
    let env = crate::app::config::ProcessEnv;
    let api_env = env.get(crate::company::composio::TINYHUMANS_API_URL_ENV);
    // Resolved here rather than through `TenantComposio::resolve` so a store
    // read failure stays distinguishable from "nothing configured". This is the
    // path that *establishes* a connection, and a connection lives on the
    // backend keyed by the account the bearer resolves to — so guessing here
    // would attribute a company's Gmail to whichever identity happened to
    // resolve during the outage. The roster path can afford to shrug and
    // withhold tools; this one must say what went wrong and connect nothing.
    let access = resolve_access(
        runtime.id(),
        runtime.secrets().as_ref(),
        crate::company::TinyhumansTokenSource::from_env(&env).map(std::sync::Arc::new),
    )
    .await
    .map_err(ApiError)?;
    if !access.credential.configured() {
        // The two routes fail for different reasons and are recoverable in
        // different places, so they say different things. A BYOK company told
        // to "set its TinyHumans credential" would be sent to a control that
        // has no effect on the route it chose.
        return Err(ApiError(crate::error::OpenCompanyError::NotConfigured(
            match access.mode {
                ComposioMode::Byok => "this company uses its own Composio account but no Composio \
                     API key is stored — paste one, or clear it to go back to TinyHumans-managed \
                     Composio"
                    .to_string(),
                ComposioMode::Managed => "no Composio credential is available for this company — \
                     set the company's TinyHumans credential, or paste its own Composio token"
                    .to_string(),
            },
        )));
    }
    Ok(crate::harness::composio::TenantComposio::from_access(
        backend_url_or_default(api_env),
        access,
        toolkits,
    ))
}

#[cfg(feature = "composio")]
async fn authorize_impl(
    runtime: &CompanyRuntime,
    toolkit: String,
) -> Result<Json<AuthorizeDto>, ApiError> {
    let config = resolve_tenant(runtime).await?;
    let connect_url = crate::harness::composio::authorize_connect_url(&config, &toolkit)
        .await
        .map_err(|err| {
            ApiError(crate::error::OpenCompanyError::TinyHumans {
                code: "composio_authorize".to_string(),
                message: err.to_string(),
            })
        })?;
    Ok(Json(AuthorizeDto { connect_url }))
}

#[cfg(feature = "composio")]
async fn connections_impl(runtime: &CompanyRuntime) -> Result<Json<Vec<ConnectionDto>>, ApiError> {
    let config = resolve_tenant(runtime).await?;
    let rows = crate::harness::composio::list_connections_detailed(&config)
        .await
        .map_err(|err| {
            ApiError(crate::error::OpenCompanyError::TinyHumans {
                code: "composio_connections".to_string(),
                message: err.to_string(),
            })
        })?;
    let defaults =
        crate::company::composio::load_defaults(runtime.id(), runtime.secrets().as_ref())
            .await
            .map_err(ApiError)?;
    let defaults = drop_dangling_defaults(runtime, &rows, defaults).await?;
    Ok(Json(group_by_toolkit(rows, &defaults)))
}

/// Drop every stored choice naming a connection `rows` does not contain, and
/// return what is left (issue #820).
///
/// Housekeeping on the one path that holds both halves of the answer. Such a pin
/// is not merely stale — it would be sent on the next `composio_execute` and
/// refused, so an account revoked outside this console (at Composio itself)
/// would silently break the toolkit for every agent. The console polls this
/// route, so the repair lands on its own. Deliberately a *removal of a dangling
/// reference* and nothing else: it cannot pick a different account, only stop
/// naming one that is gone.
///
/// Split out from the handler for the same reason [`group_by_toolkit`] is: the
/// decision is testable without standing up a Composio backend, and the rows a
/// test has to invent are exactly the ones the handler was handed.
#[cfg(feature = "composio")]
async fn drop_dangling_defaults(
    runtime: &CompanyRuntime,
    rows: &[crate::harness::composio::ComposioConnectionRow],
    defaults: crate::company::composio::ComposioDefaults,
) -> Result<crate::company::composio::ComposioDefaults, ApiError> {
    let live: std::collections::BTreeSet<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    // Iterated over a snapshot, one write per stale toolkit, and `defaults` is
    // reassigned from each write rather than mutated here: `clear_default`
    // re-reads the stored blob and returns the whole map as it now stands, so
    // the last iteration's return is the fully reduced map and the answer this
    // returns cannot drift from what was actually persisted. With nothing stale
    // the loop does not run and the map passed in is returned untouched — no
    // write on the ordinary read.
    let mut defaults = defaults;
    for (toolkit, id) in defaults
        .clone()
        .into_iter()
        .filter(|(_, id)| !live.contains(id.as_str()))
    {
        tracing::info!(
            company = %runtime.id(),
            toolkit = %toolkit,
            connection_id = %id,
            "[composio] the chosen account no longer exists at Composio; clearing the choice"
        );
        defaults = crate::company::composio::clear_default(
            runtime.id(),
            runtime.secrets().as_ref(),
            &toolkit,
        )
        .await
        .map_err(ApiError)?;
    }
    Ok(defaults)
}

/// Fold per-connection rows into the per-toolkit response shape.
///
/// `connected` is `true` when **any** account for the toolkit is active — the
/// same rule the pre-#404 route applied, kept here so the boolean the tile grid
/// reads cannot drift from what it meant before the accounts were added.
///
/// A `BTreeMap` gives the toolkit ordering the route has always had; the rows
/// arrive already sorted by `(toolkit, id)`, so the per-toolkit account order is
/// stable too. Split out from the handler so it is testable without a live
/// Composio backend.
#[cfg(feature = "composio")]
fn group_by_toolkit(
    rows: Vec<crate::harness::composio::ComposioConnectionRow>,
    defaults: &crate::company::composio::ComposioDefaults,
) -> Vec<ConnectionDto> {
    let mut by_toolkit: std::collections::BTreeMap<String, ConnectionDto> =
        std::collections::BTreeMap::new();
    for row in rows {
        let chosen = defaults.get(&row.toolkit).map(String::as_str);
        let entry = by_toolkit
            .entry(row.toolkit.clone())
            .or_insert_with(|| ConnectionDto {
                toolkit: row.toolkit.clone(),
                connected: false,
                accounts: Vec::new(),
                default_connection_id: chosen.map(str::to_string),
            });
        entry.connected = entry.connected || row.connected;
        entry.accounts.push(ConnectedAccountDto {
            is_default: chosen == Some(row.id.as_str()),
            id: row.id,
            status: row.status,
            connected: row.connected,
            created_at: row.created_at,
            account: row.account,
        });
    }
    by_toolkit.into_values().collect()
}

/// `DELETE …/composio/connections/{id}` — revoke one connected account
/// (issue #404).
///
/// **Admin-only** (issue #403), for the same reason `authorize` is: the
/// connection belongs to the company, so removing the account its agents act
/// through is a decision made on the company's behalf. Journaled on success
/// only, alongside the connect it reverses.
///
/// What this does and does not revoke matters, and the console says so before
/// asking: it removes the connection **at Composio**, so agents lose the
/// capability on their next turn. It does not sign the company out of the
/// provider, and it does not touch the native `oauth/{provider}` catalog entry,
/// which is a separate credential this plane has never owned.
async fn disconnect(
    company: AdminScopedCompany,
    Path(ConnectionPath { connection_id }): Path<ConnectionPath>,
) -> Result<Json<DisconnectDto>, ApiError> {
    let dto = disconnect_impl(company.runtime.as_ref(), &connection_id).await?;
    journal(&company, "provider_disconnected", None).await?;
    Ok(dto)
}

/// The sub-resource path (`connection_id`); the scope `id` is consumed by the
/// extractor.
#[derive(Debug, Deserialize)]
struct ConnectionPath {
    connection_id: String,
}

/// `DELETE …/composio/connections/{id}` response. A body rather than a bare 204
/// so the console can state what happened in the same words the host used.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DisconnectDto {
    /// Plain-language confirmation, scoped to what was actually revoked.
    note: String,
}

#[cfg(feature = "composio")]
async fn disconnect_impl(
    runtime: &CompanyRuntime,
    connection_id: &str,
) -> Result<Json<DisconnectDto>, ApiError> {
    use crate::harness::composio::DisconnectError;

    let config = resolve_tenant(runtime).await?;
    crate::harness::composio::delete_connection(&config, connection_id)
        .await
        // An id this company cannot see is a `404`, not a `502`. Both were the
        // latter until the route was actually run: the console would have told
        // an operator the provider was unreachable about a call that never left
        // the host, and a retry — the obvious response to a bad gateway — would
        // fail identically forever.
        .map_err(|err| match err {
            DisconnectError::NotFound(message) => {
                ApiError(crate::error::OpenCompanyError::NotFound(message))
            }
            DisconnectError::Upstream(err) => {
                ApiError(crate::error::OpenCompanyError::TinyHumans {
                    code: "composio_disconnect".to_string(),
                    message: err.to_string(),
                })
            }
        })?;
    // A pin naming the account just revoked would be sent on the next execute
    // and refused — so disconnecting the account a company *did not* choose
    // must not be what breaks the one it did (issue #820).
    crate::company::composio::forget_connection(
        runtime.id(),
        runtime.secrets().as_ref(),
        connection_id,
    )
    .await
    .map_err(ApiError)?;
    Ok(Json(DisconnectDto {
        note: "Disconnected at Composio. Agents lose these tools on their next turn.".to_string(),
    }))
}

#[cfg(not(feature = "composio"))]
async fn disconnect_impl(
    _runtime: &CompanyRuntime,
    _connection_id: &str,
) -> Result<Json<DisconnectDto>, ApiError> {
    Err(not_in_build())
}

/// `PUT …/composio/connections/{id}/default` — make that account the one this
/// company's agents act as for its toolkit (issue #820).
///
/// **Admin-only** (issue #403), for the reason `authorize` and `disconnect` are:
/// "send from billing@, not ops@" is a decision about what the company does, not
/// a per-operator preference — every agent in the company acts through the one
/// answer.
///
/// The account is named by **connection id**, not by toolkit-plus-id, because
/// the toolkit is already a property of the connection: asking the caller to
/// repeat it would invite the two to disagree, and the id alone is what the
/// console has in hand from `GET …/connections`.
async fn set_default(
    company: AdminScopedCompany,
    Path(ConnectionPath { connection_id }): Path<ConnectionPath>,
) -> Result<Json<DefaultDto>, ApiError> {
    let dto = set_default_impl(company.runtime.as_ref(), &connection_id).await?;
    journal(
        &company,
        "provider_default_account_set",
        Some(dto.0.toolkit.clone()),
    )
    .await?;
    Ok(dto)
}

/// `DELETE …/composio/connections/{id}/default` — stop naming an account for
/// that connection's toolkit, returning it to Composio's own resolution.
///
/// Unlike [`set_default`] this makes **no upstream call** and validates nothing
/// against Composio: the whole point of clearing is to be able to undo a pin
/// when the account is gone or the provider is unreachable, which is exactly
/// when a validating clear would refuse. It removes any pin naming this id and
/// says so; a request for an id that was never pinned is a no-op, not an error.
async fn clear_default(
    company: AdminScopedCompany,
    Path(ConnectionPath { connection_id }): Path<ConnectionPath>,
) -> Result<Json<DefaultDto>, ApiError> {
    let cleared = crate::company::composio::forget_connection(
        company.runtime.id(),
        company.runtime.secrets().as_ref(),
        &connection_id,
    )
    .await
    .map_err(ApiError)?;
    if cleared {
        journal(&company, "provider_default_account_cleared", None).await?;
    }
    Ok(Json(DefaultDto {
        toolkit: String::new(),
        connection_id: None,
        note: if cleared {
            "Cleared. Composio picks the account for this provider again, as it did before."
                .to_string()
        } else {
            "That account was not the default; nothing changed.".to_string()
        },
    }))
}

/// The `…/default` response: what is now pinned, and a sentence saying so.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DefaultDto {
    /// The toolkit the change applied to. Empty on a clear, where the caller
    /// named a connection rather than a toolkit and may be clearing a pin for
    /// an account that no longer exists.
    toolkit: String,
    /// The account now acting for that toolkit — `None` after a clear.
    #[serde(skip_serializing_if = "Option::is_none")]
    connection_id: Option<String>,
    /// Plain-language confirmation, in the same words the console repeats.
    note: String,
}

#[cfg(feature = "composio")]
async fn set_default_impl(
    runtime: &CompanyRuntime,
    connection_id: &str,
) -> Result<Json<DefaultDto>, ApiError> {
    use crate::harness::composio::DisconnectError;

    let config = resolve_tenant(runtime).await?;
    let toolkit = crate::harness::composio::set_default_connection(
        &config,
        runtime.id(),
        runtime.secrets().as_ref(),
        connection_id,
    )
    .await
    // Same split as `disconnect`: an id this company cannot see is a `404`
    // about a call that never left the host, not a `502` about a provider that
    // is up.
    .map_err(|err| match err {
        DisconnectError::NotFound(message) => {
            ApiError(crate::error::OpenCompanyError::NotFound(message))
        }
        DisconnectError::Upstream(err) => ApiError(crate::error::OpenCompanyError::TinyHumans {
            code: "composio_set_default".to_string(),
            message: err.to_string(),
        }),
    })?;
    Ok(Json(DefaultDto {
        note: format!(
            "Agents act as this account for {toolkit} from their next turn. Other accounts stay \
             connected."
        ),
        toolkit,
        connection_id: Some(connection_id.to_string()),
    }))
}

#[cfg(not(feature = "composio"))]
async fn set_default_impl(
    _runtime: &CompanyRuntime,
    _connection_id: &str,
) -> Result<Json<DefaultDto>, ApiError> {
    Err(not_in_build())
}

/// A `409 not_in_build` "Composio is not in this build" — the OAuth plane's
/// off-state under a non-`composio` build. Mirrors the status route's
/// `inBuild:false` semantics rather than pretending nothing is connected.
///
/// The code is what the console keys on. Under the catch-all `conflict` this
/// answer was indistinguishable on the wire from a lost publish race, so the
/// only reading available to a caller was the recoverable one — and the Apps
/// page told every operator on a default build to reload a section no reload
/// could ever fill.
#[cfg(not(feature = "composio"))]
fn not_in_build() -> ApiError {
    ApiError(crate::error::OpenCompanyError::NotInBuild(
        "Composio is not compiled into this build".to_string(),
    ))
}

#[cfg(not(feature = "composio"))]
async fn authorize_impl(
    _runtime: &CompanyRuntime,
    _toolkit: String,
) -> Result<Json<AuthorizeDto>, ApiError> {
    Err(not_in_build())
}

#[cfg(not(feature = "composio"))]
async fn connections_impl(_runtime: &CompanyRuntime) -> Result<Json<Vec<ConnectionDto>>, ApiError> {
    Err(not_in_build())
}

#[cfg(test)]
#[path = "composio_test_support.rs"]
mod composio_test_support;
#[cfg(test)]
#[path = "composio_an_empty_toolkit_list_tests.rs"]
mod tests_an_empty_toolkit_list;
#[cfg(test)]
#[path = "composio_clearing_the_managed_token_tests.rs"]
mod tests_clearing_the_managed_token;
#[cfg(test)]
#[path = "composio_credential_source_matrix_follows_tests.rs"]
mod tests_credential_source_matrix_follows;
#[cfg(test)]
#[path = "composio_set_default_route_conflicts_tests.rs"]
mod tests_set_default_route_conflicts;
#[cfg(test)]
#[path = "composio_the_api_key_test_tests.rs"]
mod tests_the_api_key_test;
#[cfg(test)]
#[path = "composio_the_managed_tier_reads_tests.rs"]
mod tests_the_managed_tier_reads;
