# Activity

Who may reach whom, who has, and who created whom.

## The address

```
#/company/comms                the whole company
#/company/comms?agent=<id>     one neighbourhood
```

`comms` is a reserved second segment of `company`, checked **before** the desk
arm in `CompanyView` — that component treats any unrecognised `sub` as a desk id,
so an unreserved segment would focus the org chart on a desk that does not exist.
Same reservation, and the same accepted collision, as `desks` and `graph`: a
company declaring a desk literally named `comms` reaches this page instead.

Reached from the roster's own action row, not a fifth sidebar row. It is a
**deep-link destination** under Rule 6 of `ledgers-console-ia.md`: the question
"who works with whom" is asked from the page you are already on when you are
looking at the team.

## Why the edges come from four places

There is no endpoint that answers "which agents talk to each other". The host
carries the pieces and nothing joins them.

| Source | Gives | Kind |
| --- | --- | --- |
| `TeamMemberDto.delegatesTo` | the desks a teammate *may* hand work to | structure |
| `DeskDto.members` | who shares a room | structure |
| `Task.originChatId` → `Task.assignee` | a hand-off that happened | history |
| `teammate_added` (SSE / journal) | who created whom | history |

**Structure is dashed, history is solid**, and that distinction is the whole
drawing. A company that has never run should look connected but idle — its
manifest says who can reach whom — not like a set of unconnected dots. Without
`delegates_to` it did.

The board is read rather than the live stream because a card is durable: a
tool-call frame naming a delegation arrives **redacted** (`TurnStep.detail`) and
does not survive a reload. Where a target is unreadable the edge is dropped, not
guessed — a graph that invents an edge is worse than one that admits a gap.

## Why not a force layout, and why not a graph library

`d3-force` is right for the knowledge graph — large, undirected, exploratory,
static per load — and wrong here. This graph is **fed live**, and a force
simulation re-heats on every insertion, so one `teammate_added` frame would drift
every unrelated node. "Who spawned whom" is a claim about structure; a layout
that moves when something else happens destroys the spatial memory that makes it
readable between glances. `layout.ts` is two stable columns, and
`comms-graph-model.test.ts` pins that an arriving node moves nothing.

`@xyflow/react` renders React nodes well but brings a pan/zoom canvas this small
layered graph does not need. The nodes here are hand-positioned HTML over an SVG
edge layer, which also means every colour resolves through the design tokens —
so it themes for free, the retrofit the knowledge graph needed.

Left-to-right *is* the direction of the arrow: agents act, desks are acted upon,
and every `may-delegate` and `handed-off` edge runs agent → desk. A `spawned`
edge runs agent → agent and so bows inside the left column.

## Files

| File | Role |
| --- | --- |
| `model.ts` | roster + desks + observations → nodes and edges — **pure** |
| `layout.ts` | column packing, edge paths, weight — **pure** |
| `CommsGraphView.tsx` | positions and paints |
| `CommsView.tsx` | fetches, polls, holds the selection |

Everything worth arguing about is in the pure half, which is why the unit lane
covers it without a browser.

## A poll, not a subscription

The host journals every structural change now, so this view *could* subscribe.
It does not, because the Observatory's rule applies with more force here: a frame
never merges into the snapshot, it only means "re-read". Once that is true a poll
does the same job and needs no state threaded down from the shell — which is
exactly the coupling the room store exists to undo, and not one worth a second
instance of. `startVisiblePolling` stops while the tab is hidden.

## What it still cannot say

A **delegation's two ends** are inferred, not read. `RunRecord` has no delegator
field and a delegate shares its parent's run sink, so the `from → to` edge of a
`delegate_to_desk` exists in no store; what is drawn instead is the board's
`originChatId → assignee`, which is desk-level. Closing that is a host change —
a `WorkHandedOff` row, which needs a `delegator` threaded through
`DelegationRunner` — not a console one.

The **structural `CompanyEvent`s themselves are unconsumed here.** `TeammateAdded`,
`DeskCreated`, `DeskDeleted`, `DeskMembersChanged`, and `DeskRoutingConfigured` are
journaled durably (host side), but `use-events.ts`'s `handleEvent` types these
events without converting any of them into the `spawned`/creator-edge
observations this graph draws. Polling `/team`, `/desks`, and `/tasks` recovers
current structure, but none of those responses carries `byAgentId`, so "who
created whom" stays undrawn even though it is now a durable fact. Wiring that
derivation is a separable follow-up, not a defect in what shipped.

## What the episode frames add

Mounted at `#/company/comms`, the view now receives the one set of observations
the shell can derive exactly: **who spoke to whom inside an episode**.
`lib/coordination.ts` folds the shell's episode ledger (`lib/episode-frames.ts`)
into `spoke` observations — a `broadcast_routed` is the author reaching every
seat the router's plan named, a `dm_delivered` is the sender reaching each
recipient, a non-returning `referral` is the asker reaching the far desk — and
a `speaking` observation per agent whose turn bracket is open. `spoke` is the
most direct claim the graph makes: not "may reach", not "handed a card", but
"said something to". Drawn solid like the other history edges, in the running
tone, labelled by the last kind seen, and merged by endpoint pair like a
hand-off. The same numbers, summarised, are what `scripts/measure-coordination.mjs`
prints for a run nobody is watching.
