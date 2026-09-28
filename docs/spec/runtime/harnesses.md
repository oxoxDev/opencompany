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

Moved to [`harnesses-acp.md`](harnesses-acp.md) — this file was over the repository's 500-line limit. See that page for the two transports, the readiness states, session continuity across a restart, and live execution state.

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

## How long a turn may take

There is exactly **one whole-turn** time bound anywhere on the path from a
workflow run to a model call, and it does not live in this repo. Individual
**tool** calls can carry a second, tighter bound of their own — see below.

```
workflow run ............................. no time bound
  └─ tinyflows node execution ............ no time bound (duration observed only)
      └─ agent capability → agent.turn() . no time bound
          └─ tinyagents run "agent_turn" . the per-turn wall-clock ceiling
              └─ each model call ......... bounded by (ceiling − run elapsed)
              └─ each tool call .......... the lesser of that and the tool's own
                  └─ sub-agent turn ...... inherits the parent's remainder
```

The ceiling is the vendored harness policy's `max_wall_clock_ms`, set in
`vendor/openhuman/src/openhuman/agent/tinyagents/mod.rs::run_policy_for`. It
defaults to ten minutes, is overridden with
**`OPENHUMAN_AGENT_TURN_TIMEOUT_SECS`** (whole seconds; `0` removes it
entirely), and is process-global — not per node, not per workflow, and not
settable from a manifest or from the console.

**It bounds the whole turn, from the moment the harness run starts.** Remaining
budget is `ceiling − Instant::elapsed()`, so model time, tool time, sub-agent
time and retry backoff all count against it; each individual call is then given
whatever is left. It is deliberately generous: a hang backstop, not a UX
deadline.

### Why the harness's own message misleads, and what this crate says instead

When the ceiling fires, the vendored leaf reads:

```
model call for run 'agent_turn' exceeded its remaining wall-clock budget (56636 ms)
```

Every word of that is true and it is almost impossible to read correctly. The
number is the budget that **remained** when that call was issued — not the
call's duration, and not the ceiling. A turn that ran the full ten minutes
therefore reports a figure ten times smaller than the limit it hit, and reads
as though one slow model call were at fault. Issue #1680 was filed on exactly
that reading: a node that had already spent about nine minutes before its last
model call started was diagnosed as a 56-second budget being too tight.

`CompanyAgent::classify_turn` (`src/harness/built_in/mod.rs`) therefore times
each turn attempt and rewrites this one class of error, naming what the turn
actually spent, saying that the harness's figure is a remainder, and naming the
environment variable that moves the ceiling. The underlying error is appended
verbatim — it is the only thing that says which call was in flight.

Two constraints on that message are deliberate:

- **It does not restate the default value.** `DEFAULT_AGENT_TURN_TIMEOUT_SECS`
  is private to the vendored crate and cannot be read from here; a copy of
  `600` would go stale on the next vendored bump without anything failing. The
  elapsed time is measured and the knob's *name* is a fact independent of its
  value, so both can be stated honestly while the number cannot.
- **A ceiling hit stays a hard failure.** It is not retried — the one-shot
  empty-reply retry would turn a ten-minute failure into a twenty-minute one —
  and it fails the node rather than degrading to a partial result.

### The per-tool bounds this crate *does* set

Several tools bound themselves, more tightly than the ceiling and independently
of it, so a turn can lose a call without being anywhere near its own limit:

- **The built-in web tools** — `web_fetch`, `http_request` and `curl`, all three
  wired in `web_tools` (`src/harness/built_in/toolbelt.rs`) from
  `HttpRequestConfig::default().timeout_secs`, which is **30 seconds**. One
  source deliberately: `curl` takes it too rather than its own schema default of
  120, and `web_fetch` is constructed with `None` so it falls back to the same
  number. This is the bound an operator is most likely to meet and least likely
  to look for, because nothing in the tool's own output names the ceiling.
- **BYO web search** — `TIMEOUT_SECS` in `src/harness/built_in/search_byo.rs`,
  thirty seconds, passed to each provider tool and applied as the HTTP request
  timeout on its client. Deliberately shorter than a turn: a search that has not
  answered in half a minute has already cost more than the answer is worth. Only
  the bring-your-own providers; the managed tool keeps upstream's own policy.
- **MCP** — each server declaration's `timeout_secs`, forwarded verbatim to the
  transport by `server_config` in `src/harness/built_in/mcp.rs`. Per server, set
  in the company's MCP config, and the one bound on this page an operator can
  actually edit.

