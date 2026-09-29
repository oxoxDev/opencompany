# Repository Guidelines

## Project Structure & Module Organization

OpenCompany is a Rust 2024 Cargo workspace rooted at `Cargo.toml`, with one
manifest per flavour under `crates/`:

- `crates/opencompany-core`: the host — package `opencompany-core`, library
  crate `opencompany`, binary `opencompany`. Its `src/`, `tests/`, `benches/`,
  `examples/` and `build.rs` live beneath that manifest. Every `src/...` path
  in this file and under `docs/` is short for `crates/opencompany-core/src/...`;
  the data the crate embeds and reads (`companies/`,
  `frontend/`, `vendor/`) stays at the repository root, `../..` from
  `CARGO_MANIFEST_DIR`.
- `crates/opencompany-app`: the Tauri desktop shell. Excluded from the
  workspace on purpose (its manifest says why); it has its own `Cargo.lock`.
- `crates/opencompany-tui`: the terminal client, embedding the host.

Rust source for the host lives under `crates/opencompany-core/src/`
(`src/` below). Public module surfaces live in
source module directories:

- `src/app/`: runtime configuration and shared Axum state
- `src/server/`: Axum router and HTTP handlers
- `src/ledger/`: dynamic ledgers — declared record shapes, the append-only fold,
  and the `derived/` folder they render into (`docs/spec/runtime/ledgers.md`)
- `src/globals/`: the global baseline — the agents, workflows, skills and
  starting tool belt every company gets whichever vertical it started from,
  authored in `companies/_globals/` and embedded at build time
  (`docs/spec/runtime/globals.md`)
- `src/harness/`: the execution engines — the embedded OpenHuman runtime and one agent per teammate (feature `openhuman`)
- `src/hive/`: hive desks — one `OpenHumanHive` per `[[group_chat]]`, speech over the `opencompany` MCP server, Jev routing, referral (`docs/spec/runtime/hive.md`)
- `src/tiny/`: optional TinyAgents crate feature/status surface

The command-line entrypoint lives in `src/bin/opencompany.rs`. Business types
are data-only definitions under `companies/` (a `company.toml` manifest plus a
`README.md` — not Cargo crates), loaded at runtime via `opencompany serve
--company companies/<name>`. What every company has regardless of which of
those it started from is authored beside them in `companies/_globals/`. The operator
console is a Vite/React app under `frontend/`. Design notes and module specifications live in `docs/`, with
`docs/spec/README.md` as the top-level architecture reference and
`docs/modules/` holding per-surface design docs.
The vendored runtime source is the `vendor/openhuman/` Git submodule. TinyAgents
is inherited from OpenHuman at `vendor/openhuman/vendor/tinyagents/`.

Prefer small modules with focused responsibilities. Keep core type definitions
in a dedicated `types.rs` file and package-local tests in a sibling
`<stem>_tests.rs` file (see "Testing Guidelines"). Every source directory
carries a `README.md` describing what lives there, file by file.

## Build, Test, and Development Commands

- `cargo fmt --all -- --check`: verify Rust formatting without changing files.
- `cargo fmt`: format Rust source files.
- `cargo clippy --all-targets -- -D warnings`: run lint checks.
- `cargo build --all-targets`: compile library, binary, tests, and examples.
- `cargo test`: run the full test suite.
- `cargo run --bin opencompany`: run the CLI.
- `cargo run --bin opencompany -- serve`: run the Axum HTTP server on `127.0.0.1:8080`.
- `cargo run -p opencompany-tui`: run the terminal client over the default data root.
- `./scripts/dump-prompt.sh --company companies/<name>`: print the system prompt each agent in that bundle is built with (`docs/spec/runtime/agents.md`).
- `git submodule update --init vendor/openhuman`: initialize OpenHuman.
- `scripts/ci/init-vendored-submodules.sh`: initialize its vendored crates.
- `cargo check -p opencompany-core --features tiny`: compile against OpenHuman's TinyAgents pin.

Run commands from the repository root. A command that names a feature or a
target names the package too (`-p opencompany-core --features ...`); the bare
`cargo fmt`/`clippy`/`test` lines cover every workspace member.

