# Product analytics: the OpenPanel wire contract

Split out of [analytics.md](analytics.md), which owns the policy — which
installs report, under what identity, and what a payload may contain. This file
owns the one HTTP call: how the body is delivered, how the credential is
presented, and what the collector does with each.

`POST {OPENCOMPANY_ANALYTICS_ENDPOINT}`, one request per event, JSON body.

The details below were read from OpenPanel's own source at commit
`3060ca10213693cf0385be2713c8743d16733a2b` — `packages/validation/src/track.validation.ts`,
`apps/api/src/utils/auth.ts`, `apps/api/src/controllers/track.controller.ts`,
`packages/constants/index.ts` — because its published docs are thinner than the
schemas and disagree with them in at least one place (they describe rate
limiting on `/track` that the router does not register).

**Headers.** Authentication is the client id alone, and it must be a UUIDv4 or
the collector answers `401` before it looks at anything else. The operator's
clients are configured with **"ignore CORS and secret"**, so no
`openpanel-client-secret` is read or sent:

| Header | Value |
|---|---|
| `openpanel-client-id` | `OPENCOMPANY_ANALYTICS_CLIENT_ID` — a **write** or **root** client |
| `openpanel-sdk-name` | `opencompany` |
| `openpanel-sdk-version` | the crate version |

The client id is marked sensitive on the `HeaderValue`, which keeps it out of
`reqwest`'s own `Debug` and out of HPACK's shared table on HTTP/2.

A credential in a header rather than in the body is the quiet improvement in
this change. Mixpanel wanted its token stamped into every event's property bag,
so the transport reached into a rendered payload and mutated it — which meant a
captured body, a recorded event or a test fixture *could* carry it, and nothing
but care stopped one. There is now no code path that could put a credential in a
payload, and `no_credential_reaches_the_request_body` asserts it on the wire.

**Body.** A discriminated union on `type`. Only the `track` variant is used:

```json
{
  "type": "track",
  "payload": {
    "name": "turn_finished",
    "profileId": "i_0123456789abcdef0123456789abcdef",
    "properties": { "outcome": "ok", "duration_ms": 12, "__timestamp": "2026-09-08T12:00:00Z" }
  }
}
```

`name` is `z.string().min(1)` plus the refusals listed above; `properties` is
`z.record(z.string(), z.unknown())`; `profileId` is a string or a number.
`groups` exists and is unused here.