None of them ends the turn. A call that trips its own bound comes back as a
failed tool result, which the agent may retry or route around; only the ceiling
above fails the node. The ceiling still counts every second they spent — which
is the reading #1680 turned on, since a turn can burn most of its budget on tool
calls that each looked fine.

So a timeout an operator sees is one of two different facts, and the message is
what tells them apart: a per-tool bound names the tool (`tool \`curl\` timed out
after 30000 ms`), while the ceiling names the run and the *remaining* budget,
which is what this crate rewrites.

### What this crate does not bound

OpenCompany imposes no run-level or node-level deadline of its own. The two
`Duration` constants in `src/workflows/runner.rs` are a progress-collector join
(`PROGRESS_DRAIN_TIMEOUT`) and a grace period that arms only after an explicit
operator cancel (`CANCEL_HARD_ABORT_GRACE`); neither fires on its own. The
per-node `elapsed_ms` on `WorkflowRunNodeRow` is recorded **after** the fact and
compared to nothing.

So a node whose agent turn walks off the ceiling is the only way a workflow run
stops on time alone, and the honest reading of that failure is "this step asked
for more than one turn can do", not "the model was slow".

---

## OpenHuman's own library front door

Upstream's `openhuman-embed` crate (`vendor/openhuman/crates/openhuman-embed`)
is the host-facing library API, and it is a **two-step** shape: one `Runtime` per process, then any number of `Agent`s on
it, each fully described and independent of the others:

```rust
use openhuman_embed::{Access, AgentSpec, McpServer, Provider, Runtime, Workspace};

let runtime = Runtime::builder()
    .workspace(Workspace::dir("/var/lib/my-product/openhuman"))
    .api_key("th_live_…")
    .build()
    .await?;

let reviewer = runtime.agent(
    AgentSpec::new("reviewer")
        .system_prompt("You review pull requests and never edit files.")
        .access(Access::readonly())
        .skills_dir("./skills/review")
        .action_dir("/srv/checkouts/pr-42"),
)?;
let fixer = runtime.agent(
    AgentSpec::new("fixer")
        .provider(Provider::openai_compatible(url, key).model("gpt-5"))
        .access(Access::full())
        .mcp(McpServer::stdio("github", "gh-mcp", ["stdio"])),
)?;

let review = reviewer.run("Summarise the risks.").await?;
let fix = fixer.turn(review.reply).send().await?;
```

What each agent owns: its provider route and model, access tier and turn
origin, `action_dir`, MCP servers, skills root
(`<workspace>/personalities/<id>/skills/`), system prompt, tool scope, sandbox
mode, allowlists, and a narrowed `DomainSet` / `ToolGroups`. Every turn is
dispatched under the agent's own `CoreContext` (a `ContextOverlay` derived
from the runtime's, run through `CoreRuntime::run_in`), so the core's config
loader, domain gate, tool-group filter, skill discovery and the intrinsic
memory tools all read *that* agent's settings. An `Agent` is a cheap clone
handle over immutable per-agent state; each turn builds a fresh session from
the definition, resumes its thread from the on-disk transcript, and drops it —
no resident session, so idle agents cost ~nothing and one agent can serve
overlapping turns. `Harness` is the one-agent shorthand over the same two
types; `Core` wraps a `CoreRuntime` the host built itself with `CoreBuilder`.

### How `built_in` uses it

`built_in` **is** this front door. `harness::openhuman_runtime::global` boots
the one `Runtime` at `serve` (workspace `<data-dir>/openhuman`, the TinyHumans
key as its credential, `TINYHUMANS_API_URL` as its backend), and
`harness::build::agent_spec_for(record, agent, deps)` renders every manifest
`[[agent]]` into an `AgentSpec`:

