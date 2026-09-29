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
use crate::company::IMPLICIT_HARNESS_ID;
use crate::company::Inference;
use crate::company::inference::catalogue;
use crate::company::inference::{
    self, EnvDefault, InferenceSource, RuntimeInference, clear_runtime_config, resolve_effective,
    save_runtime_config, store_key, validate_runtime,
};
use crate::company::runtime::CompanyRuntime;
use crate::error::OpenCompanyError;
use crate::ports::UsageMetering;
use crate::server::cognition::InferenceResolution;
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

// Keys rework (#2306), slice 4a: `pub(crate)` (not the default private) so
// `company_key::fan_out` can reach `catalogue_offer` without a second copy of
// "sort, dedupe, cap" — see that function's own doc comment.
pub(crate) mod providers;

/// Builds the inference management route fragment.
pub fn router() -> Router<AppState> {
    scoped(
        "/inference",
        get(get_status).put(set_config).delete(revert_config),
    )
    .merge(scoped("/inference/models", get(list_models)))
    .merge(scoped("/inference/test", post(test_config)))
    .merge(scoped("/inference/restart", post(restart_runtime)))
    // Add, edit, delete, enable/disable, the draft probe and the routing table.
    // A module of its own because this file is already the read plane plus the
    // legacy single-provider write, and the seam between "what is configured"
    // and "change what is configured" is the one worth splitting on.
    .merge(providers::router())
}

/// What `GET …/inference/models` answers: the catalog **this company's endpoint**
/// publishes, and what that catalog says about how tiers must be spelled for it.
///
/// The route used to answer with a bare array, and that array was always
/// OpenRouter's public registry — whatever endpoint the company had been pointed
/// at. An operator on a TinyHumans base URL was shown 421 OpenRouter models,
/// picked `anthropic/claude-sonnet-5` from them because the console offered it,
/// and got `Model 'anthropic/claude-sonnet-5' is not available` from a provider
/// that publishes `chat-v1` and `agentic-v1` instead.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelCatalogDto {
    /// The endpoint the catalog was read from — the same URL
    /// [`InferenceStatusDto::base_url`] reports, so the console can say *whose*
    /// list this is instead of implying a vendor.
    base_url: String,
    /// Every model the endpoint publishes, sorted. Empty when `error` is set.
    models: Vec<crate::server::inference_models::InferenceModel>,
    /// Why the catalog is empty, in the operator's words, or `null` on success.
    ///
    /// Carried in a 200 rather than raised as a 500 on purpose: an empty picker
    /// with no explanation reads as "this provider has no models", which is a
    /// claim we have not established. "Could not list models from
    /// `<endpoint>`" is a true statement and leaves the operator able to type an
    /// id by hand, which is exactly what they should do next.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// The endpoint a company's requests actually travel to, and the credential they
/// carry — resolved the same way [`effective_status_with`] resolves `base_url`,
/// so the catalog is read from the endpoint the turns use.
///
/// Resolved *with* the platform default in place: a `managed`/keyless
/// `openrouter` company inherits the platform endpoint and its credential, and
/// reading the catalog without them would list the wrong endpoint's models on
/// exactly the companies that never configured anything.
async fn resolved_endpoint(
    runtime: &CompanyRuntime,
) -> Result<
    Option<(
        String,
        Option<String>,
        catalogue::AuthStyle,
        catalogue::CatalogShape,
    )>,
    ApiError,
> {
    let (manifest, _harness_id) = manifest_inference(runtime).await?;
    let secrets = runtime.secrets().as_ref();
    let platform = platform_default(runtime);
    let Some(decl) = resolve_effective(runtime.id(), &manifest, platform.as_ref(), secrets)
        .await
        .map_err(ApiError)?
    else {
        return Ok(None);
    };
    let bearer = decl.bearer().await.map_err(ApiError)?;
    // Carried alongside the credential, because the two are one decision: a
    // value and the header it belongs in. Splitting them is how the catalog
    // read came to send every provider a bearer.
    let auth = catalogue::auth_style_for(&decl.provider);
    // Keys rework (#2306), slice 2a: the shape this endpoint's `/models`
    // answers in, so a `tinyhumans` row (or an env default already pointed
    // at the proxy) reads its paged envelope rather than the OpenAI shape.
    let shape = catalogue::catalog_shape_for(&decl.provider, &decl.base_url);
    Ok(Some((decl.base_url.clone(), bearer, auth, shape)))
}

/// `GET …/inference/models` — the model catalog of the endpoint **this company**
/// is configured against, cached per endpoint.
///
/// Every OpenAI-compatible provider publishes `GET {base_url}/models`, so
/// discovery follows the configured base URL rather than assuming a vendor. The
/// company's stored key is read host-side and presented as the bearer: it is
/// write-only to the console (`keyConfigured` is all the console ever sees), so
/// this route is the only place that can ask an authenticated endpoint what it
/// serves.
async fn list_models(company: ScopedCompany) -> Result<Json<ModelCatalogDto>, ApiError> {
    let runtime = company.runtime.as_ref();
    let Some((base_url, bearer, auth, shape)) = resolved_endpoint(runtime).await? else {
        // Nothing resolves — not even a platform default on this host. There is
        // no endpoint to ask, and saying so beats listing some other vendor's
        // catalog as if it were this company's.
        return Ok(Json(ModelCatalogDto {
            base_url: String::new(),
            models: Vec::new(),
            error: Some(
                "No inference endpoint is configured for this company, so there is no model \
                 catalog to list. Save a provider first."
                    .to_string(),
            ),
        }));
    };

    // Scoped to this company: an authenticated catalog read is not a public
    // property of the endpoint, so its cache entry must not be handed to another
    // company on the same URL (CodeRabbit security review on #2045).
    match crate::server::inference_models::catalog_models(
        &base_url,
        bearer.as_deref(),
        Some(runtime.id().as_ref()),
        auth,
        shape,
    )
    .await
    {
        Ok(models) => Ok(Json(ModelCatalogDto {
            base_url: catalogue::redact_endpoint(&base_url),
            models,
            error: None,
        })),
        Err(error) => Ok(Json(ModelCatalogDto {
            // Redacted, not raw. `reqwest` masks userinfo in its own `Display`,
            // but this `format!` re-adds it from the endpoint we hold — which
            // is how a stored `http://user:password@host/v1` came to be printed
            // in full in a banner an operator screenshots into a ticket.
            error: Some(format!(
                "Could not list models from {endpoint}: {error}. Enter model ids directly.",
                endpoint = catalogue::redact_endpoint(&base_url)
            )),
            base_url: catalogue::redact_endpoint(&base_url),
            models: Vec::new(),
        })),
    }
}