**There is no batch endpoint and no array body.** `/track` takes one object. The
only bulk path, `POST /import/events`, refuses a `write` client outright and
inserts raw ClickHouse rows, bypassing sessions, geo and the queue — it is a
migration tool, not a batching one. So the transport that used to POST a whole
batch now issues one request per event; see [the drain](#failure-is-silent-and-the-drain-gives-up-early).

**Timestamps are arrival time unless the body says otherwise, and this client
says otherwise.** OpenPanel's `timestamp` is not a field in the track schema at
all; the event time is read from `properties.__timestamp`, which the server then
strips before storage. This matters because events are queued for up to thirty
seconds — longer after an outage — so arrival time would record a burst of turns
as all having happened at the moment a drain finally succeeded. Each event is
stamped when it is tracked, second-precision RFC-3339 UTC, from the crate's one
date formatter rather than a second copy of the arithmetic.

Two server-side clamps apply to that field, and neither is a problem here: a
value more than **60 seconds in the future** is discarded for arrival time (this
process only ever stamps the past), and a value more than **15 minutes in the
past** marks the event a backfill, which makes it session-less on the server
(this is a server-side client with no browser session, and events are attributed
by `profileId`).

**Responses.** `200` with `{"deviceId", "sessionId"}` on success. `202` also
means accepted-and-dropped — bot suspicion, or a cloud wind-down — and is
treated as success here because a `2xx` is the collector's answer either way.
`401` is a plain-text body, not JSON. It is one of **three** classes this
transport treats as more than a dropped event — the others are a `3xx` and a
`429`/`5xx` — because none of them is an answer about the body that was posted:
see below. Two other behaviours are worth
knowing and neither applies to this client, which sends no `Origin` header:
requests with `ip`, `origin` and a client id are de-duplicated by content hash
inside a 100 ms window, and a verified secret exempts a request from bot
detection.

## Failure is silent, and the drain gives up early

`Tracker::track` is synchronous, infallible and returns nothing, so a call site
cannot await a network or branch on a telemetry error. A dead collector drops
events after one `debug!` line.

The queue is what makes that possible without batching. Losing the batch
endpoint invites the obvious simplification — drop the queue and fire a request
from `track` itself — and it is the wrong trade twice over: `track` is on a
turn's hot path and cannot await, so firing from it means spawning a task per
event, which is unbounded concurrency against a collector this process does not
control, with no back-pressure and no ceiling on memory. The queue bounds both.
At most 500 events exist at once — if the collector is unreachable long enough
to fill it, the right outcome is losing telemetry, not a tenant container — and
at most one drain runs at a time.

**A transport failure abandons the rest of the drain.** Each request has its own
5s timeout, so a full queue against a black-holing collector would be
`500 × 5s`: over forty minutes of proving the same thing five hundred times,
during which the shutdown flush is blocked behind the same lock and the
container's `SIGTERM` budget is long gone. The collector is down, the remaining
events are going nowhere, and the next interval tries again with whatever has
accumulated since. `an_unreachable_collector_costs_one_timeout_for_the_whole_drain`
asserts it on connections a black-hole listener actually accepted — one, not
three — rather than on elapsed time, which would be a flaky test.

**A per-event HTTP status does not.** A `400` for a body the collector will not
take, a `404` for a `/track` path typed wrong — the events behind it may be
perfectly good, and treating those like a transport failure would let one
malformed event silence a whole drain.

**Three status classes are not per-event answers, and each abandons the drain.**
The test is always the same question: *is this the collector's answer about the
body that was posted, or about something that will be equally true of every
event behind it?*

| Status | Why it is not per-event | Said how |
|---|---|---|
| `401` | the collector's verdict on this process's **credential** | `warn!`, once |
| `3xx` | the collector's verdict on the **endpoint** — and this client follows no redirect, so it will never resolve | `warn!`, once |
| `429`, `5xx` | the collector saying it cannot take **traffic** right now | `debug!` |

Carrying on through any of them would fire up to 500 requests every thirty
seconds — a thousand a minute at the operator's own collector — to learn
something already known. For `429`/`5xx` that is worse than pointless: it aims a
burst at a service that has just said it is overloaded, so an analytics client
becomes the thing keeping the operator's collector down.

The `warn!`/`debug!` split is the transient/permanent rule, not a judgement of
severity. A refused credential and a redirecting endpoint resolve themselves
**never**: every event for the rest of the process's life is dropped, boot said
"reporting to …", and the only trace would be a line nobody has enabled. So each
is a `warn!`, said **once** — the condition is permanent, and repeating it would
drown a busy tenant's log. The credential warning names the two variables to fix
and the UUIDv4 requirement on the client id, and never the credential itself; the
redirect warning never prints the `Location`. A `429` or a `5xx` is a collector
restarting or under load, resolves itself, and the next interval tries again, so
it gets the `debug!` #1739 settled on.

`a_refused_credential_stops_the_drain`,
`a_collector_that_cannot_take_traffic_stops_the_drain`,
`a_refused_event_does_not_stop_the_drain` and
`a_per_event_refusal_still_does_not_stop_the_drain` are the same collector and
the same three events, a status code apart, with opposite outcomes. None of them
means much without the others: the first pair without the second would also pass
for a client that gave up on any refusal at all.

## The tail a cancelled drain loses

There is a fourth way a drain ends, and it is the only one that is not a branch
the drain takes. The shutdown flush is wrapped in a `tokio::time::timeout` at its
call site (`src/bin/opencompany.rs`, bounded by `server::shutdown::flush_budget`,
**at most 2s**), so when the budget runs out the drain future is **dropped**
mid-flight. The events it had already taken off the queue are gone.

That is new with OpenPanel and follows directly from there being no batch
endpoint. Mixpanel's whole queue left in one request, so 2s was never the binding
constraint. One request per event means a queue of `n` costs `n` sequential round
trips, and at a very ordinary 25 ms each the budget is spent after about eighty —
so a busy tenant loses the tail of its telemetry on every restart.

**The loss is accepted; the silence is not.** `CancelledDrain` is armed with the
number of events still unsent, disarmed by every deliberate exit above, and on
`Drop` reports the count as a `warn!` and records it where a test can read it
(`a_cancelled_drain_reports_the_tail_it_lost`, with
`a_drain_that_finishes_reports_nothing_lost` as its control). Before it, those
events vanished behind a `debug!` at the call site that named no count at all.

Sending with bounded concurrency would fit roughly `concurrency ×` more events
into the same budget, and is **declined**: it is paid for out of the guarantee
above, because a drain issuing eight requests at once against a black-holing
collector opens eight connections rather than one, and
`an_unreachable_collector_costs_one_timeout_for_the_whole_drain` asserts exactly
one. Multiplying the hammering of an unreachable collector to shorten a shutdown
is the wrong direction, and #1739 is explicit that telemetry loss beats a
shutdown overrun — the budget exists because an overrun buys a `SIGKILL`
mid-turn. The real fix is a batch endpoint on the collector, which OpenPanel does
not have.

## Where the credential is allowed to travel

The client id is a **default header on every request**, which is what
makes the two rules below load-bearing in a way they were not under Mixpanel: a
token in a request body, to one fixed `https` address this crate chose, had no
configuration that could redirect or downgrade it.

**A `3xx` is treated like a `401`, and the client follows no redirect.** The
client is built with `reqwest::redirect::Policy::none()`. Its default policy
follows up to ten hops, and its cross-origin sanitization
(`redirect.rs::remove_sensitive_headers`, reqwest 0.12.28, read rather than
assumed) strips exactly `Authorization`, `Cookie`, `cookie2`,
`Proxy-Authorization` and `WWW-Authenticate`. `openpanel-client-id` is none of
those, so one `302` — a reverse proxy sending unauthenticated
callers to an SSO host is the ordinary way one arrives — handed this instance's
long-lived write credential to a host the operator never named.
`HeaderValue::set_sensitive` is not a defence and looks like one: it governs
`Debug` output and HPACK indexing, not redirect handling.

That sanitization also compares only host and port, never the scheme, so an
`https` endpoint redirecting to `http://` on the same host would have carried
the credential across in cleartext.

A same-origin policy would also be safe, but it is a predicate to keep correct
rather than an invariant to state, and all it buys is a collector that 301s
`/track` to `/api/track` — an endpoint the operator can type correctly once.
Following none of them makes "the credential only ever goes to the configured
endpoint" a property of the client.

Be precise about what that costs. The request **does** reach the configured
endpoint, carrying the credential, exactly as the operator asked; it is the
redirect *destination* that receives nothing. So no event is ever delivered
while the endpoint redirects — the collector answering `3xx` is not the one
storing events — and the drain abandons like a `401` and warns **once**, naming
the variable to fix. It never prints the `Location`: that is a URL the
*collector* chose, and a URL is exactly where a credential hides — the same
reason `loggable_send_error` strips the URL from a transport error.

`a_redirect_never_carries_the_credential_to_another_host` points the tracker at
a collector that `307`s to a second one on another port and asserts the second
was never touched. Its control,
`the_redirect_destination_would_have_recorded_the_credential`, sends to that
same second collector directly and asserts it records three requests *and* the
client-id header — without it, the zero would also hold for a collector that counts
nothing.

**The endpoint itself must be `https`, or loopback.** Plain `http` to a
non-loopback host would put the client id on the wire in cleartext once per event
([CWE-319](https://cwe.mitre.org/data/definitions/319.html)), so it resolves to
silence with its own reason rather than reporting. Loopback is the exception
because that traffic **does not leave the host**: it goes over the host's
loopback interface and reaches no link anyone else is on. That is a narrower
claim than "nobody can see it", deliberately — a sufficiently privileged local
process can capture `lo`, and anything that has that access on a tenant's host
already has the environment the credential was read from. The rule, its cost, and
why it is silence rather than a warning are in
[analytics.md](analytics.md#why-https-is-required-and-why-loopback-is-the-exception).

**And a cleartext endpoint never goes through a proxy**, because otherwise that
exception protects nothing. `reqwest`'s builder defaults to
`auto_sys_proxy: true`, which reads `HTTP_PROXY`/`ALL_PROXY` and takes exclusions
**only** from `NO_PROXY` — hyper-util 0.1.20's matcher has no implicit carve-out
for `localhost` or `127.0.0.0/8` (read, not assumed). So on a host with a proxy
configured and no matching `NO_PROXY`, `http://localhost:3000/track` went to the
proxy in cleartext with the credential header on it, and the endpoint check
prevented nothing. Measured rather than reasoned about: with the fix reverted,
`a_loopback_endpoint_never_goes_through_a_system_proxy` records **2** requests at
the stand-in proxy and 0 at the collector.

So an `http` endpoint builds with `ClientBuilder::no_proxy()`, which makes "it
does not leave the host" true by construction rather than a prediction about the
operator's environment — the same move as `Policy::none()` for redirects.

All of which holds only for endpoints that came through `resolve`, so that is
the only route there is: `HttpOpenPanelTracker::new` is **crate-private** and
`build` — which takes a `&Decision`, and a `Decision::Report` is what `resolve`
produces — is the way a tracker is obtained. A `debug_assert!` calling
`config::is_secure_endpoint` (not a restatement of the rule; one implementation,
no second reader to drift) catches the same mistake arriving from inside the
crate later. `the_transport_refuses_an_endpoint_that_never_passed_resolve` and
its control assert both directions.
`https` keeps its proxy support deliberately: a proxied `https` request is a
`CONNECT` tunnel, so the proxy learns host and port and never sees a header, and
egress-restricted networks need it to reach a collector at all. The scheme is the
whole test, because by the time a `Report` exists, `http` **implies** loopback.
