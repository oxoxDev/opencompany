# Crash reporting

> Errors, panics and what a report may carry. Its companion,
> [tracing.md](tracing.md), covers performance tracing, the console-to-host
> distributed trace, and why Session Replay is not shipped.

What an OpenCompany install reports when something breaks, what it deliberately
never reports, and how an operator turns it on and proves it works.

The short version, and the only four sentences most readers need:

- **A self-hosted *host* reports nothing unless an operator configures a
  DSN.** With no DSN there is no client, and in a build without the
  `crash-reporting` cargo feature there is no code that could construct one.
  The official TinyHumans builds carry **compiled-in DSNs** for their own
  projects: a hosted tenant's host (`opencompany-core`), the desktop shell
  (`opencompany-tauri`) and the production console bundle
  (`opencompany-frontend`) — **including a self-hosted operator's console**,
  unless its build set `VITE_SENTRY_DSN=off` (as `deploy/docker-compose.yml`
  and CI do). See [Who supplies each variable, per
  deployment](#who-supplies-each-variable-per-deployment) for the console/host
  split. A configured DSN always wins, and `OPENCOMPANY_SENTRY=off` /
  `VITE_SENTRY_DSN=off` silence them.
- For a self-hosted install the destination is **the operator's own Sentry
  project**. This is not [analytics](analytics.md), which reports to a
  collector *this* project runs and is therefore hosted-tenant-only and
  payload-constrained.
- What is sent is **errors and panics**: the message, the stack trace, and the
  breadcrumbs leading up to it. No performance spans, no session replay, no
  request bodies.
- **Credentials are scrubbed on the way out**, and that scrubber is compiled and
  tested in every build, not only in a reporting one.

Two surfaces report independently and are configured independently: the Rust
host and desktop shell (`OPENCOMPANY_SENTRY_DSN`) and the React console
(`VITE_SENTRY_DSN`). They
share a release-tag format so events from one build line up whether the operator
files them under one Sentry project or two.

## Why this is opt-in and analytics is not merely opt-in

[analytics.md](analytics.md) argues at length that a GPL-3.0, self-hostable
crate must not phone home, and puts the network client behind a cargo feature so
that a shipped binary *cannot*. Crash reporting keeps that gate — the same
`crash-reporting` feature name the vendored runtime uses — and adds a second: a
DSN.

But the reason is different, and the difference is worth stating because it
decides what a payload may carry. Analytics reports to a collector this project
operates, so the interesting question is *who may receive this*, and the answer
is a structural guarantee that no content can be in the payload at all. A crash
report goes to an endpoint the operator chose, in their own organisation, about
their own install. The interesting question there is *what must never be in it*
— and the answer is credentials, machine identity, and anything about a person.

The consequence, stated plainly because it looks like an inconsistency until you
know it is deliberate: a crash report **may** contain a company id, a workspace
path, a tool name, a ledger slug, or agent-authored text, if one of those was in
the error message. Analytics never carries any of them. That is not a weakening
of the analytics contract; it is a different contract, to a different recipient,
that an operator opted into for their own install.

## Configuration

### The host

| Variable | Meaning |
|---|---|
| `OPENCOMPANY_SENTRY_DSN` | The DSN. When unset or blank, a **hosted tenant** (`OPENCOMPANY_DEPLOYMENT=hosted-tenant`, or `OPENCOMPANY_TENANT_ID` set) falls back to the compiled-in `observability::config::DEFAULT_HOSTED_TENANT_DSN` (the TinyHumans `opencompany-core` project); every other deployment stays silent. The default is gated on the deployment, not only on the feature, because `crash-reporting` is also compiled into the desktop app and can be compiled into a self-hosted image via `OPENCOMPANY_FEATURES`. The desktop has its own default; see [The desktop shell](#the-desktop-shell). |
| `OPENCOMPANY_SENTRY` | `off` forbids reporting and outranks everything else. `on` is accepted and means only "not off": without a DSN there is nowhere to send, so there is nothing to force. |
| `OPENCOMPANY_SENTRY_ENVIRONMENT` | Overrides the `environment` tag. Defaults to the deployment kind — `desktop`, `self-hosted`, `hosted-tenant` (see [`app::deployment`](../../../src/app/deployment.rs)). |
| `OPENCOMPANY_SENTRY_TRACES_SAMPLE_RATE` | Fraction of requests recorded as performance transactions, `0`–`1`. **Defaults to `0`.** See [tracing.md](tracing.md), including what a rate costs. |

Reporting happens only when **all** of these hold:

1. the binary was built with `--features crash-reporting`;
2. `OPENCOMPANY_SENTRY` is not `off`, and is not some third value;
3. `OPENCOMPANY_SENTRY_DSN` is set, or the deployment is a hosted tenant (which
   falls back to the compiled-in DSN);
4. that value is a usable Sentry DSN. A set-but-malformed value is refused,
   never replaced by the default.

Condition 4 is checked with `url` — the same parser the transport is handed the
string with — plus the four things that make a URL a *Sentry* DSN: an
`http`/`https` scheme, a public key, a host, and a project id. This is the same
rule issue #673 settled for a different call site and #1739 relearned for the
analytics endpoint: a second, hand-rolled reader of a URL grammar is a bypass
waiting to be found, and a URL that parses but is not a DSN is precisely the
input that resolves to "reporting" and then never delivers.

A DSN of the pre-2016 `https://key:secret@…` form is **refused**, not repaired.
No ingest has accepted the secret half for years, so such a value is either a
stale copy — silence with a reason beats a client that 401s forever — or a
credential pasted into the wrong variable, which is worth refusing loudly.

The release tag is `opencompany@<version>+<commit>`, built from
[`BUILD_COMMIT`](../../../src/build_stamp.rs), which already resolves an
explicit `OPENCOMPANY_BUILD_COMMIT`, then `git`, then `GITHUB_SHA`, then the
literal `unknown`. No new variable is invented for something the build already
stamps. When the commit is `unknown` it is dropped rather than appended:
`opencompany@0.1.0` is an honest "this build cannot say which commit it is", and
`opencompany@0.1.0+unknown` is a release name that looks like a commit and is
not one. A `-dirty` suffix is kept — a build from a modified tree is a different
build.

### The console

Read at **build** time by Vite, so they must be set when the console bundle is
built, not when the host runs.

| Variable | Meaning |
|---|---|
| `VITE_SENTRY_DSN` | The console's DSN. Unset or blank means the compiled-in `DEFAULT_CONSOLE_SENTRY_DSN` (the TinyHumans `opencompany-frontend` project) in a production build and silence under `vite dev`; `off` means silence (CI and `deploy/docker-compose.yml` pass `off`). A *different* project from the host's: a browser bundle's DSN is public by construction. |
| `VITE_SENTRY_ENVIRONMENT` | Overrides the `environment` tag. Defaults to `development` under `vite dev` and `production` otherwise. Set it to the host's value when the two surfaces should line up in one filter. |
| `VITE_SENTRY_SMOKE_TEST` | Exactly `true` fires one `console-sentry-smoke-test` event at init. See [Verifying it works](#verifying-it-works). |
| `VITE_SENTRY_TRACES_SAMPLE_RATE` | Fraction of page loads traced, `0`–`1`. **Defaults to `0`**, which installs no tracing integration at all. See [tracing.md](tracing.md). |
| `VITE_SENTRY_TRACE_PROPAGATION_TARGETS` | Extra origins that may receive `sentry-trace` headers, comma-separated. Same-origin always may. See [tracing.md](tracing.md). |
| `VITE_BUILD_COMMIT` | The commit for the release tag, shortened to twelve characters. The frontend's spelling of `OPENCOMPANY_BUILD_COMMIT`. |

`VITE_SENTRY_DSN=off` is the console's switch: a bundle is decided at build
time, so there is nothing to turn off at run time.

### Source-map upload (CI only)

Without uploaded source maps a console stack trace names minified chunks and is
close to useless. `@sentry/vite-plugin` handles the upload and is wired in
`frontend/vite.config.ts`, gated on `SENTRY_AUTH_TOKEN`:

| Variable | Meaning |
|---|---|
| `SENTRY_AUTH_TOKEN` | The gate. Release and staging workflows provide it as a secret; local and ordinary CI builds omit it and construct no plugin. |
| `SENTRY_URL` | The Sentry instance. **Required for a self-hosted Sentry**: without it the plugin defaults to sentry.io, where the upload lands somewhere the events never will. |
| `SENTRY_ORG`, `SENTRY_PROJECT` | Where to file the release. |
| `SENTRY_RELEASE` | Overrides the computed release tag, for a CI that already knows what it is shipping. |

`build.sourcemap` is tied to the same gate rather than being on unconditionally.
The plugin deletes the `.map` files after uploading them, so emitting them
without a plugin to delete them would ship the console's source to every viewer
of every build made without a token.

Two behaviours to know before wiring this into a release pipeline:

- **A bad token fails the build.** The plugin raises; the build stops. That is
  the intended direction — the alternative is the failure mode this whole
  document keeps naming, where something reports success and silently uploads
  nothing — but it does mean a mis-set secret blocks a release rather than
  degrading it.
- **The release tag must match.** The plugin files maps under the same string
  `Sentry.init` reports, because both read one `sentryRelease()` in
  `vite.config.ts` and the bundle receives it through a `__SENTRY_RELEASE__`
  define. If those two ever diverge, Sentry never joins frames to maps and stack
  traces stay minified with no error anywhere to say why.

Rust release profiles keep packed line tables while stripping shipped binaries.
`scripts/upload-sentry-symbols.sh` uploads their dSYM/DWP/PDB companions from
the hosted-image and desktop release pipelines, using the same organization
token as the console source-map upload. Missing credentials or a failed upload
fail the release rather than silently shipping unsymbolicated stack traces.

### The desktop shell

A double-clicked `.app` has no environment, so the desktop shell carries a
fallback: the `DESKTOP_DSN` constant in `crates/opencompany-app/src/crash.rs`.
It names the **desktop's own** Sentry project (`opencompany-tauri`), never the
host's, because anyone who unzips the `.dmg` can read it and a project that
exists only for this binary is the only one it may write to.

- `OPENCOMPANY_SENTRY_DSN`, when set and non-blank, still wins.
- `OPENCOMPANY_SENTRY=off` is resolved before any DSN and silences both.
- Every desktop build carries it, including a source build or `cargo run`; set
  `OPENCOMPANY_SENTRY=off` to silence one.
- `opencompany-desktop sentry-test [--message …]` sends one event through the
  same decision and client and prints its id, without opening a window.

The shell's Rust debug symbols are uploaded to `opencompany-tauri`. The Sentry
URL (`https://sentry.tinyhumans.ai`), org (`tinyhumans`) and project slugs
(`opencompany-frontend`, `opencompany-core`, `opencompany-tauri`) are hardcoded
in the workflows; only `SENTRY_AUTH_TOKEN` is a secret.

### Who supplies each variable, per deployment

Every variable above is read from *somewhere*, and "somewhere" differs per
surface. The table is here because the failure this document keeps naming —
silence that looks like health — is almost always a variable nobody owns rather
than a variable set wrong (issue #2392).

| Surface | Host DSN comes from | Console DSN comes from |
|---|---|---|
| Hosted tenant | the tenant env if set, else the compiled-in `opencompany-core` DSN — **only if the process knows it is a hosted tenant** (`OPENCOMPANY_DEPLOYMENT=hosted-tenant` or `OPENCOMPANY_TENANT_ID`) | the compiled-in `opencompany-frontend` DSN, baked by Vite into the image |
| Desktop | the shell's process env, else the compiled-in `opencompany-tauri` DSN | the compiled-in `opencompany-frontend` DSN |
| Self-hosted | the operator, by design | the operator's build: `off` under Compose, else the compiled-in default unless set |

Two consequences worth stating rather than deriving.

**A hosted tenant's console DSN is decided when its image is built, and cannot
be changed afterwards.** Vite inlines it; an `OPENCOMPANY_SENTRY_DSN` set on a
running tenant reaches the host and not the bundle.

**A host DSN, by contrast, takes effect at the next process start** — which on
Firecracker means the next *cold* boot, because a parked tenant resumes from a
snapshot and never re-reads its environment. Editing the overrides file changes
nothing observable until the snapshot is discarded.

`OPENCOMPANY_SENTRY_ENVIRONMENT` is one repository variable read by three
workflows, and setting it defeats each one's fallback. Left unset, the hosted
image tags `hosted-tenant`, the desktop build tags `desktop`, and the host
agrees with each because both halves derive the same default from the
deployment kind. Set to a single value, all three surfaces file under it and
stop being separable in a filter. Prefer leaving it unset.

## What is collected

| | Host | Console |
|---|---|---|
| Errors | every `tracing::error!`, as an event | every uncaught exception and unhandled promise rejection |
| Panics / render crashes | every panic, via the SDK's panic hook | every React render crash, via the top-level `ErrorBoundary` |
| Stack traces | yes, including on `error!` events | yes |
| Breadcrumbs | `warn!` and `info!` events — see the note below | `fetch`, `xhr` and history navigations |
| Release, environment | yes | yes |
| Instance identity | the random instance id from [data-root.md](data-root.md), and the storage backend, as tags | none |
| Performance spans | **not unless asked for** — `0` by default; see [tracing.md](tracing.md) | **not unless asked for**, and the integration is not even installed at `0` |
| Session replay | n/a | **no**, and not shippable behind a flag either — the sample rates are pinned to zero and the integration left out. The reasoning, with measurements, is in [tracing.md](tracing.md#session-replay-deliberately-not-shipped). |

**Host breadcrumbs follow `RUST_LOG`.** The Sentry bridge sits under the same
`EnvFilter` as the terminal formatter, and this crate's default filter is
`error` — so out of the box a report carries the error and no breadcrumbs. That
is deliberate: filtering the bridge separately would enable INFO-level call
sites process-wide, a standing cost paid by every install for a benefit only a
reporting one gets. `RUST_LOG=info` collects them.

## What is never collected

**Credentials of any kind.** Every string a report can carry goes through
[`observability::redaction`](../../../src/observability/redaction.rs) on the
host and its line-for-line port in `frontend/src/lib/crash-reporting.ts` on the
console: the event message, the log entry, every exception value, every
breadcrumb message and data field, every tag, and every string leaf of the
structured fields a `tracing` event brings with it.

The scrubber removes two things:

- **tokens that identify themselves** — `sk-`, `ghp_`, `github_pat_`, `glpat-`,
  `xoxb-`, `AKIA`, `th_`, a JWT, and the rest of the list in that file;
- **the value of a key that names a credential** — `api_key=`, `"token":`,
  `password =`, `--token`, `Authorization: Bearer`, and a URL's userinfo
  (`https://user:pass@host`).

It is token-wise rather than a set of regexes, and that is the one design
decision in it. The vendored runtime's equivalent is seven regexes whose scars
are instructive: `token[=:\s]+\S+` matched `cancellation_token=` and
`next_page_token=` until a `\b` was added, and `sk-[A-Za-z0-9]{20,}` left a
trailing `_uv` behind on any key with a separator in it until the character
class grew. Both are the same failure — a regex over raw text does not know
where a word begins — so this splits the text into tokens first and asks
whole-token questions. `cancellation_token` is one token and normalises to
`cancellationtoken`, which is not a key, so the false positive cannot arise
rather than being patched out.

**It is a last line of defence, not the first.** A scrubber is heuristic by
construction and cannot recognise a secret that looks like a word. The first
line is that a credential is not printable: `SecretValue`
([`ports::types`](../../../src/ports/types.rs)), `analytics::ProjectToken` and
`observability::Dsn` all refuse to `Debug` themselves, for exactly the reason
[credentials.md](credentials.md) sets out. A call site that relies on the
scrubber is one release away from leaking.

**Anything that identifies a person.** `send_default_pii` is off on both
surfaces, so no IP address and no cookies. Neither surface ever binds a Sentry
`user`: the only stable per-person identifier either one holds is an email
address, and Sentry's `user` is the field its UI counts uniques on — which is
precisely the shape that must not leave. A crash report answers "which install,
which build, what broke". "Who was signed in" is a question for the host's own
[journal](journal.md).

**The machine's name.** `server_name` is cleared on the host; the console never
had one.

**Request bodies and URLs.** The console's `beforeSend` reduces
`event.request` to a single `User-Agent` header and drops everything else — the
URL, its query string (where a magic-link `?code=` lives), and any cookie. The
one header stays because Sentry derives `os` / `browser` / `device` server-side
by parsing it, and dropping the envelope outright loses all platform context;
that is a lesson from the vendored console, which lost it for months.

**Frame locals and source context.** Cleared on both surfaces. Nothing in the
Rust SDK populates them today, so on the host it is defence against a future
that does — but a captured local is a captured credential.

**Contexts the SDK did not derive.** The console allow-lists `os`, `browser` and
`device` and drops the rest, because an unknown context has unknown shape and an
allow-list fails in the safe direction.

### What is deliberately *not* filtered

Neither `before_send` ever drops an event. They are scrubbers, not filters.

The vendored runtime's hook is the opposite — seventeen drop branches, each
anchored to a Sentry issue id and an event count, suppressing transient provider
failures and quota errors that had drowned its dashboard. That is the right
answer for a product reporting to a project its own team triages, and the wrong
one here: this repository does not receive these events and cannot know which of
an operator's errors are noise. Suppression belongs in the operator's own
project, where they can see what they are suppressing.

The console makes one exception, `ignoreErrors`, and only for browser noise that
is not an error anywhere: `ResizeObserver loop`, `Failed to fetch`,
`AbortError`, and their kin. One trap worth recording, because it looks like
working config and is not: `defaultIntegrations: false` **silently disables
`ignoreErrors`**, because `inboundFiltersIntegration` is what consumes the
option. `frontend/src/lib/sentry.ts` re-adds it by hand for that reason.

## Where it hooks in

| Seam | File | Why there |
|---|---|---|
| `sentry::init` | `src/bin/opencompany.rs`, first statement of `async_main` | The panic hook is installed here, so anything that panics earlier panics unobserved — and a malformed data root or an unlockable home are exactly the early panics worth reporting. |
| desktop `sentry::init` | `crates/opencompany-app/src/lib.rs`, before the subscriber and Tauri runtime | The shell and every embedded host share the core's client, scrubber, panic hook and tracing bridge. The environment is wrapped in `crash::DesktopEnv`, which supplies the compiled-in desktop DSN only when none is set. |
| the `tracing` bridge | `observability::tracing_layer`, added to the subscriber | One seam for every `tracing::error!` in the tree, rather than a reporting call at each. |
| scope identity | `observability::scope::identify`, from the `serve` arm after the port is bound | The instance id and the storage backend are not known until the companies are registered — the same reason `analytics::boot::install` runs there. |
| flush | `src/bin/opencompany.rs`, after the bound host stops serving | The error that took the host down is queued at the moment it stops. Bounded at 2s (`observability::FLUSH_TIMEOUT`), sized like `analytics`'s: the collector is a third party, and a drain that overruns Kubernetes' 30s grace buys a `SIGKILL` in the middle of the shutdown those seconds protect. |
| transaction scrubbing | `observability::ScrubbingTransport`, wrapping the SDK's own transport | `sentry` 0.47 has **no `before_send_transaction`** — `Transaction::finish` posts an envelope straight to the transport — so the guarantee is made at the last point before bytes leave the process, where it covers every envelope kind rather than the ones with a callback. See [tracing.md](tracing.md#scrubbing-a-transaction-and-why-it-is-not-a-before_send). |
| HTTP transactions | `observability::instrument_http`, from `server::routes::router` | One place, added only when the live client is recording, so an install with tracing off carries no extra middleware. |
| `Sentry.init` | `frontend/src/main.tsx`, before the first render | A crash during the first render is the one worth reporting, and a boundary armed after it would miss it. |
| `ErrorBoundary` | `frontend/src/main.tsx`, outside every provider | The thing that crashes may be `ThemeProvider` itself, and a boundary inside a provider cannot catch that provider's own throw. `CrashFallback` therefore depends on no context. |

That independence has one consequence worth recording, because it was invisible
until the screen was looked at in a browser: `next-themes` stamps `class="dark"`
on `<html>` from an **effect**, so a crash during the first render means that
effect never ran and every `.dark` token in `index.css` is unset. The crash
screen painted in full light on a machine that had never shown a light pixel.
`CrashFallback` therefore resolves the theme itself — `next-themes`' own
`localStorage` key, then the OS preference — and puts `dark` on its own
container.

The console's crash screen shows the Sentry event id **only when an event was
actually sent**. The SDK mints an id locally whether or not a DSN is configured,
and showing it on an install that reports nothing hands the operator a reference
nobody can look up.

## Verifying it works

Neither surface can be verified by reading configuration; both have a way to
send one deliberate event.

### The host

```console
$ opencompany sentry-test
crash reporting: reporting to https://o0.ingest.sentry.io/0 as opencompany@0.1.0+d31e532f7c8a (self-hosted)
0d1b1a3c9f7e4a1e8f2b6c5d4e3f2a10
```

The event id goes to **stdout** and every diagnostic to stderr, so the command
pipes. It exits non-zero when reporting is off — a verification tool that
succeeds while nothing is configured is worse than none, because it retires the
question — and it warns loudly when the flush times out, because an unconfirmed
round trip reported as success is the same lie.

`--message <text>` sets the body. `--panic` panics afterwards, which exercises
the panic hook as well as the direct capture path; the process aborts, and that
is the point.

In a build without the feature the subcommand still exists and says so, rather
than looking like a subcommand that was never added.

### The console

Build once with `VITE_SENTRY_SMOKE_TEST=true`, load the console, and look for a
single `info`-level `console-sentry-smoke-test` issue. That one event proves the
DSN, the release tag and — if its stack frames name real files — the source-map
upload, all at once. Unset it afterwards.

### Without a real DSN

Both surfaces initialize against a syntactically valid but fake DSN
(`https://fake@o0.ingest.sentry.io/0`) without erroring; the events simply never
arrive. That is enough to check the wiring — the boot line, the subcommand's
exit code, the console's `Sentry.getClient()` — without an account.

## Testing

| Lane | Command |
|---|---|
| default build — the decision, the DSN grammar, the release tag, the scrubber, and the not-compiled downgrade | `cargo test --locked` |
| gated — the `before_send` hook against real protocol types | `scripts/ci/run-scoped-suite.sh "crash reporting" crash-reporting observability` |
| console — the decision, the scrubber, the event sanitizer | `npm test` (`frontend/test/unit/crash-reporting.test.ts`) |

`scripts/ci/feature-lanes.txt` records the second as `partial`, per the rule in
[`CLAUDE.md`](../../../CLAUDE.md) that every feature says which lane runs its
tests.

The split matters and is the same one `analytics` makes. The parts that have to
be provably right — whether this process reports at all, and what a string
carries when it leaves — name no `sentry::` type and run in the **default**
build, where a `crash-reporting` lane would never reach them. Only the hook that
needs the protocol types compiled in is gated, and the test that matters there
builds one event carrying a credential in every field an event can carry one in,
runs it through the real `before_send`, serializes the result, and greps it.

## The compile-time domain gate

Worth reading before changing this module, because the shape is deliberate and
copied.

Every function in `src/observability/` is compiled in **both** builds with the
**same signature**. Only the bodies that name a `sentry::` type are behind
`#[cfg(feature = "crash-reporting")]`. So `tracing_layer()` returns a no-op
`Identity` layer rather than nothing, `scope::identify` keeps its
`tracing::debug!` and drops the scope call, and `opencompany sentry-test` still
exists.

The alternative — a stub module, or a `#[cfg]` at each call site — spreads the
gate into its callers, and a gate with many call sites eventually gets one of
them wrong. This is the arrangement `vendor/openhuman/Cargo.toml` documents
under "TYPE CARVE-OUT", and the feature carries the same name there for the same
reason: `sentry` is a 0.x crate, so two different minors would link two
`sentry::Hub` statics and `sentry::init` here would leave the embedded runtime's
copy uninitialised.

## Known limitations

Named so they are countable rather than implied.

- **No debug-file upload for the host.** A stripped release binary's stack
  traces stay unsymbolicated until a `sentry-cli upload-dif` step exists. See
  the note under [Source-map upload](#source-map-upload-ci-only).
- **The compiled-in DSNs are readable from the binaries.** Anyone with the
  `.dmg`, the tenant image or the console bundle can post to those projects.
  That is why each is a project of its own: abuse costs that project's quota
  and nothing else, and rotating a key is a rebuild.
- **Desktop reporting uses the project Sentry origin only.** The release
  workflow supplies the console DSN, whose events leave from the webview;
  `crates/opencompany-app/tauri.conf.json` therefore permits
  `https://sentry.tinyhumans.ai` in `connect-src`. It remains a narrow
  allowlist: OpenPanel stays unavailable in Tauri and a different Sentry host
  requires an explicit CSP review alongside the release configuration.
- **Host cognition is not tagged per company.** A multi-company host reports one
  instance id for all of them. Which company an error belongs to is in the
  `tracing` fields on the event, not in a tag.
- **A delivered envelope is not an accepted one, and nothing here can tell.**
  `sentry-test` reports success when the queue drained, which means the ingest
  answered — not that it kept anything. An organisation over its quota answers
  `429 organization:error_usage_exceeded` and discards the event, and the SDK
  surfaces that only at its own debug log level. So an install can be correctly
  configured, correctly scrubbed, transmitting well-formed envelopes, and still
  be reporting nothing, with every signal in this product saying it is healthy.
  This was observed against a real project, not imagined — and then again, at
  scale: between 2026-09-15 and 2026-09-18 `sentry.tinyhumans.ai` recorded
  **zero** accepted events org-wide, down from ~124k/day, while Relay kept
  answering `200 {"id":…}` to every sender. Every service pointed at it reported
  into a void for three days and every one of them looked healthy. Check quota
  and accepted-outcome stats in Sentry itself; nothing in the boot line or
  `sentry-test` will say.

  The one place this *is* now caught is `release-production.yml`, which runs
  the release image as a hosted tenant with **no DSN set** (so the compiled-in
  default is what is tested), waits up to 15s for the send, and reads
  the event back through `/api/0/organizations/{org}/eventids/{id}/` after
  sending and fails the release if it was never stored. That is the only check
  in this repository that distinguishes "the ingest answered" from "the event
  exists", and it is why the gate is a readback rather than an exit code. Note
  the id must be unhyphenated for that lookup; the SDK prints a hyphenated UUID
  and the hyphenated form 404s forever.
- **Nothing enforces that the two surfaces' versions stay in step.** Both tags
  are shaped `opencompany@<version>[+<commit>]`, but the host reads
  `Cargo.toml`'s version and the console reads `frontend/package.json`'s. They
  agree as this is written, and they agree only because someone kept them that
  way: no check compares them, so a release that bumps one and forgets the
  other drifts them silently and nothing reports it. That is not hypothetical —
  `chore(release): 0.1.1` bumped `Cargo.toml`, both `crates/opencompany-app` manifests and
  `tauri.conf.json`, missed `frontend/package.json`, and the gap was found in
  review rather than by any lane. The commit half still matches for a build that
  sets `OPENCOMPANY_BUILD_COMMIT` / `VITE_BUILD_COMMIT`, which is what actually
  identifies a build; set `SENTRY_RELEASE` explicitly if the two surfaces must
  file under one exact string whatever the versions say.
- **Host spans stop at the HTTP boundary.** See
  [tracing.md](tracing.md#known-limitations).
- **`Authorization: Token <value>` with an unprefixed value is not scrubbed.**
  `token` is excluded from the no-separator key list because as an English word
  it would eat the next word of ordinary prose ("the token was rejected"). Every
  real personal access token carries an issuer prefix and is caught by that rule
  instead.