| what the company declares | where it lands on the spec |
|---|---|
| persona, bundle and context sections, team and tool briefs, skills catalogue, sandbox brief (`company/prompt.rs`, `skills.rs`, `toolbelt.rs`) | `.system_prompt(..)` |
| the OpenHuman-native subset of its grants (`shell`, `file_*`, `web_fetch`, …) | `.definition(AgentDefinitionSpec::new().tools(ToolScopeSpec::Named(..)).disallow_tools(..).max_iterations(25))` |
| `[inference]` / the agent's own `{provider, model}` pair (`company/inference.rs`), BYOK included | `.provider(Provider::openai_compatible(url, key).model(m))`, served through the loopback model bridge so usage is metered |
| the approval policy | `.access(Access::full())` — OpenHuman's runtime-wide gate stays off; OpenCompany decides allow / deny / park in its own MCP handler |
| every OpenCompany tool (ledger, tasks, pages, workspace, memory, composio, hosting, approvals) and the speech tools | `.mcp(McpServer::http("opencompany", url).auth(BearerToken).allow_tools(..))` — see [hive.md](hive.md#speaking) |
| each `mcp:*` grant | one more `.mcp(..)` |
| the company's skills, the company workspace | `.skills_dir(<home>/skills)`, `.action_dir(<workspace>)` |

`runtime.agent(spec)` mints the handle under
`session_key::runtime_agent_id(company, agent)`; `HarnessPool.ensure` rebuilds
the roster on the same fingerprints it always had, taking every old
`turn_lock` first (bounded) and dropping the old handles before minting new
ones, because an id stays reserved while any clone of it lives.

A turn is `agent.turn(message).session(openhuman_session_key).cwd(..)
.on_progress(tx).send()` under the agent's `turn_lock`; `progress_pump.rs`
maps `AgentProgress` onto `turn_stream::LiveFrame`s and reads cost from
`ModelCallCompleted` / `TurnCostUpdated`. There is no resident session, no
`Mutex<Agent>`, and no history seeding: OpenHuman owns the thread, and the
company's delta is prepended to the message ([speech.md](speech.md)).

What that buys, measured rather than argued: turns of **different** agents
overlap — within a desk round and across desks — and a turn of one agent
never overlaps another turn of the same agent. `opencompany measure` and
`scripts/measure-coordination.mjs` report both numbers on
`companies/hive_demo`; the second must be zero.

What is deliberately not on the spec: a host-built tool vector (OpenHuman has
no seam for one, hence the MCP server), a host `Memory` (the company's
`ContextStore` is reached as the `memory_*` MCP tools) and a host
`ToolPolicy` (the approval decision is made where the tool is served). The
legacy out-of-process JSON-RPC path (`src/openhuman/`, feature
`openhuman-rpc`) is gone.

---

## Implementation map

| concern | where |
|---|---|
| manifest types, kind/transport/model vocabulary | `src/company/types.rs` |
| validation, `effective_harnesses`, `harness_for` | `src/company/manifest.rs` |
| per-agent dispatch | `src/harness/router.rs` |
| building the lanes at boot, resolving `acp` engines | `src/harness/lanes.rs` |
| the built-in engine | `src/harness/built_in/` |
| the one process-wide `openhuman_embed::Runtime` | `src/harness/openhuman_runtime.rs` |
| a manifest agent as an `AgentSpec` (`agent_spec_for`) | `src/harness/built_in/build.rs` |
| `AgentProgress` → live frames and cost | `src/harness/built_in/progress_pump.rs` |
| the `opencompany` MCP server the agents' tools are served on | `src/hive/mcp_server.rs` |
| the `AcpAgent`/`AcpAgentFactory`/`AcpObserver` ports (ungated) | `src/ports/acp.rs` |
| the ACP `RunTurn` (folds a port `AcpTurn` into `TurnStep`) | `src/harness/acp/run_turn.rs` |
| live frames while an ACP turn runs (`live_frame_from`, `observer_for`) | `src/harness/acp/run_turn.rs` |
| remembering + resuming a session (`session_record_path`, `resume_session`) | `crates/opencompany-app/src/acp/local_agent.rs` |
| the transport's bounds on a tool call's title/result | `crates/opencompany-app/src/acp/local_agent.rs` (`MAX_TITLE_CHARS`, `MAX_RESULT_CHARS`) |
| wiring an `AcpAgentFactory` onto a host | `AppState::with_acp_agents` (`src/app/types.rs`), consumed by `desktop::register` |
| local transport: discovery, spawn, codec | `crates/opencompany-app/src/acp/` (`client.rs`, `discovery.rs`, `confine.rs`) |
| the `local` `AcpAgentFactory` implementation | `crates/opencompany-app/src/acp/local_agent.rs` (`LocalAcpAgent`/`LocalAcpAgentFactory`) |
| the desktop's own wiring | `crates/opencompany-app/src/embedded.rs` |
| runner transport (declared, not yet an engine) | `src/runner/dispatch.rs` |
| per-harness roster narrowing | `HarnessDeps::serves` |
