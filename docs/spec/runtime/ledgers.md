# Dynamic ledgers

A company keeps more than a task board. It keeps goals, decisions, risks,
customer promises, experiments run, invoices chased — and every one of those is
the same shape: **a set of rows with an id, a status, some prose, and a reason
each closed one closed.**

Before this, exactly one of those shapes was first-class. `TaskStore` had six
hard-coded columns, and every other axis a company needed was written into a
note, a chat message, or a workspace file nobody designed. The company's own
record of what it had decided was prose scattered across three surfaces, and
nothing could search it, bound it, or stop a teammate re-deciding it next week.

So the shape is **declared** rather than compiled. A `LedgerSpec` names its
fields, its statuses, which of them close a row, and how the rendered file is
laid out; one engine folds an append-only log into rows and renders a built-in
and a company-declared ledger identically. The runtime ships three
(`tasks`, `goals`, `decisions`) and a company writes the rest, up to
`MAX_DECLARED` (12).

## The four rules

### Append-only, and every write is a merge

There is one write operation. Opening a row, amending it, blocking it, closing
it and re-prioritising it are all *record an event against this id*; the fold
applies events in order and leaves absent keys alone. Closing a goal is
`{status: "met", reason: "shipped in March"}` and nothing else.

That is not simplification for its own sake. A vocabulary of operations means a
vocabulary of **inverses** to get wrong — what `unblock` does to a row that was
never blocked, whether `reopen` restores the old status or the default — and
every one of those is a decision the fold would have to make silently. A merge
has no inverse to get wrong, and the log keeps the history either way.

It also fixes what a rewritten-whole board loses. Storing *current state* loses
**why** (a card that moved to Done records the landing, not the verdict) and
loses **what was there before** (a rewrite that drops a row is byte-identical to
a rewrite that never had it, so a bug that eats work looks exactly like work
nobody did).

A JSON `null` clears a field — the one thing a merge cannot otherwise express.

### Agents create and record; only people delete

`AuthorKind::may_delete` is that rule, in one place. It is not a permission
setting and it is not configurable.

An agent's whole relationship with a ledger is **additive**: it opens rows,
amends them, and closes them with a reason, and every one of those is
recoverable by reading the log. Deletion is not, and a runtime where a turn can
erase the record of what it did is one whose record means nothing. Being
finished with a row is `close_entry`, which keeps the reason — and the reason is
the entire value of a closed row to whoever reads it next.

`AuthorKind::System` is deliberately **not** exempt: a sweep that could delete
rows is the same loss with nobody to ask about it.

Concretely:

| | agent | person | platform credential |
| --- | --- | --- | --- |
| read, record, close | yes | yes | yes |
| declare a ledger | yes | yes | yes |
| delete a row | no | **yes** | no |
| retire a ledger | no | **yes** | no |

The asymmetry on *declare* is the point. A company discovers which axes it needs
while it is running, so a declaration that required an operator would be
discovered and then not made. What an agent cannot do is undo one.

Enforcement lives in `company::ledgers` and nowhere else. The REST routes turn
`ScopedCompany::actor` into a `LedgerAuthor` and the agent tools stamp the
teammate — both then call the same service. A route that decided for itself
would be a second answer to the question, and the tools would need a third.

### Everything renders into `derived/`, and nothing hand-writes it

Every ledger renders one Markdown file into `derived/<NAME>.md` in the company's
shared workspace, rewritten on every write to that ledger. That is what makes a
ledger legible to everything that already reads the workspace — an agent's file
tools, the console's workspace view, a search, an export — without any of them
learning a new API.

The rule is not *these particular files are generated*; it is **nothing in this
folder is hand-written**. A per-file rule fails open: a ledger declared next week
renders a file no guard has heard of, somebody edits it, the edit is silently
erased by the next derivation, and they have no way to know. The folder rule
fails closed.

`DerivedGuardWorkspace` is that guard — a `WorkspaceStore` decorator wrapped at
the single place the store is chosen, so every writer obeys without knowing it
does (the same argument `QuotaEnforcedWorkspace` and `WorkspaceAnnouncer` make).
It refuses `write`, `create`, `adopt_or_create_folder`, `rename_move` (in **and**
out), and the binary writes. `WorkspaceOrigin::Seed` passes, which is what the
runtime's own derivation stamps.

**Deleting is deliberately allowed.** A delete is not the failure this exists to
prevent: nothing is silently lost, the next write re-derives the file, and a
retired ledger has to leave something somebody can clear.

