# Slice 2a/2b — fan-out gains a Search slot

This is issue #2342's own scope. This file exists so the onboarding work has
a concrete sequencing pointer to it, not to re-litigate #2342's plan — its
own issue body is the source of truth for exact acceptance criteria. What's
here is the subset that gates slice 4a.

Note on [implementation-plan.md](implementation-plan.md)'s "no new keys"
rule: `search/managed/key` is the one deliberate, already-reasoned exception
— it closes a real gap (Search has no company-level tier at all today, unlike
Composio/Provider) rather than duplicating something that exists. It is not
a case of skipping the reuse-first check; it's #2342's approved conclusion
after that check.

## Architecture Impact

`company_key/types.rs`'s `Slot` enum (`Composio, Inference, Provider,
Default, Health`, `:22-28`) gains a `Search` variant. `fan_out.rs`'s write
path gains a matching arm that stores `search/managed/key` the same way it
stores `composio/tinyhumans/key` and `provider/tinyhumans/key` today — same
"never overwrite a slot that already holds its own key" guard, no new
priority system. `search/resolve.rs`'s `active()` gains a check for the
company's `search/managed/key` before it falls through to the bare
instance-operator env credential.

## Files to Modify

- `crates/opencompany-core/src/company/company_key/types.rs` — add
  `Slot::Search` to the enum.
- `crates/opencompany-core/src/company/company_key/fan_out.rs` — write arm
  for the new slot; the non-overwrite guard condition, whatever it's keyed
  on for the existing slots, must apply identically here.
- `crates/opencompany-core/src/company/search/resolve.rs` — `active()` reads
  `search/managed/key` before the env-only managed fallback.
- `crates/opencompany-core/src/company/search/mod.rs` — the "a company can
  never point search at a key it controls" doc comment gets updated to
  describe the new company tier explicitly, not silently contradicted.
- `crates/opencompany-core/src/harness/built_in/provider.rs:333-334` — same
  doc-comment update, the "consults ONLY the environment" line.
- `frontend/src/views/connections/ApiKeyView.tsx:564-567` — cascade copy
  ("Saving copies it to the LLM and Composio pages...") gains Search as a
  third named surface.
- Search's Managed row (`frontend/src/views/SearchView.tsx` /
  `frontend/src/search-providers/ProviderList.tsx`) stops being pure
  badge-rendering once a `search/managed/key` exists — needs the same
  replace/update affordance the LLM page's Managed row has. `ProviderList.tsx`
  today has no "Replace key" menu item on the Managed row at all (that only
  exists on the real-provider row renderer) — this is new UI, not a tweak.

## New Files

None expected — this extends existing enums/handlers rather than adding new
modules.

## Dependencies

None beyond what #2342 itself needs. Nothing in this repo currently blocks
starting it.

## Implementation Steps

1. Add `Slot::Search` to `types.rs`; run `cargo check` to find every
   exhaustive `match` on `Slot` that now needs a new arm (the compiler does
   this enumeration for you — do not grep for it by hand).
2. Add the fan-out write arm in `fan_out.rs`, matching the existing
   `Composio`/`Provider` arms' shape and guard exactly.
3. Add the `search/managed/key` read in `resolve.rs::active()`, ordered
   before the env-only fallback, after the company's own BYO provider check.
4. Update the two doc comments (`search/mod.rs`, `provider.rs:333-334`) in
   the same commit as the resolver change — not a follow-up.
5. Update `ApiKeyView.tsx`'s cascade copy.
6. Build the Managed-row edit UI on Search's provider list, mirroring the
   LLM page's Managed row's existing pattern (same component if one already
   exists generically, new one if not — check the LLM page's Managed row
   implementation before writing a new one).

## Testing Strategy

- Rust: a fan-out test asserting `Slot::Search` is filled on a fresh
  TinyHumans key save, alongside the existing Composio/Provider assertions.
  A `resolve.rs` test asserting `active()` prefers `search/managed/key` over
  the env credential when both are present, and falls through correctly when
  only the env credential exists.
- The non-overwrite guard: a test asserting a company's own BYO search
  provider key is untouched by a TinyHumans key save.
- Frontend: a test on the Managed row's new edit affordance, and an update
  to whatever existing test asserts on `ApiKeyView.tsx`'s cascade copy
  string.

## Risks and Edge Cases

- **The security-boundary doc-comment change is easy to land as code without
  updating the comment.** Explicitly checklist this in review — the comment
  is normative documentation of an intentional decision (D-boundary-change,
  README.md), not incidental prose.
- **The Managed-row edit UI is new UI, not a copy of an existing pattern** —
  budget real design/review time for it, it is not a mechanical change like
  the enum/resolver work.

## Developer Handoff

Start with the backend (`types.rs` → `fan_out.rs` → `resolve.rs` → doc
comments) as one PR; the frontend cascade-copy string and Managed-row edit
UI can be a second PR once the backend slot exists to build against. Do not
block the backend PR on the frontend one.
