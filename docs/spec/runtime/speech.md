# One agent, one session — and talking as a tool call

Two properties that only make sense together: an agent that is continuous,
and an agent that speaks by calling a tool.

## The session

Every company agent is one `openhuman_embed::Agent` on the process-wide
runtime ([hive.md](hive.md#the-shape)), and every conversational turn it takes
— on any desk, in a DM, on the General line — resumes **one stable OpenHuman
session**, `session_key::openhuman_session_key(company, agent)`
(`{company}:{agent_id}`). OpenHuman owns the thread: it is on disk under the
runtime's workspace, it survives a restart, and nothing in this crate clears,
re-seeds or windows it. A turn that names no chat — a task card, a workflow
node — runs on a fresh session of its own.

What the turn is *handed* is the delta: the rows on the desk it is answering
in that this agent has not yet seen, attributed and cued, from
`tinyhivemind::sharing::prepare_delta` over `hive::session_log::EventLogSessionLog`:

```text
Since your last turn on this desk:
[41] operator: Ship the pricing page by Friday.
[42] engineer: I can have the backend flag ready Thursday.
[43] ceo (dm to you): Keep the copy short.
```

The watermark is `SharingState`, per (agent, conversation), persisted with
the episode (`EpisodeStateSaved.sharing`) — so a 200-turn desk hands a seat
what is new, not the desk. The ordering is a contract: **the watermark
advances only after the turn's row is durably appended.** Advancing it first
would let a failed turn leave the session believing it read something it
never saw.

There is no mailbox and no per-agent queue, because `tinyhivemind` refuses to
have one: `NoDispatchReason::SelfMention`, `NoReferralReason::SelfMention` and
`UtteranceRejection::SelfRecipient` all decline self-addressing by name. The
architecture is **stigmergic** — work leaves an attributed trace in a shared,
globally sequenced log, and the trace is the stimulus for the next turn.

### What is given up, and what is not

| | |
|---|---|
| **Channel isolation** | Given up. One session spans every desk the agent sits on; the cue on every delivered line (`[seq] author:`) is what keeps a merged transcript readable, through the same `prefix_every_line` machinery that defends attribution against forgery. |
| **Audience isolation** | **Kept.** A desk `dm` this agent is not party to never reaches its delta. `readable_by` narrows with `Viewer::Agent` exactly as the operator history does with `Viewer::Operator`. |
| **A bound on growth** | Kept: the delta is bounded by the library's own limits, and crossing them is a `Reinitialize` — the recent window instead of a partial replay. |

## Talking as a tool call

A turn's **return text is not the message**. On a desk, a seat says something
by calling one of the speech tools on the `opencompany` MCP server
([hive.md](hive.md#speaking)); the host appends the row, decides what it
means for the episode, and journals it with the turn's steps, its live SSE
frame and its board-card correlation, none of which a tool holds. The names,
argument shapes and descriptions are `tinyhivemind::speech::tool_specs()`,
rendered verbatim — nothing here invents a contract:

| Tool | Effect |
|---|---|
| `post { message }` | Say one thing to the desk being answered in. |
| `broadcast { message }` | Say one thing to the desk **and** ask the host who should pick it up — Jev picks the seats, the lead is the fallback. |
| `dm { to: [ids], message }` | Say one thing to named seats of this desk. The row is on the desk with `audience = to`; a viewer outside it sees that a DM happened, not what it said. The recipients are assigned the next round. |
| `complete_episode { message }` | Say one last thing and report this seat's assignment finished. |
| `read { limit }` | Read further back in this desk than the turn was handed. Clamped by the crate's own `READ_MAX`. |

A seat's turn ends with **exactly one** of the first four. A second speech
call in the same turn is refused by the server ("one action per turn; the
first is recorded"). A turn that made none has its reply salvaged as a `post`
when it reads as one; otherwise the seat is re-asked, up to four times, before
the host records `TurnFailed` and completes that seat with `(no action)` so
the room never waits on a silent member.

The tools are unprefixed (`post`, not `desk_post`) because they live on a
named server: OpenHuman presents them as `mcp_call_tool{server: "opencompany",
tool: "post", …}`, so there is nothing for `read` to collide with.

### A DM stays in the room

`dm` is a narrowing *within a desk everybody named is already on*. That is
what `audience` is for, and why the recipient check is at call time against
the hive's own roster (`hive.resolve_dm`): a name that is not an active seat
on this desk is a tool **error** the seat sees and retries, not a row that
lands in front of someone with no record of who let it in.

Reaching a seat on another desk, or another desk as a whole, is a
[referral](hive.md#referral) — a crossing the library models, with its own
provenance (`hive-referral`) and its own return path — never a `dm`.

### Direct, General and workflow turns

Off a desk there is no room and no episode. A DM with the operator, the
General line and a workflow copilot thread run one ordinary turn, and that
turn's return text **is** the reply, journaled by the host as it always was.
The speech tools are on the server for every surface; a `post` made there is
the reply, a `dm` or `broadcast` made there is refused with the reason. Going
quiet because a model forgot a tool call is not a failure mode this design can
produce on any surface.

## Reading it back

`GET {scope}/agents/{agent_id}/session` → `{sessionId, transcriptsDir,
lastTurnAt}`: where OpenHuman keeps this agent's one thread and when it last
took a turn. The thread itself is OpenHuman's; the company's view of what the
agent said and was handed is the journal, read through `chat/history` with
the caller's own `Viewer` — an operator sees every row, including every `dm`
audience, because privacy between agents is a deliberation device and never a
security boundary.

The console renders the agent's rows as the **Session** tab on
`#/company/agent/<id>` (`frontend/src/views/team/AgentSession.tsx`), reusing
the room's own `RoundBand`, `UtteranceChip` and `ReferralConversation`, so an
agent-to-agent exchange reads the same way there as in the desk it happened
in.

### Raw turns

`#/company/agent/<id>?tab=session&raw` renders the same rows with the chat
rendering taken off: one block per journal row, in order, each stamped
`said` or `heard` — the latter in the literal `[seq] author: text` shape the
delta prepends to a turn, so what is on screen is the string the model was
handed. Tool calls unfold into their arguments and result instead of a step
chip.

It is not a dump of the model's context window; that is OpenHuman's session
on disk. It is the company's record of what this agent was given and what it
committed, which is what an operator asking "why did it answer that" is
asking to see.

`?raw` is an address rather than component state, for the reason `?edit` is —
"look at what it actually saw" is a link one operator sends another.

### The same view from the DM

A **Raw turns** control sits in the chat header of a DM
(`frontend/src/views/room/ChatHeader.tsx`), addressed as `#/chat/dm:<id>?raw`.
It is offered only in a DM: a `#channel` has several agents and the Operator
feed has none, so the control would have to pick one for you. It shows this
conversation's turns, filtered from the same per-agent route by both DM
spellings the host registers (the bare teammate id and `dm:<id>`). Both
surfaces render `frontend/src/views/room/RawTurns.tsx` — one component,
because "what the agent saw" is a claim about the runtime, and a claim that
reads differently depending on the screen is two claims.

## Where the code is

| Path | Holds |
|---|---|
| `src/session_key.rs` | `openhuman_session_key`, `runtime_agent_id` |
| `src/hive/session_log.rs` | the journal as a `SessionLog` |
| `src/hive/prompt.rs` | the delta and the fenced instruction |
| `src/hive/tools.rs` | the speech fold: `speech::interpret` into the in-flight turn's outbox |
| `src/hive/mcp_server.rs` | the server the tools are called on |
| `src/server/chat_history.rs` | `agent_channels`, `Viewer`, the history projection |