The refusal is **per ledger**, not generic, because *what to do instead* differs:
an events ledger takes `record_entry` and the task board does not. Telling the
board's caller otherwise sends them to a tool that refuses them a second time —
a refusal naming the wrong remedy is barely better than one naming none.

### Bounds are code, not intent

A ledger's rendered file is read by people in the console **and** routed into
agent turns, so its size is a bill paid on every read. Every section is clamped
against `budget::MAX_LISTED` (40 rows) and every prose field against
`REASON_CHARS` (600) **on the way in**, so a declaration cannot grow its own file
past what a reader is asked to hold. Clamped rather than refused, so a ledger
stored when the bound was looser keeps rendering after the bound tightens.

Either bound alone is the same file by another route: forty rows of
five-kilobyte prose is not bounded. The ceiling test asserts both, and asserts
the property a ceiling alone cannot catch — *past the bound, more rows must not
mean more file*.

A section cut to its bound while reading as complete is worse than a long one,
because the reader concludes there is nothing more and re-proposes what was cut.
So every truncation says how many it dropped and names the call that fetches
them.

## The task board is the `tasks` ledger

`tasks` is `LedgerSource::Native`: its rows stay in `TaskStore` and its columns
keep firing dispatch, planning passes and run settles. None of that is
expressible as a declaration and none of it should be — a declared status cannot
open an attempt, and a company that could redefine `in_progress` into something
that does not dispatch has broken its own runtime from a JSON file.

It is registered so `list_ledgers` names **every** ledger and `read_ledger`
reads every ledger. A discovery surface that covers the ledgers a company
invented but not the one it already had is a surface an agent stops trusting,
and then stops using.

### One column table, four consumers

The board's columns lived in three places that could not check each other: a
`[&str; 6]` on the port, a `match` from id to label beside it, and a
hand-maintained `TASK_COLUMNS` in the console — whose own comment admitted the
cost: *"a Rust test cannot see the TS list, so a column added on one side and
not the other keeps this green."* A column present on one side alone either
never rendered (its cards silently vanished) or was refused by the write
boundary.

`ledger::board::COLUMNS` is now the one declaration. Each row carries the id,
the label a person reads, whether it closes a card, and the section it renders
under. Everything else derives:

| consumer | what it takes |
| --- | --- |
| `ports::tasks::BOARD_COLUMNS` | the ids, via a `const fn` — still a genuine `const` |
| `ports::tasks::column_label` | the labels |
| the `tasks` ledger declaration | one status per column, one section per heading |
| the console | the ledger's `statuses`, labels included, over the wire |

Adding a column is one edit. The labels are pinned in Rust by
`the_labels_are_the_ones_every_surface_renders` — an assertion that was
impossible to write while the console kept a copy.

**The ids stay leaf constants.** `COLUMN_IN_PROGRESS` and its siblings remain
plain `&str` consts on the port, and the table refers to them: entering
`in_progress` *dispatches the card* and the edge keys off that exact literal, so
a column's identity has to be something a `match` arm can name. Only its
presentation and its grouping moved.

**And the table is not itself declarable.** A company may declare any ledger it
likes, but not this one: a column here is a lifecycle state that spends money.
`planning` fires a model call, `in_progress` opens an attempt, and `done` is
reachable only through a human verdict. A seventh column from a JSON file would
be a state the runtime has no edge for, and a card that entered it would sit
there forever with nothing to say why.

`done` is the only closed column: a card in review or paused is *stopped*, not
finished, and calling either closed would make "what is still outstanding"
answer wrong.

## What a declaration cannot do

- **It cannot reason.** `Check` is a closed set — a required field, an unknown
  status, a close with no reason. There is no expression language: a company
  that could write predicates into a ledger declaration has written a rules
  engine nobody can review.
- **It cannot raise a bound** (see above).
- **It cannot shadow a built-in**, so `tasks` is always the board and every
  prompt and route naming it stays right.
- **It cannot claim another ledger's derived path.** Two writers on one file is
  how each one's work disappears.
- **It cannot be `native`**, which is for ledgers the runtime renders in Rust.

A declaration that breaks one of these is skipped **with a fault** rather than
failing the registry: a company that wrote one bad ledger must still reach its
board. The faults ride the listing so the company can see why something stopped
appearing.

## The agent surface: five tools, however many ledgers

`list_ledgers`, `read_ledger`, `record_entry`, `close_entry`, `define_ledger`.

The count does not grow when a company adds an axis, and that is forced rather
than chosen: the tool schema is built once when the agent is constructed, so a
ledger declared mid-run can get no tool of its own and can appear in no `enum` in
anybody's schema. `ledger` is therefore a plain **string** checked against the
registry at call time, and an unknown slug comes back with the real ones — the
discovery path a model actually follows, in one turn, without having thought to
list them first.

