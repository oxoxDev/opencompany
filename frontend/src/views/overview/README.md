# Overview — the knowledge graph

`#/overview` is the graph and nothing else: no page header, no strip, and the
console's own top bar is hidden for this view (see `app-shell.tsx`). It fills
the viewport beside the sidebar.

## What it draws

Five concentric rings, read outward from the centre:

| Ring | What | Where it comes from |
|---|---|---|
| 0 | the company, drawn as its memory constellation | `…/memory` |
| 1 | desks (pillars) | `…/desks`, plus a stated "No desk" group |
| 2 | the jobs on each pillar, and the workflows it runs | `…/tasks` grouped through their assignee; `…/workflows` |
| 3 | the teammate who does each job, each workflow stage, and the humans | `…/team` matched by `task.assignee`; humans from `…/users`; stages are the saved graph's nodes |
| 4 | the tool grants that teammate holds | `…/team` → `tools.effective` |

Rings 2 and 3 each carry two kinds. A workflow sits beside the SOP tasks
because both are work a department runs; a workflow's stages sit beside the
workers because a stage is where the flow meets the person who performs it.

Hover any node to trace its whole pillar chain. Click a pillar to grow it into
a bottom-up tree; click a job for its steps, a teammate or tool for its card.
Click the core to bloom the memory constellation, with type-to-find over it.
Drag the background to pan. `←` / `→` turn the pillar wheel; `Escape` steps
back out.

Panning is an offset on top of whatever the camera is framing, not a separate
mode: the shot still tracks its subject, just off-centre by the amount you
dragged, and re-framing (selecting a node, opening the core) resets it.

## What is real, and the one thing that is a reading (issue #601)

Three of these rings used to be invented, and the reasons recorded here were
stale. `assignDepartment` keyword-matched roles into five hard-coded pillars,
`assignTools` dealt each teammate a slice of the company-wide `[tools] allow`
list, and `WORKFLOW_TEMPLATES` made up one routine per pillar. All three are
gone.

The cause was a **DTO gap**, not carelessness: the graph is drawn from the
roster *list*, and the list carried no tools and no desks — the per-agent
*detail* read had both, one agent at a time, so drawing a whole roster meant
N+1 fetches on page load. `GET …/team` now carries `tools` and `desks` on every
row, from the same host-side constructor the detail read uses, so:

- **Departments are desks.** Host order, one pillar per desk somebody sits on.
  A teammate on several desks is drawn on its first. A company that has
  declared **no** desks gets one honestly-labelled "No desk" group and a notice
  saying so, not five invented departments.
- **Tools are the grants the host resolved** — `tools.effective`, the agent's
  own `[[agent]].tools` narrowed by `[tools] allow`, computed by
  `agent_effective_grants`, the same function the harness builds the agent
  with. The graph and the agent detail card therefore cannot disagree about a
  teammate, which is the rule issue #264 established. A tool node is labelled
  with the literal grant glob, because that is the string in `company.toml`.
- **Workflows are the company's saved graphs.** Stages are the graph's own
  nodes, sorted into run order by its own edges; a stage hands off to an agent
  only where the graph's `agent` node names one. Nothing is dealt round-robin.
- **Humans sit in the "No desk" group.** Signing in says who somebody is, not
  what they work on, and desks hold agents.

**The one reading left** — and all `DERIVED_NOTICE` now claims — is *where a
workflow hangs on the wheel*. A flow belongs to the company, not to a desk, so
it is drawn on the desk of the first teammate it runs through.

## The refresh question, answered

The page is a **snapshot**, and says so: a "Snapshot HH:MM" chip with a Refresh
button sits in the top-right corner. One paint is six reads plus one per saved
workflow, and the picture changes when an operator does something rather than
on the clock, so a timer would be a standing cost per open tab for no freshness
anybody asked for. The alternative that was rejected is silence: fetching once
and *not* saying so leaves an operator reading a stale wheel as the current
company.

## Files

`kg/` holds the graph itself — `model.ts` (the five-ring node/edge model),
`adapter.ts` (our host's data, shaped into it), `tree-layout.ts` and
`memory-core.ts` (pure layout and camera maths), and the `KnowledgeGraph` /
`KnowledgeGraphFullscreen` / `KnowledgeDetail` components. `pulse.ts` holds
the two board predicates the adapter needs. Theme tokens live under `.oc-kg`
in `src/index.css`.

The graph is the whole page, so its chrome stays minimal: a pillar selector, a
kind legend, the side paddles, and the detail card. The docked directory index
and the entity/function/action lenses were removed — with nothing else on the
page competing for attention, they covered more of the graph than they earned.
