# The company credential

How a company proves who it is to the surfaces the platform brokers on its
behalf (issue #586). The routes are listed in
[`api-write-plane.md`](api-write-plane.md#credential-bearing-surfaces-feature-gated); this is the model
behind them.

## One key per company

A company belongs to one owner. Its admin sets **one** TinyHumans key through
`PUT …/credential`, and everything the platform backend brokers for the company
rides it. Membership in the company is what grants access to it.

Composio is the first consumer, and it is the one that shows why this works: the
backend derives the Composio entity from whatever bearer it is handed, and a
TinyHumans key is a bearer it recognises. So the key **authorizes Composio
directly** — there is no provisioning step that trades it for a second token,
and no per-tenant provider application to register with Google, Slack or GitHub.
A company with a key set connects Gmail by clicking Connect.

## Where a connection lives

On the backend, keyed by the account the bearer resolves to — under this model,
the company's owning account. Nothing about a connection is scoped to the member
who made it, and nothing new is stored on the instance.

That is what makes "connect Gmail once, every teammate's agents can use it" true:
every agent in the company resolves the *same* credential from the *company's*
store, so the backend resolves one entity for all of them. It is a property of
the resolution, not a feature layered on top of it.

## Resolution — one seam

`company::company_key::resolve` is the only place a company identity is
resolved. Most specific first:

1. **The company's own key** — `tinyhumans/key` in its `SecretStore`, set by an
   admin through the console.
2. **This instance's platform identity** — `TinyhumansTokenSource`: a projected,
   audience-bound pod token the cluster rotates in place, else a static
   `TINYHUMANS_API_KEY`. Unchanged behaviour for a tenant whose admin set
   nothing.
3. **Nothing** ⇒ fail closed. No tools are wired, and the read planes report the
   degraded state rather than offering a picker that cannot work. An absent
   credential means "no tools", never a borrowed identity.

### An unreadable store is not "no key"

A `SecretStore` read error **propagates** rather than falling through to the next
tier. The tempting shortcut is to map it to "nothing stored" so a transient
hiccup cannot brick a roster build — right about availability, wrong about
attribution.

A connection lives on the backend keyed by the account the bearer resolves to.
If an unreadable store silently resolved a company that *has* a key to the
instance's identity, any connection established in that window would belong to
the instance's account. When the store recovered, resolution would return the
company key, the bearer would resolve to a different entity, and the connection
made under the fallback would no longer be the one the company sees — the same
"connect Gmail" click producing a different owner depending on store health at
that instant, with no signal either way.

Availability-degrade and identity-degrade are different decisions, and only the
first is safe to make silently. So each caller answers the error in the way its
own surface can afford:

- **The roster build** (`TenantComposio::resolve`) logs a warning and withholds
  the tools for that cycle. Fail closed: an *unknown* credential must no more
  mean a borrowed identity than an absent one does. The company loses Composio
  for a cycle and gets it back on the next.
- **The console planes** (`GET …/composio`, `GET …/credential`) surface the
  failure instead of reporting a confident "not configured" for a company that
  may well have a key.
- **`POST …/composio/authorize`** — the call that actually *establishes* a
  connection — refuses outright. It resolves the credential itself rather than
  through the roster path, precisely so a store error stays distinguishable from
  "nothing configured" and cannot be guessed past.

A surface may prepend its **own** escape hatch above that seam. Composio keeps
its BYO `composio/token` for a company that insists on using its own Composio
account, so its full order is `composio/token` → company key → instance
identity → none. What no surface may do is resolve a *company* identity some
other way.

That composed order is itself derived **once**, in
`company::composio::resolve_credential` — not in the harness. The agent-facing
`TenantComposio::resolve` builds its config from it, and the console's
`GET …/composio` reports its `source`, so the tier an operator is shown cannot
disagree with the identity the agents actually present. It lives in the
always-compiled `company::` module rather than the feature-gated `harness::`
one precisely so both callers can share it in every build; a console route that
restated the precedence instead would keep confidently reporting a tier after
the resolver stopped honouring it.

## Rotation

The rotation guarantee — "rotating the company key does not silently leave one
brokered surface on the old credential" — is structural rather than a
convention. Because every brokered surface calls the one resolver, there is no
second resolution that could drift, and no surface that could forget to re-read.

Two mechanics make it land without a restart:

- The key is read **live** from the secret store on every resolution, so a set /
  rotate / clear takes effect on the next cycle.
- The resolved credential contributes its **value** to the harness roster
  fingerprint (`Credential::hash_identity`), so a rotation rebuilds the tool
  roster. This is deliberately different from the projected platform token,
  which contributes its *path*: the cluster rotates that one every few minutes
  and hashing its value would rebuild every agent's roster on that schedule. A
  company key is rotated by a person, on purpose, and a new value really is a
  new identity.

**The fingerprint is internal and must stay internal.** It is a value-derived
hash of a live credential, which makes it a cheap confirmation oracle for anyone
who can read it: given a guess at the key, you can confirm it. It lives only in
`HarnessPool`'s in-memory `RwLock<HashMap<CompanyId, u64>>` and is compared for
equality — no `tracing` call renders it, no DTO serializes it, and no journal
event carries it. Do not log it, return it from a route, or put it in an event,
even for debugging; if a rebuild needs explaining, log *that the identity
changed*, never the hash.

## Write-only, and admin-only

The key is sent on the `key` field, stored, and never echoed. No read route
returns it; `GET …/credential` carries `configured` plus `source` — one of
`company` / `attested` / `static` / `none`, the same vocabulary `GET …/composio`
and the connections read plane already use. `Credential`'s `Debug` redacts it,
and the Composio tools feed whatever value they resolve to the scrubber as a
known secret, so it cannot survive into agent-visible output.

`PUT` requires an admin. This key is the identity every one of the company's
agents presents **and** the account they all spend against, so setting it is a
decision made for the company rather than a member's own — the same reasoning
that made `PUT …/composio/token` admin-only in issue #403. Both a set and a
clear are journaled as `ToolAccessChanged`, told apart from each other, and
attributed to whoever made the change.

## Which connected account (issue #820)

The credential decides **whose** accounts a call can reach. It does not decide
**which** of them, and for a company holding two accounts for one toolkit —
`ops@` and `billing@` Gmail — those are different questions.

Until #820 the second had no answer at all. `composio_execute` built its body as
`{tool, arguments}` and carried no connection id, so the account was resolved by
Composio for the entity, outside this codebase entirely. Two consequences worth
naming: "send from the billing account, not ops" was not sayable, and *which
Gmail did the agent send from* was unanswerable even after the fact. The only
lever was to disconnect the account you did not want.

The choice is now a per-company, per-toolkit preference:

- **Stored** as one JSON blob under `composio/defaults`
  (`{"gmail": "ca_billing"}`), beside the credential it qualifies and read the
  same way `inference/config` is. Not a secret — the ids are the same ones
  `GET …/composio/connections` already hands the console, and are useless
  without the bearer that scopes them — but company state, so it moves, backs up
  and is deleted with the rest of the company's Composio state.
- **Resolved** into `TenantComposio` by the same `resolve` the credential goes
  through, and folded into the roster fingerprint, so a change reaches the
  agents on their next turn with no restart — exactly like a rotated token.
- **Sent** as `connectionId` on the execute body, which the platform backend
  forwards to Composio as `connectedAccountId`.
- **Set** through `PUT …/composio/connections/{id}/default` (admin-only), which
  validates the id against this company's own filtered connection list first and
  refuses an account that is not usable. Cleared through the matching `DELETE`,
  which deliberately makes **no** upstream call: clearing has to work when the
  account is gone or the provider is unreachable, which is when a validating
  clear would refuse.

**Absent is the ordinary state, and it is not a degraded one.** A company that
has chosen nothing sends no connection id and gets Composio's own resolution,
byte-for-byte the behaviour that existed before — which is what keeps this
change invisible to every single-account company. Nothing invents a default from
the connection list: `list_connections_detailed`'s `(toolkit, id)` sort is a
stable render order for a read, never a choice, and a default the console
claimed but the harness did not honour would read as a guarantee. The console
says "Composio picks" rather than pointing at a row.

Two pins are dropped automatically, because a pin to a connection that no longer
exists would be sent on the next execute and refused — turning the disconnect of
one account into a broken toolkit: when the console revokes an account, and when
`GET …/composio/connections` finds a chosen id that Composio no longer lists.

## Not the inference key

`inference/key` is a different thing and must stay a different slot. It holds
whatever credential the company's *declared provider* wants — an OpenRouter
`sk-or-…`, a raw BYOK token, an `openai_compatible` key. It is provider-scoped,
not an identity, and handing it to the TinyHumans backend would present one
vendor's credential to another.

Since `managed`'s removal the two never coincide, which makes the separation
cleaner rather than looser. A company holding **no** inference key rides the
subscription on the platform's own credential — resolved from this host's
identity, not from `inference/key` — and a company that sets one is naming an
OpenRouter account that has nothing to do with TinyHumans. See
[providers.md](providers.md).

There is one such slot **per harness**: the default harness keeps the flat
`inference/key`, and every named one uses `harness/<id>/inference/key`. The
asymmetry is deliberate — the `SecretStore` has no rename, so namespacing the
default too would orphan the stored credential of every company already
running.

## What this does not cover

- **Legacy native OAuth credentials.** #838 retires
  `…/connections/{provider}/start`: through 2026-09-30 it answers a dated
  `410 native_oauth_retired`, then #1023 removes it. The callback likewise
  explains an in-flight browser redirect without exchanging its code. Existing
  `oauth/{provider}` values remain readable and revocable, but no agent can use
  them and no credential tier treats a configured host provider app as a route.
- **Chat inference and embeddings.** Both still resolve from the environment via
  `hosted_endpoint_from_env`. Moving them onto this seam is issue #585; when it
  lands they inherit the rotation guarantee by construction, because the seam is
  already here.
- **Media generation and `web_search`.** Deliberately environment-only: those run
  on the *platform's* managed credential, never a company-controlled one.

## Known limits, recorded deliberately

- **No per-member attribution.** Spend arrives as one account, so which member
  burned what is not answerable. Per-agent `budget_usd_daily` caps still work;
  per-person accounting does not exist and is not in scope.
- **Removing someone from the roster stops future access**, but nothing already
  spent is separable.
- **Two companies pasting the same key share one entity.** That cannot be
  prevented client-side; it is a deployment caveat, the same one the BYO Composio
  token already carries.
- **Media generation does not read the projected tier.** `web_search`, chat
  inference and embeddings all resolve through `TinyhumansTokenSource`, so a
  hosted tenant's rotating pod token reaches them. `media_backend_from_env` does
  not: it reads `OPENCOMPANY_MEDIA_KEY`, else a static `TINYHUMANS_API_KEY`, and
  a projected token file alone leaves media unwired. This is a migration miss
  from #189 rather than a decision, but it cannot be closed here — the upstream
  media client takes a `String` bearer for the life of the process, so flattening
  a 600-second projected token into it would trade "never works" for "works for
  ten minutes". Fixing it properly means giving that client a resolvable
  credential upstream, the same shape `SearchBackend` already has. Until then a
  hosted deployment that wants media must also carry a **non-projected** key, and
  `PlatformCredentialStatus::boot_warning` says so at boot (#879). That key
  should be `OPENCOMPANY_MEDIA_KEY`, the supported per-surface override. A static
  `TINYHUMANS_API_KEY` would also work, but it is the `docker compose` credential
  and the explicitly unsupported self-host hatch; reaching for it here would make
  that hatch load-bearing on the hosted path, which is a decision for whoever
  closes the upstream gap rather than a workaround to settle by default.