/// The company's effective inference status as the console renders it. **Never**
/// carries a credential — only a non-secret `keyConfigured` flag.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InferenceStatusDto {
    /// Provider kind (`managed` / `openrouter` / `openai_compatible` / `ollama`)
    /// **as the operator selected it**, not as it resolves.
    ///
    /// The two differ only for the managed route, and reporting the resolved
    /// kind here is what made "Managed (TinyHumans)" impossible to select: the
    /// card seeds its provider select from this field verbatim (deliberately —
    /// it is what keeps the select and the header beside it from ever naming
    /// different providers), so a `managed` save that read back as `openrouter`
    /// snapped the select to OpenRouter and took the Connect-TinyHumans button,
    /// which only the managed route offers, with it. The save had landed; there
    /// was simply no way to see it. See
    /// [`selected_kind`](inference::selected_kind).
    provider: String,
    /// Whether the saved config rides the platform's subscription proxy rather
    /// than a credential this tenant supplied.
    ///
    /// The console used to re-derive this from `provider` and `keyConfigured`
    /// (`!(provider == "openrouter" && keyConfigured)`) — a restatement of
    /// [`InferenceDecl::is_proxied`](inference::InferenceDecl::is_proxied) that
    /// only held while `provider` was the *resolved* kind. Now that it is the
    /// selected one, that derivation would read a managed company with its own
    /// OpenRouter key as proxied and point it at top-up links for an account its
    /// turns are not billed to. Reported rather than re-derived.
    proxied: bool,
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
    /// Whether the harness cognition path is reachable on this host at all (the
    /// `openhuman` feature compiled in and a harness pool attached at boot).
    ///
    /// `false` means no model configuration can ever put this company on the
    /// design path, so the console's "set up a model" call-to-action would be a
    /// dead end — the setup dialog uses this to omit it rather than send the
    /// operator round a redesign loop that cannot end.
    harness_reachable: bool,
    /// Whether this company can run a **profile design pass** — the one behind
    /// `POST {scope}/team/design` and the two `/team/…/draft` routes.
    ///
    /// Reported because the console had no way to ask, and was inferring it
    /// from [`Self::cognition`]: the reduced Add-teammate dialog treated every
    /// path but `echo` as able to draft. That is wrong for three of the six.
    /// `profile_drafter()` is built from `workflow_harness_deps`, which
    /// `RuntimeBuilder` assigns in exactly one place — inside the embedded
    /// harness arm — so `hosted`, `sidecar` and `custom` companies have no
    /// drafter either, and every one of their creates went: type a sentence,
    /// press Create, wait on a model call that could only answer `no_model`,
    /// then meet the full form and fill it in by hand.
    ///
    /// Distinct from [`Self::harness_reachable`], which is
    /// `runtime.harness().is_some()` — the pool being *attached*, not the
    /// company having *booted onto* it. A company whose config failed to
    /// resolve at boot reports `harness_reachable: true` and has no drafter.
    designs_profiles: bool,
    /// Whether this host can rebuild a company's runtime in place, so the
    /// console may offer the restart instead of only naming it (issue #1736).
    ///
    /// [`Self::restart_required`] says a restart is needed; this says whether
    /// the console is allowed to offer to perform one. They are independent
    /// facts and the card had only the first, so it rendered a "Restart now"
    /// button on hosts where `POST …/inference/restart` can only answer "this
    /// host cannot rebuild a company runtime in place; restart the process to
    /// pick up the new configuration". The operator was told a restart was
    /// required, handed the control for it, and the control could never work.
    ///
    /// Derived from [`AppState::can_rebuild_in_place`] rather than inferred
    /// from the deployment shape: the rebuilder is wired by the binary, and
    /// only the binary knows whether it wired one.
    can_rebuild_in_place: bool,
    /// Every provider this company holds, entry zero first.
    ///
    /// **Additive, and it has to stay that way.** This DTO is the "can this
    /// company think?" oracle for four surfaces that are not about inference at
    /// all — the setup dialog, the agent detail view, the copilot panel and the
    /// workflow create dialog — so every field above keeps its exact meaning. A
    /// company with one provider reports a list of one, which is the truth and
    /// already more than the single form ever said.
    ///
    /// Carries `key_configured` per entry and **never a credential**. See
    /// [`ProviderDto`].
    providers: Vec<ProviderDto>,
    /// The routing table: abstract tier → the route string an operator types
    /// (`acme:gpt-5`, `managed`, `local:llava`). A tier absent from the map is
    /// unset and resolves through the primary — never through a sibling's
    /// provider.
    ///
    /// **Additive, like `providers`.** The four non-inference readers of this
    /// DTO do not look at it, and every field above keeps its exact meaning.
    routes: BTreeMap<String, String>,
    /// What the **managed** brain would resolve to, and who pays for it.
    managed: ManagedDto,
    /// The stored company default (keys rework, issue #2306, slice 2c):
    /// `None` when unset; `model: None` when it is a legacy bare-slug
    /// default (Q1 — never rewritten by anything but an explicit
    /// set-default). No `skip_serializing_if`: `null` on the wire is itself
    /// the "no default" answer, distinct from the field being missing on an
    /// older host.
    default_choice: Option<DefaultChoiceDto>,
    /// Round-3a review P2-4: `true` when `inference/default` holds something
    /// that could not be read — a store error, or a value that failed to
    /// parse — so `default_choice` above reads `null` (unset) even though the
    /// operator may have set one. Never written back and never a 500: see
    /// [`inference::store::load_default_lenient`]'s own doc. Delete, disable
    /// and key clear all keep working while this is `true`.
    default_unreadable: bool,
}

