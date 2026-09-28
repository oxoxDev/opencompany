# Human users

Each company has its own directory of **users**: the humans who collaborate
with its agents through chat. Users are not billing subjects — the platform's
Node backend owns accounts and money, and nothing here knows about either. A
user exists inside exactly one company.

This is distinct from, and weaker than, the machine credentials in
[config.md](config.md): a user is a collaborator, never an operator.

## The shape

Everything below describes the default `email` mode. A company may instead sign
people in with a **wallet**, or have **no sign-in at all** — see
[Sign-in modes](auth-modes.md), which covers the choice, where it is configured,
and what changes under each. The roster, sessions, invites, and revocation on
this page are shared by `email` and `wallet`; only the proof of identity
differs between them. `none` mode has none of this — no invite flow, no session,
no roster beyond its single implicit local owner.

| Concern | Answer |
|---|---|
| Sign in | A password, **or** a magic link (256-bit token, 15-minute TTL, single use) where the host can mail one |
| Session | Opaque 256-bit token in an `HttpOnly; SameSite=Lax; Path=/` cookie, 14-day absolute TTL |
| Access | **Invite-only.** An uninvited address cannot log in |
| Bootstrap | The manifest's `[users] admins` list, `OPENCOMPANY_ADMIN_EMAIL`, or — on a company nobody has joined — whoever claims it first ([First admin](#first-admin)) |
| Mode | `[users] mode` — `email` (default), `wallet`, or `none`. See [Sign-in modes](auth-modes.md) |
| Roles | `admin` (may invite and administer) / `member` |
| Profile | Name and icon, self-served through `PATCH …/auth/me` — see [avatars.md](avatars.md) |

An `admin` is also what the write plane means by authority over the company: the
routes that decide what the company reaches the world as — its Composio and
provider connections, its inference provider and key, its mail identity, its
its MCP servers — require one. See
[the write plane](../../modules/server/authority.md)
for the table and the reasoning. A `member` reads those surfaces but does not
change them.

## Storage

Three ports, all keyed by `CompanyId` like every other:

- `UserStore` — users and invites (they share the email keyspace, so "invited"
  and "joined" are two states of one address and must stay consistent).
- `SessionStore` — live sessions, looked up by token hash.
- `LoginCodeStore` — pending magic-link codes.

Sessions and codes are **credential material**, which is why they are separate
from the directory: they carry their own expiry/purge rules and must never join
the export path (`opencompany export` covers company/event/memory/context
only — do not add them).

**Only hashes are stored.** The plaintext of a session token or login code
exists in exactly one place — the browser's cookie jar, or the email that was
sent — and is never written down. Lookup is *by* the hash: the presented secret
is hashed and that is what's queried. Nothing compares a secret, which is why
there is no constant-time comparison anywhere in this flow. Forging a hit would
need a SHA-256 preimage.

Passwords are Argon2id in PHC format, so each hash carries the parameters it
was made with and old hashes keep verifying after the cost is raised.

## Two isolation guarantees

A session for company A is refused for company B **twice over**:

1. **The storage partition.** `SessionStore::find_by_token_hash` takes a
   `CompanyId`; A's row simply is not in B's partition. There is no filter to
   forget — the conformance suite asserts this on all three backends.
2. **The principal check.** `GqlAuth::authorize` compares
   `UserPrincipal.company`. Belt and braces, in case a cache ever bypasses (1).

The cookie is named `oc_session_<company>` rather than a constant. Hosted mode
serves one company per container, but local development serves many from one
origin, where a constant name would mean signing into B silently destroys your
session for A. The name also lets the GraphQL handler find the company, whose
query argument lives in the request body and is unavailable to extractors.

A company id that cannot safely name a cookie (anything outside
`[A-Za-z0-9_-]`) cannot hold a session: `CompanyId::new` validates nothing, and
`evil;Path=/` would otherwise choose the cookie's attributes.

## Users cannot become operators

`resolve_claims` reads machine credentials and **cannot return
`GqlAuth::User`**. Every operator/platform write route resolves through it, so
a session cookie is unreachable on the write plane — not because each route
checks, but because the type it receives cannot represent a human. Routes that
mean to serve humans opt in by calling `resolve_principal` instead; today that
is the login routes, the admin routes, and chat.

This matters more than it looks. The REST extractors flatten `Dev | Operator`
into `Some(Self(None))`, and `authorize_address` reads `None` claims as *allow
everything*. A user mapped onto `None` would silently have become an operator.

