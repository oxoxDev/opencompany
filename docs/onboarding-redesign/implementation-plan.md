# Implementation plan

Engineer-ready breakdown of README.md's order-of-work table. Each slice below
is its own file, in the ArchitectoBot format: Architecture Impact / Files to
Modify / New Files / Dependencies / Implementation Steps / Testing Strategy /
Risks and Edge Cases / Developer Handoff. Read [reuse-mapping.md](reuse-mapping.md)
before any of them — this plan does not repeat what that file already nails
down precisely.

Code read at `docs/onboarding-redesign-roadmap`'s base, `upstream/main`
post-#2338, same as the rest of this folder. Re-verified against the live
tree during this planning pass — the corrections below are what had actually
drifted or was missing from the earlier docs.

## Standing rules — every task, no exceptions

These two apply to every task file below, on every step of every task. They
are not a preference — treat a violation of either as a sign the task isn't
understood yet, not a shortcut to take.

1. **Find the existing function before writing anything new.** Before
   implementing any piece of this plan — a handler, a component, a hook, an
   endpoint, a validation, anything — search the codebase for something that
   already does it. Only write new code once you've confirmed nothing does.
   This is the whole premise of [reuse-mapping.md](reuse-mapping.md) and the
   "Corrections" table below: this plan already found real prior art
   (`ReuseAccountKeyBanner.tsx`, the two same-named dialog pairs, etc.) that a
   surface-level look would have missed or duplicated. Assume more exists that
   this plan hasn't found yet, and look before each step, not just once at the
   start.
2. **No new secret or config keys.** Every credential this plan touches has an
   existing name and an existing storage location: `tinyhumans/key`,
   `provider/tinyhumans/key`, `composio/tinyhumans/key`, and (once #2342 lands)
   its search equivalent. Reuse those exact keys. If a task seems to need a
   new one, that is a stop-and-reconsider signal, not something to add —
   revisit the task's approach, or raise it as an open question, before
   introducing a new key.

## Corrections found during this planning pass

| What the docs said | What's actually true | Where |
|---|---|---|
| `Slot` enum lives in `company_key/fan_out.rs` | It's in `company_key/types.rs:22-28`. `fan_out.rs` writes the slots, it doesn't declare the enum. | confirmed live |
| `rebuild_if_pending`/`set_key`/`set_model`/`finish_link` in `company_key.rs:1851` | They're in `server/ops/company_key.rs` (`rebuild_if_pending` at `:465`, `set_key` `:518`, `set_model` `:612`, `finish_link` `:846`) — a different file from `company/company_key.rs`, which only holds the *other*, unrelated `store_key` (the domain-layer one, `:80`). Same basename, three files, easy to grab the wrong one. | confirmed live |
| "the LLM page's `ProvidersTab.tsx`" | `frontend/src/inference/ProvidersTab.tsx` — not under `views/connections/`. | confirmed live |
| — (not mentioned) | **Naming collision**: `AddProviderDialog.tsx` and `ProviderConnectDialog.tsx` exist twice — once under `frontend/src/inference/` (the LLM ones, correct target) and once under `frontend/src/search-providers/` (Search's own provider-add flow, unrelated to this work). Import from `@/inference/...`, not `@/search-providers/...`. Flagged in full in [plan-self-managed-step1.md](plan-self-managed-step1.md). | new finding |
| — (not mentioned) | **Prior art exists**: `frontend/src/inference/ReuseAccountKeyBanner.tsx` is a shared, already-shipped "use the same TinyHumans key here too?" banner. `ComposioSection.tsx` renders it live today (`showsComposioReuseBanner`); the LLM-side equivalent (`showsInferenceReuseBanner`) is stubbed but not wired up (`frontend/src/inference/reuse-banner.ts:19-23`, "not implemented yet"). Not something slice 4a reuses directly — the wizard's cascade fills the slot outright rather than asking — but the implementer should know this pattern exists before inventing a second one. See [plan-managed-step1.md](plan-managed-step1.md). | new finding |
| "the post-build checklist screen and its route" | Precisely `frontend/src/onboarding/OnboardingGate.tsx` (`export function OnboardingGate`, `:117`; the three items at `:167/:175/:183`; "Skip setup" at `:353`), plus `frontend/src/onboarding/IntegrationStep.tsx`. Mounted from `frontend/src/components/app-shell.tsx:3670`, gated by `shouldShowOnboardingGate` (imported `:62`, called `:3653`). | confirmed live, now exact |
| `ProvidersTab` is a drop-in component | It takes `state: InferenceState` and `actions: InferenceActions` as required props — it does not own its own data fetching. Whoever mounts it (today: the Connections page) must first call whatever hook produces those (`useInference`-shaped) and pass them down. Mounting it inside the wizard means instantiating that hook there too, not just placing the component. | confirmed live |

## Task list, in order

| # | File | Depends on |
|---|---|---|
| 2a/2b | [plan-fanout-search-slot.md](plan-fanout-search-slot.md) | issue #2342 |
| 3 | [plan-setup-way-step.md](plan-setup-way-step.md) | — |
| 4a | [plan-managed-step1.md](plan-managed-step1.md) | 2a, task 3 |
| 4b | [plan-self-managed-step1.md](plan-self-managed-step1.md) | task 3 |
| 5 | [plan-naming-move.md](plan-naming-move.md) | — |
| 6 | [plan-remove-checklist.md](plan-remove-checklist.md) | — |
| 7 | [plan-rebuild-wizard.md](plan-rebuild-wizard.md) | 4a |

Tasks 3, 5, 6 have no cross-dependency on each other or on 2a/4a/4b and can
ship in parallel branches if more than one implementer picks this up. 4a
cannot fully close until 2a/2b land (issue #2342), but can start and land its
non-search parts first — see that file's own staging note.

## Process rules

Verification standard, credential handling, and the merge gate are not
repeated per-file — see [README.md](README.md)'s "Rules for the implementer."
Every task file below assumes them, on top of the two rules at the top of
this file (reuse-first, no new keys).
