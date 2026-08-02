# Server Module

The server module owns the Axum HTTP surface. The base routes are:

- `GET /healthz`
- `GET /spec`
- `GET /tiny`

Operator chat and approvals live under `/api/v1/...` (see `server::operator`),
and feedback under `server::feedback`. Add future API routes as focused handler
groups rather than wiring behavior directly in the binary entrypoint.

## Read plane — `server::graphql`

The console's reads are one async-graphql query surface (`POST /graphql`, plus
a `GET /graphql` GraphiQL explorer). The schema is **built once at startup**
(`build_schema`) and stored on `AppState`; each request injects its resolved
`GqlAuth` principal via request data. It is query-only — REST owns writes.

The module is split one file per surface: `mod.rs` (the `Company`-rooted
`QueryRoot` — `companies`, `company(id)`, `skillRegistry`), `auth.rs`
(`GqlAuth`, claim resolution + `visible_companies`), `company.rs` (the
aggregation object every view fetches through), `pagination.rs`, and one
resolver file per view (`tasks`, `workspace`, `memory_facts`, `skills`,
`inbox`, `workflows`, `usage`, `finances`, `connections`). `schema.graphql` is
the checked-in SDL snapshot (the read contract); `graphql::sdl()` regenerates
it and a snapshot test guards drift.

## Write plane — `server::ops`

Console writes are the `server::ops` router family. Each route is registered
under **both** scope forms — `…/companies/{id}/…` and the `…/company/…`
prosumer alias — by the `scoped` helper; the `ScopedCompany` extractor resolves
the target runtime and enforces authorization per form (platform-or-operator +
address check for `{id}`, operator + `sole()` for the alias).

| Surface (`ops::*`) | Routes |
|---|---|
| `tasks` | `POST …/tasks`, `PATCH`/`DELETE …/tasks/{id}` |
| `memory` | `POST …/memory`, `DELETE …/memory/{id}` (journals `MemoryFactDeleted`) |
| `workspace` | `POST …/workspace`, `PUT …/workspace/file/{id}`, `PATCH`/`DELETE …/workspace/{id}` |
| `skills` | `POST …/skills`, `POST …/skills/{slug}/install\|uninstall`, `PUT …/skills/{slug}` |
| `team` | `POST …/team`, `DELETE …/team/{id}`, `PUT …/team/{id}/inbox` (overlay; roster-only in v1) |
| `mail` | `POST …/inboxes/{key}/read` |
| `inbox` | `POST …/inboxes/ingest` (HMAC-signed inbound email) |
| `domain` | `PUT …/domain`, `POST …/domain/verify` |
| `smtp` | `PUT …/smtp`, `POST …/smtp/test` |
| `connections` (feature `oauth`) | `POST …/connections/{provider}/start\|disconnect`, `GET /api/v1/oauth/callback` |
| `workflows` | `POST …/workflows`, `GET …/workflows`, `GET …/workflows/runs`, `POST …/workflows/cron/preview`, `GET …/workflows/{wid}`, `POST …/workflows/{wid}/run` |

### Reading a trigger's cron back (issue #262)

`POST …/workflows/cron/preview` answers what a 5-field expression means and when
it next fires. It exists because a schedule's *dangerous* failure is the one
that validates: `0 9 * * *` and `9 0 * * *` are both valid and nine hours apart,
and the dialect is always UTC — so an author in IST who wants a 9am report
writes `0 9 * * *` and gets one at 14:30 local. No validation can catch either
mistake, because neither expression is wrong.

```bash
curl -X POST "$HOST/api/v1/company/workflows/cron/preview" \
     -H "Authorization: Bearer $TOKEN" \
     -H 'content-type: application/json' -d '{"expr":"0 9 * * MON"}'
```

```jsonc
{ "description": "Every Mon at 09:00 UTC",
  "next": [1786007000000, 1786611800000, 1787216600000] }
```

`description` is `null` for a shape the humaniser declines to paraphrase (a
restricted month or day-of-month, say) — the fire times still state the schedule
exactly, so a `null` description is a designed answer rather than a failure.
`next` is epoch millis, which is what lets the console render each fire time in
UTC *and* in the viewer's own zone from one number that cannot disagree with
itself.

**A malformed expression is a 200**, carrying the parser's message:

