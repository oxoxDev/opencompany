# The console write plane

Every write the operator console makes, under `src/server/ops/`.

Split out of [`api.md`](api.md), which was over the repository's 500-line ceiling.
That file is the map of the API surface; this one is the write routes in detail.

## Console write plane (`src/server/ops/`)

The console's writes are a REST router family under `src/server/ops/`, each
route registered under **both** scope forms (`…/companies/{id}/…` and the
`…/company/…` prosumer alias) by the `scoped` helper. These are the mutations,
plus a deliberate set of **read exceptions** — reads the console must reach over
REST because it ships no GraphQL client: the two inbox `GET`s and the three
workspace `GET`s (tree, file, `search`), the task export
(`GET …/tasks/{taskId}/export`), the skill-registry browse
(`GET …/skills/registry`), the agent detail (`GET …/team/{agentId}`), the policy
read (`GET …/policy`), and the credential status `GET` — every one detailed
below or in the credential section. Every other console read goes through
GraphQL (see the [read plane](api-graphql.md)). Anything a build doesn't serve
`404`s — the console treats that as "not wired yet".

```text
POST   …/tasks                              create a task card (`originChatId` records the thread it came from, #246)
PATCH  …/tasks/{taskId}                      edit / move a task
DELETE …/tasks/{taskId}                      delete a task
GET    …/tasks/{taskId}/export               the task's record as a readable HTML document (#352)
POST   …/tasks/{taskId}/discussion           post a message to the card's thread (#335)
POST   …/memory                             add a memory fact
DELETE …/memory/{factId}                     delete a memory fact
GET    …/workspace                          the whole tree (metadata; no bodies)
GET    …/workspace/file/{nodeId}             one file: content + inbound backlinks
GET    …/workspace/search?q=…                which notes mention a phrase (#607)
POST   …/workspace                          create a folder/file (or upload)
PUT    …/workspace/file/{nodeId}             write file content
PATCH  …/workspace/{nodeId}                  rename / move
DELETE …/workspace/{nodeId}                  delete a node
POST   …/workspace/sweep-empty-agent-folders?dry_run=  tidy `Agents/` strays (#700)
POST   …/workspace/merge-duplicate-folders?dry_run=    repair a raced tree (#759)
POST   …/skills                             add a custom skill
GET    …/skills/registry                     browse the shared skill library
POST   …/skills/{slug}/install              install a registry/company skill
POST   …/skills/{slug}/uninstall            uninstall a skill
PUT    …/skills/{slug}                       enable / disable a skill
POST   …/team                               add an operator-overlay teammate
GET    …/tools/catalog                       everything this company can grant
GET    …/team/{agentId}                      one agent in full (tier, tools, desks)
PATCH  …/team/{agentId}                      edit an overlay teammate
DELETE …/team/{agentId}                      remove an overlay teammate
PUT    …/team/{agentId}/inbox                toggle a teammate's inbox
PUT    …/team/{agentId}/budget               set / change / remove a daily cap
DELETE …/team/{agentId}/budget               reset the cap to the manifest default
POST   …/setup/roster                       propose a starting team from three answers (company-setup.md)
GET    …/policy                              the autonomy tier + always-ask list
PUT    …/policy                              set the tier and/or the always-ask list
DELETE …/policy                              reset the policy to the manifest's
POST   …/inboxes/{key}/read                  mark inbox messages read
POST   …/inboxes/ingest                     HMAC-signed inbound email → inbox
GET    …/inboxes                            list inboxes + unread counts
GET    …/inboxes/{key}/messages              one teammate's mail (store order)
```

