# Onboarding redesign

How the first-run setup wizard changes shape: one branch point instead of a
raw model-picker, both branches reusing the real Connections mechanisms
instead of a separate wizard-only path, and a straight landing in the console
instead of a second post-build checklist screen.

Depends on issue
[#2342](https://github.com/tinyhumansai/opencompany/issues/2342) (the
search-managed-key fan-out slot). This folder is a plan **and** an
implementation brief — an implementer with no other context should be able to
execute a slice exactly from what's written here.

Per-slice, implementer-ready detail (files to touch, steps, tests, risks) is
in [implementation-plan.md](implementation-plan.md) and its linked task
files — read that folder before starting any slice in the table below.

- **Requested by:** the operator, 2026-09-16, worked out turn-by-turn across a
  design conversation, not a written brief. This folder is that conversation's
  record.
- **Code read at:** `upstream/main` post-#2338 merge (2026-09-16, 96 commits
  pulled that day, `da4f922d7..ac8ca7be2`). Every `file:line` in this folder
  is on that state unless marked otherwise. Line numbers drift; quote the code
  to re-find it.

## The goal in plain words

1. **The first screen is a choice, not a model-picker.** "How do you want to
   set this up?" — Managed with TinyHumans, or set it up yourself. Today's
   first screen (`PowerStep`, provider dropdown + raw API key field) is not
   what a new operator should see before they've said which kind of setup
   they want.
2. **Both branches reuse the real Connections mechanisms.** Managed's connect
   step is today's "Connect to TinyHumans" dialog, verbatim, calling the same
   fan-out `ApiKeyView.tsx` already calls (`setCompanyCredential` /
   `setCompanyCredentialModel`), not the wizard's own separate
   `company::inference::store_key`. A one-click "Login with TinyHumans" OAuth
   grant button was considered and is explicitly **out of scope** for this
   implementation — see [reuse-mapping.md](reuse-mapping.md) §1. Self-managed's
   provider and Composio steps mount the real Connections → LLM add-provider
   dialogs and the real Connections → Composio credential dialog verbatim —
   not simplified, not new components.
