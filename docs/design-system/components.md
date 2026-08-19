# Components

The primitives in `frontend/src/components/ui/`, what each is for, and the
states each must handle. Built on [Base UI](https://base-ui.com) via shadcn's
`base-nova` style — so behaviour, focus management and ARIA come from the
primitive, and this system supplies only the visual layer.

Rendered reference for every state below: `#/styleguide`.

---

## Rules that apply to all of them

**Focus is never removed.** Every interactive primitive shows
`focus-visible:ring-3 focus-visible:ring-ring/50`. The ring is brand-coloured.
If a control cannot show a ring, it is not a control.

**Disabled is `opacity-50` plus `pointer-events-none`** — never a colour
change. A disabled control that merely looks grey is indistinguishable from a
low-emphasis one.

**Invalid is `aria-invalid`**, which the primitives already style. Never style
an error state by hand at the call site.

**Icons are 16px (`size-4`) at default size**, dropping to `size-3.5` at `sm`
and `size-3` at `xs`. The button primitive sets this automatically for any
child SVG — do not size icons at the call site.

**Restyling at the call site is the smell.** If a primitive needs a look it
does not have, add the variant to the primitive. A `className` that overrides
background or border colour is a design-system bug filed in the wrong place.

---

## Button

`variant` × `size`. The most-used primitive in the console.

| Variant | Fill | Use |
| --- | --- | --- |
| `default` | `bg-primary`, white text | The one primary action on a view |
| `secondary` | `bg-secondary` | Neutral actions of equal weight |
| `outline` | Border, transparent fill | Actions beside a primary |
| `ghost` | Transparent until hover | Toolbars, icon rows, table row actions |
| `destructive` | `bg-destructive/10`, red text | Deletes and cancels — tinted, not solid red |
| `link` | Underline on hover | Inline navigation inside prose |

| Size | Height | Notes |
| --- | --- | --- |
| `xs` | 24px | Dense table rows |
| `sm` | 28px | Panel toolbars |
| `default` | 32px | Everywhere else |
| `lg` | 36px | Empty-state calls to action |
| `icon-xs`/`icon-sm`/`icon`/`icon-lg` | 24/28/32/36px square | Icon-only — **requires `aria-label`** |

**One primary action per view.** Two `default` buttons side by side means
neither is primary.

`destructive` is deliberately a tint, not a solid red fill. A solid red button
draws the eye harder than the primary action, which is backwards for something
you mostly want people *not* to click by accident.

Press gives `translate-y-px` — except on menu triggers, where the popup makes
the movement read as a glitch.

---

## Badge

Variants: `default`, `secondary`, `outline`, `destructive`, `ghost`, `link`.

For a **noun** — a count, a label, a category. Not for status: status uses the
`--status-*` tokens with an icon (see [color.md](color.md#status)), because a
badge alone carries no meaning for anyone who cannot separate the hues.

Never interactive. If it can be clicked, it is a Button.

---

## Alert

Variants: `default`, `destructive`. Composed of `AlertTitle`,
`AlertDescription`, and optionally `AlertAction`.

For a condition affecting the whole surface, in place, that the operator has
not just caused. Something they *did* just cause is a toast (Sonner) instead.

Title states what happened; description says what to do next. Both follow the
error rules in [`../brand/README.md`](../brand/README.md#2-voice) — explain and
instruct, never apologise.

---

## Card

`Card`, `CardHeader`, `CardTitle`, `CardDescription`, `CardContent`,
`CardFooter`. No variants — a card is a card.

Separates by `--card` surface plus a `--border` hairline. **No shadow at rest.**
A card that floats is not a card, it is a popover.

`CardTitle` sits at `text-sm`/`font-semibold` in this console, not the larger
default — the console is dense and a card title is a label, not a heading.

---

## Input, Textarea, Label, Switch

Form controls. All 32px high except `Textarea`.

- Always pair a control with a `Label` and a matching `htmlFor`/`id`.
  Placeholder text is not a label — it disappears exactly when it is needed.
- Placeholders show a **realistic example**, never a restatement of the label:
  `acme-marketing`, not "Enter company name".
- `Switch` is for a setting that applies immediately. A change that needs
  saving is a checkbox with a Save button.
- Validation messages go **below** the field, in `text-2xs text-destructive`,
  and say how to fix it.

---

## Tabs

`Tabs`, `TabsList`, `TabsTrigger`, `TabsContent`. One `variant` today
(`default`), via `tabsListVariants`.

For switching between **peer views of the same subject** — never for steps in a
sequence, and never as navigation between unrelated screens.

Tab labels are one or two words, sentence case. The panel does not restate the
tab label as a heading.

---

## Tooltip

`Tooltip`, `TooltipTrigger`, `TooltipContent`. `TooltipProvider` wraps the app
in `main.tsx` with a 200ms delay.

**Tooltips label; they do not explain.** They name a truncated value or an
icon-only control. Anything longer than a short phrase belongs in the interface
itself.

Never put an action, a link, or information available nowhere else in a
tooltip — it is unreachable by touch and by keyboard-only users on many
platforms. An icon button still needs its `aria-label` regardless of tooltip.

---

## Dialog, Sheet, AlertDialog, DropdownMenu, Select, Popover

Floating surfaces. All render on `--popover` at `shadow-lg` — this is what
elevation is *for*.

- **Dialog** — a focused task that must be finished or abandoned. Always
  dismissible with Escape and a visible close control.
- **AlertDialog** — destructive confirmation only. Not dismissible by clicking
  outside. The confirm button names the act (*Delete company*), never "OK".
- **Sheet** — a side panel for context that accompanies the page rather than
  replacing it. Enters at `duration-slow`.
- **DropdownMenu / Select** — menu rows use `bg-accent` on hover with neutral
  text. Destructive items are red text on a neutral row, and sit last, below a
  separator.

Never nest a modal inside a modal.

---

## ScrollArea, Separator, Skeleton, Avatar, Sonner, Chart

- **ScrollArea** — a JS-driven scrollbar for panels that need one they can
  position. Native scrolling is themed globally (see **Scrollbars** below), so
  reach for this only when a panel needs the overlay behaviour, not merely to
  make the bar look right.
- **Separator** — a single `--border` hairline. Two adjacent separators, or a
  separator against a border, is one too many.
- **Skeleton** — `bg-muted` blocks matching the shape of what is loading.
  Prefer it to a spinner for content that has a known shape; a spinner is for
  an action whose duration is unknown.
- **Avatar** — `AvatarFallback` carries initials on `--muted`. Never colour
  fallbacks by hashing a name into a random hue; that invents a colour
  vocabulary the system does not have.
- **Sonner (toast)** — bottom-right, `richColors`, for the outcome of something
  the operator just did. Never for background events; those belong in the
  Inbox or Approvals.
- **Chart** — Recharts wrapper. Series use `--chart-1…5` in order. Axis labels,
  gridlines and legends use `--muted-foreground` and `--border`, never a series
  colour.

---

## Scrollbars

Native scrollbars are themed once, globally, in `index.css` — nothing opts in
and nothing per-view is needed.

| State | Thumb |
| --- | --- |
| Rest | `--scrollbar-thumb-rest` — ~22% `--muted-foreground` (30% in dark) |
| Scrolling | `--scrollbar-thumb-active` — 48% (58% in dark) |
| Thumb hover / drag | `--scrollbar-thumb-grab` — 70% (80% in dark) |

The track is transparent, so the thumb composites onto whatever surface it sits
on; the lane is 10px with a 6px pill inset inside it, matching the rail
`ScrollArea` draws.

Three things to know before touching it:

- **The bar never disappears.** It fades in weight, not out of existence — it
  is the only affordance saying there is more content, and a panel whose action
  is below the fold must keep it. Never replace this with `width: 0`.
- **"Scrolling" is a JS signal.** `src/lib/scroll-activity.ts` marks the
  scrolled element with `data-scrolling` from one capturing document listener
  and clears it after an idle beat. It is not `:hover`, which matches every
  ancestor of the pointer up to `html` and so lights every nested scroller at
  once.
- **The engines are mutually exclusive.** Chromium ignores `::-webkit-scrollbar`
  as soon as `scrollbar-width`/`scrollbar-color` are set, so the standard
  properties live behind `@supports not selector(::-webkit-scrollbar)` — the
  Firefox arm — and the pseudo-element block serves Chromium and WebKit.

Under `prefers-reduced-motion: reduce` the bar holds the active weight
permanently: no transition and no state change to notice.

---

## Adding a component

1. **Check it is not a composition of existing primitives.** Most "new
   components" are a Card with a specific layout, and belong in the view.
2. **Take it from shadcn (`base-nova`) if it exists there.** `components.json`
   is configured; `npx shadcn@latest add <name>` lands it in the right place
   with the right imports.
3. **Express every colour, size and radius as a token.** If a value has no
   token, add it to layer 2 of `index.css` — see
   [`README.md`](README.md#the-one-rule).
4. **Add it to `#/styleguide`** with every variant and state, including
   disabled and invalid. A state absent from the styleguide is a state nobody
   will notice breaking.
5. **Write out class names in full.** Tailwind finds classes by scanning source
   text, so a template-assembled name like `` `bg-status-${key}` `` is never
   generated and fails silently at runtime.
