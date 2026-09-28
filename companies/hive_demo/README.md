# Hive Demo Co

> The smallest company whose desks answer as **rooms**: two desks of two seats
> each, sharing the CEO. Every message on a desk opens an episode on that desk's
> hive (`tinyhivemind-openhuman`), the seats run **concurrently** in rounds of
> two, a seat may refer a question across to the other desk, and the episode
> ends when a seat calls `complete_episode`.

It exists to be measured. `companies/openhuman_demo` is the same three agents
with one seat per desk — a one-seat desk runs no round — so this is the copy
that exercises the thing the runtime promises about coordination: two desks'
rounds overlap, and one agent's turns never do.

## Roster and desks

| Agent | Desks | Responsibility |
| --- | --- | --- |
| Chief Executive | engineering, content | Sets direction; the **shared seat**. |
| Engineer | engineering (lead) | Explains how things are built and proposes technical plans. |
| Writer | content (lead) | Turns rough notes into short, clear written drafts. |

Both desks declare `[group_chat.routing] round_width = 2` and
`[group_chat.routing.referral] enabled = true, max_hops = 1, returns = true`
(see `docs/spec/runtime/manifest-semantics.md`). There is no remote MCP server:
the only tools a seat needs are the speech tools the host serves on its own
`opencompany` MCP server.

## Measuring it

Against the scripted mock brain, deterministic and offline:

```bash
scripts/measure-coordination.sh --mock
```

which boots `frontend/test/e2e/mock-brain.mjs`, a host built with
`--features openhuman,mcp` serving this company, posts one task to
`engineering`, tails `/events` until every episode completes, and prints the
numbers against the thresholds (max concurrent turns ≥ 2, ≥ 1 cross-desk
referral, ≥ 1 agent→agent dm or broadcast, ≥ 2 distinct pairs, every episode
completed; the exit code is the number of failures). Against a real model:

```bash
TINYHUMANS_API_KEY=<jwt> scripts/measure-coordination.sh
```

The console shows the same run live: open `#/chat/engineering` and the round
band draws two lanes working at once, the dm and broadcast chips, and the
completion marker; `#/company/comms` draws who spoke to whom; the Observatory
draws each round as a band across the seats that ran it.

`npm run e2e:hive` in `frontend/` drives the same company from a browser.

## Human in the loop

You keep **asking the desks questions and approving anything costly**; the
seats run everything else. The company's output is **decisions and short
drafts, reached by desks answering together**.