```jsonc
{ "error": "cron `every day` needs 5 fields (minute hour day month weekday), found 2" }
```

That is deliberate. The console previews while the author is still typing, so a
half-written expression is the normal live state, not an exception — and the
console's HTTP client throws on any non-2xx, so a 400 per keystroke would force
`try`/`catch` as ordinary control flow. The rejection that matters is unchanged:
`POST …/workflows` still validates the schedule and refuses to save a bad one.

Optional `"after": <epoch millis>` pins the instant the fire times are counted
from; it defaults to now and exists so tests need not assert against a moving
clock.

### Workflow runs and report delivery

A workflow's terminal `output` node may carry a `destination` — `owner`,
`email`, or `channel` — saying where its report goes once the run finishes. It
rides the create body and the read shape under the same key, and the model
type is reused verbatim in both directions (`kind` / `target` are single words,
so there is no camelCase mirror to drift from).

Delivery itself is **not** a route concern. It runs host-side in the shared
`WorkflowRunner` path (`src/workflows/delivery.rs`) once the engine returns,
because the orchestrator's `run_workflow` tool and the trigger scheduler drive
that same port — and a scheduled run is exactly the case where nobody is
watching the console. An **on-demand** run's response therefore carries
`deliveries`: one row per attempt (`sent` / `skipped` / `denied` / `failed`)
with an operator-readable reason. A delivery failure never fails the run, so on
that run the list is where an operator learns a report did not go out; an
unwired runtime writes a loud `failed` row rather than skipping silently.

A **scheduled** run is not persisted, so its delivery outcomes are not surfaced
yet. The scheduler logs each undelivered report and drops the run value — see
`src/runtime/workflow_scheduler.rs`. That makes a failed scheduled delivery
diagnosable in the host's stdout, which is not the same as operator-visible.
Surfacing those outcomes is issue #228; the durable record it needs is the
first-class `Run` tracked by issue #242.

Authoring a destination and reading the result back:

Both routes go through `ScopedCompany`, so both need an operator credential —
`$TOKEN` below is the bearer token the `Authorization` header is parsed from.

```bash
# Create a graph whose output node reports to the company's admins.
curl -X POST "$HOST/api/v1/company/workflows" \
     -H "Authorization: Bearer $TOKEN" \
     -H 'content-type: application/json' -d '{
  "id": "weekly_digest",
  "name": "Weekly digest",
  "nodes": [
    { "id": "start", "kind": "trigger", "name": "Monday 09:00", "schedule": "0 9 * * MON" },
    { "id": "write", "kind": "agent",  "name": "Draft it", "agent": "chief_of_staff" },
    { "id": "done",  "kind": "output", "name": "Owner summary",
      "destination": { "kind": "owner" } }
  ],
  "edges": [ { "from": "start", "to": "write" }, { "from": "write", "to": "done" } ]
}'

# Run it now. `deliveries` says what happened to the report.
curl -X POST "$HOST/api/v1/company/workflows/weekly_digest/run" \
     -H "Authorization: Bearer $TOKEN" \
     -H 'content-type: application/json' -d '{"input":{"request":"last week"}}'
```

```jsonc
{
  "output": { "nodes": { "done": { "items": [ { "json": { "text": "…" } } ] } } },
  "pendingApprovals": [],
  "deliveries": [
    { "node": "done", "kind": "owner", "target": "ada@acme.test",
      "status": "sent", "detail": "emailed the company's admin" }
  ]
}
```

Swap `{ "kind": "owner" }` for `{ "kind": "email", "target": "ada@example.com" }`
and a recipient who has never written in comes back as
`"status": "skipped"` with the reason, having sent nothing:

```jsonc
{ "node": "done", "kind": "email", "target": "ada@example.com",
  "status": "skipped",
  "detail": "this recipient has never written to the company, so a workflow may not open the conversation — send once from the inbox first" }
```

The gating is fail-closed and differs per kind. `owner` resolves server-side to
the company's active admins (the graph names nobody) and falls back to the
`operator` channel. `channel` must name an adapter the deployment already
wired. `email` is the only kind that can address an outsider, and it needs
**both** an `email` grant in the manifest's `[tools].allow` **and** an
established inbound thread from that address — the same rule the agent send
path applies; a cold recipient is skipped and reported, never mailed. Note the
grant half is satisfied by default: since #230 an unset `[tools].allow` defaults
to `["*", "media", "composio"]` and `*` covers `email`, so on a
default-configured company the established-thread rule is the gate actually
holding the line. Narrow `[tools].allow` explicitly to close the first one.