Reads are `ReadOnly`; the three writes are `Write`, so a supervised policy parks
them like any other consequence. There is no delete tool and no retire tool.

The prompt carries a **catalogue** — every ledger named with its purpose — not a
sentence saying `list_ledgers` exists. A tool granted, unmentioned and never
called is the observed failure mode, not a hypothetical one. The catalogue is
built from the registry resolved at agent-build time, so a ledger declared
mid-run is reachable by every tool immediately and named in the *prompt* from the
next build. That is the honest limit: system prompts are assembled once, and
nothing can retroactively edit one already in flight.

### Per-agent visibility and access: `[[agent]].ledgers`

The five tools above are the whole company's surface, uniform across every
agent. `[[agent]].ledgers` narrows that per teammate: a list of
`{ name, access = "read" | "record" }` grants, omitted by default.

An omitted `ledgers` key is **unrestricted** — every ledger, at `record` — the
tool surface every agent had before this field existed, so adding it to a
manifest is a no-op for a company that never opts in. A declared list confines
`list_ledgers`/`read_ledger` to exactly the slugs it names (an undeclared slug
is invisible, not merely unwritable) and requires `access = "record"` for
`record_entry`/`close_entry`; a bare `{ name = "tasks" }` defaults to `read`,
the safer of the two.