The two inbox `GET`s are the read exception, and they are **REST twins of the
`Company.inboxes` GraphQL resolver**: the operator console ships no GraphQL
client, so without them the Inbox view had no reachable per-agent read at all
and fell back to a client-side fixture (issue #173). They read the same
`InboxStore` both inbound paths — the ingest webhook and the IMAP poller — file
into, and `GET …/team` tags each teammate with `inboxEnabled` so the Team toggle
reflects that store too. Messages come back in append order; the console sorts
them newest-first. The GraphQL resolver stays the canonical read for any client
that does speak GraphQL — these routes duplicate it, they do not replace it.

The two original workspace `GET`s (#177) are the same story one issue later:
the console had no reachable workspace read either, so the Workspace tab
persisted to `localStorage` and the operator and the agents looked at two
different trees —
a note written by an agent through its `workspace_*` tools (#237) was invisible
to the operator, and vice versa. They are REST twins of `Company.workspaceTree`
/ `workspaceFile`, differing only in timestamp shape (epoch millis, matching
every other console read, rather than ISO-8601 strings). The backlink scan is
literally shared code (`company::workspace_links`), so the two surfaces cannot
report different backlinks for the same note. The tree read carries metadata
only — bodies are fetched per file, so a navigation read does not grow with the
size of the workspace. Reading a folder id as a file is a `404`, never an empty
note.

`GET …/workspace/search?q=…` (#607) is the **third workspace read**: it answers
which notes mention a phrase, so discovery costs one call rather than a listing
plus one read per candidate.
Matching is a plain **case-insensitive substring** over node names and text
bodies — no tokenising, no stemming, no ranking — which is the only definition
that answers identically on all three storage backends, and it is defined once
in `company::workspace_search`, shared with the GraphQL `Company.workspaceSearch`
resolver and the agent `workspace_search` tool. Optional `prefix` scopes to a
subtree; optional `limit` pages the answer (default 20, hard cap 50). A hit
carries the node, its logical `path`, whether the `name` or the `content`
matched, and — for a content match — a short `excerpt`. `total` reports every
match, so a capped page says it is one. An empty `q` is a `400`, not "match
everything": that is the tree read above, and answering it here would turn a
cleared search box into a full-tree fetch. `limit=0` is a `400` for the same
reason — it is never read as "no limit". A **binary** node matches on its name
only: a text read of a payload is empty by the port's definition, and its bytes
are never scanned or excerpted.

`POST …/workspace/sweep-empty-agent-folders` (#700) removes the empty
`Agents/<id>/` folders a pre-#570 company still carries. Operator-triggered
rather than automatic — the affected tenants are hosted, so a subcommand would
be unreachable for the operators who need it, and a boot sweep would change a
tree on an upgrade nobody asked for. `?dry_run=true` answers
`{"wouldRemove":[{id,name}…]}` and touches nothing, so the console can name
every folder on a confirm dialog; the real call answers `{"removed":[…]}`, and
which field carries the list is what actually happened. A node qualifies only
when it is a direct child of the `Agents` root **by id**, is a folder, and has
**no children counted structurally** — over every node in the tree, before any
path is rendered, because a folder whose only child carries a path separator
reads as empty to anything path-shaped while the recursive delete would still
take it (#671), and sqlite and mongodb both accept such a name. An ambiguous
root is a `409`, not a guess. Each removal announces its own
`WorkspaceChanged{removed}` (#327); a second run removes nothing.

`POST …/workspace/merge-duplicate-folders` (#759) repairs a tree a publish race
already broke. Two concurrent first publishes of one deliverable can each create
the folder they both read as missing; from then on every publish beneath that
path is refused as ambiguous, for every agent, until somebody edits the tree.
Stopping new races does nothing for a tenant already in that state, and on a
hosted tenant this route is the only way out of it — operator-triggered, never
automatic, for the reason the sweep above is.

A duplicate set is two or more sibling **folders** sharing a name, matched by
`parent_id` and never by rendered path. The oldest by `updated_at_millis` wins,
with the node id breaking a tie so a preview and the confirm behind it cannot
disagree. The losers' children are `rename_move`d into the winner — ids
preserved, so an artifact recorded against a published node still resolves —
and a folder-folder collision among those children becomes another merge in the
same run, iterating to a fixpoint. Nothing is renamed, nothing is overwritten,
and a loser is deleted only once it is **structurally empty in a fresh read**
taken after the relocations, so a publish landing mid-merge leaves its folder
standing rather than losing anything (#671 / #700 discipline).

Any collision involving a **file** is refused and reported: two files at one
path are two documents, and every rule for picking one discards somebody's work.
`?dry_run=true` answers `{"wouldMerge":[{id,name,intoId,moved:[{id,name}…],
removed}…],"residuals":[…]}` and touches nothing; the real call answers
`{"merged":[…],"residuals":[…]}`, and which field carries the folds is what
actually happened. `residuals` is on **both**, always — each entry is
`{id,name,parentId?,cause}` with `cause` one of `fileSharesTheName` (a note
shares the duplicated name, so the whole group was left alone), `fileInTheWay`
(the winner already holds a note of that name), or `treeMovedOn` (the store
refused the move because the tree changed; run it again). It is the half of the
answer that says whether the tree is actually repaired. Moves and deletes
announce `WorkspaceChanged` (#327); a second run with nothing left to do changes
nothing.

All three workspace `GET`s — tree, file and `search` — and the `POST` / `PATCH`
node bodies carry `createdBy` and `updatedBy` (#326), each
`{"kind":"seed"|"operator"|"agent",
"id"?}` with `id` present exactly when `kind` is `agent`. `createdBy` is fixed
at creation; `updatedBy` follows content writes only, so an operator rename does
not repaint an agent's authorship. The console renders the creator as a badge
and the last writer only when the two differ. Both fields are always serialized,
and a node predating the field reads back as `operator`. The `PUT` write route
stamps `operator`; agent writes stamp `agent{id}` from the agent's roster id,
which is fixed at agent-build time and never taken from tool arguments. Agents
reach the same tree through `workspace_list` / `workspace_search` /
`workspace_read` / `workspace_create` / `workspace_write`, and a created note has its default home
in the reserved `Agents/<agent-id>/` folder (#551) — a convention the persona
brief steers toward, not a boundary the routes enforce. `workspace_rename` and
`workspace_delete` (#671) are the exception: those two *are* bounded to
`Agents/<agent-id>/`, checked on the resolved node so an `id` argument refuses
exactly as its path would. Neither restamps authorship; a delete leaves any
artifact version that pointed at the node with a dangling `workspaceNodeId`,
which is the same state the `DELETE` route above produces and is read-guarded
before reuse. Boot scaffolds the
`Agents/` root empty; an individual `Agents/<agent-id>/` is minted the first
time that agent writes into it, and the `Desks/` root is minted whole the first
time a desk produces something (#645) — so a tree read on a fresh company shows
exactly one root and no member folders.

Team writes are an **operator overlay** persisted through the store, merged
into the manifest roster at read time — the version-controlled `company.toml`
is never rewritten. Overlay teammates are addressable: since issue #71 the
harness builds a real agent for each one, with the company-wide tool grant, no
cognition tier, and never the orchestrator.

`POST …/team` and the orchestrator's `add_agent` tool both **derive the roster
id from the display name** (issue #686): "Dana Designer" becomes
`dana_designer`, in the same snake_case grammar the manifest validator enforces
on a hand-authored `[[agent]].id`. They used to mint an opaque
`{millis}-{counter}` id, which #570/#552/#607 render as a workspace folder and
in search-hit paths — so half a company's tree read as
`Agents/019fad5ada20-000000000003/` beside `Agents/backend_engineer/`.

- **Collisions suffix, they do not refuse.** A slug already held by a manifest
  agent, another teammate, a desk id or name, or a reserved word (`operator`,
  `Agents`, `Desks`) becomes `<slug>_2`, `_3`, … Duplicate display names have
  always been accepted here, and an unsuffixed collision with a *manifest* id is
  worse than a refusal: the roster build skips it, so the teammate would persist
  and never materialise.
- **Minted once, never re-minted.** `PATCH …/team/{agentId}` renames a teammate
  and leaves the id alone; a name-keyed id would orphan its workspace folder,
  budget row, desk memberships and inbox on every correction.
- **Removal frees the slug**, so re-adding the same name takes the id back and
  **adopts the old `Agents/<slug>/` folder** — the intended remedy for a typo'd
  name, and not a way to get a clean slate.

Teammates carrying generated ids are **not migrated**: rewriting them would
rewrite the `WorkspaceOrigin` stamps issue #326 keeps honest, and every path
into their folders. They keep working, reachable by display name through
`crate::runtime::assignee`.

`GET …/team/{agentId}` is the **agent detail** read (issue #264). `GET …/team`
answers "who is on the roster"; this answers "what is this agent", and before it
existed neither the console nor any other client could reach an agent's tier,
its tool grants or its desk membership — the roster row carried none of them, so
checking what a company actually grants an agent was not possible from outside
the process.

The `tools` object is the reason the route earns its keep. It carries four
lists, because only the last is the answer:

| field | meaning |
|---|---|
| `requested` | the agent's own `tools` globs. **Empty means the company's standard grant**, not "no tools" |
| `companyAllow` | the `[tools].allow` ceiling |
| `deskAllow` | the union of the `tools` ceilings of the desks this agent sits on, already narrowed by `companyAllow`. **Empty means no desk narrows anything** |
| `effective` | what the agent actually holds, after all three levels |

The three ceilings shrink monotonically, so a console can render them as a
chain. Both "empty" readings above are the same trap in two places: an empty
grant list means *inherit*, never *nothing*, and a surface rendering it as "no
tools" has inverted the meaning.

`effective` is computed by the same `agent_scoped_grants` the harness calls when
it builds the agent, so the readout cannot drift from what is enforced. The
constructor takes the company record and agent id rather than a pre-extracted
allow-list, because the desk level cannot be derived from the company grant
alone — that shape is what makes "forgot to apply the desk ceiling"
unrepresentable at a call site rather than something three callers each have to
remember.

`GET …/tools/catalog` is the companion read: every grantable thing this company
has — built-in families, `[[mcp_server]]` entries, `[tools.composio]` toolkits —
in one vocabulary, each row carrying the exact grant token an operator would
write. See [runtime/tools.md](tools.md).
`isOrchestrator` is likewise resolved by the roster rule (a `tier =
"orchestrator"` agent, else the first declared) rather than read off `tier`, so
a company that tags nobody still names its orchestrator.

`PATCH …/team/{agentId}` edits an **overlay** teammate's `name`, `role` and
`description`. It is a patch: an omitted key is left alone, and `"description":
null` clears it — the two must stay apart or every partial save would erase an
agent's instructions. A blank `name`/`role` is `400`, an unknown teammate `404`,
and a **manifest** teammate is `409`: its fields live in the version-controlled
`company.toml`, and the console does not rewrite the blueprint. The one thing
that *is* changeable on such a teammate is its daily budget, and that works
because #343 modelled it as an override rather than as a rewrite. Every detail
response carries an `editable` list naming the fields this route will accept, so
a client renders read-only from the host's answer instead of re-deriving the
rule. `tier` and `tools` are read-only for both kinds: there is no override
layer for either, and adding one is a policy decision rather than a form field.

The two **budget** routes (issue #343) are how a teammate's `budget_usd_daily`
becomes changeable without a redeploy. Both are **admin-only** — a member gets
`403` and an unauthenticated caller `401` — and both stamp who set the cap and
when, surfaced as `budgetSetBy` / `budgetSetAtMillis` on the roster row. A
stored cap wins over the manifest, and the change is enforced on the teammate's
**next dispatch**: the harness fingerprints the override set alongside its other
freshness axes, so the roster is rebuilt before the next turn rather than at the
next process start.

`PUT` takes `{"budgetUsdDaily": <number|null>}` and the three cases stay apart
on the wire, which is the point of the route:

| body | effect |
|---|---|
| `{"budgetUsdDaily": 5}` | cap at $5/day |
| `{"budgetUsdDaily": 0}` | cap at nothing — a real cap, not "uncapped" |
| `{"budgetUsdDaily": null}` | remove the cap, beating a manifest cap |
| `{}` | **`422`** — an omitted key is never read as "remove the cap" |

A negative or non-finite amount is `400`; an unknown teammate is `404`.
`DELETE` drops the override so the manifest default applies again — distinct
from `PUT null`, and not expressible by it. `POST …/team` also accepts an
optional `budgetUsdDaily`, so a console-created teammate can be given a cap at
creation; only that form of the add requires an admin.

The three **policy** routes (issue #562) are the company-scoped twin of the
budget pair. Before them the autonomy tier lived only in `[policy].mode` and
nothing in the console read or wrote it, so an operator drowning in approval
cards could change it only by redeploying an edited `company.toml` — or, on a
hosted tenant with a read-only manifest snapshot, not at all.

`GET` returns the tier and always-ask list **in force**, what the manifest would
restore, whether an override is set and by whom, and the selectable tiers with
the host's own description of each (`POLICY_MODES` narrowed to tiers the console
has text for, so it never offers one the host would downgrade). `PUT` takes `mode`
and `alwaysApprove`, both optional and independent — `{"mode": "auto"}`
leaves the list alone, `{"alwaysApprove": []}` clears it (a real state, not a
reset), `{"mode": null}` stops overriding the tier, and `{}` is a **`422`**
because a body that sets nothing is never stored. An unknown `mode` is `422`
too, not accepted-and-downgraded, or the console would show a tier the gate was
not running. Both writes are admin-only and attributed. `DELETE` restores the
manifest's `[policy]` — its own verb, since a `PUT` of the manifest's current
values would pin them. The change takes effect on the company's **next turn**
(`ApprovalPolicy` is built per roster build, and this override is fingerprinted
alongside the other freshness axes). It survives a rebuild unless the seed's
`[policy]` itself changed: version control wins when it speaks, so tightening
`company.toml` clears a looser tier set here, and a redeploy that changed
nothing does not.

### Credential-bearing surfaces (feature-gated)

These write secrets to the `SecretStore` and expose only non-secret status.
The native OAuth compatibility routes below deliberately **do not** write a
credential: the old credential was unreachable by agents.

```text
GET    …/credential                         whether the company has its own key + which tier it presents
PUT    …/credential                         set / rotate / clear the company's TinyHumans key  [admin]
PUT    …/domain                             set the custom domain
POST   …/domain/verify                       server-side DNS check
PUT    …/smtp                               store SMTP credentials (secret store)
POST   …/smtp/test                           send a test email
POST   …/connections/{provider}/start        retired native OAuth bridge → 410 JSON until 2026-09-30  [feature: oauth]
POST   …/connections/{provider}/disconnect   drop a legacy stored OAuth token  [feature: oauth]
GET    /api/v1/oauth/callback                retired browser landing page → 410 HTML until 2026-09-30  [feature: oauth]
```

`…/credential` is the company's **one** TinyHumans key, presented by every
surface wired to it (**Composio today**) — see
[`credentials.md`](credentials.md) for the resolution order, the rotation
guarantee, and which surfaces are deliberately outside it.

### Retired native OAuth callback

`/api/v1/oauth/callback` stays reachable for a browser that began consent
immediately before a deploy. It returns a non-caching `410 Gone` HTML page that
says the authorization was not saved, why native OAuth cannot make agents able
to use the provider, and to use Composio instead. It ignores the provider's
`code` and `state` rather than exchanging or storing them.

`POST …/connections/{provider}/start` is likewise a `410 Gone` JSON response
with stable code `native_oauth_retired`, an explanatory message, and
`removalAfter: "2026-09-30"`. Both temporary endpoints send `Deprecation:
true` and a `Sunset: Wed, 30 Sep 2026 00:00:00 GMT` header. #1023 removes the
bridge after the cache compatibility window established by #979; it keeps
Disconnect and the read projection so tenants can release credentials written
before #828.
