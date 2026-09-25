# Hive Mind unification — verified findings

Working notes from a session spent building, breaking and reading the two agent
paths. Written for whoever picks this up next.

**Read the status column.** Every claim below is marked `verified` (read in the
code on this branch, or observed in a live run), `observed` (seen once live,
not isolated) or `unverified` (reasoned, not checked). The unverified ones are
where a future session should start, not where it should build.

Companion: the teammate's "Hive Mind Unification Plan" (opencompany#2465)
proposes four PRs. This file is evidence for and against parts of it, not a
replacement.

## The one-line finding

The pooled agent and the episode seat differ in six settings, **five of which
vary per turn or per episode, not per agent**. `AgentSpec` can only express
per-agent facts. That is the entire reason there are two build paths.

## What was verified

### The two paths differ in six things (verified)

`episode_seat` sets fourteen things. Eight are identical to what the pooled
spec wants — they come from the same blueprint. The six that differ:

| Setting | Why | Scope |
| --- | --- | --- |
| `tools` | belt plus *this episode's* tools | per episode |
| `visible_tool_names` | follows that belt | per episode |
| `tool_policy` | `belt.admit(...)` in front of company policy | per episode |
| `memory` | `SeatMemory` returns nothing | per shape — unresolved |
| `auto_save` | `false`; the journal is the only log | per conversation |
| session handling | cleared and re-seeded every turn | per turn |

`tool_dispatcher` used to be a seventh; it is not any more (see below).

### The two personas are string surgery, not a design (verified)

`build_episode_seat` takes the ordinary roster blueprint and cuts pieces out:

```rust
blueprint.tools.retain(|t| !EPISODE_WITHHELD_TOOLS.contains(&t.name()));
for brief in [orchestrator_brief(), member_delegation_brief()] {
    if let Some(at) = blueprint.system_prompt.find(&brief) { /* delete */ }
}
```

A prompt edited by substring match. Reword a brief upstream and the cut stops
matching. The causal chain, from the code's own comment: no drain inside an
episode → strip the board tools → the prompt now describes tools that are gone
→ cut the prompt → two profiles.

### The pooled path ran a text tool dialect (verified, fixed on this branch)

`default_agent_tool_dispatcher()` returns `"python"`
(`config/schema/agent.rs`), and `resolve_dispatcher_kind` matches `"python"`
**before** consulting native support. `CodeDialect::should_send_tool_specs()`
is `false`, so the model never saw a structured tool spec — the catalogue
arrived as prose and it was asked to write Python.

Confirmed empirically: every `config.toml` written by this session's live runs
carried `tool_dispatcher = "python"`. Pre-existing configs on the machine say
`"auto"`, so this bites freshly provisioned data roots — which is what a hosted
tenant gets.

**Native tools are inert without this.** With the belt correct (175 tools, 33
visible, `workspace_list` present) nothing reached the wire until the dispatcher
was pinned. The two changes are one change.

### Native tools bypass the approval gate (verified)

Every OpenCompany tool call used to pass `McpAgent::serve_call`, which checks
the company's `ApprovalPolicy`. A native tool runs inside OpenHuman's loop and
never reaches that handler. `a_read_only_desk_is_denied_the_search_even_when_it_is_wired`
went red on the port and green once the gate rode along with the belt.

Narrower than it first looks: every agent is built with human-in-the-loop
disabled, so what actually stops applying is readonly, hard denies and explicit
`request_approval`.

### `spawn_task` is withheld only from seats, and only for one reason (verified)

`EPISODE_WITHHELD_TOOLS = ["spawn_task", "delegate_to_desk", "delegate_to_teammate"]`,
applied in `build_episode_seat` and nowhere else. The episode itself only
exists when a desk has two or more bound members (`graph.rs`: `if
bound_members.len() < 2 { continue; }`).

So: a desk with one member, an operator DM, a card and a workflow node all keep
`spawn_task`. It is missing **only while a desk is actually deliberating**.

The reason is the drain, not the capability — "no brain drains inside an
episode". Withholding was chosen over refusing because a seat that reached for
one told the operator to go and fix a board that was never broken.

### Nothing in a conversation dispatches a card once delegation goes (verified)

- `TaskDispatched` fires on the `in_progress` edge (`ports/types.rs`)
- `assign_task` says so itself: *"This records ownership; it does not start the
  work — move the card to In Progress to dispatch it."*
- `spawn_task` is for work that *"must NOT start in this turn"*
- the delegate tools opened a card **already dispatched** — "the hand-off IS the
  card"

Delete the delegate tools and a conversation can open a card and assign it, and
it sits there until a human moves it on the console. **A board column-move tool
is needed** (`start_task`, or `assign_task` gaining `start: true`). This is a
board operation, not a delegation one.

### Cards and conversations are two separate loops today (verified)

`CompanyEvent::TaskDispatched` → the **brain** calls `run_task`
(`brain.rs`), a background turn with `chat_id: None`. The driver is not
involved. Also: `run_task` originally ran *exactly one* background turn, and
what let a card's work span turns was draining what that turn queued — i.e.
delegations.

**Consequence:** removing delegation makes a card one-shot unless something
else lets its work span turns.

### `prompt.rs` is dead, not merely duplicated (verified)

`SeatPrompt::render` and `delta_for` have no production caller in OpenCompany —
only `prompt_tests.rs`. The seat's prompt is composed by `journal.compose` in
Tiny Hive Mind.

### The seat's context contract (verified)

- seeded history is **journal rows only** — no tool calls, no tool results
- role mapping (`hosted/seed.rs`): the seat's own rows → `assistant`, everyone
  else's → `user` with the author's name in the text