Every credential-shaped value written here lands in the `SecretStore`; the
responses expose only non-secret status. The networked seams (DNS, SMTP, OAuth
exchange) are dependency-inverted behind traits carried on `ConnectionsRuntime`
and default to empty (offline) — a surface whose seam is absent returns
`404 {"code":"not_wired"}`, which the console degrades gracefully.

## tiny.place A2A inbound + discovery (`tinyplace` feature)

Behind the `tinyplace` feature the server mounts the agent-to-agent surface
(`server::a2a`). With the feature off, none of these routes exist and the
default build links no crypto.

| Route | Purpose |
| --- | --- |
| `POST /a2a/{handle}` | JSON-RPC `tasks/send` from a counterparty agent |
| `GET  /a2a/{handle}` | the company's Agent Card (directory record) |
| `GET  /a2a/{handle}/skill.md` | human/agent-readable priced-skill catalog |
| `GET  /.well-known/agent-card.json` | the sole company's card (prosumer) |
| `GET  /companies/{handle}/.well-known/agent-card.json` | a named company's card |

`POST /a2a/{handle}` enforces the trust boundary in a fixed order before any
work reaches cognition:

1. Resolve a **discoverable** company (`[place].discoverable = true` with a
   matching `[company].handle`); a miss is `404`.
2. Verify the SIWX `Authorization` header (skew window + single-use replay
   protection via a host-global nonce cache). A bad/missing header is `401`.
3. For a skill priced above `0.00`, require a valid x402 authorization; without
   one the response is a `402` challenge naming the amount and the company's
   own tiny.place address.
4. Sanitize the counterparty payload (a minimal promptguard pass — control
   characters are stripped) before it becomes an `A2aTaskReceived` event and
   drives exactly one cycle. Paying customers run under the same approval gates
   as any other stimulus.

An unreachable tiny.place backend maps to `503`; any other transport failure is
`502`.

## Enable discovery for all companies

Every company declares its own discoverability in its manifest:

```toml
[company]
name = "Acme SEO"
handle = "acme"

[place]
discoverable = true
skills = [{ id = "seo.audit", price_usd = "25.00", description = "Full audit" }]
```

To opt **every** loaded company into going public regardless of its manifest,
pass `serve --discoverable`. It marks each company discoverable and synthesizes
a `@handle` (a slug of the company name) when one is missing, so Agent Card
generation and validation succeed:

```bash
cargo run --features tinyplace --bin opencompany -- \
  serve --discoverable \
  --company companies/agentic_law_firm \
  --company companies/agentic_marketing_agency
```

At boot each discoverable company runs the going-public flow (lifecycle step 3):
load-or-generate the Ed25519 keypair, `ensure_registered`, then publish the
Agent Card — all best-effort. An unreachable tiny.place degrades the company to
"private" with a warning and never blocks or fails boot.

Relevant configuration:

- `TINYPLACE_API_URL` — tiny.place economy base URL (default
  `https://api.tiny.place`).
- `OPENCOMPANY_PUBLIC_URL` — public host base embedded in published Agent Card
  endpoints. When unset, the endpoint falls back to `http://{bind}`.

## Inbound channel webhooks (`hooks.rs`)

`POST /hooks/{company}/telegram` is the **optional hosted fast-path** for
Telegram inbound, not the default. Telegram can only deliver to it from the
public internet, so it is surfaced only when `OPENCOMPANY_PUBLIC_URL` is a
public **https** URL (`AppConfig::public_webhook_base_url`); otherwise
`GET …/channels/telegram` reports `webhookUrl: null` and
`POST …/channels/telegram/webhook` is refused with `400`, rather than handing an
operator a `http://127.0.0.1:<port>/hooks/…` URL that can never receive a
delivery. Everywhere else — local and most self-hosted deployments — inbound
arrives over `getUpdates` long-polling
([`runtime::telegram_poller`](../runtime/README.md#background-listeners)) and a
bot token is the whole setup: no webhook secret, no `setWebhook`, no public URL.