/// The company's stored `{provider, model}` default, on the wire (`store::DefaultChoice`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DefaultChoiceDto {
    provider: String,
    model: Option<String>,
    /// Round-3a review P2-3: `true` when this is a **full** `{provider,
    /// model}` default and the provider it names is missing or switched off.
    /// F6 means a turn never falls back to a different provider in that
    /// case — it fails closed — so `false` here is not "this default is
    /// fine", only "nothing here contradicts the turn path"; see
    /// [`default_full_broken`]. Always `false` for a bare-slug
    /// (`ProviderOnly`) default: D-legacy's fallback to the first enabled
    /// provider *is* the real turn-time behaviour there.
    broken: bool,
}

/// Maps a parsed [`inference::store::DefaultChoice`] to the wire shape, pure
/// so the bare-slug and unset cases are unit-tested without a store.
fn default_choice_dto(
    choice: &inference::store::DefaultChoice,
    broken: bool,
) -> Option<DefaultChoiceDto> {
    use inference::store::DefaultChoice;
    match choice {
        DefaultChoice::Unset => None,
        DefaultChoice::ProviderOnly(provider) => Some(DefaultChoiceDto {
            provider: provider.clone(),
            model: None,
            broken: false,
        }),
        DefaultChoice::Full(c) => Some(DefaultChoiceDto {
            provider: c.provider.clone(),
            model: Some(c.model.clone()),
            broken,
        }),
    }
}

/// Round-3a review P2-3: whether a **full** default names a provider this
/// company no longer holds, or holds switched off.
///
/// Only a full default can be "broken" by this definition. A bare-slug
/// (`ProviderOnly`) default naming a gone or disabled provider is a
/// different, already-handled case: D-legacy keeps its pre-existing
/// behaviour of falling back to the first enabled provider there, so a row
/// claiming `isDefault` in that fallback is describing the truth, not
/// contradicting it. `resolve_for_turn`'s F6 fail-closed rule is what makes a
/// **full** default different: nothing falls back to it, ever, so a status
/// read that let some other row claim `isDefault` in its place — or said
/// nothing was wrong — would describe a company that can think when every
/// unpinned turn on it fails with `copy::default_broken`.
fn default_full_broken<'a>(
    default: &inference::store::DefaultChoice,
    mut providers: impl Iterator<Item = (&'a str, bool)>,
) -> bool {
    match default {
        inference::store::DefaultChoice::Full(choice) => {
            !providers.any(|(slug, enabled)| slug == choice.provider && enabled)
        }
        _ => false,
    }
}

/// The managed tier's honest state.
///
/// It exists because the row for it used to carry a permanent "Always on"
/// badge, inherited from a design where the same company runs the managed
/// backend. Here the managed tier needs a credential and can resolve to
/// nothing — and a row claiming availability while agents cannot think is the
/// failure the five-state `CognitionState` exists to prevent.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagedDto {
    /// `provider_key` / `company_account` / `instance` / `none`.
    ///
    /// The last two are kept apart on purpose: one bills the company's own
    /// account and the other bills whoever runs the server, and an operator
    /// deciding whether to connect their account needs to know which they are
    /// on.
    source: String,
    /// Whether it can be reached at all. Derived from `source`, carried so the
    /// console does not re-derive it and disagree.
    configured: bool,
    /// The endpoint managed requests travel to — the platform's own.
    base_url: String,
    /// Whether it is a routing target.
    ///
    /// A provider like any other in this one respect: "stop routing work here"
    /// and "remove the credential" are different statements, and managed can be
    /// told the first without the second. Switching it off leaves every step of
    /// its chain exactly where it was.
    enabled: bool,
    /// What was last learnt about reaching it, if anything. Same rule as a
    /// provider row's: silent until something has actually been learnt.
    #[serde(skip_serializing_if = "Option::is_none")]
    health: Option<ProviderHealthDto>,
    /// Whether the console renders this separate legacy Managed row (keys
    /// rework, issue #2306, slice 2a). `false` once `providers` already lists
    /// a `tinyhumans` row — an added row, or entry zero on a managed config —
    /// because that row is then the one TinyHumans row this page ever shows
    /// (decision Q3). Computed in `effective_status_with`, which has the
    /// provider list this function does not.
    legacy_row: bool,
    /// Whether this legacy row resolves to a credential but has no model
    /// explicitly chosen for it anywhere in the new sense (decision
    /// D-key-without-row / X5, 2026-09-15): `provider/tinyhumans/key` set
    /// with no `tinyhumans` row is a credential, not a configured provider —
    /// D-set's "set ⇔ a row exists" is unchanged, so this must never read as
    /// connected/healthy the way an indexed row with a chosen model does.
    /// `true` whenever `legacy_row && configured`: every source this chain
    /// can resolve through (a key, the company account, the instance
    /// identity) sends whatever the legacy tier-substitution rule decides
    /// rather than an operator-chosen id.
    needs_model: bool,
}