- the turn's own channel goes in the *history*; every **other** channel goes in
  the prompt as `elsewhere`. No duplication
- `clear_history()` drops the runtime session outright, so a seat gets a **new
  session every turn**; what persists is the host shell (belt, gate, memory,
  prompt builder, model)
- a seeded turn renders no system prompt, which is why the persona is
  re-inserted at index 0 each turn — and why a seat can never serve a stale
  prompt

### The pooled path's session is the opposite (verified)

One stable session per agent (`{company}:{agent_id}`) spanning every chat. On
resume, `leading_system_prefix` reuses the persisted leading system messages
verbatim, for prefix-cache warmth. So a console tool-revoke narrows the belt
while the frozen prompt keeps advertising the tool — enforced and still
advertised.

`isolated_session(turn_chat_id, history_seed)` already mints a fresh session per
turn when `history_seed` is false; the episode seam sets it, the DM path does
not.

### A pair channel should be an audience, not a conversation (verified)

`dm { to, message }` puts the row **on the desk with `audience = to`**; an
outsider sees that a DM happened, not what it said. `aside/mod.rs`: *"A
redaction is a row, not an absence."*

A separate `dm:<a>+<b>` conversation is therefore a covert channel by the
crate's own definition — the operator cannot see it happened or who was in it.

## What was tried and failed

A branch (`feat/dm-consult-handoff-v2`, abandoned, 17 commits, kept locally)
added `consult_teammates` and `hand_off` as DM-side tools. Both are
reimplementations of primitives the episode layer already has, and both failed
the same way: **a pooled turn is synchronous, so work had to happen inside one
turn**.

| Built | Should have been | How it failed |
| --- | --- | --- |
| `consult_teammates` | `ask` (ADR 0023) | reach bounded by `delegates_to`, so a PM could only room with its desk-mate; spun a second hive; needed a wall-clock timeout; returned nothing |
| `hand_off` | seed the other agent's operator DM | ran a detached episode in `dm:<a>+<b>`; nothing to return, so the result overclaimed ("they will answer this turn") |

Observed live: asked to get two engineers' views, the PM was refused by the
reach bound, found `delegate_to_teammate` through `mcp_list_tools`, called it
twice, was told they would answer this turn, then spent ~15 calls hunting the
board, ledgers, memory and workspace for an answer that lands nowhere, and
escalated.

**Every one of those failures is "an unfinished question had nowhere to live".**

## What MCP cost, measured

One live PM turn: **22 `mcp_call_tool` + 1 `mcp_list_tools`**, three of them
wasted guessing argument names (`teammates`/`message`, then `question`, then
`with`). With MCP the only schema on the wire is `mcp_call_tool`'s own; the
inner `arguments` object is free-form, so no provider can validate it or decode
against it. A native tool carries its own schema.

## The direction this points

Layers that are currently welded and should not be:

1. **The journal** — one log; conversations with membership and watermarks
2. **The driver** (`tinyhivemind-driver`) — who runs next, awaiting, child
   conversations. The async lifecycle. *Every* conversation wants this
3. **The hive** (`tinyhivemind-hive`) — quorum, salience, cross-inhibition.
   **Opt-in by its own charter**; only when several agents decide together

A one-agent DM needs (2), not (3). That is what makes this cheaper than "make
every DM an episode" — no conductor, no rounds, no quorum for a conversation
with nobody to deliberate against.

Five primitives, each meaning one thing:

| Primitive | Means |
| --- | --- |
| Card | work to be tracked and done later |
| `ask` | a question I need answered before continuing |
| Hand-off | this conversation is yours — seeded into *their* operator DM |
| Room | several agents must decide together (the hive) |
| Workflow | a saved graph to run |

`delegate_to_desk` / `delegate_to_teammate` are a sixth overlapping all of them.
What they uniquely offered was a synchronous answer inside the caller's turn,
which is the property that should not exist. Their useful half is `assign_task`.

## Unverified — start here

- **Can the driver bind a conversation with no chat surface** (a card)? The
  crate is host-neutral and names no host chat id, which is encouraging. Not
  seen done.
- **Can an `ask`'s child conversation be a thread of a `Direct` conversation?**
  ADR 0023 says "a thread of the desk". This decides whether the DM work is
  small.
- **Is `WorkflowRefQueue` drained on a seat turn?** `run_workflow` /
  `create_workflow` stage onto it; it is not one of the four queues the plan's
  PR 0 names. Possibly a fifth queue nobody has counted.
- **Is a seat's null `memory` a property of being a seat, or of being in a
  conversation where the journal is the whole context?** If the second, it is
  per-conversation like `auto_save` and belongs with the others.
- **Does a resumed session's frozen prompt actually mis-describe a belt that
  changed under it?** Reasoned from `leading_system_prefix`, not tested. Same
  failure class as the revoked-tool mismatch.
- **Does the console's reply path survive a reply becoming a speech call?** The
  expensive unknown. Note their audit already says pooled DM agents are offered
  `post`/`dm`/`broadcast` and the utterance is dropped — so this path is
  half-built and owed regardless.
- **Whether a DM turn wants the "exactly one speech call per turn" fence.**

## Branches

| Branch | Holds |
| --- | --- |
| `feat/native-agent-tools` | OpenCompany tools native; dispatcher pinned; gate on the belt; `SharedTool` adapter. 8395 passing, 11 failing — the failures are the queue-drain bug arriving early |
| `vendor/openhuman` `feat/agentspec-tool-factory` | `AgentSpec::tools`, `HarnessBuilder::tools`, `Tool` re-exported, gitbooks updated. Unpushed |
| `feat/dm-consult-handoff-v2` | abandoned; kept for the `consult`/`hand_off` evidence and the openhuman bump adaptation |
