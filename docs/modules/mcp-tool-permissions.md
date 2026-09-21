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

## Not yet wired

A directory install's policy is not loaded at harness-build time, so no
`blocked` can exist for the registry bridge tool yet, and nothing writes a
policy document at all until the tool-permissions routes land. Per-tier defaults
reach only tools that already have an entry, because no tool inventory is
persisted for them to name.

