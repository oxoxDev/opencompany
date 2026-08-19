# A run's verdict — `WorkflowRunVerdict` (issue #981)

The rows on `WorkflowRun.deliveries` say *why* a report did not go out.
`verdict` says what the whole run adds up to, in one word, on both run DTOs —
the synchronous `POST …/workflows/{wid}/run` body and every row of
`GET …/workflows/runs`:

```text
running | failed | stopped | blocked | undelivered | awaiting-approval | ok
```

**Always serialized**, unlike the optional fields around it. Its whole purpose
is to be the field a client reads *instead of* re-deriving the reading, and an
omitted verdict pushes every reader straight back into the six-field ladder it
replaces.

## Why the host owns it

A run's outcome was spread across `running`, `error`, `cancelled`,
`blockedNodes`, `deliveries` and `pendingApprovals`, and nothing said what they
added up to. The only place that answered "did this run succeed?" was the
console's TypeScript, so every other reader re-derived it — and the obvious
derivation is wrong in exactly the case that matters. Delivery is **host-side
and post-engine** (`src/workflows/delivery.rs`): by the time a destination is
refused the engine has already returned, so the graph's nodes all report `ok`
and nothing about a node's status moves.

The 2026-08-18 QA pass watched one run paint its `output` node `DONE`, green,
list it as `ok` in the Steps panel, and score PASS in a harness folding
`nodes[].status` — while the run's own delivery row read `channel-not-wired`
and the report was gone. Three readers, three transcriptions of the same
ladder, and the one fact that mattered in none of them.

## The order is the check

Each arm below the first exists because the state it names had been scoring
green on some surface:

| verdict | read from | why it sits here |
| --- | --- | --- |
| `running` | `running` | an unsettled run has no error, no cancel and no deliveries yet, so without this it falls to `ok` |
| `failed` | `error` | **the more serious fact first** — a run that broke mid-graph *and* dropped a report reads `failed`, with its delivery rows still on the body |
| `stopped` | `cancelled` | issue #383: a stop somebody asked for is not a fault, and a cancelled run has no deliveries to weigh |
| `blocked` | `blockedNodes` | issue #881: carries no error, is not cancelled, is not running and routed no report — the shape that fell through every check |
| `undelivered` | `deliveries` | issue #981: a report that will not go out without a change outranks one waiting on a human |
| `awaiting-approval` | `pendingApprovals` **and** `pending` delivery rows | issue #846: a run that paused at a gate reached no `output` node, so a delivery-only read scored the gated case clean |
| `ok` | — | finished, delivered what it routed, waiting on nobody |

An empty `error` string is not a failure. No producer writes one, and the
console's `if (run.error)` has always read it as falsy — the host agreeing costs
nothing and removes a way for the two to disagree.

## Undelivered is its own reading, not a failure

A delivery failure does **not** populate the run's `error` and does **not** flip
any `nodes[].status`. The nodes really did run and their work is valid; the fix
is a destination or a runtime wiring, not a node. Marking the run failed would

- point the copilot's fix-from-run at a graph that was fine,
- inflate the failure count and hide real breaks among them, and
- collapse the three terminal readings issue #383 keeps apart.

So `undelivered` sits between them: not `ok`, not `failed`, and named for what
happened. Every existing consumer of `error`, `cancelled`, `running` and
`nodes[].status` sees exactly what it saw before.

## Derived, never stored

`CompanyEvent::WorkflowRunFinished` gains no field. `GET …/workflows/runs`
computes each row's verdict in a single pass **after** the fold has settled every
open row and after the issue-#1009 cross-check has flipped the dead ones — the
position is the correctness argument, since every input the verdict reads is
written by the settle arm *after* its row was pushed.

Three things follow:

- **No migration.** Every run already in a company's journal re-scores on
  deploy, including rows written before this existed.
- **No third state to keep in sync.** The read-side settle (issue #1081)
  rewrites `running` and `error`; a stored verdict would have to be rewritten
  alongside them, and the one that was forgotten would be the bug.
- **No new failure mode.** A verdict cannot disagree with the rows it was read
  from, because there is only ever one reading.

One consequence worth stating out loud: anyone counting successful runs off this
endpoint sees their rate drop, with no change in behaviour. The dropped reports
were always there.

## Consumers

`WorkflowRunVerdict` lives in `src/ports/workflow_verdict.rs`, beside the
`DeliveryReport` rows it reads. The console's `runTone`
(`frontend/src/views/workflows/run-health.ts`) is a lookup on `verdictOf`, which
takes the host's word when there is one and falls back to the same ladder for a
host predating this — the fallback is what keeps a run's meaning stable across
hosts, not legacy tolerance for its own sake. `qa/oc-qa.js` reads it the same
way, and `frontend/test/unit/qa-harness.test.ts` pins the two together.

The orchestrator's `run_workflow` tool summary
(`harness::orchestrator::summarize_run`) reports the dropped reports too, using
`DeliveryReason` and never `detail` — the closed set is the log-safe half of the
pair (issue #248), and a summary rides wherever the model's turn rides.

## Not in scope

**What counts as undelivered is unchanged**: any row that is neither `sent` nor
`pending`, exactly as the console's badge, the scheduler's log line and the SSE
toast have always counted it. Three reasons that land on `skipped` arguably do
not belong in that count — `already-delivered`, `no-destination-configured` and
`dry-run` each describe a report that was never owed to an address — but
reclassifying them moves the badge and the verdict apart unless every surface
moves together. That is its own change; this one moved the ladder to the host
without moving the rungs.

The SSE `workflow_run_finished` frame carries no verdict. It already reads the
delivery rows correctly and toasts an undelivered report, so nothing there
scores a dropped report green.