This is visibility and read/record — a different axis from
[`writers`](#agents-create-and-record-only-people-delete), which stays the
authoritative check on whether a write actually lands. An `access = "record"`
grant to a built-in ledger whose `writers` excludes that agent is a manifest
validation error, not a disagreement discovered at call time; a
company-declared ledger cannot be checked that early, since it may not exist
yet, so any conflict there is an ordinary tool refusal. `can_declare_ledgers`
(default `true`) gates `define_ledger` alone. See
`docs/spec/runtime/agents.md`.

## Storage

`LedgerStore` (`ports/ledgers.rs`) keeps two things with very different
lifetimes: **declarations** (small, rewritten in place, at most 12) and
**events** (append-only, unbounded). Built-ins are never stored — they ship with
the runtime, and persisting a copy would let a company's stored version drift
from the code every prompt and route is written against.

The store's only ordering obligation is that `events` returns what `append`
appended, in that order. Everything the fold promises rests on that and on
nothing else — in particular not on an event's timestamp, which is written by
whichever replica is running.

All three backends implement it:

| backend | declarations | events |
| --- | --- | --- |
| fs | `ledgers.json` | `ledgers/<slug>.jsonl`, one `write_all` per line under `O_APPEND` |
| sqlite | `ledger_specs` | `ledger_events`, ordered by `AUTOINCREMENT seq` |
| mongodb | `ledger_specs` | `ledger_events`, ordered by a per-company counter |

One event file per ledger rather than one shared log: the fold reads a single
ledger at a time, and a shared file would make every read of the goals scan every
task event ever written.

`delete_spec` leaves the events alone. A ledger nobody reads is worth retiring;
the work recorded in it is not, and deleting a log to tidy a registry is exactly
the loss the append-only shape exists to prevent. Deleting the rows too is
`purge_ledger` — a separate, deliberate act, and a person's.

## The console

The Ledgers section renders from the ledger's own `fields`, `statuses` and
`sections`, never from anything hard-coded. A ledger a teammate declared this
morning renders correctly this afternoon with no console release; a screen that
hard-coded the goals columns would have made "declare your own axis" a promise
the UI quietly broke.

Two things it shows rather than hides. The delete control exists here and nowhere
an agent can reach, with **Close** offered first as the ordinary way to be
finished with a row. And a native ledger renders its `writtenBy` sentence in
place of a compose box, rather than offering a form whose save the host refuses.

The compose form reads `needsReason` off the declaration and asks for the reason
*before* the save — the same rule the host enforces, met earlier.

### One board component, one screen

Any ledger with statuses renders as columns — one per status, in declaration
order, labelled by the host. That falls out rather than being designed: a status
list *is* a lifecycle, and the only thing that differs between the board and a
hiring pipeline is which call a drop makes. A native ledger's drop goes through
`patchTask`, because entering a column fires work; every other ledger's is an
ordinary `record_entry` merge. A drop into a status that demands a reason opens
the compose form instead of writing, since the host refuses a silent close.

`views/LedgerBoard.tsx` is that board, and it is the **only** one. It owns the
columns, the counts and the drag mechanics; the card is a `renderCard` slot.

There was a standalone Tasks page beside Ledgers until issue #1140, rendering
the same records through this same component, and an operator who met their work
twice had to learn which of the two was the real one. Now one screen uses the
board, and it chooses its card by what is behind the row:

| rows | card |
| --- | --- |
| the `tasks` ledger's rows, joined to `Task` records from `…/tasks` | `views/TaskCard.tsx` — priority, assignee, cost, plan badge, output link, and a paused card's blocker and Resume |
| any other ledger's `LedgerEntry` rows | title, owner, id |

**The card is a slot, not a role-driven renderer.** That was tried and is wrong:
a task card carries a priority, a plan badge, an output link and a Resume
button, none of which is a ledger field and all of which come off the `Task`
record rather than the ledger's projection of it. A generic renderer would have
had to drop them or grow a special case per ledger, and the second is the slot
with more steps.

Which column a row is in is a `statusOf` accessor rather than a required
property, because the two sides spell it differently — a `Task` says `column`,
a `LedgerEntry` says `status` — and a board renaming its callers' fields is a
board deciding what their data is called.

The history here is worth keeping, because it is the argument for the shared
component. The board screen was first deleted and re-implemented inline in the
Ledgers section, and that re-implementation silently lost all three of issue
#334's fixes: the edge auto-scroll, the miss message, and the trailing gutter.
Nothing failed — there was no test on the new path — until the ported drag spec
was actually run. The board is one component now so that cannot happen again,
and #1140's deletion is the proof: retiring the screen a second time moved a
`renderCard` and a route, and touched none of the gesture.

#### An empty column collapses to a rail

Six columns are wider than an ordinary window, and the three that fit are the
three a working company empties first. So an operator opened the board, read
three confident zeros, and never learned that the hundred cards they came for
were two columns off the right edge (issue #1101): the board that had moved the
most work looked the most finished.

A column holding nothing therefore renders as a ~40px rail — label set
vertically, count beneath it — and the populated columns take the room back.
Three rules keep that from breaking the gesture the board exists for, since the
empty columns are precisely the ones a card is dragged *into*:

* **A rail is a whole drop target.** The drop handlers sit on the column
  wrapper, which is the element that shrinks.
* **A rail opens under a drag.** The `over` state that draws the drop highlight
  also suppresses the collapse, and the empty placeholder becomes a dashed
  "Drop it here" while a card is in hand.
* **Nothing collapses unless another column is populated.** Six rails and no
  board would answer "show me the work" worse than the zeros this fixes.

Clicking a rail pins it open — a real button, with the column and its count in
its accessible name — and a pinned column stays open until the operator folds it
back. A column that re-collapsed itself while somebody was reading it would be
this bug's mirror image. A column whose `columnHeader` slot holds a control
never collapses either: a rail has nowhere to put it, so folding one would hide
an affordance rather than some whitespace.

### What the ledger drives, and what it does not

**Drives**: the columns, their order, their labels, which one closes a card, and
therefore what "outstanding" counts. The console declares no column.

**Does not drive**: what is *on* a card. A task is a `Task` — a priority, an
assignee, a plan brief, a published output, a deliverable kind — and the
ledger's native projection deliberately does not carry any of it
(`src/ledger/native.rs`). So `LedgersView` reads `…/tasks` alongside the ledger
and joins the two by id: the ledger supplies the shape of the board and which
column a card is in, and the task store supplies what is on it. A row whose
record has not arrived yet renders as an ordinary ledger row rather than
waiting, so a card never flickers in and out of its column.

Creation keeps its own dialog (`views/CreateTaskDialog.tsx`) rather than becoming
a ledger compose form: the board's write path is `POST …/tasks`, and
`record_entry` is refused for this ledger precisely because entering a column
fires work. It is offered on the board and nowhere else, and it lands a card in
To-do — the one column new work may enter.

`#/tasks/<id>` remains the card detail — a timeline, a plan brief, a discussion,
its attempts, a workflow proposal, the steer controls — none of which has
anywhere to live on a column, and none of which Ledgers tries to reproduce. It
is the one address that outlived its page: `views/TaskDetailRoute.tsx` mounts it,
`tasks` stays in the shell's `HIDDEN_VIEWS` so the router still knows the head,
and `#/tasks` with no card is rewritten to `#/ledgers/tasks` rather than
resolving to Overview.

The board is covered end to end by two specs at that one address:
`board-columns.spec.ts` drives its shape (columns, order, intake, and the two
addresses #1140 had to keep), and `board-drag.spec.ts` drives the gesture (the
three #334 fixes).
