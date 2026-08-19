# First-run setup: what the host enforces

The product decisions behind first-run setup live in
[company-setup.md](company-setup.md). This file holds the four things the host
*enforces* rather than requests, and why each is a boundary instead of a line in
a prompt.

A prompt is advice. Every rule here was a prompt instruction first, and each one
was observed to fail: coverage went unchecked while the prompt asked for it, the
reference team came back verbatim while the prompt called it a quality bar, and
an unrecognised tool focus quietly widened a teammate's authority instead of
narrowing it. What follows is the enforcement that replaced the hoping.

## The claim is checked, not trusted

**Decision D6: the host splits the jobs, the model claims which it owns, and the
host checks the claim.** "Every job they mention should have an obvious owner"
was in the prompt from the start, and nothing verified it — a prompt is advice.

So `job_items` (in `src/company/setup.rs`) splits the automation answer on the
separators a person actually types, numbers the items, and sends them as a
checklist. Each returned agent lists the numbers it owns in `covers`. After
validation the host computes `uncovered` by set maths over **its own** list.

The order matters: if the model both listed the jobs and reported covering them
it would be marking its own homework, and the two halves would agree by
construction. Coverage is only a check because something other than the answer
decides what was asked for.

A gap buys exactly **one** re-ask, which marks the unowned items in place and
keeps the first ask's numbering — renumbering the gaps from zero makes the second
answer's `covers` refer to a different list, and the two silently disagree. One,
because a second is a conversation and somebody is watching a build-out screen:
if naming the missing jobs outright did not produce an owner, a third phrasing
will not either. What survives is reported to the operator on the review screen
rather than hidden — an honest gap they can act on beats a roster that quietly
ignored a third of what they asked for.

A roster that is **entirely** the reference team is reported as curated, not
designed, whatever produced it. The reference roster goes into the prompt as a
quality bar, and a model that reads it as a menu can hand the whole thing back —
an answer whose shape is perfectly valid, so validation admits it, and the
operator is then told "built from what you told us" about a roster nobody
designed. The guard is on the line-up, not the prose: one role of the model's own
is a decision, and a team that borrows a sentence is still designed. The prompt
asks for the operator's own words; the host only refuses to call a copy an
original.

Coverage is a claim only the design pass makes. A curated fallback was chosen by
keyword and never read the list, so it reports its provenance instead and claims
nothing.

## A teammate asks for its tools

**Decision D7: a designed agent names the belt it needs, and the model picks a
job shape rather than a tool.**

`manifest_from_setup` builds from a name-only manifest, so `[tools]` took the
globals default `["*", "media", "composio"]` — and an agent whose `tools` list is
empty inherits the company belt whole. Every teammate a first-run operator
created therefore held shell, code, web, subagent, files, docs, **media** (real
money) and **composio** (per-tenant credentials), for a company described in
three sentences. The globals teammates sitting next to them already do the
opposite, and `globals/agents/researcher.toml` says why: a request is intersected
with `[tools].allow`, so naming one can only ever narrow.

Each proposed agent now carries an `AgentFocus` — `research`, `writing`,
`operations` or `analysis` — and the host maps it to a belt. The model never
names a tool. Tool grants are a permission boundary, and letting free text a
stranger typed reach `[tools]` would put that boundary inside the prompt's blast
radius; a closed enum means the worst a hostile answer achieves is the wrong belt
from a list of four the host wrote. `media`, `composio`, `search`, `repo` and
`shell` are unreachable from every focus, and a test quantifies over the whole
vocabulary so a focus added later cannot quietly widen it.

An unrecognised focus gets the narrowest working belt, never an empty list. It
used to inherit, on the reasoning that an unknown value should degrade to the
pre-focus behaviour — and that inverted the control, because an empty `tools` list
means *inherit the company belt*. An **invalid** focus therefore produced a wider
agent than any valid one, and the operator's free text reaches a model that writes
that string. Fail closed: the failure mode is a teammate that cannot browse, not
one holding a spend authority.

The curated templates declare a focus too. An operator with no credential must
not end up with the *wider* company.

## A fallback says which fallback

Three different situations produce the curated team, and the review screen said
"we couldn't reach a model to tailor it" for all of them. That is false in the two
where a model answered and its answer was unusable, and it matters because the
operator's next move differs: *add a key* versus *tell us more about the
business*. `FallbackReason` carries `no_model` or `not_designable` to the console,
which says the right sentence.
