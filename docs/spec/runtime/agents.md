# Agent definitions

*How a teammate is declared, and what reaches its system prompt.*

Terms: [glossary](../glossary.md). Tool scoping is
[tools.md](tools.md); which workspace documents a role is routed is
[orchestration/context-routing.md](orchestration/context-routing.md).

---

## Two authoring forms

A company's roster may be written either way:

**Inline** — `[[agent]]` blocks in `company.toml`. Unchanged, still valid, still
the smallest thing that works.

**Per file** — one `agents/<id>.toml` per teammate under the company bundle.

```
companies/acme/
├── company.toml          # everything except the roster
├── agents/
│   ├── copywriter.toml   # the id is the filename
│   ├── seo_specialist.toml
│   └── prompts/
│       └── house-style.md
└── workspace/
```

The per-file form exists because a teammate is more than four fields once it
carries a custom prompt and its own briefing documents. A multi-line TOML string
inside an array-of-tables is unreadable at roster length, and prose belongs
beside the agent it configures.

### The two forms are exclusive

A bundle with both an `agents/` directory and `[[agent]]` entries is a
**validation error**, not a precedence rule. Either precedence rule silently
discards teammates an operator wrote down, and the roster is the one part of a
manifest where a silent omission stays invisible until the missing teammate
fails to answer.

### The filename is the id

`agents/copywriter.toml` declares the agent `copywriter`. An `id` key inside the
file is accepted only when it agrees with the stem; a mismatch is an error
naming both, because silently preferring one leaves an operator renaming the
other and wondering why nothing changed.

Files are read **sorted by stem**. Roster order is load-bearing — a company that
tags nobody `tier = "orchestrator"` gets its first-listed teammate — and
readdir order varies by filesystem, so an unsorted read would make which agent
runs the company depend on which machine parsed the bundle. A company that
relied on declaration order under the inline form MUST state
`tier = "orchestrator"` when moving to the per-file form.

Only the immediate directory is read. `agents/prompts/` holds documents, not
teammates.

## Schema

Every key below is available in **both** forms — one type, one validator, one
consumer, so adopting a custom prompt does not require adopting the bundle
layout first.

```toml
# agents/copywriter.toml
role = "Copywriter"                     # required
description = "Write ads and campaign copy."
tier = "reasoning"                      # cognition hint; never selects a model
harness = "deep"                        # which [[harness]] runs this agent's
                                        # turns — see harnesses.md. Omitted
                                        # means the company's default harness.
tools = ["docs.*", "mcp:notion"]        # grant globs — see tools.md
delegates_to = ["creative"]             # desks this agent may hand work to
budget_usd_daily = 5.0                  # per-agent daily cap

prompt = """                            # appended to the generated persona
Write for the reader, not the client.
"""
prompt_files = ["prompts/house-style.md"]   # checked-in, bundle-relative
context = [                                 # live workspace documents
    "Brand/Brand voice.md",                 #   read only (the bare-string shorthand)
    { path = "Agents/copywriter/drafts", access = "write" },  # + workspace_write/workspace_create
]
classes = ["evidence"]                      # routing exclusions — see below

ledgers = [                                 # per-agent ledger access (omit for unrestricted)
    { name = "tasks", access = "record" },
    { name = "decisions", access = "read" },
]
can_declare_ledgers = false                 # may this agent `define_ledger`? default true
```

### `tier` versus `harness`

They answer different questions and are deliberately separate fields.

`tier` names a **workload** (`reasoning`, `vision`, …) and is resolved against
whatever provider the agent's harness turns out to use. `harness` names the
**engine and the credential**. So an agent keeps its tier when it moves between
harnesses, and two agents sharing a tier on different harnesses run on different
models — which is the point of naming more than one.

Naming a harness the company does not declare is a validation error, reported
against both the agent and the id. Naming none is not: every roster written
before `[[harness]]` existed binds nobody, and all of them keep working.
### `context` write access

A bare string in `context` is read-only — routed into the prompt, nothing
more. `{ path, access = "write" }` additionally puts that exact path in this
agent's `workspace_write`/`workspace_create` scope.

**Omitting every write entry is unconfined**, matching every manifest written
before this existed: `workspace_write`/`workspace_create` reach anywhere in the
company's tree, as they always could. Declaring **at least one** write entry
confines this agent's `workspace_write`/`workspace_create` to exactly the paths
it declared, plus its own `Agents/<id>/` home, which stays writable regardless
— a role narrowed to a real access list must not also lose the ability to
produce and revise its own work. See `src/harness/workspace_tools.rs` for the
enforcement and why the pre-existing unconfined default is otherwise
unchanged.

### `ledgers`

Which of the company's ledgers this agent's five ledger tools
(`list_ledgers`, `read_ledger`, `record_entry`, `close_entry`,
`define_ledger`) can see and use, and at what access.

An omitted `ledgers` key is **unrestricted** — every ledger, at `record`
access — the tool surface every agent had before this field existed. A
declared list restricts `list_ledgers`/`read_ledger` to exactly the slugs
named (an undeclared slug is invisible, not merely unwritable), and
`record_entry`/`close_entry` additionally require `access = "record"` on that
entry. A bare `{ name = "tasks" }` with no `access` key defaults to `read` —
the safer of the two.

