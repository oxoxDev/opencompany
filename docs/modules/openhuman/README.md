# OpenHuman Module

OpenHuman is the tenant harness, embedded as a **library**. The
`src/harness/` module links `openhuman_core` and `openhuman_embed`
(`vendor/openhuman`) directly and, under `feature = "openhuman"`, runs **one
process-wide `openhuman_embed::Runtime`** (`harness::openhuman_runtime`, on its
own tokio executor) and instantiates **one `openhuman_embed::Agent` per manifest
`[[agent]]`** from an `AgentSpec` (`harness::build::agent_spec_for`). The default
build links none of it and keeps its offline, echo-brained behaviour.

What each agent is built from (`harness::build::AgentBlueprint`):

- **Persona** → the system prompt: manifest `role` at the company, the bundle,
  team and tool briefs, the skills catalogue, the sandbox brief. Passed as the
  spec's inline definition prompt and — because a hosted turn resolves its
  agent by id through OpenHuman's definition registry — declared again as a
  custom `AgentRegistryEntry` in the agent's own config.
- **Inference** → `harness::provider::HostedProvider` / `TenantProvider` (the
  company default or the agent's own `{provider, model}` pin), served to the
  runtime over the loopback OpenAI-compatible **model bridge**
  (`harness::model_bridge`): the runtime's inference client talks to
  `127.0.0.1` with a per-agent bearer, the bridge forwards to the provider,
  and taps every call's usage (with the backend-charged amount) so a turn is
  metered from what the provider reported. A scripted test model is served the
  same way. Managed search, Composio and media reach the TinyHumans backend
  through the transport `harness::backend_transport` installs once per
  process.
- **Tools** → the OpenHuman-native subset of the manifest grants (`shell`,
  `file_*`, `web_fetch`, …) is the spec's `ToolScopeSpec::Named`. This crate's
  own tools (ledger, tasks, pages, workspace, composio, hosting, memory, speech,
  approval) are assembled on the blueprint but **unattached**: OpenHuman has no
  seam for a host-built tool, so they become the per-agent MCP catalogue in
  plan hive-desks Phase 3. The workflow copilot's three tools run on the
  host-side loop `harness::host_loop` instead.
- **Session** → every conversational turn resumes the agent's one stable
  session (`session_key::openhuman_session_key`), which OpenHuman owns; a turn
  that names no chat (a card, a workflow node) runs on a fresh session of its
  own. Turns of one agent are serialised by its `turn_lock`; different agents
  run concurrently.
- **Tool policy** → `harness::policy::ApprovalPolicy` is carried on the
  blueprint for the Phase 3 tool handler; the runtime's own approval gate is
  off (`Access::full()`).

See [`docs/modules/runtime/README.md`](../runtime/README.md) for `HarnessPool`
and [`docs/spec/integrations/openhuman.md`](../../spec/integrations/openhuman.md)
for the full integration contract.

## `HarnessBrain` — cognition on the embedded runtime

`harness::brain::HarnessBrain` implements the `Brain` cognition port over a
`HarnessPool`: each operator message runs one openhuman agent turn and returns
the agent's reply, in place of the offline `EchoBrain`'s `"You said: …"`. A
company routes through it when the `RuntimeBuilder` has both a harness pool
(`with_harness`) and any inference source that resolves at build time, and no
explicit brain — brain precedence is `with_brain` > harness > hosted/echo. The
`opencompany` binary's `attach_harness` resolves the managed default from the
environment (below).

Which brain a company runs is chosen once, when its runtime is built. A company
that resolved **no** inference source at boot is on the offline echo brain and
stays there for as long as that runtime lives, no matter what the console saves
afterwards — a company runtime is built once and cached in the
`CompanyRegistry`. That transition is reported honestly as `restartRequired`
(issue #266) and cleared by rebuilding the runtime in place (issue #290, see
[`docs/spec/runtime/rebuild.md`](../../spec/runtime/rebuild.md)) rather than by
a process restart.

Everything *after* that first transition is live: once a company is on the
harness path, `TenantProvider` re-resolves the effective config — console
runtime override > manifest `[inference]` > managed env default — on every turn,
so a provider switch or key rotation reaches agents on the next turn with no
rebuild at all.

## Explicit approval requests

Every roster agent receives `request_approval` as an intrinsic tool. It takes a
short `title`, a precise yes/no `question`, and optional `context`. Calling it
pushes one `request_approval` effect onto the shared `ApprovalRequestQueue`;
`HarnessBrain` drains and journals that request through the existing approval
inbox. The tool tells the agent to stop the turn and wait.

Resolving the card starts a continuation turn for the requesting agent. Approve
and deny are both delivered as decisions; approval does **not** re-run the
`request_approval` tool. The agent continues (or stops) based on the answer.

Ordinary tools no longer enter HITL because of `[policy].mode`,
`always_approve`, budget thresholds, or per-call judgement. Existing parked
tool-call approvals and their grants remain redeemable during migration. The
`readonly` brake stays a hard denial, not a prompt the operator can override.

## Inference config (environment)

`harness::provider::harness_inference_from_env` resolves the endpoint, key, and
default model, most specific first:

| Value | Source | Fallback |
| --- | --- | --- |
| key | `OPENCOMPANY_INFERENCE_KEY` | `TINYHUMANS_API_KEY` — **no key ⇒ echo brain** |
| url | `OPENCOMPANY_INFERENCE_URL` | `{TINYHUMANS_API_URL}/agent-integrations/openrouter` |
| model | `OPENCOMPANY_INFERENCE_MODEL` | `chat-v1` |
| window | `OPENCOMPANY_CONTEXT_WINDOW` | `240000` — context window advertised on the managed profile; `off`/`0` disables compression and trimming. Lower it for a smaller model — see [history protection](../../spec/runtime/providers.md#history-protection) |

The two key names keep a per-tenant override distinct from the platform-wide
credential the hosting manager injects.

This is the **lowest**-precedence source. A company's own key, set write-only
through the console (`PUT …/inference` with `key`, stored under the
`inference/key` secret), wins over both env names — including on the `managed`
provider, where only the credential changes and the platform endpoint is kept.
Clearing it (`PUT …/inference` with `key: ""`, the console's **Remove key**)
falls back to the env credential rather than 401ing.

## Cost metering

`harness::cost` maps a completed turn's usage onto the ledger and the
`UsageMeter`. `HarnessPool::run` reads the per-turn token/cost totals from the
runtime's `AgentProgress::ModelCallCompleted` / `TurnCostUpdated` frames
(`progress_pump.rs`), cross-checked against what the model bridge saw the
provider charge, so metering is **live**. Gating differs by
surface: a usage sample is recorded whenever tokens moved (the `/openai/v1`
passthrough reports tokens but bills backend-side, echoing no USD), while a
ledger `inference.spend` entry is written only when the turn actually cost USD —
so a token-bearing zero-cost turn meters usage without a `$0.00` spend line. An
offline provider that reports no usage yields a zero turn, which writes nothing.

## Desks

A `[[group_chat]]` of two or more is one `tinyhivemind_openhuman::OpenHumanHive`
over these same agent handles (a shared agent is one handle bound into every
hive that lists it), driven by `src/hive/` as completion episodes: concurrent
rounds, speech over the `opencompany` MCP server, Jev routing, cross-desk
referral. See [`docs/modules/hive/README.md`](../hive/README.md) and
[`docs/spec/runtime/hive.md`](../../spec/runtime/hive.md).

## The removed JSON-RPC path

`src/openhuman/` — the `opencompany open-human` launcher, the `OpenHumanRpc`
transport, `OpenHumanToolProvider` and `OpenHumanChannelAdapter`, behind the
`openhuman-rpc` feature — is gone. A manifest naming `provider = "openhuman"`
on `[tools]` or on a channel builds on the built-in tool provider and the
operator channel with a boot warning; `OPENCOMPANY_OPENHUMAN_URL` attaches to
nothing.
