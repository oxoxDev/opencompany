# Delegation, direction, and desks

*Phase P4. Waiting for delegated work and steering a run in flight — and why
the desk collapse this page used to end with is withdrawn.*

Terms: [glossary](../../glossary.md).

---

## The join primitive

### The gap

Board delegation today is fire-and-forget. `spawn_task` (and the
orchestrator's `assign_task`) push onto a `DelegationQueue` that is drained
**after** the parent turn by the `DelegationRunner`. There is no handle a
caller can hold, no future to block on, and **no way for a turn to wait for
work it asked for**.

Conversation is the exception, by construction. A seat on a desk does not
delegate to a colleague; it speaks (`post`, `dm`, `broadcast`), the round
commits when every seat has spoken, and the next round is the reply
([../hive.md](../hive.md)). The synchronous hand-offs that used to run a
colleague's turn *inside* the caller's — `delegate_to_desk`,
`delegate_to_teammate` — are gone with the relay they needed.

### What ships

- **`spawn_task` returns an id immediately.** A spawn is not a wait.
- **`await_task` / `await_tasks`** join over a set, concurrently, so a batch
  costs the slowest child rather than the sum. Omitting the id list waits on
  everything outstanding.
- **`peek_task`** reads status without blocking.

### Rules that are not optional

**A caller MUST be able to outwait its child's full budget.** The await ceiling
is derived from the child's run budget, not set independently. An await that
expires before the work it is waiting for can finish is a timeout that reads as
a failure.

**A permit is held for a child's entire life, including while that child awaits
its own children.** It follows that the concurrency semaphore MUST have headroom
well above the maximum fan-out depth. That headroom *is* the deadlock argument —
if every permit can be held by a parent waiting on a child that cannot get a
permit, the system stops. Size it accordingly and say so where it is set.

**A batch validates every brief before launching any of them.** A half-launched
batch leaves children running for a call that returned an error.

**Depth and cycles are enforced at the tool boundary**, dynamically, against the
live scope chain — not by which tools were wired. Belts are cached per roster,
so depth cannot be a property of the belt.

---

## Operator directives

### The gap

A run in flight is closed to its operator. Prompts are assembled once, budgets
are read at launch, and editing a document mid-run changes nothing because it
was already read into every system message that will ever be sent. Someone
watching a run take a wrong turn can only keep watching.

OpenCompany has the *enforcement* half — a stop hook checked between tool-loop
iterations, and pause/cancel/redirect actions. It does not have the *operator*
half: a durable place to put an instruction that a running loop will pick up.

### The queue

An append-only JSONL queue with a separate cursor, and the writer split is the
whole design:

- The **host only appends** to the queue.
- The **runtime only writes** the cursor.
- Neither side writes what the other writes, so **neither needs a lock**, and
  the one number they share is owned by the side that advances it.

The writer split removes the reader/writer race, not the writer/writer one.
**Appends to the queue MUST be serialized through one designated append
path** — a single host-side operator-directive service, or an equivalent
`O_APPEND`-with-single-writer guarantee — so two host processes never
interleave partial writes into the same file. This is narrower than a lock:
it is a rule about how many writers the queue is allowed to have, enforced by
only ever running one.

**A directive's id is its line number.** Not a stored field — which is exactly
why recovery MUST NOT change the line count. A line the reader cannot parse is
skipped **and still counted**: a torn append costs one directive (its id is
consumed and never reused), not the alignment of every later one.

Before appending, the writer MUST check whether the current final line is a
complete, parseable record. An incomplete trailing record from a prior crash
is recovered by **tombstoning, not truncating**: the writer truncates back to
the last complete newline to discard the torn bytes, then immediately writes
a fixed, parseable tombstone record terminated by its own newline in that
line's place, restoring the line count to what it would have been had the
crashed append completed. Only then does the new directive get appended, as
the next line. Plain truncation — discarding the torn bytes and appending the
new directive directly after — would instead let the new directive reuse the
torn line's number, silently violating "still counted": the reader would see
one fewer directive id than was actually consumed, and a receipt filed
against the torn id (if one raced ahead of the crash) would end up describing
the wrong line's content. The tombstone is what makes recovery a same-costs-one
event instead of an id collision.

The cursor MUST be written staged-and-renamed, so a torn cursor reads as zero
rather than as a garbage offset.

### Delivery

- Drained **once**, by a single consumer, which is what preserves the cursor
  guarantee. It is then posted to every interested party's mailbox.
- Delivered **verbatim** into the next attempt, above whatever the loop
  concluded on its own, and labelled as coming from the operator.
- **Nothing waits for it.** A directive reaches the work in seconds to minutes,
  and the run keeps going whether or not anyone is watching. A loop that blocked
  on a human would be the slow-participant failure with no ceiling at all.
- A **receipt is written whatever happened**. An operator who sees nothing
  cannot tell a directive still queued from one picked up and lost.

### What a directive cannot do

A directive is **asserted, not established**. It MUST NOT be filed as a claim,
and the role acting on one MUST NOT be routed the
[claim ledger](alignment.md#claimsmd--the-evidence-ledger).

It MUST NOT force a restart, end a run, or make unverified work count as
answered. Redirecting work and fabricating a result are different powers.

---

## Narrowing budgets

Budget constructors that can **only ever narrow**: a judging budget, a
housekeeping budget. A curator or a judge does not need a worker's allowance,
and a narrowed budget that could widen is not a bound.

These are constructors on the existing capability budget, not a second budget
system.

---

## Desks are rooms

### The claim this page used to make

A desk was `{id, name, description, members}` plus three overlay types, and
its entire runtime behaviour was: resolve the desk, take the first member who
is a real roster teammate as the lead, run that member's turn, relay the
reply. On that reading a desk was a workflow with one node, no error
handling and a bespoke resolver, and the plan was to ship a `desk` workflow
template, alias `delegate_to_desk` over `run_workflow` + `await_task`,
migrate the four things a desk id means, and remove `GroupChat`.

### Why it is withdrawn

The reading was true of the relay and false of the desk. What a desk is *for*
is several people at one table, and the relay never gave it that: one lead
answered, and a colleague was reached only by running their turn inside the
lead's. [`hive.md`](../hive.md) gives the desk the thing the workflow engine
does not have — a room whose seats run at the same time, read each other's
committed rows, address each other directly, and end when the work is
reported complete rather than when a graph runs out of nodes.

So the collapse runs the other way. The entity that went away is the relay:
`delegate_to_desk`, `delegate_to_teammate`, the in-turn `ConversationDispatch`
and the `TurnSpeech` fold. Desks stay, and the four things a desk id means —
a chat thread, a channel adapter, an assignee, a workflow output destination
— stay meaningful because every one of them is a way to reach the room.

### What a workflow is for

A workflow is the static, inspectable graph for work that is a **pipeline**:
retries, `on_error`, `requires_approval`, conditions, `sub_workflow` nesting,
cron. A desk is for work that is a **conversation**. A workflow `agent` node
still runs the same harness turn as a seat — same `AgentSpec`, same tools over
the same MCP server, same metering — on a fresh session rather than the
agent's standing one, and a workflow may deliver its output to a desk, which
is how a pipeline's result reaches a room.

### Migration of shipped companies

Every shipped company that declared `[group_chat.hive]` (the quorum, budget
and move-grammar knobs) declares `[group_chat.routing]` instead; the old
block is refused at load with a migration hint. A company that declares desks
and no routing block gets the defaults.

---

## Verification

- An await outlives its child's full budget.
- A batch validates every brief before launching any; a rejected brief launches
  nothing.
- The concurrency semaphore's headroom exceeds maximum fan-out depth, asserted
  where it is configured.
- A parent awaiting a child that itself delegates does not deadlock.
- Depth and cycle rejection are enforced against the live scope chain, not the
  wired belt.
- A torn line in the directive queue costs exactly one directive; every later
  directive still lands at the line number it would have without the crash.
- An append that follows a torn trailing record tombstones it first (rather
  than merging into it, and rather than reusing its line number for the new
  directive).
- A torn cursor reads as zero, never as a garbage offset.
- A receipt is written even when the consuming step failed.
- A directive cannot mark unverified work answered, force a restart, or end a
  run.
- The role acting on a directive is not routed the claim ledger.
- Every shipped company that declares desks loads with `[group_chat.routing]`
  or the defaults, and a `[group_chat.hive]` block is refused with the hint.
