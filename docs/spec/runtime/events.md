# Company Events

The `CompanyEvent` vocabulary carried by the
[`EventLog`](ports-state.md#eventlog) port, and the correlation rules that let a
reader fold one company's append-only journal back into per-task, per-approval
and per-run views.

Split out of [`ports.md`](ports.md) (issue #371) on this repo's 500-line cap for
a Markdown file; the port contracts have since split the same way (issue #427).
Those files own the *traits and their method signatures*; this one owns the
*event vocabulary* those traits carry, the half that keeps growing.

## The journal's shape

Append-only, replayable, **single-writer**, and company-scoped. Boot replays the
tail to rebuild in-flight state. Every variant is serialized internally-tagged
under `kind`, so each JSONL line is self-describing.

Three properties are load-bearing everywhere below:

* **Additive-only.** A new variant, or a new field carrying
  `#[serde(default, skip_serializing_if = …)]`, cannot change how an
  already-persisted line loads or how an existing event serializes. That is
  what lets the vocabulary grow with no journal migration and no break in the
  cross-backend export/import round-trip. The cost is accepted and stated: an
  **older binary cannot decode a newer journal's variants** — the same posture
  every variant addition has shipped with.
* **One writer per company.** The journal is opened by exactly one process, and
  a rebuild inherits the open handle rather than reopening it (two handles
  interleave a record and its newline onto one line and brick the next replay).
  Several boot-time sweeps rest on this; see
  [Interrupted runs](#interrupted-runs-issue-371).
* **A sequence is a stable id, and retention keeps it that way** (issue #275).
  Only the three workflow-run kinds and `McpCallFailed` may ever be pruned;
  everything else is permanent, because it is either the audit trail or the
  referent of a stored `EventSeq` (a thread `parent`, a `ReactionToggled`
  target, a `TaskDiscussionRedacted` tombstone). Pruning renumbers nothing and
  leaves gaps. See [Retention](ports-state.md#retention-issue-275).

## Variants

`CompanyEvent` variants: `OperatorMessage`, `WebhookReceived`,
`ScheduleFired`, `A2aTaskReceived`, `ApprovalParked` (issue #379 — an effect
is now waiting on the operator; see [In-conversation
approvals](#in-conversation-approvals-issue-379)), `ApprovalResolved`,
`FeedbackFiled`,
`PaymentReceived`, `LifecycleChanged`, `AgentReply`, `MemoryFactDeleted`,
`TaskDispatched`, `McpCallFailed`, `WorkflowCreated` (a new saved workflow
graph was authored + enabled via the console `POST …/workflows` route or the
orchestrator's `create_workflow` tool; journaled best-effort after persist —
`WorkflowUpdated` / `WorkflowDeleted` are journaled the same way and now reach
the same two surfaces, since issue #661 gave the orchestrator `update_workflow`
and `delete_workflow` alongside it),
`TaskSteered` (an operator paused, cancelled, or redirected an in-flight task
or delegation), `DeskTaskCompleted` (a dispatched board task finished its run —
the terminal anchor a per-task timeline ends on; "completed" means the run
stopped, not that it succeeded, `column` carries where the card landed,
`artifact_ids` names the deliverables the run published — empty, and omitted
from the wire, for the many tasks that produce no file; see
[artifacts.md](artifacts.md) — and `origin_chat_id` records the conversation the
card was raised from, absent when none did, see
[events-settle-marker.md](events-settle-marker.md), with `origin_parent`
naming the thread inside that channel since issue #1890),
`TaskDiscussionPosted` (a human posted to a card's discussion thread, issue
#335 — the Discussion tab's whole store, folded back out by
`GET …/tasks/{task_id}` beside that card's timeline), `WorkflowUpdated` /
`WorkflowDeleted` (issue #259 — a saved graph was replaced wholesale or removed;
neither carries the TOML body, deliberately, since the journal reaches readers
that have no business holding agent prompts or destination addresses),
`TeammateAdded` / `DeskCreated` / `DeskDeleted` / `DeskMembersChanged` /
`DeskRoutingConfigured` (the structural audit trail: a teammate or desk minted
at runtime, a seat moved, or a desk's routing block installed or restored). These
exist because **no durable row otherwise records that any of it happened** — the
company record carries current state and nothing carries the change, so "who
created whom" was unanswerable from the journal and a console could only infer
it from a redacted tool-call frame that does not survive a reload. Both teammate
creation paths (the orchestrator's `add_agent` tool and `POST {scope}/team`)
journal the same variant, because two paths answering that question differently
is how the gap opened. None carries a configuration body — the same rule
`WorkflowUpdated` follows, since the journal reaches readers with no business
holding an agent's prompt or a desk's routing block. All five are **permanent**
under the retention rule; their lifetime cardinality is "how often does an
operator author these by hand", so permanence costs almost nothing),
`WorkflowRunFinished` (issue #228 — the durable record of what a run did, from
every entry point) and, from issue #371/#382, `WorkflowRunStarted` /
`WorkflowNodeStarted` / `WorkflowNodeFinished` (the per-node progress trail; see
[Workflow run progress](#workflow-run-progress-issue-371)), and, from issue
#983, `TurnStarted` / `TurnFailed` (see [Chat turn brackets](#chat-turn-brackets-issue-983)),
and the desk-episode trail — `EpisodeOpened`, `RoundStarted`, `RoundCommitted`,
`BroadcastRouted`, `DmDelivered`, `EpisodeCompleted` (see
[Hive episodes and rounds](#hive-episodes-and-rounds)).

### Per-task event correlation (issue #185)

The journal is company-scoped, so the events a dispatch *produces* cannot be
filtered back to their task by shape alone. `AgentReply` and `McpCallFailed`
therefore carry an optional `task_id`, stamped by the harness when the
producing turn ran inside a `TaskDispatched` cycle and absent for an ordinary
chat turn. Together with the `TaskDispatched` / `DeskTaskCompleted` anchors,
that is what `GET …/tasks/{task_id}` filters on to assemble a task's timeline.

Both fields are additive — `#[serde(default, skip_serializing_if = …)]` — so
every already-persisted event loads unchanged and an untagged event serializes
byte-for-byte as it did before the field existed. No stored log needs
migrating, and the cross-backend export/import round-trip is unaffected.

`TaskRecord` gains `parent_task_id` on the same contract, recording the
task-to-task edge that `origin_chat_id` (a *conversation*, shared by every
sibling spawned in that thread, and absent entirely on a board-native card)
cannot express. It is the parent half of the Task Detail screen's lineage.

`OutboundMessage` gains `task_id` on the same contract (issue #246): the card a
chat turn **opened**, so the console can say a card exists instead of leaving an
operator to notice it on the board. It is journaled onto that turn's
`AgentReply.task_id`, which widens that field's meaning from "the dispatch that
produced this reply" to "the card this reply is about" — a card-creating reply
now also appears on that card's timeline, which is the lineage an operator
wants and costs no schema change. A turn that opens several cards reports the
**first**: the journal field is a single optional id, and widening it would
break the byte-identical round-trip, so the claim is incomplete but never wrong.
Both `chat/history` surfaces (REST and GraphQL) project it from the shared
`MessageView`, so the chip survives a transcript reload on either.

### Threads and reactions (issue #364)

A transcript survived a reload; the structure *on* it did not.

**A thread is a parent id, not an object.** `OperatorMessage` and `AgentReply`
each gain `parent: Option<EventSeq>` — the position of the message replied to. A
thread object would need a lifecycle, a membership, and a second addressing
scheme beside `chat`, for zero rendering benefit: the console already folds a
transcript by parent, so a thread *is* "the messages pointing at this one". The
answer to a threaded question takes the **same parent as the question** — both
halves belong under the row the thread hangs off, and pointing the answer at the
question would nest a thread inside a thread.

**A reaction is a per-person row, event-sourced.** `ReactionToggled
{ message_seq, emoji, on, by }`. A count answers neither question the console
asks — *who* reacted, and *have I* — and a mutable tally cannot live on an
append-only log. Reads fold the last event per `(message, actor, emoji)`; a row
that ends `off` is dropped, not kept as a zero. `on` is explicit, which is what
makes the route idempotent under a retry or two consoles racing.

`OutboundMessage` gains `message_id`, stamped by the chat route **after**
journaling — a brain emits an answer, it does not know where it will land in the
log. It is the enabler for both: before it, a sent bubble had only a
browser-minted counter, so anything durable naming it named nothing.

All three are additive on the terms above, and **nothing is migrated or
backfilled**: a pre-#364 message loads unparented, which is the truth about it.
`ReactionToggled` is **deliberately not projected** onto `/events` — the frame
would have to carry the reacting person and that stream has no per-viewer
projection to resolve an actor into a label. Pinned by a test rather than left
to the deny-by-default fall-through, so a later decision to stream reactions is
made out loud.

### Per-task approval correlation (issue #333)

`ApprovalResolved` carries an id, a verdict and an actor — never a task — so
the same problem reaches the approval queue, and worse: a *parked* approval has
no event at all. A task's Approvals tab could therefore only filter the
timeline for resolutions that happened to fall inside the card's run window,
which showed nothing while an approval was actually waiting and let a second
card worked in that window absorb the first's sign-offs.

The link is recorded where the approval is: the runtime journal's
`ApprovalParked` record gains a `task` field, stamped by the cycle that parked
the effect. A cycle knows which card it is working from its own trigger
events — a `TaskDispatched`, or an `ApprovalResolved` whose approval was itself
parked for a card, which is how a run needing two sign-offs keeps the link
through the first.

`task` is a two-armed link, not an optional id, and the distinction is the
whole correctness of the feature:

| On disk | Means | Read side |
| --- | --- | --- |
| `{"link":"task","id":"t-1"}` | that card owns it | shows on `t-1`, and only there |
| `{"link":"unlinked"}` | no card owns it | shows on no card |
| *absent* | written before #333 | falls back to the run window |

An optional id collapses the middle row into the last one, and the middle row
is not an edge case: every workflow delivery, operator-chat turn and scheduler
tick parks unlinked. Treating those as "unknown" sends each of them to whatever
card happened to be running, along with that card's `waitingSince` — the exact
misattribution this issue exists to end. So a host from #333 onward always
writes one of the first two, and absence means one thing only.

#### Which key is authoritative

Two keys correlate an approval to work, and they are kept **both**:
`Effect.run_id` (issue #242) is **attempt-level**, and `ApprovalParked.task` is
**card-level**. The read side resolves them in this order:

```text
card = if let Some(run_id) = effect.run_id { run_store.get(run_id).task_id }  // authoritative
       else { approval_parked.task }                                          // fallback
// neither recorded → genuinely unlinked, and NOT the run-window fallback
```

`run_id` wins wherever it is present because a `RunRecord` names its card, so a
run id resolves to a task — while a task id can never say which *attempt*
parked an approval. #183 settled that repeat trips through review are normal,
so two attempts on one card is the expected case, and only `run_id` separates
them.

It cannot be the only key, though: `run_id` is `None` by design for every park
with no attempt behind it — a chat turn, a workflow delivery, a scheduler tick,
and the hosted brain's own gate — whereas `task` is stamped in
`CycleHostImpl::park`, which *every* park path passes through. Neither key is a
superset of the other, so "pick one" is not available. The card-level key also
inherits through a resolution (an `ApprovalResolved` whose approval was itself
parked for a card keeps that card), which the attempt-level one does not do.

This is why the three-state link above is load-bearing rather than pedantic:
with two keys, "unlinked because chat-parked — no run and no card" has to stay
distinguishable from "not stamped because it predates #333", and only the
second may fall back to the run window.

A batch is ambiguous — and stamps nothing — when it names two different cards,
or when it carries a card's dispatch alongside a turn that is its own work (an
operator message, a webhook, a schedule tick, an inbound A2A task, or a
resolution of an approval known to belong to no card). A cycle is a unit of
batching, not of work, so "the card this cycle is for" is only well defined
when nothing else rides along. Issue #357 guards the same seam per *attempt*
with a queue-position boundary; this rule only has to stop the cross-turn leak.

The journal keeps a per-approval origin index (park instant, effect kind,
task link) because the parked effect is dropped from the queue on resolution
and nothing else can answer what a resolved approval was.
`GET …/tasks/{task_id}` returns `approvals[]` from it, joined by id.

**That index is unbounded.** It holds one entry per approval ever parked, for
the life of the process, and is never pruned — resolution and expiry remove the
queue entry but deliberately not the origin. #333 widens each entry from a
`u64` to a `u64` plus two `String`s (the effect kind, and the task id when
linked). No journal rotation exists today, so replaying every `ApprovalParked`
line on `load` is the only path to rebuild it, and it is the correct one. If
rotation is ever added, this index is the first thing that must survive it: a
rotated-away park line silently makes its approval unreadable.

The field is additive on the same contract as the rest — a pre-#333 line
replays with no link and keeps the old run-window correlation, so existing
history still renders.

### In-conversation approvals (issue #379)

A parked approval had no event at all — parking was journal-only — so a console
learned about a new request when its approvals feed next polled. That is far too
late to raise the request *inside the conversation that produced it*, which is
where an operator is actually looking when their agent stops to ask.

Two additions, both on the pattern above.

**`CompanyEvent::ApprovalParked`**, appended best-effort at the single park
choke point (`CycleHostImpl::park`) immediately after the journal write
succeeds. The journal is the binding record of what is parked; the event is an
advisory nudge, so a failed log write never undoes a park that already
happened — the same division `sweep_expired_approvals` draws for expiry.

It carries **an id, a dotted kind, and a thread — nothing else**. No payload and
no asker, deliberately: the parked effect's arguments are redacted and bounded
in exactly one place (`pending_approvals`, issue #372), and a payload-bearing
durable event would open a second surface that has to redact, and eventually
will not. A reader reacts by re-reading the approvals feed and renders from the
redacted `ApprovalSummary`. That costs one round trip between the frame and the
card, and buys one redaction surface instead of two.

**`ApprovalParked.thread` on the journal record**, stamped at park time from the
cycle's own trigger events by `cycle_thread_id` — the sibling of #333's
`cycle_task_id`, same exhaustive-match discipline, same refusal to guess. It
also surfaces on `PendingApproval`, `ApprovalOrigin` and `ApprovalSummary`, and
is copied onto `GrantedCall.origin_thread` when an approval mints a grant.

The id is read off `OperatorMessage.chat`, which is the only field that can do
this job. `Effect.agent` cannot: a desk channel and a direct message to that
desk's lead are answered by the same teammate, so placing a card by asker raises
one conversation's request inside the other. `chat` carries a desk id for a
channel and a roster agent id for a DM — **different strings even when the same
agent answers both**.

It inherits through a resolution, exactly as the task link does: an
`ApprovalResolved` whose approval was itself raised in a thread keeps that
thread, so a follow-up turn that needs a *second* sign-off re-parks in the
channel the first one was asked in rather than falling out of the conversation.

A plain `Option<String>`, not a two-armed link like `task`, and for a precise
reason: nothing downstream falls back to a heuristic when it is absent. An
approval with no thread matches no channel filter and stays Approvals-page-only,
which is exactly today's behaviour. So "no conversation produced this" and
"written before #379" need not be told apart — both are correct as the same
answer, which is the condition #333's enum exists to handle and this does not
have.

A batch is ambiguous — and stamps nothing — when it names two different threads,
or when it carries an addressed chat turn alongside work that is its own (a task
dispatch, a webhook, a schedule tick, an inbound A2A task). An **unaddressed**
operator message (`chat: None`) is itself a rival rather than a neutral
pass-through: it went to the orchestrator with no conversation of its own, so a
batch holding one cannot say which conversation a parked effect came from.

**The redemption reply is routed by this thread too**, and that is a bug fix
rather than a new capability. `redispatch_granted_call` journaled the
continuation `AgentReply` with `chat_id: grant.agent` — correct for a DM by
coincidence, and wrong for a desk channel, where the agent's continuation landed
in the desk lead's private line instead of the channel the operator asked in. It
now uses `grant.origin_thread`, falling back to the agent when there is none,
which is the previous behaviour kept for exactly the cases it was already right
for.

### The settle a channel can see (issue #377)

Moved to [events-settle-marker.md](events-settle-marker.md): the
card-linked marker a settled dispatch leaves in the conversation that
raised it, the captured origin — channel and thread — behind it, and why
a card nobody raised from a conversation belongs to none.

### What a retry would repeat (issue #351)

Re-entering a run re-runs its effects, and the two facts needed to warn about
that already existed separately: the gate classifies `Sign` / `Publish` /
`Identity` / capped `Spend` / first-contact `Send` as the effects it refuses to
wave through, and the journal's executed-key set records what was committed to
run. Neither reached the operator, because the key is opaque — it answers "has
this run?" and nothing else.

`EffectExecuted` therefore carries an optional `ExecutedEffect` alongside the
key: the effect kind, its amount, the board task it ran for, and whether the
gate called it irreversible. The classification is made **at execution time**,
by `ManifestApprovalGate::is_irreversible` (which delegates to the supervised
taxonomy, so there is one copy of the rules), and it is deliberately
mode-independent: a `full`-mode company executes a filing without ever parking
it, which is precisely when a retry dialog is the only warning anyone gets.

There is **no payload**. The record is read back onto an operator's screen
through `GET …/tasks/{task_id}`, which scrubs by construction, so recipients and
message bodies are never retained in the first place.

**The amount is admin-only** (issue #705). Unlike the payload, the amount *is*
retained — the whole point of the record is to say what a retry would repeat,
and "sent a payment" is a materially weaker warning than "sent a payment of
$2,400". So it is restricted at read time rather than dropped at write time,
through the *same* predicate that restricts an approval's amount for issue #618
(`approval_visibility::may_read_approval_contents`). A Member gets the row —
what a retry would re-do stays visible to whoever is doing the work — without
the number.

Two consequences worth stating, because both were a defect before:

* **The decision travels, not the role.** `ScopedCompany` deliberately drops the
  role at the edge, so it carries what the predicate *decided*. Nothing
  downstream can answer the question differently, because nothing downstream
  holds the input.
* **Hidden is not absent.** `amountUsd` is omitted from a Member's response and
  `amountHidden: true` is set in its place — but only where an amount was
  actually withheld. Without that flag a withheld payment and a free tool call
  are the same bytes, and a reader cannot tell "this cost nothing" from "you may
  not see what this cost". The flag is skipped when false, so an admin's
  response is byte-identical to before.

The redaction is applied inside `assemble_detail`, which the JSON route and the
export document (issue #352) both call, so the guarantee covers the exported
record too rather than depending on two callers remembering.

The task attribution comes from the cycle that ran the effect. Under
`supervised` an irreversible effect never executes in the cycle that emitted it
— it parks, and the operator's approval opens a fresh cycle carrying only
`ApprovalResolved` — so `ApprovalParked` also gains an optional `task_id`, and
the approved execution reads the card back off it. Without that, every effect
that went through the approval gate the way the policy intends would be
attributed to nothing.

**Committed, not completed.** The record is written *before* the effect is
performed — that ordering is the at-most-once guarantee — and a failed perform
leaves it standing. So an entry means "this was committed, and the runtime will
never re-attempt it", which is what the warning needs: the operator has to
assume it happened, because nothing will finish it and nothing will retry it. It
does not mean the effect is known to have completed, and the dialog's wording
says so rather than rendering the list as flat fact.

**Approved tool calls are described at redemption.** An approved effect carrying
an `agent` is settled by minting a single-use grant, not by
`execute_effect_once` — the tool then runs inside the agent's next turn — so it
writes no `EffectExecuted` line at all. `GrantConsumed` therefore carries the
same optional `ExecutedEffect`, built from the park record (retained past
resolution, payload scrubbed, superseded by an approve-with-edit) joined to the
gate's classification. It is attached at **redemption** rather than at minting,
because a grant that expires unredeemed is a call that never ran and must not
appear on a warning.

**A journal that cannot describe itself says so.** Both fields are additive: a
line written before #351 replays as a committed key with no description, which
keeps the at-most-once guarantee exact and simply contributes no warning. That
makes an empty list ambiguous on an upgraded company, so replay raises a
company-wide flag when it reads an undescribed executed key, surfaced as
`historyIncomplete` on `GET …/tasks/{task_id}`. With it set the console confirms
a retry regardless and says earlier activity cannot be described, instead of
presenting the gap as an all-clear. The flag is company-wide rather than
per-task by necessity — an undescribed record carries no card either. The
related pre-#351 case it cannot detect directly: an approval parked before the
upgrade has no `task_id`, and that record is byte-identical to a legitimately
card-less park written today, so flagging it would misreport every company that
has ever parked an approval from operator chat.

**Scope.** Task Detail only. The board's own re-dispatch — dragging a card back
into `in_progress` (`company/runtime.rs`, `upsert_task` → `dispatch_task`) — has
the same shape and now has this read available to it, and is deliberately left
for a follow-up rather than half-gated here.

## Chat turn brackets (issue #983)

An operator chat turn brackets itself the way a workflow run does.
`TurnStarted` is appended the instant the request is accepted — before the turn
takes the per-company serial lock, which is the whole point: the operator's
message is journaled at the same moment, so `chat/history` is right from
acceptance rather than from whenever the turn wins that lock. `TurnFailed`
closes the bracket when the turn errors, and the boot sweep
(`runtime::sweep_interrupted_turns`) writes one for any start left unmatched by
a dead host.

Both are structural. `TurnStarted` carries the turn id, the desk, the thread
parent and the asker; there is no message text on it, because the text is on the
`OperatorMessage` it brackets. `TurnFailed` carries a tenant-scoped reason that
is deliberately **not** projected onto the operator SSE stream.

The turn also mints a `RunRecord` (see [ports-runs.md](ports-runs.md)), and the
two are not redundant. The row answers *status* and is read by a poll; the event
is the *transcript*, and "a turn was accepted for this message" cannot be
inferred from the log without it — an `OperatorMessage` with no reply after it
is indistinguishable from a chatter message that legitimately produced none, so
silence is not evidence of a lost turn until the acceptance is explicit.

Retention (see [Retention](ports-state.md#retention-issue-275)) splits them
deliberately: `TurnStarted` is **prunable** — it is not evidence, nothing is
addressed by its sequence (the turn is joined by `turn_id`), and the only thing
that reads it back is a boot sweep that by construction runs before any
retention pass on that host. `TurnFailed` is **permanent**, being the only
record that a question was accepted and never answered.

Like every sweep of this shape, the boot sweep is **suppressed on a live runtime
rebuild** — and here the mis-fire is worse than for a workflow run, because
`rebuild_company` drains the cycle lock but a turn's spawned task journals its
replies and settles its row after the cycle returns. Sweeping mid-life would
tell the operator their turn failed moments before its answer arrived.

## Hive episodes and rounds

A desk of two or more answers as a room: one **episode** per operator message,
routed to seats with a `RoutingPlan` and run **concurrently** in **rounds** until
a seat calls `complete_episode` or the round cap, a timeout, a failed turn or a
membership change ends it. That contract grew past this file's 500-line cap:

- [hive-episodes.md](hive-episodes.md) — the episode and round frames, the
  `RoutingPlan` shapes, what a committed utterance carries, and which rows are
  permanent against prunable.

## Skill changes

`SkillChanged` records every write that changes a skill's document. The store
keeps one row per slug and rewrites it in place, so this journal is the whole
record that a playbook every agent reads was installed, replaced or removed.

| field | meaning |
| --- | --- |
| `slug` | the skill's id |
| `change` | `installed` \| `updated` \| `removed` |
| `tier` | `builtin` \| `company` \| `registry` \| `custom`, from the row's provenance at the moment of the change |
| `digest` | SHA-256 of the document written; absent on a removal, which writes none |
| `by` | who did it, when the surface carried an attributed actor |

Emitted by install (both arms), console authoring, each stored file of an upload,
and `POST …/skills/{slug}/update`
([api-skill-authoring.md](api-skill-authoring.md)). A **toggle emits nothing**:
there is no variant for it, a toggle writes no document, and recording one as
`updated` would tell a reader a re-pin happened that did not. The append
propagates its failure rather than being dropped — an unrecordable change
defeats the row's whole purpose.

**No document text rides the row**, the rule `WorkflowUpdated` and
`DeskRoutingConfigured` follow: a skill body is instructions an agent will read,
and this journal is shared by chat, audit and run history. `digest` pins *which*
document without reproducing it, and is the value the install recorded, so a row
and a pin can be matched. Projected as `skill_changed` with `slug`, `change` and
`tier`; `by` is dropped, the deny-by-default actor omission every attributed
frame uses, and `digest` with it, since a live console reacts by re-reading the
row. Permanent, and low-cardinality because an operator authors these by hand.

## Workflow run progress (issue #371)

A workflow run brackets itself on the journal with four variants —
`WorkflowRunStarted`, `WorkflowNodeStarted`, `WorkflowNodeFinished` and
`WorkflowRunFinished` — under a run-id correlation rule and an ordering
guarantee, with an interrupted-run boot sweep and operator stop/cancel
semantics. That contract grew past this file's 500-line Markdown cap and now
lives in its own focused file:

- [workflow-events.md](workflow-events.md) — the run brackets and why the
  journal carries them, run-id correlation, the ordering guarantee, the
  interrupted-run sweep (issue #371), the node-started bracket (issue #382), and
  stopping a run (issues #383/#398).