## Bootstrap: `[users] admins`

Access is invite-only, so someone must send the first invite — and there is no
operator token to do it with ([config.md](config.md) explains why). The company
manifest is the root of trust:

```toml
[users]
admins = ["ada@example.com"]
```

Listing an address does not create an account; it makes the address *eligible*.
Redeeming a link mints the user as an admin. Removing an address stops it
bootstrapping again but does not delete an account it already created — use the
admin routes.

Manifest admins appear in the invite list as synthetic `manifest:` entries.
Revoking one is refused: the manifest would re-grant it on the next login, so
succeeding would be a lie.

## Bootstrap: `OPENCOMPANY_ADMIN_EMAIL`

A company the *platform* provisions has an empty `[users] admins`. The person
who asked for it is recorded on the control plane's tenant row, which the
manifest never sees — so nobody is eligible, and there is no operator token to
send the first invite with. The company is unreachable by the human who created
it (issue #321).

The deployment therefore may name **one** more standing admin through the
environment:

```sh
OPENCOMPANY_ADMIN_EMAIL=ada@example.com
```

It is the same grant as a manifest entry, not a second kind of one:

- listing the address makes it *eligible*; only redeeming a link mints the user,
  as an `admin`
- unsetting it stops future bootstrapping and does not delete an account it
  already created
- it is normalized with the same `normalize_email`, so case and surrounding
  whitespace do not matter
- unset, empty, and whitespace-only are one behaviour — no grant at all. The
  platform renders the variable for every tenant, so a tenant with no recorded
  creator must be indistinguishable from a deployment predating the variable.

It admits exactly that address. It is **not** "trust whoever the platform says
owns this instance": the root of trust stays with what the company is
configured with, never with an assertion made at sign-in time.

The address appears in the invite list as a synthetic `platform:` entry, and
revoking it is refused the same way — the error points at the variable rather
than at `[users].admins`. An address named in both places renders once, as
`manifest:`: that is the grant that outlives the deployment's variable.

**Recovery for a company already provisioned with an empty admin list**: set the
variable on the instance and restart it. The workload reads it at boot, and
eligibility is evaluated per login rather than cached, so the next link request
from that address succeeds.

### First admin

Every path above makes an address *eligible*; none of them gives it a
credential, and a self-hosted host — `docker compose up`, a laptop — usually
has **no mail transport** to deliver one. So while a company has **no users at
all**, its sign-in screen offers the first person in the admin account instead
of a form nobody can pass:

```text
POST …/auth/claim  {email, password}   → session cookie + the user
```

The person picks the login and the password (the console generates one and
shows it in the clear, editable), the host mints them as an `admin`, and they
are signed in. `email` need not be a mailbox: on a host with no mail it is a
username, and `admin` is a fine one. The route closes for good the moment the
first user exists — it answers `409 already_claimed` from then on — so it
admits exactly one person, after which the roster is the only way in. `GET
…/auth/config` reports the state as `claimable`.

Where the deployment already named its first admin — a manifest
`[users].admins` entry or `OPENCOMPANY_ADMIN_EMAIL` — only that address may
claim (`403 not_the_named_admin`). A stranger who reaches a provisioned tenant
before its owner must not be able to take it. Where nothing names anybody, the
first visitor chooses: that is the same first-run claim every self-hosted
product with a login screen makes, and the alternative was a shell command
nobody running `docker compose up` had been told about.

The [first-run setup wizard](setup.md) does the same thing one step earlier:
its "You" step asks for the login *and* a password, the apply creates the
account, and the wizard signs the operator in with it.

### Recovery without a mailbox

A company that already has users, on a host with **no mail transport at all**
(or one that is failing), can still strand an address: with the claim closed
and no admin reachable, no one can set a password from the console
(issue #1718). For that case the host issues the first password directly:

```sh
opencompany issue-password --company <id> --email ada@example.com
```

It reads storage directly, so its authority is possession of the host and its
data — which an operator has and an HTTP caller never does. It admits exactly
the standing grants above — a manifest `[users].admins` entry or
`OPENCOMPANY_ADMIN_EMAIL` — and makes one usable without mail; it does **not**
create one. Committing revokes the user's existing sessions and pending login
codes first, and by default flags the password for replacement, matching the
admin temporary-password route ([Passwords](#passwords)). It requires the
effective email auth mode, takes a company id in the bare or namespaced
`<tenant>--<id>` form in shared-database mode — a bare id is namespaced to the
current `OPENCOMPANY_TENANT_ID`, and one carrying a different tenant's prefix is
refused — reads the password from stdin to
keep it out of argv, and on the filesystem store holds the same data-root lock
as `serve`, so it fails cleanly if a server is running on that root. See the
[CLI reference](../../gitbooks/developers/cli.md) for the full semantics.

## Routes

Login routes are **unauthenticated by construction** (`PublicCompany`), because
asking for a link is what someone does when they have no credential. Both
addressing forms work: `/api/v1/companies/{id}/…` and `/api/v1/company/…`.

| Route | Purpose |
|---|---|
| `GET …/auth/config` | The sign-in mode this company uses, the name it goes by, whether a link can be mailed and whether the first admin is still unclaimed — everything the console needs to pick a screen |
| `POST …/auth/request` | Mail a magic link. Always `{"sent": true}` |
| `POST …/auth/verify` | Redeem a link → session cookie |
| `POST …/auth/login` | Login + password → session cookie |
| `POST …/auth/claim` | The first admin's login and password, on a company with no users → session cookie. See [First admin](#first-admin) |
| `POST …/auth/password` | Set/replace your own password (needs a session) |
| `POST …/auth/wallet/challenge` | Mint a nonce for a wallet to sign (`wallet` mode) |
| `POST …/auth/wallet/verify` | Answer a challenge → session cookie (`wallet` mode) |
| `GET …/auth/me` | Who this session belongs to |
| `PATCH …/auth/me` | Your own name and icon — see [avatars.md](avatars.md). No user id in the path, so it can only ever be you |
| `POST …/auth/logout` | Revoke this session |
| `GET …/users` | The roster (admin) |
| `GET …/users/invites` | List outstanding invites (admin) |
| `POST …/users/invites` | Invite an address, and mail them (admin). Answers `delivery: "sent" \| "no_transport" \| "failed" \| "no_mailbox"` alongside the invite — `no_mailbox` for a `wallet`-mode invite, which has no address to mail |
| `DELETE …/users/invites/{id}` | Revoke an invite (admin) |
| `PATCH …/users/{id}` | Role, status, display name (admin) |
| `POST …/users/{id}/password` | Set a temporary password (admin) |
| `DELETE …/users/{id}/sessions` | Sign a user out everywhere (admin) |

### Every login failure is identical

A route a company's mode does not serve refuses with `409 auth_mode` rather
than a 404, and names the mode — see
[One mode, one door](auth-modes.md#one-mode-one-door) for why that does not
breach the rule below.

`auth/request` always returns `{"sent": true}`. `auth/verify` and `auth/login`
always fail with one `401 invalid_login` — for unknown address, uninvited
address, expired code, spent code, wrong code, wrong password, no password set,
and suspended user alike. `auth/claim` is the one exception, and only while the
roster is empty: its refusals are specific because the single fact they
disclose — that nobody has joined — is one `auth/config` already publishes.

This is deliberate. Any difference turns these routes into a **membership
oracle**: someone who can ask "is bob@acme.com a user of this company?" learns
the org chart, and every answer is a phishing target. It is also why
`password::dummy_verify` burns equivalent work where there is no hash to check
— response *time* would otherwise answer what the body refuses to.

Clients must not undo this. The console renders one vague message.

## No ecosystem sign-in

A host wired to the TinyHumans hub **used to** offer Google, GitHub and X: the
browser went to the hub's OAuth start, came back with a platform token, and
`POST …/auth/hub` asked the hub whose it was. That is gone. A company's sign-in
is its own — password, magic link, wallet, or none — and an ecosystem account is
never a way into one. Sharing a login between two products meant the weaker of
the two decided the security of both, it briefly handed every tenant a
credential carrying the person's whole ecosystem account, and on every
self-hosted host it drew three buttons that led to a refusal on return.

The hub is still asked for one thing, a **key** (`src/server/ops/company_key.rs`
and `src/server/hub_identity.rs`): the person approves that on the hub's own
site, and this host never holds a credential belonging to them.

## Passwords

The ordinary sign-in on a host with no mail, and an optional shortcut past the
mailbox on one with. A user who never sets one on a mailing host is unaffected
(`password_hash` is `None`); the first admin always has one, because the claim
and the setup wizard both create the account with it.

There is **no password-reset credential**. "Forgot my password" is a magic-link
login followed by setting a new one where the host mails, and an admin-issued
temporary password where it does not — reusing paths that already exist rather
than adding a second emailed secret to get wrong.

An admin may instead set a **temporary password**, which revokes the user's
sessions and pending codes and sets `mustChangePassword`. A host with no
browser session to do this through uses `issue-password`, which commits the
same semantics from the command line — see
[Recovery without a mailbox](#recovery-without-a-mailbox). Note two things:

- The admin knows that password and must convey it out-of-band. That is
  inherent to the option, not a defect.
- `mustChangePassword` is **advisory**: it is surfaced to the console, which
  drives the change. It is not a per-route boundary — the user is authenticated
  either way — so it constrains a cooperating client, not an adversarial one.

Policy is length-only (12–512 characters, counted as characters), with no
composition rules: NIST SP 800-63B recommends against them, as they produce
`Password1!` and buy no entropy.

## Two carriers, one session

A browser presents its session as the `oc_session_<company>` cookie. A
non-browser client cannot: `SameSite=Lax` means the browser never sends a cookie
cross-site, and a desktop webview is cross-site with every server it talks to.
Allow-listing an origin does not help — the cookie is simply never sent.

Such a client presents the same session in the `x-opencompany-session` header,
valued `<company>.<token>`. The company travels *inside* the value because a
header has no name to carry it, and the GraphQL handler's company argument lives
in the request body where extractors cannot reach it — the cookie solves that
with its name, and the header has to solve it too.

One session, two envelopes: same token, same TTL, same revocation, resolving to
the same principal through `authenticate_session`. Adding a carrier must never
add a second, weaker check beside it, which is why carrier selection and
authentication are separate functions.

Not a CSRF regression. A cross-site HTML form cannot set a request header at
all, and a cross-site `fetch` that sets a custom one is preflighted, which CORS
answers for allow-listed origins only. The header is the stricter carrier — it
is never attached ambiently the way a cookie is.

**A browser gets the cookie unless it asks otherwise, and it only asks when the
cookie cannot work.** The default is unchanged and is what every same-origin
console still gets: the token reaches the browser only as `Set-Cookie`, where
`HttpOnly` keeps it away from JavaScript.

The exception is the [hub console](hub-console.md), which is cross-origin with
every host it operates and therefore receives no cookie at all.

### Asking for the header carrier

A client that cannot receive a cookie sends `x-opencompany-session-carrier:
header` on a sign-in request. The response then carries the ready-made header
value as `session` in its JSON body and sets **no** cookie:

```http
POST /api/v1/companies/acme/auth/verify
x-opencompany-session-carrier: header

200 OK
{ "id": "…", "email": "ada@example.com", …, "session": "acme.<token>" }
```

One session, one carrier — deliberately. Issuing both would leave the cookie
half as a third-party cookie that some browsers keep and others discard, so
whether logging out actually ended the session would vary by browser.

Every browser login path routes through one `mint_session`, so all four —
magic link, password, the first-admin claim and wallet — support this
identically.

Opting in this way is safe for the same reason the header carrier itself is: a
cross-site HTML form cannot set a request header, and a cross-site `fetch` that
sets one is preflighted, which CORS answers for allow-listed origins only. A
hostile page therefore cannot make someone's browser request the readable
carrier on its behalf. Anything other than `header` — absent, empty, or a value
nobody defined — degrades to the cookie rather than to no session.

## Device pairing was removed

A desktop client used to enrol itself by redeeming a pairing code an
already-signed-in human minted (`POST …/devices`, `…/devices/claim`), receiving
a long-lived `SessionKind::Device` session. Those routes and the **Settings →
Devices** page are gone: the frontend client holds its own session, so the host
no longer runs a second enrolment protocol, a second code keyspace and a second
revocation list beside the one sign-in it already has.

What is unchanged is the carrier: a client that cannot receive a cookie asks for
the header carrier (above), and that is still how a non-browser client presents
a session.

## Revocation

The user record is re-read on **every** authenticated request, so suspending or
removing someone takes effect immediately rather than whenever their cookie
happens to expire. That costs a second store read per request — on the fs
backend, a whole-file read. Use sqlite or mongodb for anything with real users.

Changing your own password revokes every *other* session but keeps the current
one: it is what you do when you think a session is stolen.

## Chat attribution

`CompanyEvent::OperatorMessage` carries `by: Option<Actor>`.

- `Some(Actor { kind: User, id })` — a signed-in human.
- `None` — an operator/platform/dev credential, or an event journaled before
  attribution existed. Both read as `"operator"`; there is no person to name.

`ActorKind::User` is fieldless and the id rides on `Actor.id`, because
`ActorKind` is `Copy` and a `String`-carrying variant would take that away from
every existing holder.

`serde(default)` + `skip_serializing_if` mean every already-persisted event
loads and an unattributed event serializes byte-for-byte as before — no
migration, no stored record touched.

`mine` is per-viewer, so `MessageGql::project` takes a `Viewer`. Authors render
as a display name or the email's **local part**, never the full address: a desk
history is read by every member and should not hand each of them everyone
else's email.

## Mail

Login mail uses the host-level provider (`OPENCOMPANY_MAIL_*`, see
[config.md](config.md)) — a login link is sent on the platform's behalf, not
the company's. With no transport configured, `auth/request` returns the code in
a `dev_code` field and logs a warning, so scripted local development works; a
host that can send mail never echoes it. The console does **not** use the echo:
a host with no transport reports `magicLink: false` and draws the password form
alone, because "email me a link" on a host that emails nothing was the most
confusing thing the sign-in screen did. A login with no `@` — a username — has
no mailbox, and a link asked for it goes nowhere on any host.

**Invite mail** goes out over that same host-level provider, gated on the same
"is a transport wired" predicate. Not the company's own `__smtp` secret, and
deliberately: an invite mailed from a host whose platform mail is unwired
invites someone into a dead flow, because the sign-in link they then ask for
cannot be sent. One transport, one truthful answer.

An invite mail is a **notification, not a credential**. It names the company,
the inviter (display name or the email's local part, never the full address,
per [Chat attribution](#chat-attribution)), and the sign-in URL — no code, no
token, not even the invite id. The recipient still goes through
`auth/request` like anyone else, so the roster stays the only gate. That is
what makes it safe to send to an address a human typed and may have typed
wrongly.

`POST …/users/invites` **reports delivery** rather than assuming it. Unlike
`auth/request`, this route is admin-authenticated and the caller supplied the
address, so there is no enumeration oracle to protect and nothing the response
could disclose that the caller did not already know. The mail is sent strictly
*after* the invite record is written — a refusal (already a member, already
invited) must mail nobody — and a failed send never rolls the grant back, since
a mail outage should not become a silent refusal to add people. A sent invite
stamps `notifiedAtMillis` on the record, which the console reads to say whether
the person was actually told; absent means nobody was, and the operator owes
them a message. That stamp is an **update, never an insert**: an admin who
spots a mistyped address and revokes the invite while its mail is still in
flight wins, and the stamp quietly does nothing rather than putting the revoked
address back on the roster. Issue #584: the route previously wrote the record
and mailed nobody while the console reported unconditional success.

Sending is bounded (30s) in the SMTP adapter, so a relay that accepts a
connection and then stalls reports a failed delivery instead of holding the
admin's request open.

## Abuse and exposure

- **Resend throttle**: one *mailed* link per address per minute. A throttled
  request returns the same `202` as a sent one (or the throttle is itself a
  membership oracle) and leaves the live code alone (or anyone could invalidate
  a victim's link on demand). It does not apply where the code is echoed rather
  than mailed — a loopback-only bind with no transport wired. There is no
  mailbox to spare there, only the plaintext's hash is stored (so a throttled
  answer cannot re-echo the live code), and throttling would lock the sole local
  sign-in path for a minute after every use.

  The silence has a cost the console has to absorb: nothing in the response
  distinguishes a mailed link from a swallowed one, so the "check your email"
  card cannot ask whether a resend would land. It therefore keeps its own copy
  of the window — it stamps the moment each `202` arrives and disables its
  "Resend link" button, with the remaining seconds in the label, until a minute
  has passed (`frontend/src/views/login/resend.ts`, issue #1333). The constant
  there must track `RESEND_INTERVAL_MILLIS`: too short and the button fires
  into the throttle and reports a send that never happened.
- **Login codes are never echoed** from a host that is reachable from anywhere
  else. `dev_code` appears only on a loopback-only bind with no mail
  transport. A routable host with broken mail lets nobody in rather than
  handing the credential to whoever asked.
- **CORS** is off unless `OPENCOMPANY_CORS_ORIGINS` lists exact origins, and a
  wildcard is refused — it is illegal with credentials, and echoing an
  arbitrary origin would let any site act as the signed-in user. See
  [config.md](config.md).

## Known gaps

- **`resolve_principal` reads the user record on every request**, so revocation
  is immediate — at the cost of a second store read per request. On the fs
  backend that is a whole-file read; use sqlite or mongodb with real users.
