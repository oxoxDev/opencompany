# Chat — the workspace

`#/chat` is a channel-and-DM workspace: a channel rail, a threaded timeline, a
composer, an optional thread panel, and an optional member pane. It replaces
three older surfaces — the Conversation page's chat list, the Team page, and
the desks the two shared without ever being connected.

## Routing

`#/chat/<channelId>` — the channel id is the hash's second segment, so a
channel is linkable and survives a refresh.

- A desk's channel id is the host's desk id, which is also its chat thread id.
- A DM is `dm:<teammate-id>` — e.g. `#/chat/dm:designer` for a host roster
  agent, or `#/chat/dm:member-product-manager-1f3k` for a teammate this console
  invented.
- A host with no `.../desks` route falls back to `#general`, `#strategy`,
  `#creative`, `#front-desk` (`lib/desks.ts`).

Nothing resolves until `/desks` has answered — the view holds a loading state
rather than resolving against the fallback desks and swapping under you, which
is what made every deep link flash `#general` (issue #370). An id that doesn't
resolve *after* they land opens the first channel and says so in a notice above
the timeline, so the URL and the content never disagree silently. A `/desks`
failure that isn't "this host has none" (404 or an empty list) is a retryable
error state, not invented channels.

DM ids key on the teammate's **id** (issue #364). A host roster agent has always
had a stable one; a console-invented teammate now does too — `lib/team.ts`
derives it from the role (or, for a hand-added teammate, the name) rather than
minting it from a counter. Keying on the id means renaming somebody does not
move their DM's URL or orphan the history already journaled under it.

Ids minted before #364 were `dm:<slug-of-name>-<hash>`. `resolveDmChannelId`
still resolves those for one release so a saved link lands, but nothing is ever
addressed or stored under one.

## What is real and what is console-local

Every channel and DM posts to the **same** company chat endpoint. A channel
scopes a transcript and fixes the company side's identity; it is not a separate
backend, and there is no per-channel routing on the host.

**Real — the host's, and reload-visible to every operator** (issue #364):

| | |
|---|---|
| Transcripts | Journaled by the host and rehydrated from `GET {scope}/chat/history` on load. This has been true since #65 — the "per-session memory" this file used to claim was already out of date. The rehydration is *not* instant, and the timeline has to say so: see [Empty, or not answered yet](#empty-or-not-answered-yet). |
| Channel scoping | Every message carries the desk it was sent to, and the host filters server-side. Two people in two channels are not in the same room, and have not been since #53/#65. |
| Threads | A reply posts its `parent` — the parent message's own id — and comes back under it. Both halves of the exchange hang off the row the thread opened from. |
| Reactions | One durable row per person per emoji, so a chip says who reacted and whether one of them was you. `POST {scope}/chat/messages/{seq}/reactions` with an explicit `on`, which makes a retry idempotent. |
| Message ids | A sent message comes back with the id it was journaled under (`messageId`), which is what a thread reply or a reaction names. Until the id lands the row's reply/react actions are disabled and say why. |
| Message intent | The composer's three positions — "Just chatting" / "Do it once" / "Build me the workflow" — travel as `deliverable` on the message and are journaled with it. Only a non-default value is sent, so an unmarked line is byte-identical on the wire to a pre-#580 one. `chat` withholds the card the host would otherwise open by construction; it does **not** take the orchestrator's own `spawn_task` away, so it means "not automatically carded", not "never carded". |

**Still console-local:**

| | |
|---|---|
| Unread counts | Derived here from when this tab last looked at a channel — the host keeps no read receipts, so two consoles will disagree. The badge's tooltip says so. |
| A console-only teammate's other half | A starter-roster teammate is not on the company, so nothing answers their DM. The transcript is still saved; a notice above the composer says which half is missing. |

Reactions are deliberately **not** on the SSE feed: the frame would have to
carry the reacting person, and that stream has no per-viewer projection to turn
an actor into a label. They arrive on the next read.

Nothing was migrated. A message journaled before #364 loads with no parent and
no reactions, which is the truth about it. History journaled under the old
counter-minted `member-N` teammate ids stays orphaned — there is no honest
mapping from `member-3` to a person, and inventing one would be worse than the
loss.

## Empty, or not answered yet

`Transcripts` is `Record<string, ChatMessage[]>`, and the timeline reads it as
`transcripts[channel.id] ?? []`. That `??` collapses two different facts into
one: *nobody has asked the host about this channel* and *the host says this
channel has nothing*. The channel intro then printed the second — "This is the
start of your direct message with …" — while the first was true, so reloading a
DM with months of history rendered it as brand new for as long as the fetch took
(issue #934). Nothing was lost; it just read exactly like it had been.

`HistoryHydration` in `model.ts` is the missing fact, and `historyReady()` is
the one place that reads it:

| State | May the intro claim "this is the start"? |
|---|---|
| `byChannel[id] === "loading"` | No — the request is in flight. |
| `byChannel[id] === "ready"` | Yes. Settled, including a host that answered with nothing or failed outright. |
| No entry, `discovered === false` | No. The shell's pass has not reached this channel yet — `ChatView` resolves its own desk list independently, so it can paint a channel a moment before the shell marks it. |
| No entry, `discovered === true` | Yes. The pass ran and did not claim it, so nothing is coming — a console-only teammate, or a host with no `chat/history`. Holding a spinner forever would be a worse lie than the one this prevents. |

`AppShell` owns the map because it owns the fetches. A channel is marked
`loading` *before* its request goes out, never after — the gap between "this
channel exists" and "its history is in flight" is precisely the window the bug
lived in.

While a channel is pending and has no rows, `MessageTimeline` renders skeleton
rows in place of the claim. The avatar and title above them still render: those
state where you are, which is not a claim about history. Rows that arrived
locally — a message sent before hydration landed — render immediately; only the
assertion of emptiness waits.

## Membership

A desk's members come from the host (`GET {scope}/desks` → `members`, lead
first). They scope the header's count and the member pane, so a two-person desk
reads as two people rather than as the whole company (issue #369). A DM is a
two-person line: the header states 2, and the pane shows the teammate on the
other end.

The fallback desks in `lib/desks.ts` carry no membership — there is none to
carry — so a channel built from them falls back to the whole roster, and the
pane renders one plain list. The rest of the company is always one section
below, so adding a teammate or opening somebody's DM never needs a different
surface.

### Editing it is somewhere else, on purpose (#485)

The pane **links** to the org chart — "Manage on the org chart" beside "In this
channel", "Staff it on the org chart" when the desk is empty — and grows no
membership controls of its own. The link opens `#/company/<deskId>`, which the
chart scrolls to and focuses. A desk's channel id *is* its desk id
(`deskFromDto`), so nothing maps between them.

The reason is the pane's own rule. `channelMembers` **drops** a member id that
resolves to no roster teammate: you cannot message nobody, so no row is drawn
for one. The chart does the opposite and **badges** it "Not on the roster". A
membership editor has to show every seat, and that ghost seat is exactly the one
an operator most needs to remove — so an editor here would either break the drop
rule this pane is built on or be unable to remove ghosts. Editing lives where
ghosts are visible. Keep the two behaviours as they are; the divergence is what
makes the split coherent rather than arbitrary.

The link is offered only for a **host-backed desk channel**. A DM is not a desk,
and a fallback desk names one the host does not have — the chart would open on
nothing, so neither gets a link. There is no admin gate, because the chart has
none either (its controls are gated by provenance).

The channel rail stays flat, and #485 settled that too: it already is the org
chart's desk level, since no desk can name a parent desk. See
`views/company/README.md`.

## Files

| | |
|---|---|
| `model.ts` | Channels, senders, timeline grouping, formatting. All pure. |
| `ChannelRail.tsx` | The channel/DM list, with collapsible sections. |
| `ChatHeader.tsx` | The bar above the timeline. |
| `MessageTimeline.tsx` | The scroll body: day dividers, channel intro, loading skeleton, typing row. |
| `MessageRow.tsx` | One line — avatar gutter, author, body, reactions, hover action bar, and the board-card chip (link plus its dismissal, issue #984). |
| `MessageComposer.tsx` | The composer dock; also used compact in the thread panel. |
| `ThreadPanel.tsx` | Replies to one message, with their own composer. |
| `MembersPane.tsx` | Who is in this channel, then the rest of the roster. |
| `AddMemberDialog.tsx` | Define a teammate. |

`../ChatView.tsx` owns the state and composes them.

## Grouping rules

Consecutive lines from one sender within five minutes collapse into a run: the
first row carries the avatar and the author, and continuation rows leave the
gutter empty and reveal their timestamp there on hover. A run also breaks on a
day boundary, and on any row that has replies — a summary row between two lines
that read as one utterance is worse than an extra avatar.

## One face per teammate

`TeammateAvatar` (`@/components/teammate-avatar`) draws its mascot from an
`avatar` key seeded on the teammate's **id** — `TeamMember.avatar`, computed
once by `fromDto` in `lib/team.ts` — so a rename never changes anyone's face
(issue #1185). It falls back to hashing the `name` it is given only when no
`avatar` prop is passed, which is the honest answer for a voice with no
roster entry behind it (a channel, a cross-posted agent line `senderOf`
couldn't match against the roster).

A DM is where seeding it wrong bites hardest: the rail row and `ChatHeader`
sit on screen together, and seeding them differently would put two faces on
one teammate — worse than the generic glyph the header drew before issue #1170.
Both go through `dmFace(channel)` in `model.ts`, which reads
`channel.member.avatar`; a channel and a DM with no roster entry get `null`
there and wear a glyph (`#`, `Lock`, `CircleDot`) instead, because neither has
one person behind it. The header draws its tile at 24px, the floor below
which `TeammateAvatar`'s `markOnly` says a mascot is a smudge and the bare
tone tile is the honest mark.

The main timeline's `senderOf(message, channel, members)` carries the same
seed for a message whose `channel` field names a distinct originating voice:
it looks that id up against the roster (`members.find`) the same way
`ChatView` already does elsewhere, and simply leaves the mascot unresolved —
falling back to the name seed, never a wrong face — when the id names a desk
rather than a teammate.

## One name per teammate

The header's title is `channel.name`; the muted slot past the divider is
`channelSubtitle(channel)`. A DM's `purpose` is the teammate's **description**,
falling back to their role — the field parallel to a desk's blurb, since both
answer "what is this line for". It used to be the role alone, which is an
identity field in a description slot, and `fromDto` resolves a teammate's name
as `dto.name?.trim() || dto.role`: a company that declares roles and names
nobody made the two slots one string, and every DM header in it read
`Backend Engineer │ Backend Engineer` (issue #1180).

So `channelSubtitle` returns `null` — not `""` — for a purpose that is empty or
that only repeats the title, compared case- and whitespace-insensitively. The
header drops the entire `<span>` when it does, divider included: the `border-l`
lives on that element, so keeping it empty would leave a rule hanging beside the
name. `ChannelRail`'s row tooltip and `MessageTimeline`'s conversation-intro
clause read the same helper, for the same reason. The rule is kind-agnostic: a
desk whose blurb just restates its slug is the identical non-fact under `#`, and
a blurb that says something the slug does not is untouched.