/// One provider on the wire.
///
/// Derives `Serialize` and holds no credential field, which is not a
/// coincidence: the two facts have to be checked together every time this struct
/// is edited. The record it is built from
/// ([`store::Provider`](crate::company::inference::store::Provider)) derives no
/// `Serialize` at all, precisely so that adding a key to it could not silently
/// put one on a wire — and this is the shape that *is* serialized, so the rule
/// lands here as "no key field, ever".
///
/// `key_configured` is derived by asking the store whether a value exists,
/// never by reading a stored flag. A flag goes stale the moment a secret is
/// cleared by another path, and then this tells the console a key exists that
/// does not.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderDto {
    /// Stable, opaque identity. Never shown to an operator.
    id: String,
    /// Routing key — what a routing entry names.
    slug: String,
    /// Display label.
    label: String,
    /// Provider kind.
    kind: String,
    /// Resolved OpenAI-compatible base URL.
    base_url: String,
    /// Abstract tier → concrete model id.
    models: BTreeMap<String, String>,
    /// Whether this is available for routing. Distinct from deleted.
    enabled: bool,
    /// Whether a credential is stored. **Never the credential.**
    key_configured: bool,
    /// Which slot this record physically lives in: `entryZero` or `indexed`.
    ///
    /// **The console could not tell them apart**, and entry zero refuses three
    /// operations with three separate 400s — disable ("cannot be switched off
    /// from the list; reset the inference config instead"), edit ("is changed
    /// through the inference config, not as a list entry") and remove ("is
    /// cleared by resetting the inference config"). Correct rules, and the wrong
    /// place to learn them: the only signal was `id == "prv_entry_zero"`, a
    /// constant nothing outside `store.rs` reads, so the row rendered all three
    /// controls live and every one of them was a round trip to a refusal.
    ///
    /// The rules stay exactly where they are — this is what lets the console
    /// stop offering the controls that cannot work.
    ///
    /// It is not a *kind* — entry zero can be any kind — it is where the record
    /// lives, and that is the thing the write routes branch on. One field rather
    /// than a `legacy` boolean beside it, because two spellings of the same fact
    /// are two things to keep in step.
    origin: &'static str,
    /// Whether this is the provider an **unset** workload goes through.
    ///
    /// The *resolved* answer, not the raw marker: a company that has never said
    /// reports its first enabled provider here, which is what it has always
    /// resolved to. So the console can render "which provider is my default"
    /// without knowing whether it was chosen or inherited — and the operator
    /// sees the same answer either way.
    is_default: bool,
    /// What was last learnt about reaching it, if anything.
    ///
    /// Absent when nothing has been learnt, which is the honest answer: a row
    /// that has never been probed is not a row that is working. The alternative
    /// — a green tick by default — is the state the design this is ported from
    /// is in, where a provider whose key was revoked an hour ago looks identical
    /// to one that works.
    #[serde(skip_serializing_if = "Option::is_none")]
    health: Option<ProviderHealthDto>,
    /// This row's one model, read without guessing (keys rework, issue
    /// #2306, slice 2c). `None` when it has none, or when `modelAmbiguous`
    /// is `true` — never guessed by picking one of several stored ids.
    model: Option<String>,
    /// Two or more distinct ids under the row's tier keys: nobody chose one
    /// model for this row (`store::ModelOnRow::Ambiguous`). The console
    /// shows "Needs a model"; never resolved on its own.
    model_ambiguous: bool,
    /// What else depends on this row (keys rework, issue #2306): the company
    /// default, agents pinned to it. Omitted (not `null`) when nothing does
    /// — see `docs/key-reworks/in-use-guards.md` §1.
    #[serde(skip_serializing_if = "Option::is_none")]
    used_by: Option<crate::error::UsedBy>,
}

/// `(model, model_ambiguous)` from a row's collapsed [`inference::store::ModelOnRow`].
fn model_on_row_dto(row: inference::store::ModelOnRow) -> (Option<String>, bool) {
    use inference::store::ModelOnRow;
    match row {
        ModelOnRow::One(m) => (Some(m), false),
        ModelOnRow::None => (None, false),
        ModelOnRow::Ambiguous(_) => (None, true),
    }
}

/// What the system last learnt about reaching a provider.
///
/// Recorded from things that already happen — the add-time probe and the manual
/// Test — rather than from a poller. A poller costs a request per provider per
/// interval across every company on this host, most of them answering about a
/// provider nobody is using this hour.
///
/// **The turn path does not write here, and this field does not claim it does.**
/// `send_plan` invalidates a credential on a 401 and goes no further, so a key
/// revoked after its last Test leaves this reading whatever that Test found
/// until somebody presses Test again. That is the same honesty the `Option`
/// above is for: absent means nobody has checked, not that it works. Latching
/// `auth` from a real turn needs the secret store threaded into the turn path,
/// which is a feature rather than a wording fix.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderHealthDto {
    /// `ok`, or the probe class of the last failure.
    state: String,
    /// When it was learnt, RFC 3339. Names when the *episode* began rather than
    /// when it was last retried — see [`store::record_health`].
    at: String,
}

/// The provider list for the status DTO.
///
/// A projection over [`store::list_providers`] plus the health map. The records
/// themselves derive no `Serialize`; this is the shape that does, and it holds
/// no credential field. Those two facts have to be checked together every time
/// either is edited.
///
/// `default` is loaded once by the caller (round-3a review P3-6: this used to
/// load it again here via `load_default_slug`, on top of every row's own
/// `provider_used_by` loading it a third time) — passed in rather than
/// re-read, so a status response reads `inference/default` exactly once no
/// matter how many providers this company holds.
async fn provider_list(
    runtime: &CompanyRuntime,
    default: &inference::store::DefaultChoice,
) -> Result<Vec<ProviderDto>, ApiError> {
    use crate::company::inference::store;

    let secrets = runtime.secrets().as_ref();
    let providers = store::list_providers(runtime.id(), secrets)
        .await
        .map_err(ApiError)?;
    let health = store::load_health(runtime.id(), secrets)
        .await
        .map_err(ApiError)?;
    // Round-3a review P2-3: a *full* default whose provider is gone or off
    // fails every turn closed (F6) — nothing falls back to it — so no row may
    // claim `isDefault` in its place. See `default_full_broken`'s own doc.
    let broken = default_full_broken(
        default,
        providers.iter().map(|p| (p.slug.as_str(), p.enabled)),
    );
    // Resolved through the same function the turn path uses, so the row the
    // console marks and the provider a turn actually reaches cannot disagree
    // — except in the `broken` case just above, where nothing actually
    // reaches any row and no row may say otherwise.
    let primary = if broken {
        None
    } else {
        crate::company::inference::resolve::primary(&providers, default.provider())
            .map(|p| p.slug.clone())
    };
    // Round-3a review P2-1 / P3-6: loaded once for the whole list rather than
    // once per row through `provider_used_by`. This is a read, not a guard
    // (see `providers::used_by_from`'s own doc on the fail-closed/degrade
    // split), so a load failure here degrades to "no agents named" with a
    // warning instead of failing the whole status response.
    let record = match runtime.store().load(runtime.id()).await {
        Ok(record) => record,
        Err(err) => {
            tracing::warn!(
                company = %runtime.id(),
                error = %err,
                "could not read the company record while listing providers; showing no agent \
                 usage on any row",
            );
            None
        }
    };
    let mut out = Vec::with_capacity(providers.len());
    for provider in providers {
        let key_configured = store::provider_key_configured(runtime.id(), secrets, &provider)
            .await
            .map_err(ApiError)?;
        let health = health.get(&provider.slug).map(|h| ProviderHealthDto {
            state: h.state.clone(),
            at: h.at.clone(),
        });
        // Read before `provider.models` moves into the struct literal below.
        let (model, model_ambiguous) = model_on_row_dto(provider.model());
        let used_by = providers::used_by_from(default, record.as_ref(), &provider.slug);
        out.push(ProviderDto {
            is_default: primary.as_deref() == Some(provider.slug.as_str()),
            id: provider.id.as_str().to_string(),
            slug: provider.slug,
            label: provider.label,
            kind: provider.kind,
            base_url: catalogue::redact_endpoint(&provider.base_url),
            models: provider.models,
            enabled: provider.enabled,
            key_configured,
            origin: match provider.origin {
                store::ProviderOrigin::EntryZero => "entryZero",
                store::ProviderOrigin::Indexed => "indexed",
            },
            health,
            model,
            model_ambiguous,
            used_by,
        });
    }
    Ok(out)
}

