# Uploading a skill, and drafting one

The console routes that create a skill from something other than the four-field
form: an uploaded file, and a conversation with a teammate. Both are part of the
write plane in [`api-write-plane.md`](api-write-plane.md); they live here so
that file stays under the repository's 500-line ceiling.

Both sit behind the same admin gate as every other skill write. A skill's
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

The answer is `{results: [{file, ok, skill?, error?}]}`, one row per file in the
order they were sent, and the status is `200` whenever the request itself was
well-formed. A refusal is **per file**: an operator who drops five files and
mistypes one gets four stored skills and one row saying what was wrong with the
fifth, rather than a status code that cannot say which file it meant. The
request as a whole fails only for something true of all of it — a body over the
8 MiB limit (`413`), more than 16 files, or no `file` part at all.

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
