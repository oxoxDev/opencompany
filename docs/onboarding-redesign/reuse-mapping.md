# Reuse mapping

The point of this redesign is that most of it is not new code. This file
names, for every piece of the target flow, the exact existing
function/component/endpoint it must call — and separately, honestly, the
pieces that are not reuse no matter how they're described, because the thing
being "reused" doesn't actually exist yet. Conflating the two is the failure
mode this file exists to prevent.

## §1 Managed step 1 — the login/key mechanism

**Reuse, real and available today:**

- `setCompanyCredential(client, company, key, model?)` — `PUT …/credential`
  (`frontend/src/api/credential.ts`). This is what `ApiKeyView.tsx`'s
  paste-a-key dialog (`AccountKeyDialog`) calls today (`write()`,
  `ApiKeyView.tsx:309-402`).
- `setCompanyCredentialModel(client, company, model)` — `PUT
  …/credential/model` (`credential.ts`). Finishes the row off a *stored* key
  when there's no `pendingKey` — this is what a redeemed grant uses
  (`writeModel()`, `ApiKeyView.tsx:411-458`).
- The fan-out these trigger server-side, `company_key/fan_out.rs`'s `Slot`
  enum (`Composio, Inference, Provider, Default, Health`, plus the new
  `Search` from #2342). Confirmed real: `ApiKeyView.tsx:564-567`'s own copy —
  "Saving copies it to the LLM and Composio pages wherever they hold no key
  of their own" — and it genuinely does not overwrite a slot that already has
  its own key (`:556-563`).
- The rebuild-in-place mechanism: `set_key`, `set_model`, `finish_link` in
  `company_key.rs` all call `rebuild_if_pending` (`company_key.rs:1851`) when
  the fan-out filled/rotated the provider or default slot and
  `restart_required` holds. This is issue #290's mechanism, extended to the
  key-save path by #2338.

**Required change, not an assumption:** the wizard's `PowerStep` today does
**not** call any of the above. It calls `company::inference::store_key()`
(`inference.rs:952-967`), a completely separate mechanism that writes exactly
one secret (`inference/key`) plus the manifest's `inference` block
(`server/setup.rs:962-965`) — no Composio, no Search, no fan-out at all. Slice
4a (README.md's order-of-work) is this exact swap: Managed step 1 must call
`setCompanyCredential`/`setCompanyCredentialModel`, not
`inference::store_key`. Until this swap happens, "connecting TinyHumans in
onboarding" and "connecting TinyHumans on the Account page" are two different
features that happen to look similar.

**Out of scope for this implementation — descoped, not missing:** a real
one-click "Login with TinyHumans" button. Managed step 1 is the existing
"Connect to TinyHumans" dialog exactly as it stands today — an API-key
input, a plain external "Get an API key ↗" link to the TinyHumans dashboard,
and the "saving also adds this key to the LLM page… and connects it for
Composio" copy. No OAuth grant button is being added in this pass.

For the record, since it was traced before being descoped: `ApiKeyView.tsx`
has exactly one entry point today, this same "Connect to TinyHumans" dialog.
An actual "Sign in with TinyHumans" button was removed from that page on
2026-09-14 (`account.ts:119-120`). The only place that phrase exists
anywhere in the app is a plain external link in the *current* wizard's
`PowerStep` (`SetupWizard.tsx:1886-1894`, `<a href={keySource.url}>`) — a
link-out to copy-paste a key manually, not an OAuth grant, and that's fine:
it's the same shape as the "Get an API key ↗" link this dialog already has.

The grant machinery (`POST /api/v1/company/credential/link/start`,
`useRedeemKeyGrant`, `pending-key-link.ts`'s module-level stash) is real and
working — nothing about it is broken — but nothing in the console calls
`link/start` today, and this redesign is not adding a caller for it. It
stays exactly as reachable as it is right now. If a one-click grant button
becomes worth building later, the landing-page constraint (the stash is a
module-level variable only `ApiKeyView.tsx` reads today, via
`useRedeemKeyGrant`) is the first thing to resolve — not a concern for this
implementation.

## §2 Self-managed step 1 — Provider + Composio

**Reuse, real and available today — literal, not simplified:**

- **Provider.** The exact LLM page's add-provider sequence. `ProvidersTab.tsx`
  opens `AddProviderDialog` (pick a provider from the catalogue) then
  `ProviderConnectDialog` (the BYOK form — key, base URL, model, live probe;
  `ProvidersTab.tsx:14-17`). Submit calls `actions.add({...})`
  (`ProvidersTab.tsx:311-316`) — `useInference`'s `add` (`use-inference.ts:173`)
  → `addProvider(client, company, input)` → `POST …/inference/providers`
  (`api/inference.ts:538-540`). Same two dialogs, same handler, same endpoint,
  mounted inside the wizard.

  **Corrected while implementing 4b-i.** Two of those sentences did not survive
  contact:

  - **`ProvidersTab` itself cannot be mounted, and must not be.** It is a
    controlled view over `InferenceState`/`InferenceActions`, and `useInference`
    opens with `GET {scope}/inference`, which answers `CompanyNotFound` before a
    company exists. The two dialogs *are* mountable verbatim — neither makes a
    request of its own on the add path — so the wizard's step owns the
    orchestration between them instead, in the shape `submitConnect` already
    has. `inference-connect-dialog-offline.test.ts` pins the offline claim.
  - **The endpoint is reached at the apply, not from the step.** `POST
    …/inference/providers` is admin-scoped to an existing company, so the step
    stages the add's own body and the apply runs it through
    `add_provider_inner` — the handler's whole body, split out. The two new
    first-run routes (`POST /api/v1/setup/inference/probe` for the model list,
    and the apply's `provider_draft` field) are the same functions behind the
    first-run gate, not second implementations of them.
- **Composio.** The exact Composio page's credential dialog —
  `ComposioSection.tsx`'s inline `Dialog` (`:811` on), backed by
  `useComposioCredential` (`use-composio-credential.ts:90`). Its `submit()`
  (`ComposioSection.tsx:576-590`) calls `setComposioApiKey(client, company,
  value, skipVerify, true)` or `setComposioToken(client, company, value)`
  depending on `form.credential` (`api/composio.ts:439`, `:402`). Same dialog,
  same hook, same two calls.

  **Corrected while implementing 4b-ii.** Two more:

  - **The dialog was not a component.** It was inline JSX closing over seven of
    the page's locals, so "mount it" was not an available move until it was
    lifted into `ComposioCredentialDialog.tsx` — JSX moved, not rewritten, with
    the eight `composio-*` unit files untouched as the evidence.
  - **`useComposioCredential` is not mounted and must not be.** It opens with
    `GET {scope}/composio` and a `GET …/auth/me`, neither of which a
    pre-company host can answer. The form's shape comes from the pure pair
    instead: `composioRows(null)` (which tolerates a null status — `modeOf`
    reads it as `managed`) and `composioForm(pending, rows)`. The real
    `ComposioRowList` renders over those, so the card is the Connections card.
  - **The secret keys are `composio/byok/key` and `composio/tinyhumans/key`.**
    This folder's "no new keys" list named `composio/managed/key`, which does
    not exist anywhere in the crate; every mention is corrected.

Each mounted **as-is** — not rebuilt, not trimmed, not a condensed variant —
each independently skippable via its own "set this up later." No fan-out
involved on this branch: neither credential comes from a TinyHumans key.

**Correction:** an earlier draft of this file called these "simplified
Provider + Composio views." That was wrong — there is no simplified version
to build. The wizard step mounts the same components Connections → LLM and
Connections → Composio already ship.

## §3 Step 2 — Name the company + pick a template

**Reuse:** `BusinessStep`'s existing template/industry logic, verbatim.

**New:** a company-name field, moved here from `ReviewStep`. Today
`ReviewStep` shows an **editable company name** alongside the designed roster
(`SetupWizard.tsx:1989` area, traced in current-flow.md). D-name-once
(README.md) requires this field to be deleted from Review, not duplicated —
naming happens exactly once. `submit()`'s `SetupInput.name` field is
unchanged; only which step populates it moves.

## §4 Rebuild-in-place for the wizard's key-save

**Reuse:** `rebuild_if_pending` (`company_key.rs:1851`), same as §1.

Once Managed step 1 calls the real fan-out (§1's required change), it gets
this for free — `set_key`/`set_model`/`finish_link` already call
`rebuild_if_pending` themselves. The wizard's final submit needed the same
call, and slice 4a's `store_account_key` makes it.

**Resolved, two branches** (slice 7; full reasoning in
[open-questions.md](open-questions.md)):

- **Managed** sends no `company.inference`, so the seeded company boots on the
  echo brain. The fan-out then fills the `tinyhumans` row and the default,
  `restart_pending` flips true, and the rebuild fires — which is why
  `store_account_key` calls `rebuild_if_pending` on both of its seed
  sub-paths.
- **Self-managed / BYOK** sets `manifest.inference.provider` *before*
  `seed_generated_company`, so the company boots already configured on
  `HARNESS_PATH` and `restart_pending` is false. No rebuild is owed there —
  not by this slice and not by 4b.

Neither branch races `register()`, which resolves inference fresh at boot.
Both are now pinned by tests in `server/setup/test.rs`; the managed one is
gated on `openhuman`, because `harness_reachable` is a `false` stub at default
features and the rebuild is unreachable without a pool.

## §5 The search tier (#2342)

**Not reuse — a dependency.** Issue #2342 is what makes `search/managed/key`
exist as a fan-out slot at all. Today there is no `Search` variant in
`company_key/fan_out.rs`'s `Slot` enum, and Search's "Managed" row is not a
real provider (`catalogue.rs:23-25`: "`managed` is not in this table"). Slice
4a (Managed step 1 calling the real fan-out) will silently *not* set up
search until #2342 ships — the wizard doesn't need to do anything extra for
search once #2342 lands, but it cannot claim to set up search before then.
Do not implement a wizard-side search step to compensate; the fan-out is the
right layer for this, per #2342's own "no new fallback complexity" framing.
