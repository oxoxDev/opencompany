# Harnesses

*What actually runs an agent's turn, and how a company picks.*

Terms: [glossary](../glossary.md). The models a harness talks to are
[providers.md](providers.md); the roster it runs is [agents.md](agents.md).

---

## What a harness is

A **harness** is one answer to "what runs this agent's turn". A company declares
a named set of them and binds each teammate to one, so a single roster can span
a cheap model, an expensive one, and the operator's own coding CLI.

Two kinds ship:

| kind | what runs the turn | credential |
|---|---|---|
| `built_in` | the embedded OpenHuman/tinyagents loop, in this process | its own `[harness.inference]` |
| `acp` | an external agent over the Agent Client Protocol | the agent's own |

`built_in` is the default and the only kind that consults
[providers.md](providers.md). An ACP agent already holds a credential — that is
the point of it — so it needs nothing from us.

### The case this exists for

A desktop company with **no key at all**. The operator has Claude Code installed
and signed in; OpenCompany drives it over ACP against their existing
subscription. Nothing to configure on first run, which is a materially different
product from one that opens on a credential form.

The same seam serves two more things at no extra cost: reverse dispatch (a cloud
host hands work to a runner on someone's machine, which is an ACP agent as far
as this is concerned) and any other harness that speaks the protocol.

---

## Declaring harnesses

```toml
[[harness]]
id      = "embedded"
kind    = "built_in"
default = true

[harness.inference]                 # attaches to the entry above
provider = "openrouter"

[[harness]]
id   = "deep"
kind = "built_in"

[harness.inference]
provider       = "openrouter"
api_key_secret = "harness/deep/inference/key"
models         = { "reasoning-v1" = "<openrouter-slug>" }

[[harness]]
id   = "my_laptop"
kind = "acp"

[harness.acp]
transport = "local"
agent     = "claude"
model     = "claude-opus-4-5"       # optional — see "Model", below
```

`[harness.inference]` and `[harness.acp]` attach to the **most recently
declared** `[[harness]]`. That is ordinary TOML array-of-tables sub-table
syntax, but it is easy to misread as a company-level section, so it is worth
reading twice.

### Model

