# Plan → Workflow

*The board's workflow builder pass and the review-before-creation gate (issue #580).*

Most board cards are done once. Some describe work that should happen again and
again — "email the digest every Monday", "triage new issues each morning". For
those, an operator marks the card's **deliverable** as `workflow` instead of
`once`, and entering In Progress builds a *reusable workflow* rather than
dispatching the work to a teammate.

This document is the contract for that path: how a card is routed, what the
builder pass is allowed to do, why the graph lands **In Review** before it
exists, and what apply and reject do.

Implementation: `src/harness/workflow_build.rs` (the pass),
`src/ports/tasks.rs` (`TaskDeliverable`, `TaskWorkflowProposal`),
`src/company/workflow_create.rs` (the host-authority conversion + courtesy
validation + `create_company_workflow`), `src/server/ops/tasks.rs` (the
apply/reject routes), `src/metering/workflow_build.rs` (what it costs and who
pays).

---

## The operator chooses; nothing guesses

`deliverable` is an **explicit** choice (decision D2a). There is no heuristic
that reads a card and decides it "looks automatable" — a card is a one-off
unless a person marked it `workflow`, on the create body, on a patch, or on the
chat payload that opened it. `once` is the default and the historical behaviour,
and it stays off the wire so a one-off card is byte-identical to a pre-#580 card.

## The contract in one table

| | |
|---|---|
| **Trigger** | the transition *into* `in_progress` of a `workflow`-deliverable card, edge-fired in `CompanyRuntime::dispatch_task` |
| **Work done** | exactly one model call, no tools, no retry — the planning station's shape |
| **Deadline** | 120s, hard |
| **Cost** | one `SampleKind::Inference` sample, charged to the card's **assignee**, carrying the attempt's `run_id` |
| **Run row** | one — building the workflow *is* the card's In-Progress work |
| **Locks** | none |
| **Exit** | automatic, four-way (below) — the card never stays in `in_progress` |

### The four exits

| Outcome | Landing | Card carries | Attempt |
|---|---|---|---|
| a graph that could be created | `in_review` | the `TaskWorkflowProposal` awaiting approval | Succeeded |
| the plan is not automatable | `todo` | the model's reason, no proposal (decision D2c), and `deliverable: once` | Succeeded |
| the model decided nothing | `todo` | the reason only, no proposal; stays `deliverable: workflow` | Failed |
| the pass itself failed | `todo` | the reason only, no proposal; stays `deliverable: workflow` | Failed |

