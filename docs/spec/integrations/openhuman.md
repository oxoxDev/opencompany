# OpenHuman

OpenHuman (vendored at `vendor/openhuman`, written against v0.58.x) is the
local-first personal-AI product: a Rust core with a Tauri/React shell. For
OpenCompany it is the **preferred backend for tools, channels, credentials,
and policy** — roughly 60 mature domains (memory, threads, channels,
subconscious, routing, providers, tools, skills, cron, workflows, wallet,
security, people, embeddings, …) that the kernel should consume, not copy.

## Integration: embedded as a library (current)

OpenHuman is consumed as an **embeddable Rust library**, not an
out-of-process daemon. The `src/harness/` module links `openhuman_core` and
`openhuman_embed` directly and, under `feature = "openhuman"`, boots one
process-wide `openhuman_embed::Runtime` and mints one `openhuman_embed::Agent`
per manifest `[[agent]]` from an `AgentSpec`
([`docs/modules/openhuman/README.md`](../../modules/openhuman/README.md),
[runtime/harnesses.md](../runtime/harnesses.md#openhumans-own-library-front-door)).
What the company declares reaches the agent through the spec:

- **Inference provider** → `Provider::openai_compatible(url, key).model(m)`
  from the company default or the agent's own pair, served through the
  loopback model bridge so every call is metered.
- **Tools** → the OpenHuman-native subset of the grants as the spec's tool
  scope; every OpenCompany tool (ledger, tasks, pages, workspace, memory over
  the [`ContextStore`](../runtime/ports-state.md#contextstore), composio,
  hosting, approvals) and the room's speech tools over the `opencompany` MCP
  server, because the library has no seam for an in-process host tool
  ([runtime/hive.md](../runtime/hive.md#speaking)).
- **Approvals** → OpenHuman's runtime-wide gate stays off (`Access::full()`);
  OpenCompany's `ApprovalPolicy` decides allow / deny / park where the tool is
  served. Every agent also gets the intrinsic `request_approval` tool.
- **Desks** → `tinyhivemind-openhuman` binds the agents into one
  `OpenHumanHive` per `[[group_chat]]`; the host runs its completion episodes
  ([runtime/hive.md](../runtime/hive.md)).

The default build links **none** of this and keeps its offline, echo-brained
behaviour. When the `openhuman` feature is off, tool/channel behaviour degrades
to built-ins and the operator channel — never a boot failure
([runtime/config.md](../runtime/config.md)).

**Realized upstream candidate #2 (library-crate split).** Embedding
`openhuman_core` directly is exactly the "expose the domains as an embeddable
crate so co-located hosts can link instead of RPC" workstream below —
delivered, not pending.

### One teammate, one named session

Every company agent is one `openhuman_embed::Agent` — a cheap clone handle
over immutable per-agent state, not a resident session. Each conversational
turn resumes the agent's one stable OpenHuman session, named
`{company}:{agent_id}` by `session_key::openhuman_session_key` — company
first, because the process is multi-tenant and an `agent_id` is unique only
within its company — and OpenHuman keeps that thread on disk under the
runtime's workspace. The runtime id the agent is minted under is
`session_key::runtime_agent_id` (`{company}--{agent}`, lowercased, hashed
past 64 characters, since ids match `^[a-z0-9][a-z0-9_-]{0,63}$` and stay
reserved while any clone lives).

Turns of one agent are serialised by its own `turn_lock`, which the
`CompanyAgent` holds beside the handle; turns of different agents run at the
same time, which [openhuman#6208] made safe by replacing the conversation
store's process-wide mutex with per-root and per-thread locks and proving 100
overlapping turns on distinct sessions against one live core. That is the
condition a desk round runs under: several seats at once, one turn each, and
never two turns of one seat ([runtime/hive.md](../runtime/hive.md)).

The confined workflow copilot is named the same way. It does not come off the
roster, so it does not inherit the roster's call, and an unnamed session there
would put the one turn that runs under a *confinement* back in the crowd.

A `dm` between seats is a row on the desk with an `audience`; the recipient
is assigned the next round and reads it in its delta
([runtime/speech.md](../runtime/speech.md)). Both sessions are logged at
`debug` as `from_session` / `to_session`, which is the only place both are
known at once.

[openhuman#6208]: https://github.com/tinyhumansai/openhuman/pull/6208

### Cost metering

A turn's usage arrives on the `on_progress` channel as
`AgentProgress::ModelCallCompleted` / `TurnCostUpdated`; `progress_pump.rs`
folds them into the `TurnCost` the harness maps onto the ledger and the
[`UsageMeter`](../runtime/ports-console.md#usagemeter). The model bridge
additionally taps every call's provider-reported usage, so a turn is metered
from what the provider charged rather than from a count the host made.

### Group-chat / desk routing

openhuman is single-agent; a desk is OpenCompany's composition over it.
`tinyhivemind-openhuman` binds the desk's agents into one `OpenHumanHive`,
its `CompletionDriver` proposes rounds and folds what the host commits, and
the host — `src/hive/` — runs the seats concurrently, journals every
utterance, routes broadcasts through Jev over the TinyHumans System One proxy,
and crosses desks by referral ([runtime/hive.md](../runtime/hive.md)).

## Legacy: JSON-RPC launcher/wire path — removed

The out-of-process seam (`src/openhuman/`, feature `openhuman-rpc`: the
`opencompany open-human` launcher, the JSON-RPC `OpenHumanRpc` transport and
the `OpenHumanToolProvider` / `OpenHumanChannelAdapter` adapters) is gone.
A manifest that still names `provider = "openhuman"` on `[tools]` or a
channel builds on the built-in tool provider and the operator channel with a
boot warning, never a failure. `OPENCOMPANY_OPENHUMAN_URL` attaches to
nothing.

## Desktop story

The Tauri app is a natural prosumer install path: OpenHuman as the shell,
OpenCompany as the company runtime behind it. Whether the prosumer UI ships
as an OpenHuman mode or a separate frontend is an open product question
([product/prosumer.md](../product/prosumer.md)); the runtime API is the same
either way. OpenCompany's own desktop shell is `crates/opencompany-app`
([runtime/desktop.md](../runtime/desktop.md)); launching OpenHuman's Tauri
host is done from the OpenHuman checkout with its own scripts, not from this
binary.

## Upstreaming policy

Glue that adapts OpenHuman's RPC to kernel ports lives in OpenCompany.
Anything that changes OpenHuman behavior goes upstream. Candidate PRs
identified so far:

1. **Headless multi-workspace mode** — `openhuman-core serve` today serves
   one local persona; a `--workspace <id>` scope (or workspace param on RPC
   methods) would let one daemon serve N companies.
2. **Library-crate split** — *realized.* The tool/channel/credential/policy
   domains are consumed as the embeddable `openhuman_core` crate; the harness
   links them instead of speaking RPC.
3. **Public turn-usage accessor** — *realized* as `AgentProgress`'s
   `ModelCallCompleted` / `TurnCostUpdated` frames on the embed API's
   `on_progress` channel; the harness meters from them.
4. **External approval hook** — policy tiers currently resolve in-app; a
   webhook/RPC callback would let OpenCompany's `ApprovalGate` be the
   resolver of record.
5. **Namespaced credentials** — per-workspace credential scoping so company
   A's secrets are invisible to company B.
6. **Documented `/events` schema** — the REST event stream exists but has no
   stable documented schema for external consumers.
7. **Brain-protocol port** — make OpenHuman's own orchestration loop
   pluggable so an OpenHuman instance could delegate cognition to a hosted
   Medulla brain (the inverse of our integration).
