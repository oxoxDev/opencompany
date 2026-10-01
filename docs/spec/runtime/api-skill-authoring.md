# Uploading a skill, drafting one, and updating an install

The three console routes that write a skill's document from something other than
the four-field form: an uploaded file, a conversation with a teammate, and the
shared library's current revision. All are part of the write plane in
[`api-write-plane.md`](api-write-plane.md); they live here so that file stays
under the repository's 500-line ceiling.

All three sit behind the same admin gate as every other skill write. A skill's
document joins **every** agent's effective prompt company-wide, so authoring one
decides something for the company rather than for the caller.

## `POST …/skills/upload` — several files, one outcome each

`multipart/form-data`. Parts named `file` are the skills; a `force` part
carrying `true` overrides a blocking scan verdict for this request only, the
same flag `POST …/skills/{slug}/install` takes. Every part is read before any is
stored, so a `force` that arrives after the files it applies to is still
honoured.

Accepted, by extension rather than by sniffing:

| Extension | Shape |
| --- | --- |
| `.md` | the `SKILL.md` itself; its frontmatter must carry `name` and `description` |
| `.zip`, `.skill` | an archive holding one `SKILL.md`, at the root or inside a single top directory |

The answer is `{results: [{file, ok, skill?, error?, scanBlocked}]}`, one row
per file in the order they were sent, and the status is `200` whenever the
request itself was well-formed. A refusal is **per file**: an operator who
drops five files and mistypes one gets four stored skills and one row saying
what was wrong with the fifth, rather than a status code that cannot say which
file it meant. The request as a whole fails only for something true of all of
it — a body over the 8 MiB limit (`413`), more than 16 files, or no `file`
part at all.

`scanBlocked` is `true` only when a blocking scan verdict refused the file —
the one refusal a `force` resend overrides.

A stored row's `skill` is the same `InstalledSkill` the create and install
routes return, carrying the `scan` report of the write that stored it.

### The slug an upload lands under

The Agent Skills spec says a skill's directory names it, so an archive with a
top directory is stored under that directory — validated as a slug, and refused
when it is not one. A bare `.md` has no directory, so it is slugged from its own
frontmatter `name`, exactly as console authoring slugs the name typed into the
form.

### Archive handling

An archive is a list of paths and byte counts supplied by whoever built it. The
shape is judged from the archive's directory **before** anything is
decompressed, so a bomb is refused by arithmetic rather than by running out of
memory:

- an entry-count ceiling (64);
- the sum of the declared uncompressed sizes against 1 MiB;
- absolute paths, `..` traversal at any depth, and backslash-separated paths;
- symbolic links — how an archive reaches a path it never names;
- an archive nested inside the archive.

The one entry that is read is read through a bounded reader as well, behind
whatever the archive reader itself does with a header that disagrees with its
entry.

### Bundled resource files are refused, not dropped

`SkillState.custom_doc` is a single document
(`ports/skills_state.rs`), so there is nowhere to keep a script or a reference
file an archive carries. An archive with extras is therefore **refused, naming
the files**. Keeping the `SKILL.md` and silently discarding the rest would hand
the operator a skill whose procedure references files no agent will ever find —
and nothing on screen would say so.

This is the deferral the design brief recommends for the first slice. When
bundled resources get somewhere to live, `SkillState` is extended; a parallel
store is not added.

### Nothing is persisted before both gates run

The reader decides what the file is, the write plane's own 256 KiB ceiling
bounds the assembled document, and the shared validator
(`company::skill_validate`) and content scan (`company::skill_scan`) run last. A
`block` verdict returns the report and writes nothing. There is deliberately no
setting that silences a class of finding for a whole host — the override is a
per-request flag on the one upload.

## `POST …/skills/draft` — one copilot turn, writing nothing

The same contract as the teammate copilot's draft routes
([`api-team-drafting.md`](api-team-drafting.md)): the body carries `messages`,
the conversation so far, oldest first, each `{role: "operator" | "copilot",
text}`; empty means the opening turn. The console owns the transcript and the
host stores nothing — no journal, no thread id, nothing to clean up when the
dialog closes. It is bounded host-side all the same (the last 16 turns, 2,000
characters each), because a transcript the caller composes is one the caller can
grow without limit.

The answer is `{reply?, text?, source, reason?, scan?}`. `reply` is what the
copilot says, absent when no model could answer at all. `text` is the **whole**
`SKILL.md` as it now stands, never a diff, and is absent on a turn that asked a
question instead of drafting. `source` is `model` or `unavailable`, and
`reason` names which of the refusals it was, so the console can say "wire up a
model" rather than showing an empty box.

**This route never writes.** It composes a prompt and returns text. The draft
becomes a skill only if the operator takes it and saves it through `POST
…/skills/upload`, which runs the validator and the scan like any other write.

### Availability

Drafting needs a model, so it inherits the rule the teammate copilot already
obeys: `runtime.profile_drafter()` is built from the embedded harness deps and
is absent on a `sidecar` or `custom` cognition path. `GET …/inference` reports
`designsProfiles` — the same `profile_drafter().is_some()` — and the console
hides the draft control when it is `false`. Do not render a control that can
only answer `no_model`.

### The draft is scanned before it is shown

The drafted document goes through the same validator and scan the save path
runs. Either can withhold it, and `reason` says which did, because the two ask
the operator for different things:

| What happened | `reason` | What the console tells the operator |
|---|---|---|
| The scan blocked the document | `refused_by_scan` | Say it differently and try again — the findings are in `reply` |
| The document did not validate | `unreadable` | Say more about what the skill is for, or write it by hand |

