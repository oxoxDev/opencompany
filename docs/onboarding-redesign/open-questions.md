# Open questions and risks

Not decided. Each needs a real answer before or during its slice, not an
assumption baked into the implementation.

## Deferred: the grant-landing relocation problem

A one-click "Login with TinyHumans" OAuth grant button was considered for
Managed step 1 and explicitly descoped from this implementation — the
existing "Connect to TinyHumans" paste-a-key dialog is Managed step 1
instead, verbatim. The relocation problem this would have raised (the
redeemed grant's one-time code lives in a module-level variable,
`pending-key-link.ts:20`, read today by exactly one caller,
`useRedeemKeyGrant` inside `ApiKeyView.tsx` — a new caller in the wizard
would need to either share `App.tsx`'s boot sequence or a relocated stash)
is not this implementation's concern. Recorded here only so it isn't
re-discovered from scratch if a login button is built later.

## Resolved: does a freshly-registered company ever need `rebuild_if_pending`?

Yes on the managed branch, no on the self-managed one, and slice 4a already
made the call on both. The two differ in what the manifest says at the moment
the company is registered, which is the only moment brain selection happens.

**Managed.** The wizard sends no `company.inference` — it sends an account key
and a model instead. So the seeded company boots with nothing to resolve and
gets the offline echo brain. The fan-out that follows fills the LLM copy, the
`tinyhumans` row and the default, at which point a tenant config resolves,
`restart_pending` flips true, and `rebuild_if_pending` moves the company onto
the harness. Without that call the operator's first chat echoes behind a
"restart required" notice they have no reason to look for.

**Self-managed / BYOK.** The apply writes `manifest.inference.provider` (and
its base URL and models) *before* `seed_generated_company`, so the company is
already configured when its runtime is built and boots straight onto
`HARNESS_PATH`. `restart_pending` is false, and no rebuild is owed there — not
by this slice, and not by 4b.

There is no race either way: `register()` resolves inference fresh at boot from
the record it just wrote, and the managed key-save happens after the company
exists rather than before it. What was missing was not a call but a test — the
rebuild is unreachable at default features, where `harness_reachable` is a
`false` stub, so the whole capability sat unproven. The gated
`the_wizards_account_key_rebuilds_the_company_it_just_seeded`
(`server/setup/test.rs`) is the proof; its self-managed sibling pins the
ordering the "no rebuild owed" half depends on.

## The security-boundary shift for search's managed credential

Already called out explicitly in issue #2342 and in this folder's
D-boundary-change: today, `search_backend_from_env`'s doc comment
(`provider.rs:333-334`) states the managed-search credential resolver
consults only the environment, "so a company can never point search at a key
it controls." #2342 deliberately crosses this line for the company tier. This
folder inherits that decision rather than re-deciding it, but flags it again
here because onboarding is the surface that will make this common — most
companies that connect TinyHumans during setup will now be exercising the
crossed boundary from their very first session, not as an edge case reached
later. Worth a second look from whoever reviews #2342's implementation,
specifically asking: does making this the *default* path (via onboarding)
change the risk calculus versus it being an opt-in a company reaches later
via Connections → Account?

## Answered: Composio's skip-for-later state, self-managed branch

**Skipping records nothing, on either half of the step.** There is no
"explicitly deferred" state to store and none is added.

Composio's card already has the resting state, and it is the honest one:
`composioRows(null)` returns both rows for a company with no status at all —
`modeOf` reads a missing mode as `managed`, which is where a company with
nothing configured genuinely is — with a token to add on the managed route and
the own-account route offering to be chosen. That is the same card Connections
draws, so the wizard mounts it rather than inventing an empty state for it.

What is *not* stored is the distinction between "deferred" and "never tried".
`null` is the truth about a company minutes old, and nothing downstream could
act on the difference: no surface reads it, no later prompt is gated on it, and
a company that skipped and a company that has not got there yet want exactly the
same thing offered next. So the reassurance is shown while the operator is
standing on the step — "you can add this later under Connections" — and
forgotten when they press Next, which is what the step component unmounting does
for free.

One shape was deliberately not reused: the old model step's `tested =
{kind:"skipped"}`. That is a single verdict slot read by the step gate *and* by
the design pass's `modelless`, so recording a Composio skip in it would have
suppressed the roster design brief — a real bug, from an operator saying "later"
to their integrations.

## Whether `visibleSteps`' hide-conditions still make sense with a branch point

Today's `visibleSteps` filtering (`SetupWizard.tsx:665-674`) hides `power`
when the host already supplies inference. With `power` replaced by the
step-0 branch + two step-1s, the equivalent condition ("this host already has
inference — skip both step-1 branches entirely and go straight to step 2") is
new logic, not a direct port of the old one. Needs its own explicit handling
in slice 3, not an assumption that hiding a step generalizes cleanly to
hiding a branch.