`[harness.acp].model` is a hint forwarded to the agent's own model lever —
not a credential, so it does not join `[harness.inference]`'s prohibition on
`acp` harnesses (see [Validation](#validation)). Optional; a harness with none
runs whatever the agent's own config or CLI default resolves to.

`LocalAcpAgent` reaches that lever one of two ways, confirmed live against the
real adapters (issue #1245), not guessed — whichever this build knows for that
`agent`:

| `agent` | lever |
|---|---|
| `claude` | startup env var `ANTHROPIC_MODEL` |
| `goose` | startup env var `GOOSE_MODEL` |
| `codex` | no startup env var (`OPENAI_MODEL`, `CODEX_MODEL`, `MODEL` and `OPENAI_DEFAULT_MODEL` all tried, none had any effect) — instead, `session/set_config_option` right after `session/new`, using the `configOptions` entry `codex-acp` itself advertises with `category: "model"` |

The `set_config_option` fallback is not codex-specific in the code — it fires
for any agent whose startup env var this build does not know, whenever the
fresh `session/new` response advertises a `category: "model"` option matching
the requested value. It is per-session state, confirmed live: a second,
independent session on the same subprocess starts back at the adapter's
default, not the previously-set model.

`transport = "local"` only, for now: the `runner` wire protocol does not carry
`model`, so validation rejects it there rather than accepting and silently
dropping it — the same "my model setting does nothing" failure mode
[Validation](#validation) already guards against for `[harness.inference]`.

### Binding an agent

```toml
# agents/researcher.toml
role    = "Researcher"
harness = "deep"
```

Inline `[[agent]]` entries take the same field. An agent naming no harness runs
on the one marked `default = true`.

### The implicit harness

A company with **no `[[harness]]` block** gets one implicit `built_in` harness,
marked default, inheriting the company-level `[inference]`. Every bundle under
`companies/` and every existing tenant lands here, so named harnesses are purely
additive: nothing has to be rewritten to keep working.

Read harnesses through `CompanyManifest::effective_harnesses`, never the bare
`harnesses` field. A company that declares none still runs on a harness, and a
caller reading the raw field would see an empty list and conclude it has no
engine, which is never true.

---

## Validation

`CompanyManifest::validate` rejects, in prosumer language:

- a duplicate, empty, or non-snake_case `id`
- zero or more than one `default = true`, naming the candidates either way
- an agent naming a harness nothing declares, naming what *is* declared
- `[harness.inference]` on an `acp` kind, or `[harness.acp]` on a `built_in` one
- `transport = "local"` with no `agent`, or naming a `runner`; and the reverse
  for `transport = "runner"`
- an empty `model`, or one set on `transport = "runner"` (see
  [Model](#model))

A section on the wrong kind is an **error, not an ignored key**. This is the
same rule [agents.md](agents.md) applies to a bundle carrying both roster forms,
and for the same reason: a silently discarded declaration stays invisible until
the thing it configured misbehaves, and "my model setting does nothing" is an
expensive way to discover that `[harness.inference]` needs `kind = "built_in"`.

---

## ACP transports

```toml
[harness.acp]
transport = "local"      # spawn an agent on this machine
agent     = "claude"     # claude | codex | goose

[harness.acp]
transport = "runner"     # reach one that dialed in
runner    = "stevens_laptop"
```

**A remote runner is a transport, not a third kind.** `transport = "local"` and
`transport = "runner"` resolve to the same `AcpAgent` port
(`crate::ports::acp::AcpAgent`); only how bytes reach the agent differs.
Modelling the runner as a third kind would add a resolution path that resolves
to the same place.

The transports differ in where they live, which is why `AcpAgent` is a **port**
rather than an ACP client in the host crate: a subprocess over stdio belongs to
the desktop shell, a WebSocket to the runner lane. The same inversion the
storage ports use — and, concretely, why the port itself lives at
`crate::ports::acp`, ungated, rather than under `crate::harness` (behind
`openhuman`): the desktop shell that supplies the `local` implementation does
not enable that feature. See that module's own docs for the full reasoning.

`local` has a real implementation as of issue #1245 — `LocalAcpAgent`
(`src-tauri/src/acp/local_agent.rs`), wired through `AppState::with_acp_agents`
and `desktop::register`. `runner` does not yet: `src/runner/dispatch.rs`
declares `RunnerDispatch`, but it does not implement `AcpAgent`, and nothing
wires it into `lanes::build`. A `runner`-transport harness resolves
`unavailable` on every build today, `local` included.

### Readiness

For `transport = "local"`, the desktop probes four states rather than two:

| state | what to do |
|---|---|
| `NotInstalled` | install it |
| `NotSignedIn` | sign in |
| `Ready` | — |
| `SpawnFailed` | read the reason |

**Installed but not signed in** is the most common state on a fresh machine, and
it looks identical to "not installed" if all you check is `which`. The fixes are
completely different, so collapsing them tells someone to do the wrong thing.

Sign-in is probed by looking for the harness's credential file, not by running
it: asking a harness whether it is logged in means starting it, which is slow on
a list refreshed whenever a settings pane opens, and for some prompts
interactively. The probe can be wrong in one direction — a stale credential
reads as signed in — and that is the acceptable direction, because the failure
then surfaces on first use with the harness's own message, which is more
accurate than anything guessed.

---

## Routing

`HarnessRouter` (`src/harness/router.rs`) holds one `RunTurn` per declared
harness and forwards each call to the one its agent names. `RunTurn` already
carried `agent_id` on all three of its methods, so the dispatch point always
existed — nothing had ever varied on it.

The lanes are built at runtime-build time by `harness::lanes::build`, and
`HarnessBrain` routes through them. **A company declaring one harness (or none)
builds no router at all** — `run_turn()` hands back the single lane directly, so
the overwhelmingly common path is byte-identical to what it was.

Each `built_in` lane gets its own `HarnessPool` and its own `HarnessDeps`,
differing in exactly two fields: the provider (scoped to that harness's config
and credential slots) and `serves`, which narrows the pool to the agents bound
to it. That narrowing is what makes one-pool-per-harness affordable — without
it, a ten-agent roster across three harnesses would stand up thirty live agents
to use ten.

All three methods route. A method forwarding to a fixed engine would send
*dispatched card* turns to the wrong model while operator chat looked correct.

### A harness with no engine fails the turn

A harness can be declared, valid, and still have no engine. That is every `acp`
harness on a server build (no transport is wired there at all), every
`runner`-transport harness on any build (its socket transport isn't wired
yet), and a `local`-transport harness on a desktop build that was not given an
`AcpAgentFactory` (`AppState::with_acp_agents` — every embedder but the
packaged desktop app). Those turns fail, naming the harness and the fix.

They MUST NOT fall back to another harness's engine. That is the worst outcome
available: the turn would succeed, on a model and a credential nobody chose, and
the only evidence would be a billing line. This also covers the agent itself
failing to start (not installed, not signed in, or a spawn error) — that
surfaces as the same kind of failure, naming the harness and the reason, not a
silent fallback either.

---

## What a harness does not decide

- **`[brain].mode`** (`hosted` | `sidecar`) is a separate axis. It selects the
  cognition seam *within* the built-in harness.
- **Tools, policy, budgets, desks.** All company- or agent-scoped, and unchanged
  by which engine runs the turn — **except `local`'s own permission prompts**
  (`session/request_permission`), which are not routed through
  `ApprovalRequestQueue` at all. `LocalAcpAgent` auto-approves whatever its CLI
  still asks about, by option `kind` rather than a configured id, mirroring
  `buzz-agent`'s own answer to the same protocol gap
  (`crates/buzz-acp/src/acp.rs::handle_permission_request`): the CLI's own
  permission mode is the trust boundary, the same as it is for a developer
  running that CLI interactively themselves. This is a deliberate choice, not
  a placeholder — an ACP-run teammate is not gated by the company's approval
  policy the way a `built_in`-run one is.
- **Which model an agent's `tier` means.** A tier names a workload and is
  resolved against whatever provider its harness turns out to use, so an agent
  keeps its tier when it moves between harnesses. See
  [providers.md](providers.md).

---

## Implementation map

| concern | where |
|---|---|
| manifest types, kind/transport/model vocabulary | `src/company/types.rs` |
| validation, `effective_harnesses`, `harness_for` | `src/company/manifest.rs` |
| per-agent dispatch | `src/harness/router.rs` |
| building the lanes at boot, resolving `acp` engines | `src/harness/lanes.rs` |
| the built-in engine | `src/harness/built_in/` |
| the `AcpAgent`/`AcpAgentFactory` ports (ungated) | `src/ports/acp.rs` |
| the ACP `RunTurn` (folds a port `AcpTurn` into `TurnStep`) | `src/harness/acp/run_turn.rs` |
| wiring an `AcpAgentFactory` onto a host | `AppState::with_acp_agents` (`src/app/types.rs`), consumed by `desktop::register` |
| local transport: discovery, spawn, codec | `src-tauri/src/acp/` (`client.rs`, `discovery.rs`, `confine.rs`) |
| the `local` `AcpAgentFactory` implementation | `src-tauri/src/acp/local_agent.rs` (`LocalAcpAgent`/`LocalAcpAgentFactory`) |
| the desktop's own wiring | `src-tauri/src/embedded.rs` |
| runner transport (declared, not yet an engine) | `src/runner/dispatch.rs` |
| per-harness roster narrowing | `HarnessDeps::serves` |
