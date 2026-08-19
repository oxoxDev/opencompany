# Typography

**Geist Variable** for everything the operator reads. A mono face for values
that change in place. Three weights. One scale.

---

## The scale

Tailwind's own steps are untouched — 460 call sites already depend on them.
What the system *adds* is the two rungs below `xs` that this console genuinely
needs and had been spelling as arbitrary values.

| Class | Size | Line height | Tracking | Use |
| --- | --- | --- | --- | --- |
| `text-3xs` | 10px | 14px | +0.01em | Table meta, graph node labels, badge counters |
| `text-2xs` | 11px | 16px | +0.005em | Captions, timestamps, key/value rows, sidebar section headers |
| `text-xs` | 12px | 16px | — | Dense body — the console's workhorse |
| `text-sm` | 14px | 20px | — | Default body, form labels, buttons |
| `text-base` | 16px | 24px | — | Long-form prose, empty-state copy |
| `text-lg` | 18px | 28px | — | Card titles |
| `text-xl` | 20px | 28px | — | Section headings |
| `text-2xl` | 24px | 32px | — | View titles |

`text-3xs` and `text-2xs` are defined in the `@theme` block of `index.css`.
Both carry slight positive tracking: below 12px, default spacing closes up and
legibility drops faster than size alone predicts.

**This scale starts lower than most products', on purpose.** 11px appears 109
times in this codebase and 10px 50 times. Those are not one-off exceptions to
be stamped out — they are the two densest rungs of the real scale, and the
system's job was to name them, not to deny them.

**Below 10px is not a size, it is a bug.** Nothing smaller is defined. See
[sizes below the scale](#sizes-below-the-scale).

---

## Weights

| Weight | Class | Use |
| --- | --- | --- |
| Normal (400) | `font-normal` | Body text |
| Medium (500) | `font-medium` | Labels, buttons, active nav, table headers |
| Semibold (600) | `font-semibold` | Headings |

Bold (700) is **not** in the system. Where you want more emphasis than
Semibold, the answer is size, colour, or position — not weight.

---

## The mono face

Mono is for **values that change in place**: run ids, durations, token counts,
timestamps, byte sizes, diff hunks.

The reason is mechanical rather than stylistic. A proportional `1` is narrower
than a `8`, so a live counter reflows its row on every tick. Mono, plus
`font-variant-numeric: tabular-nums`, holds the column still.

Prose is never mono. A paragraph of explanatory text in mono is a style choice
this product does not make.

```
--font-mono: "Geist Mono Variable", ui-monospace, "SF Mono", "Menlo", monospace;
```

Installed and live: `@fontsource-variable/geist-mono` is a dependency and
`index.css` imports it. Verified in the running console with
`document.fonts.check('400 12px "Geist Mono Variable"')` rather than by
eye — a missing face falls back silently, and the fallback is also monospace.

`tabular-nums` is applied automatically to `table` elements and to anything
carrying `data-numeric`, set once in the base layer rather than per component.

---

## Markdown prose

`components/markdown.tsx` is the one renderer behind every surface that shows
agent-written text — chat, memory, ledgers, artifacts, workflows — and it wraps
its output in `prose prose-sm dark:prose-invert`, styled by
`@tailwindcss/typography`.

The plugin is written for hand-authored HTML, so two of its defaults are wrong
here and `index.css` overrides them:

| Default | Override | Why |
| --- | --- | --- |
| `code::before`/`::after` draw a literal `` ` `` | `content: none` | The markdown parser already consumed the fences; the plugin drew them back on, so every type name rendered as `` `UsageDto` `` (issue #1108) |
| Inline `code` gets no ground of its own | `--muted` + hairline `--border`, `0.125em 0.375em` padding | At `0.875em` the mono face alone is a weak signal in a chat line |

Two things about those overrides that are easy to undo by accident:

- **They must stay unlayered.** `prose` is a utility, so everything the plugin
  emits lands in `@layer utilities`, and an unlayered rule outranks every
  layer. Tidying them into `@layer base` hands the backticks back.
- **The chip rule excludes `pre *`.** The plugin clears the ground inside a
  fenced block; being unlayered, this rule would otherwise outrank that too and
  paint a second, bordered ground inside every code block.

`frontend/test/unit/markdown-inline-code.test.ts` compiles `index.css` with the
real plugin and resolves the cascade against a rendered document, so both of
those fail a test rather than shipping.

---

## Sizes below the scale

**None remain.** Below 10px is not a size, it is a bug, and the console no
longer has one.

The interesting case was `views/chat/MessageRow.tsx`, where a reply facepile
set 7px initials in a 16px tile. Resizing could not fix it: two letters fit a
16px tile only below 10px, and growing the tile would have reshaped the chat
row. The facepile is `aria-hidden` decoration capped at three faces, so the
glyph was carrying nothing a reader could use. `Avatar` gained a `markOnly`
prop — the tile keeps the tone colour that distinguishes one voice from the
next, and draws no glyph at all.

That is the general shape of the answer whenever type wants to go under 10px:
**it is not text, it is a mark**, and it should stop pretending otherwise.

---

## Migration

**Complete.** The console has zero arbitrary font sizes. Verify with:

```sh
cd frontend
grep -rn 'text-\[' src --include="*.tsx" --include="*.ts"
```

192 sites were migrated in two passes.

### The mechanical 159

`text-[11px]` → `text-2xs` (109 sites) and `text-[10px]` → `text-3xs` (50).

Worth knowing, because it is easy to describe this as a no-op and it is not
quite one: an arbitrary `text-[11px]` sets font size *alone* and inherits its
line height, whereas `text-2xs` carries the scale's line height (16px) and
tracking (+0.005em). 32 of the migrated sites pin an explicit `leading-*` and
are unaffected; the rest tighten slightly, which is the scale doing its job.

### The 33 judgement calls

The knowledge panel carried its own six-level scale in half-pixels — 12.5 /
11.5 / 10.5 / 9.5 / 9 / 8. Half-pixel type does not render as a half pixel;
those sizes were arrived at by nudging until something looked right.

They collapsed onto three rungs:

| Was | Now | Role |
| --- | --- | --- |
| 12.5px bold | `text-xs` | Panel title |
| 11.5px semibold | `text-2xs` | Row title |
| 10.5px mono | `text-2xs` | Link |
| 9.5px mono dim | `text-3xs` | Sub-label |
| 9px mono uppercase | `text-3xs` | Badge |
| 8px uppercase | `text-3xs` | Tag |

Six sizes became three, and nothing was lost: weight, case and colour were
already carrying the hierarchy those extra sizes were standing in for.

`TourTooltip` moved the other way — 15/13 to `text-base`/`text-sm`. It is
onboarding prose, not console chrome, so it belongs on the reading rungs.

`Button`'s `sm` size dropped shadcn's stock `text-[0.8rem]`: 12.8px is not a
rung of this scale, and a button is not the place to invent one.
