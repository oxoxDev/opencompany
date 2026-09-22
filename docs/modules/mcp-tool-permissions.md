# Per-tool permissions for MCP servers

Split out of [MCP Servers](mcp.md), which holds everything else about
per-tenant tool servers: where they come from, how credentials are stored, how
agents are scoped to them, and the directory.

Each server carries a policy document that says, per remote tool, what happens
when an agent calls it. It is stored at `mcp/{name}/tool_policies` (and
`mcp_registry/{server_id}/tool_policies` for a directory install), separate from
the credential and from the declaration.

Three modes:

| Mode | Effect |
|---|---|
| `always_allow` | Runs without parking for a human. |
| `needs_approval` | Parks under the standing approval rules, as every bridge call does by default. |
| `blocked` | Refused before the call reaches the transport. No approver can wave it through. |

And three tiers a tool can be grouped under — `read_only`, `interactive`,
`write_delete` — each of which can carry a bulk default so "allow everything
read-only on this server" is one decision rather than one per tool.

## What resolves a call

Two ladders. The tier is an operator's reclassification, else a suggestion, else
`interactive`. The mode is the tool's own override, else the tier's stored bulk
default, else a hardcoded fallback.

**A suggested tier never grants `always_allow` on its own.** The suggestion
comes from a name heuristic (`get_`/`list_`/`read_`/`search_` reads;
`delete_`/`remove_`/`drop_` destroys), and a heuristic deciding who skips the
approval gate would mean that the day it gains a verb, calls that used to park
quietly stop parking. A suggestion groups a row and pre-selects a control; a
stored tier default or a per-tool override is what actually allows. The same
reasoning is why a server's own `readOnlyHint`/`destructiveHint` annotations are
not a source: they are self-reported by whoever runs the server, and a directory
install can come from anyone.

## The legacy declaration is still live

A server's `read_only_tools` list is the baseline the stored document layers
over, field by field — not a one-shot migration input. A stored entry naming
only a mode keeps the baseline's tier, and editing one row cannot retire the
declaration's remaining rows.

An **unreadable** document is not the same as an absent one. Absent means the
declaration is the whole policy. Unreadable drops the declaration too and parks
everything, because the damaged document may have carried a refusal, and falling
back to the declaration would restore an allow the operator had taken away. The
degrade is scoped to the one server named in the warning: the loader never
surfaces the failure, because MCP resolution's caller treats an error as "this
company gets no MCP servers at all".

## Where a block is enforced

A company agent does not reach a declared server through this crate's bridge
tool. `AgentSpec::mcp` attaches the granted servers to the agent, and the names
in `OPENHUMAN_NATIVE_TOOLS` — `mcp_call_tool` among them — always resolve to
OpenHuman's own implementation over those attachments. The bridge tool's guard
runs only where that tool is the one dispatched.

So a blocked tool is denied where the server is attached: its name goes on the
attachment's deny list, which the transport filters on before anything is
listed or dialled, and where deny outranks allow. The declaration's own
`disallowed_tools` is kept — the policy adds to that list rather than replacing
it. Only `blocked` is denied this way; a tool that merely parks stays reachable,
because parking is what the approval gate is for.

That is resolved when the agent is built, so a block reaches the native path on
the next build. A **directory install** is the other shape: it is addressed by a
`server_id` argument at call time rather than by the grant its tool was wired
under, so there is no build-time snapshot to attach a policy to. Its scoping
decorator reads the install's document when the call arrives — the grant answers
whether this agent may name the install at all, the policy whether that tool may
run, and both refuse before anything is dialled. A deployment with no secret
store cannot read a policy and does not invent one; the grant stays the whole
gate.

An install carries no `read_only_tools`: that is a manifest affordance of a
declared server, so for the registry the stored document is the whole policy and
the persisted inventory is the only thing that can say which tier a tool is in.

The guard in the bridge tool stays as the same refusal for any path that does
dispatch it, and both paths word it with one function so an agent cannot tell
from the message which one refused it.

## Where the tiers come from

A tier default can only reach tools something has named. Discovery persists an
inventory — tool name to suggested tier — beside the policy, from the same
listing the health probe already performs, so a bulk "block everything
write/delete on this server" reaches the tools that server actually has. A
failed probe leaves the previous inventory standing rather than emptying it, and
neither write can fail the probe.

An inventory on its own grants and blocks nothing. It is a proposal the console
renders and the resolver reads as a suggestion; only a stored decision changes
what happens to a call.