3. **One company-level TinyHumans key fills three surfaces, not two.**
   Provider, Composio, and (once #2342 lands) Search — see
   [reuse-mapping.md](reuse-mapping.md) part 2 for exactly what's missing
   today.
4. **Naming happens once, early.** Company name moves from the Review step
   (today: an editable field shown after the roster is designed) to step 2,
   alongside the template choice.
5. **A saved key takes effect immediately.** The rebuild-in-place mechanism
   `set_key`/`set_model`/`finish_link` already have (issue #290, #2338) applies
   here too — the wizard's key-save step should get the same no-restart
   treatment, not the toast-and-manual-restart path the wizard's own
   `inference::store_key` leaves you on today.
6. **The company opens directly.** The post-build "Let's get your company
   running" checklist screen (Name / Connect an integration / Run an
   automation) is removed in full — not trimmed, not made skippable-by-default,
   removed. See [removed.md](removed.md).

## The flow, before and after

Full detail: [current-flow.md](current-flow.md) (today, as traced) and
[target-flow.md](target-flow.md) (the redesign). Short version:

**Today** — `SetupWizard.tsx`'s `STEPS` (line ~106): `power, business, signin,
account, advanced, review`, filtered by `visibleSteps` (`:665-674`) down to
whatever a given host actually needs. `power` (Model) is always the first
visible step when a host has no inference of its own.

**Target** — a new `setup-way` step becomes first, branching into two
different step-1 screens (`managed-login` / `self-managed-connect`), which
converge back into the existing `business` → `signin` → `review` sequence.
`account` and `advanced` stay conditional exactly as they are today. The
post-build checklist screen is deleted, not gated.

## Order of work

One slice = one reviewable commit (or short run of commits) on its own
branch. Do them top to bottom; each depends on the ones above it.

| # | Slice | Depends on | What |
|---|---|---|---|
| 0 | this folder (docs only) | — | plan + brief |
| 1 | [reuse-mapping.md](reuse-mapping.md) — **read before writing any code** | — | not a code slice: the exact function/component/endpoint map every later slice must follow |
| 2a | fan-out: add the `Search` slot | issue #2342 | `company_key/fan_out.rs` `Slot` enum gains `Search`; `search/managed/key` written alongside `provider/tinyhumans/key` and `composio/tinyhumans/key` |
| 2b | `search/resolve.rs` reads the company tier | 2a | `active()` checks `search/managed/key` before the bare instance-operator env credential |
| 3 | wizard step 0: the setup-way choice | — | new `SetupWayStep` component + `STEPS` reorder; no backend change |
| 4a | Managed step 1: reuse the real fan-out | 1, 2a | `PowerStep`'s TinyHumans path replaced by a call to `setCompanyCredential`/`setCompanyCredentialModel`, not `inference::store_key` |
| 4b | Self-managed step 1: mount the real Provider + Composio dialogs | 1 | `AddProviderDialog`+`ProviderConnectDialog` and `ComposioSection`'s credential dialog, verbatim, each independently skippable |
| 5 | move company naming to step 2 | — | `BusinessStep` gains the name field; `ReviewStep` drops its own |
| 6 | remove the post-build checklist | — | delete the "Let's get your company running" screen and its route; land straight into `AppShell` |
| 7 | rebuild-in-place for the wizard's key-save | 4a | wizard's key-save path gets the same `rebuild_if_pending` treatment `set_key`/`set_model`/`finish_link` already have |

**Stop points — report instead of guessing:** #2342 is not yet implemented,
so slice 4a cannot ship until 2a/2b land; a slice needs a new secret-store key
beyond `search/managed/key`.

## Decisions (taken; do not re-open)

| Id | Decision |
|---|---|
| D-no-fallback-tier | The company-level search tier is a sibling fan-out write, not a new priority/fallback layer. `search/resolve.rs` checks it before the instance env credential; the instance env credential is untouched as the last resort. Confirmed with the operator directly — see [reuse-mapping.md](reuse-mapping.md) part 2. |
| D-boundary-change | `search/mod.rs`'s "a company can never point search at a key it controls" and `provider.rs:333-334`'s "consults ONLY the environment" are being deliberately superseded for the company tier. This is called out explicitly in #2342 and must stay called out in the code comments that change, not silently dropped. |
| D-checklist-gone | The post-build checklist is removed, not made optional-by-default. No flag, no "don't show again" — the screen stops existing. |
| D-name-once | Company naming happens exactly once, at step 2. The Review step's own editable name field is deleted, not just hidden when step 2 already set one. |
| D-reuse-not-rebuild | Wherever the redesign says "reuse," it means calling the existing function/endpoint/component, not writing a parallel implementation that happens to match its behavior today. If the real mechanism can't be reused as-is, that is a stop point, not a license to reimplement — see [reuse-mapping.md](reuse-mapping.md) for every place this bites. |

## Glossary

- **Fan-out** — `company_key/fan_out.rs`'s cascade: one TinyHumans key save
  fills every empty managed slot it knows about.
- **Slot** — one fan-out target (`Composio`, `Inference`, `Provider`,
  `Default`, `Health`, and the new `Search`).
- **Managed row** — a Connections page's row for the TinyHumans-backed
  option, as distinct from a BYO provider row.
- **Rebuild-in-place** — `rebuild_if_pending`/`rebuild_company`
  (`company_key.rs:1851`, issue #290): swapping a company's live inference
  harness without a restart.
- **Grant** — the PKCE-style hub login (`link/start` → hub consent →
  `link/finish`), distinct from paste-a-key. Exists in the backend and in
  `useRedeemKeyGrant`; has no working start button in the console today, and
  this implementation isn't adding one — see
  [reuse-mapping.md](reuse-mapping.md) §1.

## Rules for the implementer

- **Find the existing function before writing anything new.** Search for a
  handler, component, hook, or endpoint that already does it before adding
  code — on every step, not just once at the start. See
  [implementation-plan.md](implementation-plan.md)'s "Standing rules."
- **No new secret or config keys.** Reuse `tinyhumans/key`,
  `provider/tinyhumans/key`, `composio/tinyhumans/key`, and #2342's search
  equivalent as-is. A task that seems to need a new key needs a different
  approach, not a new key.
- Rust: `cargo fmt --all -- --check` locally; clippy/tests on CI, verified by
  head SHA, zero failures **and** zero pending.
- Frontend: three typecheck gates (`typecheck`, `typecheck:unit`,
  `typecheck:e2e`); `scripts/ci/assert-design-tokens.sh` rejects raw hex.
- A UI change is verified only in a real browser, light and dark, screenshot.
- Never a real credential on disk. Tests use `th-not-a-real-key` and friends.
- Every Markdown file ≤ 500 lines. Absolute dates.
