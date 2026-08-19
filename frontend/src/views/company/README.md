# Company — the org chart

`#/company` draws the company's declared structure as a three-level tree and is
the one place desks are created, staffed and led.

## Why this exists

Issue #302 removed the Desk nav entry for the v1 IA cut and said what that cost:
"desk creation and membership editing become unreachable", editable by hand in
the manifest and nowhere else. It called that temporary.

By the time #311 was picked up it was worse than #302 described. The chat
rebuild removed `"desks"` from `app-shell.tsx`'s `View` union altogether, so
`DesksView.tsx` and `DeskCreateDialog.tsx` were imported by nothing and
`#/desks` silently rewrote to Overview. Five host routes with a full backend
test suite behind them were reachable from no UI by any path, including by
typing a URL.

Issue #311 is explicit that the answer is not to put the flat list back: "build
the hierarchy surface that makes the Desks page unnecessary, and route creation
and membership editing through it." So the old screen was retired into this one
rather than re-listed.

## Three levels, and why nothing enforces it

```text
company            level 1   the company itself
└── desk           level 2   a [[group_chat]], or an operator-created overlay desk
    └── seat       level 3   one teammate on that desk
```

The cap is **structural**. A desk cannot name a parent desk — neither
`GroupChat` nor `OverlayDesk` has such a field — so a fourth level is not
rejected by a check, it is unrepresentable. Nothing in `lib/org.ts` validates
depth, and nothing should: #311 settles that this is "a new reader over existing
data, not a data change", and a parent pointer is precisely the data change it
rules out.

`MAX_DEPTH` and `depthOf()` exist so the number can be read and asserted against
a real tree instead of trusted from a comment.

## Rules the host owns, that this surface must not re-derive

| | |
|---|---|
| **The lead is `members[0]`** | It is the host's routing target. Changing the lead is `PUT …/desks/{id}/order` moving somebody to the front; there is no set-lead call and the console must not invent one. Never re-derive the lead by sorting or by name. |
| **Provenance decides the controls** | The host refuses to delete a blueprint desk or remove a blueprint member at runtime. The chart offers neither, because a control that always fails is worse than none. `overlayCreated` marks the desk, `overlayMembers` marks the seats. |
| **A write is followed by a refetch** | Every write changes something the host derives — the effective member union, the order, the overlay subset. Patching locally would be a second implementation of rules the host already owns, and the two would drift. |

## What is drawn beside the tree, not in it

**Not on a desk** (roster teammates staffed nowhere) and **People** (the humans
who can sign in) are listed alongside the chart. Neither has a position the
company declares. Placing them under a node would be inventing structure, which
is the failure `views/overview/README.md` already documents about its own
keyword-derived departments — this surface exists to be the truthful one.

A seat whose id resolves to no roster teammate is a third case, and it is
handled the opposite way: it stays, badged "Not on the roster". The chat member
pane drops such an id, which is right for a chat — you cannot message nobody.
Here it is a fact about the structure that only the operator can fix.

That divergence is **load-bearing**, not incidental: it is the reason membership
editing lives here and not on the member pane. See below.

## A teammate on the chart opens (#1102)

Every **roster teammate** this surface names — a staffed seat the roster
resolves, and a **Not on a desk** chip — is a link to `#/team/<agentId>`, the
sub-page `TeamView` has routed to `AgentDetailView` since #264. The two
exceptions are deliberate and are set out below: a seat the roster cannot
resolve, and the **People** chips. Before this the chart drew teammates and
wired none of them: a seat's name was plain text, and the chips were bordered
pills with no `href`, no handler and no tooltip. The destination was never
missing; only the link was.

Three decisions, so they are not re-argued:

**A real `<a href>`, never a `div` with an `onClick`.** The console routes on
the hash, so the hash *is* the address. An anchor gets middle-click, cmd-click,
the browser's own hover preview of the target, and keyboard reach with a focus
ring — none of which a handler on a `div` gets without re-implementing all four.

**The name is the target, not the whole row.** The seat row is the drag handle
for re-ordering. A full-row link would make every attempt to drag a seat read as
a click on it, so the anchor is the name (with a chevron), and it carries
`draggable={false}` so a drag starting on it falls through to the row.

**A seat the roster cannot resolve is not linked.** `#/team/<id>` for an id the
host has never heard of lands on the detail view's "no such teammate" state —
a dead end that only repeats what the badge beside the name already says.