A **not-automatable verdict is an answer, not a fault** (issue #873). The builder
was asked whether the work should be automated and it decided; that settles
**Succeeded**, with the reason on the card note and **no** `error` on the run row
— the same convention the [draft-from-description endpoint](../../../src/server/ops/workflows.rs)
already follows in answering `200` for the identical verdict.

The verdict also **converts the card to `deliverable: once`**. This is what lets
the card proceed: dispatch routes a `workflow`-deliverable card to this pass
rather than to its assignee, so a declined card left carrying `workflow` would
re-enter the builder on its next dispatch, draw the same verdict, and fail again
— builder → To-do → builder, with no path to the person who could simply do the
work. As `once`, the next dispatch reaches the card's assignee.

A build that could not be *attempted* — an unreadable company state, a model
timeout or error, or a draft that parsed but decided nothing — keeps
`deliverable: workflow` on purpose, because retrying the **build** is the right
next move for a fault.

An operator who moves the card out from under the pass wins: the pass discards
its result and the attempt settles **Cancelled** — the tokens stay metered
because they were genuinely spent (the same optimistic-settle guard planning
uses, on `updated_at_millis`).

## Evidence before prescription

The direction is inverted exactly as in [Planning](planning.md): the **host**
gathers the roster (which teammates an `agent` node may name), the node-kind
vocabulary (the builder emits within `BUILDER_NODE_KINDS`, a subset of the
authoring set — see [workflow-vocabulary.md](workflow-vocabulary.md)), the
channel ids an `output` node's `channel` destination may name
(`deliverable_channel_ids()` — the same set the console's destination picker is
served from, issue #1191), and the names of the workflows that already exist,
and hands the model a complete picture. The model runs with **no tools** and only synthesizes
— so collisions and unknown-agent references are rare by construction, and a
builder pass can no more act on the world than a planning pass can.

The channel section is the newest of these and was added for a reason worth
recording: the pack had a roster section and a tools section and **no** channel
section, and the graph contract named the concept without listing ids. Asked to
"post a summary to the engineering desk", the model wrote `engineering-desk` —
the desk's display name with `-desk` appended — for a runtime whose channels are
`engineering`, `product_design`, `go_to_market`. Grounding is the fix; courtesy
validation (below) is the guard behind it.

The card's own plan (`TaskPlan`) is the strongest input when it has one: its
steps become candidate nodes, its prerequisites are grounding a node cannot
exceed, and its verification suggests an `output` node's summary. A card with no
plan still builds — from its title and note.

## The one difference from planning: an attempt row

A planning pass mints no run, because it is not an attempt at the work. A builder
pass **is** the card's work, so it mints a `RunRecord` (before the spawn, in
`open_run`, so a host that dies mid-build leaves a visible orphan the boot reaper
settles). That run is:

- the attempt whose `run_id` the proposal carries;
- the attempt the pass's inference spend is **metered against**, attributed to
  the card's assignee — the mirror image of planning's whole-company bucket,
  because a workflow card is dispatched to a teammate the operator (or a prior
  plan) already chose (`src/metering/workflow_build.rs`);
- the attempt the applied card's `TaskOutput` points at, so #339's "every card
  links to what produced it" stays honest.

The run settles `Pending → terminal` directly (the build runs outside the cycle
machinery), the same legal move `abandon_run` uses.

## Review before creation — the graph does not exist yet

The builder pass **never creates a workflow** (decision D2b). It generates a
graph, courtesy-validates that it *could* be created — the same shape, render,
roster and destination checks `create_company_workflow` runs, minus persistence
(`courtesy_validate_draft`) — and stamps it on the card as a proposal. A proposal
that could never apply never reaches In Review.

There is no disabled ghost draft in the workflow list. The graph reaches the list
only when a person **applies** the proposal.

### `POST …/tasks/{id}/workflow-proposal/apply`

The one path a proposal becomes a real workflow, and the host is its authority:

1. rebuild a `RawWorkflow` from the **stored** `ops` graph — never a
   client-supplied body — via `raw_workflow_from_spec`;
2. run `create_company_workflow`, which takes the company write lock,
   re-validates shape + roster + destinations + id/name uniqueness, and (issue
   #276) lands any schedule-carrying graph **switched off** until a person arms
   it. "The same validation an editor save runs" was not literally true until
   issue #1191: the channel-destination rule lived on the two write routes, so
   this path — the one path the operator did not author the graph — persisted a
   graph `PUT …/workflows` then refused, and marked the card Done for it;
3. stamp the card's `TaskOutput` linking the created workflow to the build
   attempt, and move the card to **Done**; clear the proposal.

If the create is refused — the roster drifted since the proposal was generated, a
name has since been taken, a destination names a channel nobody wired — the
reason is appended to the card's note, the card **stays In Review** with its
proposal intact, and the refusal is a 400 carrying the `workflow_invalid`
envelope, so the console names the node rather than only the sentence.

### `POST …/tasks/{id}/workflow-proposal/reject`

Clears the proposal and returns the card to **To-do** (decision D2c). The card
keeps its `workflow` deliverable, so dragging it back into In Progress builds
again; an operator who wants a one-off instead flips `deliverable` with a patch.

## The one authoring path

The **only** workflow write is `create_company_workflow`. There is no second
authoring path: the builder produces a proposal (an input to validation), and
apply runs the same validated-persist core the console's `POST …/workflows` route
and the orchestrator's `create_workflow` tool run. The stored artifact is the
`{id, name, description, nodes, edges}` graph, and the host owns the workflow id
(a safe, unique, deduped stem) so the model cannot pick a colliding or unsafe one.

## Compiled under `openhuman`

Like the rest of the harness. Without it, the runtime holds no builder and the
dispatch branch is inert — a `workflow` card entering In Progress dispatches as a
one-off, exactly as before #580. The apply/reject routes and the usage-sample
contract live outside the gate, so the write boundary and the spend contract are
built and tested in the default CI lane.

---

## Risks accepted

- **Prompt injection via card text** is bounded rather than eliminated: the
  parse is typed, the graph is validated deterministically host-side, there are
  no tools, and no secret is in the prompt. The worst a hostile card can do is
  produce a graph a person then reviews and can reject.
- **A discarded pass costs one metered call** with no landing — the tokens were
  spent, so they are metered.
- **Name collision at apply** (a name taken between build and approval) surfaces
  as a 400 and keeps the card In Review, rather than being pre-empted at build
  time — id-and-name uniqueness is a function of the live record at apply, not
  build.
- **A crash mid-build** leaves the card in In Progress and its Pending run for
  the boot reaper, the same as a crashed dispatch.