/// The routing table for the status DTO: tier → the route string an operator
/// would type.
///
/// On the status read rather than a route of its own because the console renders
/// the Providers tab and the Routing tab from one load, and a second request for
/// four strings would be a second thing that can be stale relative to the first.
async fn routing_table(runtime: &CompanyRuntime) -> Result<BTreeMap<String, String>, ApiError> {
    use crate::company::inference::store;

    let routes = store::load_routes(runtime.id(), runtime.secrets().as_ref())
        .await
        .map_err(ApiError)?;
    Ok(routes
        .into_iter()
        .map(|(tier, route)| (tier, route.to_route_string()))
        .collect())
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

/// Loads the inference the company actually boots and runs on: the *default
/// harness's* `[harness.inference]` when that harness declares one, falling back
/// to the company-level `[inference]` section. Also returns the default
/// harness's real id, alongside the config, for callers that need to name it —
/// [`test_config`] threads it into [`probe`](crate::harness::provider::probe)
/// so a repair hint on a harness-owned table points at that table rather than
/// the (possibly shadowed) company-level one, the same distinction
/// [`TenantProvider::invoke`](crate::harness::built_in::provider::TenantProvider::invoke)
/// already makes for live turns.
///
/// This mirrors [`RuntimeBuilder::build`](crate::runtime::RuntimeBuilder::build),
/// which resolves `default_harness_inference()` before the company-level
/// fallback. The status, probe, and runner-gap paths all read this, so a company
/// whose only inference lives in `[harness.inference]` must resolve here too —
/// otherwise it would report `managed`, reject `/inference/test` as
/// `not_configured`, and mislabel its status after a reset, while turns run on
/// the harness configuration the same record holds.
async fn manifest_inference(runtime: &CompanyRuntime) -> Result<(Inference, String), ApiError> {
    let record = runtime.store().load(runtime.id()).await.map_err(ApiError)?;
    Ok(record
        .map(|r| {
            let inference = r
                .manifest
                .default_harness_inference()
                .unwrap_or_else(|| r.manifest.inference.clone());
            (inference, r.manifest.default_harness_id())
        })
        .unwrap_or_else(|| (Inference::default(), IMPLICIT_HARNESS_ID.to_string())))
}

/// What resolving this company's inference configuration produced.
///
/// The same three-way read [`runner_gap_for`] performs, lifted out so
/// [`crate::server::ops::capabilities`] can classify cognition for the chat
/// surface (issue #1735) from the identical evidence. One reader, so the two
/// surfaces cannot tell one operator two different next steps about one
/// company: what `restartRequired` and `inference_required` mean on the
/// Inference card is what the chat banner says, by construction.
///
/// "Manifest unreadable" and "resolve failed" are deliberately folded together
/// as [`InferenceResolution::Unreadable`] — the operator can act on neither,
/// and [`runner_gap_for`] already folds them the same way.
pub(crate) async fn inference_resolution(runtime: &CompanyRuntime) -> InferenceResolution {
    let Ok((manifest, _harness_id)) = manifest_inference(runtime).await else {
        return InferenceResolution::Unreadable;
    };
    match resolve_effective(runtime.id(), &manifest, None, runtime.secrets().as_ref()).await {
        Ok(Some(_)) => InferenceResolution::Resolved,
        Ok(None) => InferenceResolution::Nothing,
        Err(_) => InferenceResolution::Unreadable,
    }
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
///
/// Shared with [`crate::server::ops::capabilities`], which asks the same
/// question for the chat surface's cognition state (issue #1735): whether
/// Settings → Inference is a remedy or a dead end is one fact, and two copies
/// of it would let the two surfaces disagree about the same company.
#[cfg(feature = "openhuman")]
pub(crate) fn harness_reachable(runtime: &CompanyRuntime) -> bool {
    runtime.harness().is_some()
}

#[cfg(not(feature = "openhuman"))]
pub(crate) fn harness_reachable(_runtime: &CompanyRuntime) -> bool {
    false
}

/// Whether a profile design pass can actually run for this company.
///
/// The same question `build_design` and `build_draft` ask before they do
/// anything (`server::ops::team_agent`), asked from the one route the console
/// reads at boot — so a dialog can decide its shape from the capability rather
/// than guessing at it from a cognition label. `false` here and `NoModel` there
/// are the same fact, which is the point: two answers to one question is how
/// the console came to offer a reduced dialog on three paths that can only
/// refuse it.
#[cfg(feature = "openhuman")]
pub(crate) fn designs_profiles(runtime: &CompanyRuntime) -> bool {
    runtime.profile_drafter().is_some()
}

/// No harness compiled in, so there is no drafter to build and nothing that
/// could make one — the same unconditional `NoModel` `build_design` answers.
#[cfg(not(feature = "openhuman"))]
pub(crate) fn designs_profiles(_runtime: &CompanyRuntime) -> bool {
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
pub(crate) fn restart_pending(runtime: &CompanyRuntime, configured: bool) -> bool {
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
    let Ok((manifest, _harness_id)) = manifest_inference(runtime).await else {
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

/// This runtime's platform managed default — the same `(base_url,
/// credential)` pair the harness routes on, handed to the runtime by the
/// builder that built its brain
/// ([`CompanyRuntime::platform_default`](crate::company::runtime::CompanyRuntime::platform_default)).
///
/// Read from the runtime and never from the process environment: this used to
/// re-derive it from `OPENCOMPANY_INFERENCE_URL` and the credential variables,
/// which was a second copy of the boot-time resolution — one that did not know
/// the host's `api_url` (a `config.toml` value is not an environment variable),
/// and that was `None` whenever the environment held no credential, at which
/// point every card fell back to the production constant. A desktop pointed at
/// staging said `api.tinyhumans.ai` on the LLM page while its key had been
/// minted on `staging-api`.
///
/// `None` only for a runtime that was never built through the builder (a
/// hand-assembled test runtime): every builder-made runtime has one, with the
/// credential reporting `configured() == false` when the deployment holds no
/// instance identity — which is what every managed gate tests, so a bare
/// endpoint is never advertised as a source that would route somewhere.
fn platform_default(runtime: &CompanyRuntime) -> Option<EnvDefault> {
    runtime.platform_default().cloned()
}

/// Resolves the effective status DTO against the real process environment and
/// this host's own capabilities.
async fn effective_status(
    state: &AppState,
    runtime: &CompanyRuntime,
) -> Result<InferenceStatusDto, ApiError> {
    effective_status_with(
        runtime,
        platform_default(runtime).as_ref(),
        state.can_rebuild_in_place(),
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
    can_rebuild_in_place: bool,
) -> Result<InferenceStatusDto, ApiError> {
    let (manifest, _harness_id) = manifest_inference(runtime).await?;
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
            .map_or_else(|| platform.base_url.clone(), |d| d.base_url),
        // No platform endpoint on this deployment: nothing to inherit, so the
        // tenant resolve already holds the whole answer and the second read is
        // skipped.
        None => decl
            .as_ref()
            .map_or_else(inference::platform_base_url, |d| d.base_url.clone()),
    };
    // This route is `ScopedCompany`, not admin — every console reader gets this
    // field on every page load. A credential embedded in the endpoint is
    // refused at every point one can be set, but an endpoint stored before that
    // rule existed, or one arriving from a `company.toml` or
    // `OPENCOMPANY_INFERENCE_URL` this host does not own, still has to be safe
    // to *say*.
    let base_url = catalogue::redact_endpoint(&base_url);
    // What the company actually booted onto, not what the config implies.
    let cognition = runtime.cognition();
    let restart_required = restart_pending(runtime, decl.is_some());
    // Keys rework (#2306), slice 2c; round-3a review P2-4: the stored
    // default, independent of `decl` — a company can have a full default
    // that names a now-gone provider (X14) and still have `decl` resolve
    // through the legacy chain underneath it. Read leniently and once for
    // the whole status response (P3-6): a corrupt or unreadable value must
    // never 500 this route, and `provider_list` below reuses this same
    // value rather than reading it again per row.
    let (default, default_unreadable) =
        inference::store::load_default_lenient(runtime.id(), secrets).await;
    if default_unreadable {
        tracing::warn!(
            company = %runtime.id(),
            "inference default could not be read for the status response; reporting it as unset",
        );
    }
    let providers = provider_list(runtime, &default).await?;
    let routes = routing_table(runtime).await?;
    let mut managed = managed_state(runtime, platform).await?;
    // Keys rework (#2306), slice 2a: exactly one TinyHumans row is ever shown
    // (Q3). The legacy row renders only while its chain resolves AND no row
    // in the list already carries the `tinyhumans` slug — a `tinyhumans` row
    // means either an operator added one, or entry zero is already `managed`
    // (`store::provider_from_runtime` gives it slug `tinyhumans` too), and
    // either way that row is now the one TinyHumans row this page shows.
    managed.legacy_row =
        managed.configured && !providers.iter().any(|p| p.slug == inference::MANAGED_SLUG);
    // D-key-without-row (X5): moot once the legacy row itself is hidden.
    managed.needs_model = managed.legacy_row && managed.configured;
    let default_choice = default_choice_dto(
        &default,
        default_full_broken(
            &default,
            providers.iter().map(|p| (p.slug.as_str(), p.enabled)),
        ),
    );
    Ok(match decl {
        Some(d) => InferenceStatusDto {
            provider: d.selected_provider().to_string(),
            proxied: d.is_proxied(),
            slug: d.telemetry_slug().to_string(),
            base_url: if d.selected_provider() == inference::MANAGED_SLUG {
                managed.base_url.clone()
            } else {
                base_url
            },
            models: d.models.clone(),
            source: source_label(d.source).to_string(),
            key_configured: d.key_configured(),
            cognition: cognition.path.to_string(),
            usage_metering: cognition.metering,
            restart_required,
            harness_reachable: harness_reachable(runtime),
            designs_profiles: designs_profiles(runtime),
            can_rebuild_in_place,
            providers,
            routes,
            managed,
            default_choice,
            default_unreadable,
        },
        None => InferenceStatusDto {
            provider: "managed".to_string(),
            // `decl` is `None` because this deployment has no platform default
            // to inherit, so there is no subscription to ride.
            proxied: false,
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
            harness_reachable: harness_reachable(runtime),
            designs_profiles: designs_profiles(runtime),
            can_rebuild_in_place,
            providers,
            routes,
            managed,
            default_choice,
            default_unreadable,
        },
    })
}

/// Whether the managed brain can actually answer for this company.
///
/// The one fact [`resolve::infer_routing_mode`](crate::company::inference::resolve::infer_routing_mode)
/// needs beyond the routes, read through the same [`managed_state`] the status
/// card renders so the mode and the badge cannot disagree about it. Three store
/// reads on a route nobody calls in a loop, in exchange for the console never
/// again being told Managed on a company where managed resolves to nothing.
async fn managed_resolves(runtime: &CompanyRuntime) -> Result<bool, ApiError> {
    Ok(managed_state(runtime, platform_default(runtime).as_ref())
        .await?
        .configured)
}

/// What the managed brain would resolve to for this company.
///
/// Reads the three facts and hands them to
/// [`inference::managed_source`](crate::company::inference::managed_source),
/// which holds the branching. The inputs are a store read each; the decision is
/// pure and tested with three booleans.
async fn managed_state(
    runtime: &CompanyRuntime,
    platform: Option<&EnvDefault>,
) -> Result<ManagedDto, ApiError> {
    use crate::company::inference::store;

    let secrets = runtime.secrets().as_ref();
    // Both addresses for the one meaning: the new per-provider slot and the
    // legacy flat slot it converges from.
    let inference_key =
        inference::load_managed_key(runtime.id(), secrets, &inference::HarnessScope::default())
            .await
            .map_err(ApiError)?;
    let company_account = crate::company::company_key::load(runtime.id(), secrets)
        .await
        .map_err(ApiError)?;
    let source =
        inference::managed_source(!inference_key.trim().is_empty(), &company_account, platform);
    let health = store::load_health(runtime.id(), secrets)
        .await
        .map_err(ApiError)?
        .get(inference::MANAGED_SLUG)
        .map(|h| ProviderHealthDto {
            state: h.state.clone(),
            at: h.at.clone(),
        });
    Ok(ManagedDto {
        source: source.as_str().to_string(),
        configured: source.resolves(),
        base_url: catalogue::redact_endpoint(
            &platform
                .map(|p| p.base_url.clone())
                .unwrap_or_else(inference::platform_base_url),
        ),
        enabled: store::managed_enabled(runtime.id(), secrets)
            .await
            .map_err(ApiError)?,
        health,
        // Placeholder: `effective_status_with` overrides both once it has the
        // provider list this function was not given. `source.resolves()` is
        // the right placeholder for `needs_model` too — if this row never
        // renders (`legacy_row` ends up `false`), `needs_model` is moot; if it
        // never resolves (`configured` is `false`), there is no credential to
        // call out as model-less in the first place.
        legacy_row: source.resolves(),
        needs_model: false,
    })
}

/// `GET …/inference` — the company's effective inference status.
async fn get_status(
    State(state): State<AppState>,
    company: ScopedCompany,
) -> Result<Json<InferenceStatusDto>, ApiError> {
    Ok(Json(
        effective_status(&state, company.runtime.as_ref()).await?,
    ))
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
        // A rotation changes what the endpoint will answer without changing the
        // cache key, which is deliberately made of non-secret ids only. Left
        // alone, the catalog read with the *previous* credential would keep
        // answering for up to `MODEL_CATALOG_TTL`, so an entitlement-changing
        // rotation would never present the new bearer to `/models`: turns could
        // hold the old vocabulary and the console could offer models the new
        // account cannot reach (Codex review on #2045). Evicting on the write is
        // the fix that does not require the credential to become part of the key.
        crate::server::inference_models::evict_company_catalogs(runtime.id().as_ref());
    }

    let status = effective_status(&state, runtime).await?;
    // Issue #290: the not-configured → configured transition is the one a save
    // alone cannot deliver, because the brain was chosen at build time. Rather
    // than telling the operator to restart a container they may have no access
    // to, rebuild this company's runtime in place and re-read the status from
    // the successor.
    if status.restart_required {
        match crate::runtime::rebuild_company(&state, runtime.id()).await {
            Ok(successor) => {
                let status = effective_status(&state, successor.as_ref()).await?;
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
async fn revert_config(
    State(state): State<AppState>,
    company: AdminScopedCompany,
) -> Result<Json<MutationResponse>, ApiError> {
    let runtime = company.runtime.as_ref();
    let secrets = runtime.secrets();
    clear_runtime_config(runtime.id(), secrets.as_ref())
        .await
        .map_err(ApiError)?;
    // Also clear any stored credential: a "Reset to managed" is supposed to be a
    // full reset, not a half-clear that leaves a stale credential behind. The
    // stored key would otherwise make `keyConfigured` appear false in the UI while
    // secretly still being present, and the console's remove-key button would
    // remain hidden — leaving the operator stranded with a credential they cannot
    // clear. See issue #993 / inference.spec.ts cleanup.
    inference::clear_key(runtime.id(), secrets.as_ref())
        .await
        .map_err(ApiError)?;
    // Reset changes the effective credential just as a rotation does, so it owes
    // the same eviction `set_config` performs. Clearing the runtime key makes
    // resolution fall back to the manifest's `api_key_secret`; when that manifest
    // points at the same base URL, nothing in the cache key moves and turns would
    // keep reading the *previous* credential's catalog for up to
    // `MODEL_CATALOG_TTL` without ever presenting the manifest key (Codex review
    // on #2045). Every path in this module that writes the credential evicts.
    crate::server::inference_models::evict_company_catalogs(runtime.id().as_ref());
    Ok(Json(MutationResponse {
        status: effective_status(&state, runtime).await?,
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
    let status = effective_status(&state, successor.as_ref()).await?;
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

/// Why a resolved config could not authenticate against its own endpoint, or
/// `None` when the probe is worth sending (issue #1737).
///
/// [`send_plan`](crate::harness::provider::request_plan) omits the
/// `Authorization` header entirely when no bearer resolves, so a keyless config
/// aimed at an endpoint that demands one produces a vendor 401 that reads like a
/// rejected key. That is a fact this process holds before the request leaves it.
///
/// **Only the `openrouter` kind is judged**, which after
/// [`normalize_provider`](inference::normalize_provider) is also every legacy
/// `managed` config. Both endpoints it can resolve to — OpenRouter's own and the
/// platform proxy in front of it — reject an unauthenticated request
/// unconditionally, so refusing there can never be wrong. `ollama` takes no
/// bearer by design, and an `openai_compatible` endpoint is the operator's own
/// and may legitimately want none; refusing either would turn a working
/// configuration into a false alarm, which is worse than the outbound request it
/// would save.
#[cfg(feature = "openhuman")]
async fn unauthenticated_reason(
    decl: &inference::InferenceDecl,
) -> Result<Option<String>, OpenCompanyError> {
    if inference::normalize_provider(&decl.provider) != inference::DEFAULT_PROVIDER {
        return Ok(None);
    }
    // `bearer()` rather than `key_configured()`: a platform token source reports
    // itself configured while its projected file can still yield nothing, and it
    // is the value on the wire that decides whether the request authenticates.
    if decl.bearer().await?.is_some() {
        return Ok(None);
    }
    // The endpoint is named rather than the vendor. The `openrouter` *kind* no
    // longer implies OpenRouter's endpoint — the same kind carrying a
    // tenant `base_url` reaches whatever that URL points at — so "save an
    // OpenRouter key" is advice that sends the operator of a differently-pointed
    // company to buy a credential their provider will never see.
    Ok(Some(format!(
        "No inference key is stored for this company, and this host has no platform credential to \
         fall back on — a request to {base} would carry no Authorization header and be rejected, \
         so none was sent. Save a key that {base} accepts above, or point this company at an \
         endpoint that needs none.",
        base = catalogue::redact_endpoint(&decl.base_url)
    )))
}

/// The operator-facing reading of a failed probe, and its response code.
///
/// A vendor's own 401 is evidence and is kept verbatim, but it is not an
/// explanation: OpenRouter answers a key it cannot parse with `Missing
/// Authentication header`, which reads as "nothing was sent" and is how issue
/// #1737 came to be filed against the wrong layer. The header *was* sent. What
/// this route knows, and the vendor does not, is that the credential is stored
/// against whichever provider was selected when it was saved — so a key for
/// another vendor fails here while the card still reports one is set. Saying so
/// is the difference between a dead end and a next step.
#[cfg(feature = "openhuman")]
fn probe_failure(decl: &inference::InferenceDecl, error: &anyhow::Error) -> (String, &'static str) {
    let raw = error.to_string();
    let rejected = matches!(
        error.downcast_ref::<tinyinference::Error>(),
        Some(tinyinference::Error::Provider(error)) if error.status == Some(401)
    );
    if rejected || raw.contains("401 Unauthorized") {
        return (
            format!(
                "{} rejected the credential stored for this company. The request did carry an \
                 Authorization header — the provider would not accept what was in it. A key is \
                 stored against the provider selected when it was saved, so a key for another \
                 vendor fails here even while this card reports one is set. Re-save the key under \
                 {}, or Remove key to fall back. The provider said: {raw}",
                catalogue::redact_endpoint(&decl.base_url),
                decl.provider
            ),
            "credential_rejected",
        );
    }
    (format!("Inference probe failed: {raw}"), "probe_failed")
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
    let (manifest, harness_id) = match manifest_inference(runtime).await {
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
            let decl = match platform_default(runtime) {
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
            // Issue #1737: refuse locally rather than send a request this
            // process already knows cannot authenticate. The card warns that
            // Test "sends one real message… and your provider may charge for
            // it", so a doomed request is not merely untidy — and relaying a
            // vendor's 401 for it hides a configuration fact we hold here.
            match unauthenticated_reason(&decl).await {
                Err(err) => return ApiError(err).into_response(),
                Ok(Some(reason)) => {
                    return (
                        StatusCode::CONFLICT,
                        Json(serde_json::json!({
                            "ok": false,
                            "error": reason,
                            "code": "no_key",
                        })),
                    )
                        .into_response();
                }
                Ok(None) => {}
            }
            // No vocabulary discovery any more (keys rework, issue #2306,
            // slice 2d): resolve the stored choice before `probe`, so a tier
            // name can never reach the wire. A company whose Test resolves no
            // real id gets `NO_MODEL_CHOSEN` back instead.
            //
            // The default harness's real id, whether or not it declares its own
            // `[harness.inference]` — `model_unavailable_advice` names the same
            // table either way (its own, or the company's as the harness's
            // fallback), so this always passes it rather than gating on
            // `is_default` the way `TenantProvider::invoke` deliberately does not
            // (Codex review on #1824's #1811 follow-up).
            let failure = |error: &anyhow::Error| {
                let (error, code) = probe_failure(&decl, error);
                (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({
                        "ok": false,
                        "error": error,
                        "code": code,
                    })),
                )
                    .into_response()
            };
            let model = match inference::model_on_the_wire(
                &decl,
                crate::harness::provider::DEFAULT_HOSTED_MODEL,
            ) {
                Ok(model) => model,
                Err(err) => return failure(&anyhow::Error::new(err)),
            };
            match crate::harness::provider::probe(
                &decl,
                &model,
                Some(harness_id.as_str()),
            )
            .await
            {
                Ok(()) => Json(serde_json::json!({
                    "ok": true,
                    "provider": decl.provider,
                    "note": "Reached the provider and got a reply.",
                }))
                .into_response(),
                Err(err) => failure(&err),
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
#[path = "inference_test_support.rs"]
mod inference_test_support;
#[cfg(test)]
#[path = "inference_a_managed_manifest_also_tests.rs"]
mod tests_a_managed_manifest_also;
#[cfg(test)]
#[path = "inference_clearing_the_key_of_tests.rs"]
mod tests_clearing_the_key_of;
#[cfg(test)]
#[path = "inference_configuring_only_managed_after_tests.rs"]
mod tests_configuring_only_managed_after;
#[cfg(test)]
#[path = "inference_disabling_keeps_the_route_tests.rs"]
mod tests_disabling_keeps_the_route;
#[cfg(test)]
#[path = "inference_restart_rebuilds_the_registered_tests.rs"]
mod tests_restart_rebuilds_the_registered;
#[cfg(test)]
#[path = "inference_rotating_the_key_does_tests.rs"]
mod tests_rotating_the_key_does;
#[cfg(test)]
#[path = "inference_status_reports_the_default_tests.rs"]
mod tests_status_reports_the_default;
#[cfg(test)]
#[path = "inference_switching_to_managed_reads_tests.rs"]
mod tests_switching_to_managed_reads;