`rust-toolchain.toml` pins an **explicit** Rust version (issue #1298), and
every `dtolnay/rust-toolchain` call site in `.github/workflows/` passes that
same version. Do not change either back to `stable`. Both said `stable` until
rustc 1.98.0 shipped on 2026-08-18 with a newly-enforced
`clippy::result_large_err`, at which point every open PR in the repo went red
at once — including PRs touching no Rust — while everyone still on 1.97.x
passed `cargo clippy` locally and could not reproduce it. A pin turns the next
stable release into one reviewable bump PR instead.

To bump: edit `rust-toolchain.toml`, then run
`scripts/ci/assert-toolchain-pin.sh`, which fails and names any workflow call
site still on the old version. That script runs in the `rust` job, so a
half-finished bump is caught rather than shipped. Because the pin is in
`rust-toolchain.toml`, a plain `cargo` in this checkout already uses it — you
do not need `cargo +<version>`, and if your `rustup` lacks the toolchain it
will fetch it.

`.cargo/config.toml` sets `RUST_MIN_STACK = 8388608` for every cargo-invoked
process (issue #895). Do not drop it. The gated suite exceeds libtest's 2 MiB
default thread stack, and when it does the failure is a `SIGABRT` that aborts
the **whole test binary** — so every test after it is skipped, and the symptom
reads as "I broke the harness" rather than "this needs a bigger stack".

8 MiB is 2.7x the measured floor: the whole gated suite aborts at 2 MiB and
passes 4182/4182 at 3 MiB (aarch64-darwin, default parallelism). The margin
covers x86-64 CI frame layout, higher CI parallelism, and growth. The depth is
cumulative `async fn` composition in the vendored OpenHuman turn chain (~117 KiB
for its largest single future, against ~32 KiB for the largest OpenCompany-owned
one), so it is not bounded from this repo — `Box::pin` at our own seam was tried
and moved it by nothing. Full evidence is in `.cargo/config.toml`.

If you change the value, say what you measured; an unexplained ceiling is the
ratchet #895 exists to complain about. Export `RUST_MIN_STACK` yourself to
override it — the file does not use `force`.

## Coding Style & Naming Conventions

Use standard `rustfmt` output and Rust 2024 idioms. Module and file names should
be `snake_case`; public types should be `PascalCase`; functions, methods,
fields, and local variables should be `snake_case`. Return `Result<T>` using
the crate error type from `src/error.rs`.

No file under `crates/*/src` may exceed **750 lines**;
`scripts/ci/assert-rs-source-layout.sh` fails the `Rust source layout` job
otherwise. Fix it by splitting along a real seam, never by deleting: keep
`foo.rs` as the module root, move a coherent part into `foo/<part>.rs`
declared with `mod <part>;`, and re-export so every existing `crate::...`
path still resolves. Integration targets under `tests/` and `examples/` are
exempt because CI selects those per file (issue #475).

Every `pub` item gets a `///` doc comment saying what it does and why it
exists, and every file a `//!` header; the surrounding comments in this repo
explain reasoning and cite the issue that motivated them — match that.

## Testing Guidelines

Add focused tests with every behavior change. Keep tests near the module they
exercise unless they verify cross-module behavior, in which case place them in
the consuming module or in `tests/` as an integration target.

Unit tests live in a sibling file named after the source stem — `foo.rs` →
`foo_tests.rs`, `foo/mod.rs` → `foo/foo_tests.rs` — never in an inline
`#[cfg(test)] mod tests { ... }` block (the same CI script as the line cap
rejects those). Declare it from the source file as

```rust
#[cfg(test)]
#[path = "foo_tests.rs"]
mod tests;
```

so the module is still `foo::tests`, `use super::*;` still reaches private
items, and existing test paths keep working. When a test file passes 750
lines, split it by topic into `foo_<topic>_tests.rs`, each declared the same
way with its own module name.

A new file under `tests/` is not covered until a CI job both selects it and
enables the features its crate-level `cfg` needs (issue #475). A target missing
either builds, runs and reports zero without failing anything. The gated `Rust
(openhuman, tinycortex)` job runs `--tests` and then asserts a non-zero count
per target via `scripts/ci/assert-integration-targets-run.sh`; if your target
needs a feature set no lane builds, add the lane and run that script there too
rather than loosening the `cfg`.

A feature-gated test has the same problem one level up (issue #770). Cargo
features are additive and every CI lane pins an explicit feature set, so a test
behind `#[cfg(feature = "x")]` is compiled by `Check (--all-features)` and
executed by nothing unless some lane enables `x` — and nothing reports the
silence. Every feature therefore needs a row in `scripts/ci/feature-lanes.txt`
saying which lane runs its tests (`tested`/`partial`) or why none does
(`compile-only`, with a reason). `scripts/ci/assert-feature-lanes.sh` fails on an
unclassified feature, and fails a `compile-only` row that turns out to have a
gated test. When you add the lane, run it through
`scripts/ci/run-scoped-suite.sh`, which asserts a non-zero count — a filter that
selects nothing exits 0.

Maintain at least 80% coverage for meaningful library behavior. Document any
intentionally untested edge case in the PR description.

## Documentation Expectations

Keep `README.md`, `docs/spec/README.md`, and module docs in `docs/modules/`
aligned with code changes. Prefer concrete examples over vague descriptions,
especially for Axum routes, OpenHuman launcher behavior, and `tiny*` feature
integration.

Keep every Markdown file, including this one, at 500 lines or fewer. When a
topic grows past that limit, split it into focused files and link them from the
module's `README.md`.

## Running under the platform harness (hosted mode)

This repo is also the tenant workload of the OpenCompany hosting platform:
the `opencompany-manager` control plane (the superproject at
`tinyhumansai/opencompany-microservices`, where this repo is the
`opencompany/` submodule) builds this crate into a per-tenant container and
injects its environment. When developing hosted behavior, know the seams:

- The manager injects `OPENCOMPANY_COMPANY`, `OPENCOMPANY_BIND=0.0.0.0:8080`,
  `OPENCOMPANY_DATA_DIR=/data`, and `OPENCOMPANY_PUBLIC_URL` into every
  tenant container. `OPENCOMPANY_DATA_DIR` is the instance data root for
  the workspace layout, the company-bundle home, **and** the embedded OpenHuman
  runtime's own root: `serve` derives `<data-dir>/openhuman` and exports it as
  `OPENHUMAN_WORKSPACE`, because the vendored runtime otherwise defaults its
  durable agent journal into `$HOME` — the read-only root filesystem in a tenant
  (issue #446). An unwritable journal root aborts boot; see
  `docs/spec/runtime/storage.md`. `deploy/entrypoint.sh`
  additionally forwards it as `--home "$OPENCOMPANY_DATA_DIR"`, which resolves
  identically (the flag outranks the variable). Locally it is the only knob that
  isolates two `serve` processes from each other — see
  `docs/spec/runtime/storage.md`. It also injects `OPENCOMPANY_ADMIN_EMAIL`, the
  address that provisioned the instance: a standing admin invite equivalent to a
  manifest `[users].admins` entry, without which a provisioned company (whose
  manifest names nobody) has nobody eligible to sign in — see
  `docs/spec/runtime/users.md`. Plus — when database-per-tenant storage is enabled —
  `OPENCOMPANY_STORAGE=mongodb`, `OPENCOMPANY_MONGODB_URI` (credentials
  scoped to that tenant's database only), and `OPENCOMPANY_MONGODB_DB`.
- In the alternative **shared-single-DB** mode (all tenants on one logical
  MongoDB), the manager also injects `OPENCOMPANY_TENANT_ID=<tenant-slug>`.
  The workload then namespaces company ids with `<tenant>--` and records
  `owners` rows so tenants stay apart in the shared database. Isolation is
  application-layer only in this mode — a compromised container can reach
  every tenant's documents; db-per-tenant stays the security default. See
  `docs/spec/runtime/storage.md`. Unset (the default) is a full no-op.
- The manager should also inject `OPENCOMPANY_DEPLOYMENT=hosted-tenant`. That
  is the one telemetry input a tenant needs: a hosted tenant reports product
  analytics to the TinyHumans OpenPanel (`https://panel.tinyhumans.ai/api/track`,
  compiled-in client id) and crashes to the compiled-in `opencompany-core`
  Sentry DSN. `OPENCOMPANY_ANALYTICS_CLIENT_ID` / `OPENCOMPANY_ANALYTICS_ENDPOINT`
  / `OPENCOMPANY_SENTRY_DSN` override those defaults, `OPENCOMPANY_ANALYTICS=off`
  / `OPENCOMPANY_SENTRY=off` silence them, and there is no client secret. Any
  endpoint must be `https`, or `http` to a loopback host: the client id is a
  request header on every request, and the workload refuses a plain-`http`
  collector rather than warning about it. These particular defaults — the
  hosted-tenant analytics client id/endpoint and the `opencompany-core`
  Sentry DSN — never apply to desktop or self-hosted deployments. The desktop
  shell has its own, separate compiled-in Sentry default (the
  `opencompany-tauri` project); see `docs/spec/runtime/crash-reporting.md`.
  None of them is required to boot: an instance that says nothing is treated as **self-hosted**
  and reports nothing, which is the safe direction and the documented default
  (`docs/spec/runtime/analytics.md`). `OPENCOMPANY_TENANT_ID` alone also implies
  a hosted tenant, so shared-single-DB tenants are covered without the new
  variable; db-per-tenant tenants need it.
- Storage backend selection and the MongoDB backend are documented in
  `docs/spec/runtime/storage.md`; the port traits it implements are the
  entire persistence contract (`docs/spec/runtime/ports.md`).
- The container must serve `/healthz` on `:8080` quickly — the manager's
  wake-on-request proxy blocks on it and gives up after its startup timeout.
- Run the full platform locally by following
  `docs/local-development.md` in the superproject.

## Commit & Pull Request Guidelines

Use concise, imperative commit subjects. Keep the first line specific to the
change and avoid bundling unrelated work.

Keep commits small and concise. Commit each coherent, validated slice on its
own rather than batching many changes together, and keep the message short and
focused on that one change.

Pull requests should include a short summary, the commands run locally, and any
API or behavior changes. Include updated examples or docs when public APIs,
architecture, or expected usage changes.
