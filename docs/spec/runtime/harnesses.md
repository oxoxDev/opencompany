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
```

`[harness.inference]` and `[harness.acp]` attach to the **most recently
declared** `[[harness]]`. That is ordinary TOML array-of-tables sub-table
syntax, but it is easy to misread as a company-level section, so it is worth
reading twice.

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

**A remote runner is a transport, not a third kind.**
`src/runner/dispatch.rs::RunnerDispatch` already implements the same `AcpAgent`
port the local subprocess does, so the only thing that differs is how bytes
reach the agent. Modelling it as a third kind would add a resolution path that
resolves to the same place.

The transports differ in where they live, which is why `AcpAgent` is a **port**
rather than an ACP client in the host crate: a subprocess over stdio belongs to
the desktop shell, a WebSocket to the runner lane. The same inversion the
storage ports use.

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

A harness can be declared, valid, and still have no engine. Today that is every
`acp` harness on a server build: the transports live in the desktop shell (a
stdio subprocess) and the runner lane (a socket), and neither is wired into the
server. Those turns fail, naming the harness and the fix.

They MUST NOT fall back to another harness's engine. That is the worst outcome
available: the turn would succeed, on a model and a credential nobody chose, and
the only evidence would be a billing line.

---

## What a harness does not decide

- **`[brain].mode`** (`hosted` | `sidecar`) is a separate axis. It selects the
  cognition seam *within* the built-in harness.
- **Tools, policy, budgets, desks.** All company- or agent-scoped, and unchanged
  by which engine runs the turn. An ACP agent is still subject to the company's
  approval policy.
- **Which model an agent's `tier` means.** A tier names a workload and is
  resolved against whatever provider its harness turns out to use, so an agent
  keeps its tier when it moves between harnesses. See
  [providers.md](providers.md).

---

## Implementation map

| concern | where |
|---|---|
| manifest types, kind/transport vocabularies | `src/company/types.rs` |
| validation, `effective_harnesses`, `harness_for` | `src/company/manifest.rs` |
| per-agent dispatch | `src/harness/router.rs` |
| building the lanes at boot | `src/harness/lanes.rs` |
| the built-in engine | `src/harness/built_in/` |
| the ACP `RunTurn` and its port | `src/harness/acp/run_turn.rs` |
| local transport: discovery, spawn, codec | `src-tauri/src/acp/` |
| runner transport | `src/runner/dispatch.rs` |
| per-harness roster narrowing | `HarnessDeps::serves` |