The **People** chips are the deliberate exception, and they say so in their
styling. A person is a console user, not an agent: `#/team/<id>` resolves
against the roster, and there is no person detail page to link to instead. So
the pill treatment is dropped — a filled, borderless label with a `title`
explaining what it is — rather than left promising a page that does not exist.
`teamHref()` is the one place an id becomes an address, and it answers `null`
for a blank one, so no chip can render `#/team/undefined`.

## How chat reaches this surface (#485)

`#/company/<deskId>` opens the chart with that desk scrolled to, focused, and
ringed until the operator's first click or keypress. The chat member pane links
in at exactly that address — "Manage on the org chart" beside "In this channel",
or "Staff it on the org chart" when the desk is empty.

Three decisions #485 settled, so they are not re-argued:

**The channel rail stays flat.** It already *is* this tree's desk level. A desk
cannot name a parent desk, so nesting is unrepresentable (see above), and both
surfaces render the same `GET {scope}/desks` in the same host order. "A desk
thread becomes a view of the org chart" is satisfied by the two surfaces being
the same list with a link between them — not by restructuring the rail into a
tree it has no data to draw.

**DMs are untouched.** Under a flat rail the question does not arise. Recorded
for a future attempt: if grouping were ever adopted, DMs would be a separate
flat section *below* the tree — they are not desks, and hanging them off one
would invent the same structure `Not on a desk` exists to avoid.

**Membership editing stays here; the pane only links.** The member pane drops an
id that resolves to no roster teammate. A membership editor must show every
seat, and the ghost seat is precisely the one an operator most needs to remove —
so an editor on that pane would either violate the pane's documented drop rule
or knowingly be unable to remove ghosts. Editing belongs on the surface that
keeps ghosts visible. No admin gate on the link: this chart has none either,
because its controls are gated by provenance instead.

An unknown or deleted desk id in the hash is a **silent no-op** — the chart
renders normally. `useHashView` hands the second segment back unvalidated, and a
stale bookmark deserves the company, not a banner. The hash is never rewritten:
`#/company/<deskId>` is a shareable address.

## Two kinds of message in the desk creator

`DeskCreateDialog` keeps field-level complaints apart from whole-form ones
(issue #1100). "Give the desk a name." renders **at the Name field** — inline
under the input, `role="alert"`, wired to the input by `aria-describedby` with
`aria-invalid` on it — and an invalid submit focuses that input and scrolls it
into view. What the host says when it refuses the create stays in the banner
above the footer, because it is about the whole form.

They used to share the banner slot, which put a complaint about the first field
below the entire roster picker: off-screen in a `max-h-[85vh] overflow-y-auto`
dialog on any real company, with nothing moving the operator to it, so Create
read as inert. The distance grew with the roster.

## Files

| | |
|---|---|
| `OrgChartView.tsx` | The view, the tree, and the desk/seat controls. |
| `DeskCreateDialog.tsx` | The create form. Moved here from `views/`, where nothing had rendered it since #302. |
| `@/lib/org.ts` | The pure model: `buildOrgTree`, `depthOf`, `addableTo`, `reorderedIds`. All of it testable without a DOM. |

## Tests

`test/unit/org-tree.test.ts` covers the derivation — lead by position,
provenance, the unresolvable seat, the unplaced roster, and the cap.
`test/unit/desk-create-name-error.test.ts` covers the create dialog's two
message slots — which node holds each, where it sits relative to the field, its
ARIA wiring, and the focus move.
`test/e2e/org-tree.spec.ts` covers the surface in a browser, including the
reachability failure this issue is about, and asserts every write survives a
reload against a stateful stub.
`test/unit/org-chart-teammate-links.test.ts` renders the chart and reads the
DOM for #1102: the failure was invisible to every derivation test above, because
`buildOrgTree` was correct the whole time the surface was inert. It pins the
`href` rather than a handler — a `div` with an `onClick` would type-check, look
identical and still break middle-click, cmd-click and the keyboard.

## Deliberately not here

- **A nested channel rail** — #485 settled that the rail already is this tree's
  desk level, and that a fourth level is unrepresentable. See above.
- **The Overview graph's invented departments** — replacing them with this real
  structure belongs with the replacement, not the foundation. Filed as #486.