In both cases `text` and `scan` are withheld, `source` is `unavailable`, and the
findings that would have gone in `scan` are folded into `reply` instead.
Reporting a validation failure as a scan refusal told the operator to reword a
draft the scan had never objected to.

The assistant must not be able to hand the operator a document that the Save
button would then refuse, and a model writing a skill is untrusted text
reaching a prompt like any other.

### Prompting

The system brief carries the spec's own authoring guidance: a description states
what the skill does **and** when an agent should use it, because that line is
what every agent reads when deciding whether to open the skill at all; the body
stays short. The 1024-character description limit
(`company::skill_validate::MAX_DESCRIPTION_CHARS`) is stated to the model, and
the console shows a live count against the same number.

## `POST …/skills/{slug}/update` — take the library's current revision

An install pins a snapshot: the library's `SKILL.md` is persisted verbatim and a
later library edit never rewrites it. That is deliberate — a document every agent
reads must not change under a company because a publisher republished it — so
moving onto a newer revision takes an explicit act. This route is that act.

Body: `{force?: boolean}`, or nothing at all. `force` overrides a blocking scan
verdict for this request only, the same flag install and upload carry.

The answer is the re-pinned `InstalledSkill`, carrying both the `scan` report of
the write and its **post-update** drift, so a console folding it into the list
clears the badge the update just fixed.

### The four refusals

Each is a `409` with its own sentence (`server::ops::language`), because they ask
the operator for four different things and a shared "can't update this" would say
none of them:

| Situation | What the sentence says |
| --- | --- |
| No row for the slug, or a row that pinned nothing (authored here, or bundled) | there is no registry copy to move to |
| The slug has left the library | nothing newer exists; the install keeps working from the copy it holds |
| The stored copy was edited after it was pinned | updating would replace those changes, so it is left to a person — uninstall and install again to take the registry's version |
| The install already matches the library | there is nothing to update |

The yes/no is `SkillDrift::update_allowed()` — something newer exists **and** the
stored copy still matches its pin — the same predicate both reads project as
`updateAvailable`/`modified` and the console mirrors in `canUpdateSkill`. One
rule, three surfaces, no second copy to drift.

The modified refusal is the one the recorded digest exists for, and
`PUT …/skills/{slug}/doc` (below) is what makes it reachable: once an operator
has rewritten an install's document, that text exists only in this company's own
row. A host that silently chose the library's version would destroy it with no
record that it had.

### An update is a write, held to a write's gates

The library document is re-rendered, size-checked against the same 256 KiB
ceiling, and **re-scanned** before it is stored. A library entry that has since
grown something the content scan refuses must not reach a company through an
update when an install of the same slug would have been refused.

`enabled` is **carried from the existing row**: taking a newer document says
nothing about wanting a skill the operator switched off switched back on.

The write is journaled as `SkillChanged { change: updated }`
([`events.md`](events.md)) — the store rewrites one row per slug in place, so the
journal is the only record that the document an agent reads was replaced.

### Nothing is snoozed

There is no "remind me later" flag, here or on the row. A console's Keep writes
nothing, so the badge returns on the next read. That is correct: the library has
genuinely moved on, and an install that stays behind is a fact about the company
rather than a notification to be dismissed.

## `GET`/`PUT …/skills/{slug}/doc` — the document itself

Every other read on this surface is metadata: a name, a description, a category,
a version. The `SKILL.md` — the procedure an agent actually follows — had no
address, so the console could only ever offer a greyed-out Edit; an editor that
loaded metadata alone would save it back over a body it never read.

**The read is a member's read.** A skill's content joins every agent's prompt
company-wide, so any member may see what their teammates are told to do. It is
resolved through `company::skill_effective::resolve`, the same derivation
`GET …/skills` reports from, so the editor opens on the layer actually in effect:
a console-authored or installed document comes from the row, a global baseline
one from its rendered text, and a company bundle's off disk.

Answer: `{slug, markdown, editable}`. `markdown` is the whole document,
frontmatter included. `editable` is the host's own statement about whether a
write would be accepted, so the console does not re-derive that rule from
`source` and then offer an editor the host refuses. `404` when the slug is not in
the company's effective set, and when the row it has supplies no document — one
sentence for both, because the operator's next move is the same either way.

**The write is admin-only and provenance-limited** to `Registry` and `Custom`,
the same two arms `uninstall` accepts. A `Company` skill is authored in the
repository and its document travels with sibling resource files a stored delta
cannot carry, so storing an inline body over it would silently drop them; that is
a `409` naming the reason.

Body: `{markdown, force?}` — the whole document, not a patch, because the stored
copy is what every agent reads and a partial write would leave the authoritative
text between two versions. Held to every gate a write path shares: the 256 KiB
ceiling, the shared validator, the content scan, under the same per-company write
lock, so a document an install of the same text would have been refused cannot
arrive through the editor instead. `enabled` is carried from the existing row —
an edit says nothing about switching a disabled skill back on.

### Editing an install is the local-copy flow

The row's `install` pin is carried through untouched. The pin still names what
the library shipped, the stored document no longer digests to it, so the row
reports `modified` and `update` then refuses rather than discarding the edit.
That is the whole mechanism — there is no separate fork or detach step.

One consequence worth stating: `skill_effective::registry_heal` replaces a
degenerate stored snapshot with the library's live document, and an operator's
edit could in principle be degenerate (a body equal to its own description). The
heal is therefore also skipped for any row that no longer matches its pin, which
is exactly the rows an edit produces.

The write is journaled as `SkillChanged { change: edited }`
([`events.md`](events.md)) — a separate word from `updated`, which takes the
library's text rather than replacing it with the operator's.