This is the **visibility and read/record** half of ledger access; a ledger's
own `writers` list (`docs/spec/runtime/ledgers.md`) stays the authoritative
check for whether a write actually lands. Declaring `access = "record"` for a
built-in ledger whose `writers` excludes this agent is a manifest validation
error — the two must not silently disagree. A company-declared ledger is not
cross-checked at manifest-load time, since it may not exist yet; any
disagreement there is an ordinary tool refusal at call time.

`can_declare_ledgers` (default `true`) governs `define_ledger` alone — a
company discovers which axes it needs while running, so declaring one is
unrestricted by default; set it `false` to keep a narrow role from growing the
registry.

## The prompt

An agent's system prompt is assembled in this order, and the order is a decision:

1. the generated **persona** — who this teammate is, at which company;
2. its inline **`prompt`**;
3. its **`prompt_files`** bodies;
4. tool briefs (workspace, publishing, skills catalogue);
5. its routed **`context`** documents.

Static material first, volatile last. The prompt prefix is what a provider cache
reuses across turns, so a workspace note the operator edits between two turns
must not invalidate the briefing behind it.

Step 5 is resolved by the async caller before the (synchronous) agent build and
fingerprinted over document **bodies**, so editing a routed note reaches the next
turn rather than the next restart. See
[context-routing.md](orchestration/context-routing.md).

**A named teammate is told its name.** A manifest `[[agent]]` is addressed by its
role, and its persona reads *"You are the Content Writer at Acme."* An
operator-added teammate also has a display name — the one the console puts on the
DM header, the subtitle and the composer — and its persona names it too: *"You are
Alex, the Content Writer at Acme. … Teammates and the operator address you as
Alex; it is how you are called here, not a separate character to play."* The name
is an addressing handle, not an identity to build a character around, and it never
replaces the role. A name that is blank, or that only restates the role, falls
back to the role-only wording (issue #1105).

**`prompt` is appended, never substituted.** The generated line is what binds the
agent to *this* role at *this* company; a prompt that replaced it would silently
cost the agent its identity and hand it back the runtime's own assistant
persona. What belongs in `prompt` is how the role works, not who it is.

### `prompt_files` versus `context`

They are the static and dynamic halves of the same idea, and they differ on
exactly one rule that matters:

| | `prompt_files` | `context` |
| --- | --- | --- |
| Source | the company bundle, under `agents/` | the live workspace tree |
| Read | once, at manifest load | on every roster rebuild |
| Missing file | **validation error** | skipped |
| Position | early (cache-stable) | last (volatile) |

The missing-file split is deliberate. A `context` entry names operator-owned
live state that may legitimately not exist yet. A `prompt_files` entry names a
file in the same commit as the agent referencing it, so a typo there yields a
role whose prompt was written around a briefing it silently never received —
which fails confidently rather than visibly.

A `prompt_files` path may not escape `agents/`. The check is on path components,
before touching the filesystem, rather than by canonicalizing: canonical
comparison resolves symlinks, and whether a bundle is valid must not depend on
how the checkout was laid out on the reading machine.

### Budgets

Each document section is clamped to `PROMPT_FILE_BUDGET_CHARS` (10,000
codepoints, a tokenizer-free upper bound on the brief budget in
[alignment.md](orchestration/alignment.md)). The clamp keeps the **leading**
portion, cuts on a character boundary, and appends a visible marker.

The budget applies to the **section**, not per document: a role routed five
documents and a role routed one spend from the same prompt. Clamping happens at
assembly, where the text is spent — refusing the read would cost the company the
whole document, while clamping the tail costs only the tail.

An empty or whitespace-only document is dropped rather than rendered as a bare
heading. An empty section reads to the model as a source that exists and says
nothing, which is worse than its absence.

## What a turn is allowed to spend

Two ceilings bound one turn, and they bound different things.

### Tool iterations — 25

A turn may run **25** tool-calling rounds before the runtime pauses it and hands
the operator a resumable checkpoint instead of an answer.

The number is stated by this crate, not inherited. It used to be inherited: the
agent builder was never told a cap, so every teammate silently ran on the
vendored default of ten. Ten is a summariser's budget. A product manager asked
for a feature spec reads the standards, reads the release checklist, reads the
nearest prior spec, drafts, and publishes — and spends the ten before delivering
anything, which is the incident the raise comes from.

Twenty-five is ~2.5x headroom over that shape without the 5x of the runtime's
"extended" 50. **Cost grows faster than the multiplier**: each iteration re-sends
a transcript longer than the last one's, so 2.5x the rounds is more than 2.5x the
spend. That is why the ceiling is the smallest number that covers the observed
work rather than the largest one that would be safe.

It is a **global** default, deliberately: it reaches every shipped template
without editing each one. A teammate cannot raise or lower it from its manifest
today.

**Reaching it is a pause, not a failure.** The runtime stops the tool loop, asks
the model once more (with tools withheld) for a resumable "Done so far / Next
steps" checkpoint, and returns that as an ordinary successful reply. There is no
error to catch and no error to match on, which is precisely why a capped turn
used to be invisible — the operator read a tidy plan with no deliverable behind
it and no way to tell the agent had been cut off mid-task. So the harness reads
the runtime's cap flag while the turn's agent lock is still held and carries it
out on the turn's outcome, OR'd across every turn behind one operator bubble (the
responder, any desk lead it handed work to, and the relay turn that folds their
answers back together). When any of them paused, the operator gets a **second,
unauthored bubble** after the reply saying the turn stopped at its step limit,
that nothing errored, and that replying "continue" asks the agent to pick up
from there. It is a separate bubble rather than an addition to the reply because
the reply — and only the reply — is written back to the context store as memory;
appending would file the platform's notice as something the agent said and
recall it into later turns. See `src/harness/mod.rs`
(`TurnOutcome::hit_iteration_cap`), `src/runtime/delegation.rs` for the fold, and
`src/harness/brain.rs` for the notice.

### In-turn spend — armed only for a teammate with a declared daily budget

The company's other two spend controls — the plan-level token ceiling and a
teammate's `budget_usd_daily` — are both **pre-dispatch**. They decide whether a
turn may *start*; neither can see inside one. So a turn that begins one cent
under a cap can finish arbitrarily far over it, and raising the iteration ceiling
widens that window in proportion.

A running turn is therefore additionally metered by an in-turn brake — openhuman's
`BudgetStopHook` — an after-call threshold check installed between iterations.
It records each completed model call, then compares cumulative spend
(`TurnCost::total_usd()`) against the cap and pauses the turn before the next
provider call once spend is at or beyond it. The brake is installed **only** for
a teammate who declares a `budget_usd_daily` cap. Because the check runs after a
call has already been charged, a crossing call lands on the ledger before the
next one is prevented — the turn can finish at or slightly above the cap, so the
worst-case overshoot is bounded by a single model call rather than an entire
turn ("one call" rather than "one turn, of unknown size").

This mirrors the vendored runtime's own posture rather than inventing one.
OpenHuman constructs `BudgetStopHook` nowhere — it is an available primitive, not
an applied policy — and the only hook it installs is `GoalBudgetStopHook`, opt-in
and tied to a user-declared goal. Its own docs are explicit: *"we never
hard-stop a user-present turn that isn't actively burning a live budget."* So a
teammate with no declared budget gets no in-turn brake, and there is deliberately
no blanket per-turn dollar figure that no operator can see or change (it would
not be in `company.toml` and not in the console). Four shipped templates do set
`budget_usd_daily` — three agents in `signals_opportunity_studio` and one in
`e2e_harness` — so the opt-in path is genuinely exercised, not dead code.

Since a budget halt and an iteration-cap pause are different outcomes, the
runtime reports them separately: `TurnOutcome::hit_iteration_cap` is read from
[`Agent::last_turn_hit_cap`](oh::agent::Agent::last_turn_hit_cap), which stays
`false` for a hook-driven stop — the run paused below the 25-round ceiling, so
the cap predicate never held. A cap pause means the teammate ran out of rounds
with work still to do and can be resumed via the "continue" bubble above; a
budget halt means it ran out of money, returns whatever reply the model produced
before the hook fired, and gets no such bubble today. Anything that renders one
to an operator must not label it with the other.

## `classes`

The explicit epistemic classification
[context-routing.md](orchestration/context-routing.md) requires. Three values,
each subtracting one document:

| Class | Excludes | Prevents |
| --- | --- | --- |
| `evidence` | the assertion board | a role weighing evidence scoring an unevidenced sentence beside a real one |
| `judge` | the scratch | provisional working-out read as progress, which keeps a loop retrying |
| `directive` | the claim ledger | a role carrying out an instruction filing that instruction as a finding |

Declaring none is *unclassified*, which imposes no exclusion and is the right
default: an ordinary teammate is not judging anything.

An exclusion **outranks** both the tier default and an explicit `context` list.
That is what makes a declared class a control rather than a suggestion someone
can edit away. The universal method document is exempt — it is method, not
assertion, and a role excluded from it could not follow it.

The classification MUST be declared, never inferred from `role`: `role` is prose
an operator writes for humans, so matching on it would make a company that
renames "Critic" to "Reviewer" silently lose an exclusion, and a control a
rename can switch off is not a control.

## Where this lives

| Concern | File |
| --- | --- |
| Bundle loading, `prompt_files` resolution | `src/company/agent_file.rs` |
| Prompt composition and clamping | `src/company/prompt.rs` |
| Routing table and exclusions | `src/company/context_routing.rs` |
| Roster type and constants | `src/company/types.rs` |
| Manifest wiring and validation | `src/company/manifest.rs` |
| Iteration cap, stated on every built agent | `src/harness/build.rs` |
| In-turn spend brake, installed per turn | `src/harness/mod.rs` |

The first three are **always compiled**, though the harness that spends the
prompt is behind the `openhuman` feature. Composition, clamping and the
exclusion table are pure decisions with real edge cases, and the exclusions are
controls — they deserve tests in every build, not only where the agent runtime
links.
