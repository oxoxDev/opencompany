# The desktop client

OpenCompany ships as an orchestration **server** and a **desktop client**. The
console stays one codebase and the server stays one binary; what the split adds
are the seams a desktop needs — several hosts at once, a credential a webview
can carry, and a host running in-process.

Code: `src-tauri/` (a separate crate, not a workspace member) and
`frontend/src/connections/`.

## Why `src-tauri` is not a workspace member

Making it one would put the whole Tauri tree into `cargo metadata --locked`,
which CI runs first on a runner with no webkit or GTK — turning a desktop-only
dependency into a hard requirement for checking the server. The host stays a
plain `path` dependency, so a change to it is picked up with no publishing step.

The cost is that no root `cargo` invocation reaches the desktop, including
`--all-features`. The `Desktop` job in `.github/workflows/ci.yml` is what builds
and tests it; without that lane the crate would be compiled by nothing, which is
[issue #475](https://github.com/tinyhumansai/opencompany/issues/475)'s shape.

There is a second Tauri crate in the tree — `frontend/src-tauri/`, the console's
wrapper — and it is an independent workspace with its own `Cargo.lock` for the
same reason this one is separate: OpenHuman's vendored dependencies own nested
workspaces, which Cargo cannot resolve beneath another workspace root.

Which one a `tauri` invocation picks up is decided by the working directory, and
not the way most people expect: **the CLI searches subfolders of the working
directory, not ancestors.** From `frontend/` it finds the wrapper; from the
repository root or from `src-tauri/` it finds this one. That is worth knowing
before reading a build failure, because the two apps share a `productName`.

## Packaging is a claim the lane has to make

Compiling and packaging are different claims. `cargo fmt`, `cargo clippy` and
`cargo test` drive `cargo` directly; none of them reads `tauri.conf.json`, so a
lane built from those three can be green over an app that cannot be assembled at
all. That is what happened: `beforeBuildCommand` named a path that escaped the
repository, `cargo tauri build` and `cargo tauri dev` failed on their first step
for every developer, and the `Desktop` lane never noticed because it builds the
console itself with `working-directory: frontend` and then calls `cargo`.

The `Package` steps close that. They run the real CLI —
`tauri build --debug --no-bundle` — so the config is executed rather than merely
committed. `--debug` because the `Test` step already compiled that graph in the
dev profile and a release build would recompile the host for no extra claim;
`--no-bundle` because the failure being gated happens at the first step of
`tauri build`, long before a `.deb` exists.

There are two of them, from the repository root and from `src-tauri/`. Every
other step in this lane runs from the repository root, which is the one place
the broken hook happened to work — a single-directory packaging step is how #616
stayed invisible. Nothing working-directory-dependent survives in the config
today, so what the pair defends now is that none comes back.

### Build the console first: there is no `beforeBuildCommand`

Both hooks are empty, and that is deliberate. **Build `frontend/dist` before you
package**:

```sh
npm --prefix frontend run build     # from the repository root
cargo tauri build                   # or: frontend/node_modules/.bin/tauri build
```

`frontendDist` is resolved relative to `src-tauri/`, where `tauri.conf.json`
lives, so it means the same thing from every working directory. A hook does not:
Tauri runs it from an app directory it *derives*, by scanning for a
`package.json`, and which one it finds is not stable across machines. The
committed `../frontend` escaped the repository entirely from `src-tauri/`
([#616](https://github.com/tinyhumansai/opencompany/issues/616)), and the
opposite prefix fails from the repository root — each is correct in exactly the
directory that hides the other:

| hook value    | from repo root | from `src-tauri/` |
| ------------- | -------------- | ----------------- |
| `../frontend` | passes         | **fails** — what shipped |
| `frontend`    | **fails**      | passes            |

Resolving the path inside the hook does not rescue it either. `$(git rev-parse
--show-toplevel)/frontend` passes from both of those, and still broke in CI: the
hook landed in `vendor/openhuman/` — another directory with a `package.json`,
reached first because a Linux runner enumerates directories in a different order
than a developer's macOS checkout — and `git rev-parse` inside a submodule
answers with the *submodule's* root. The CLI offers no flag, config key or
environment variable naming the app directory, so nothing computed from the
working directory can be trusted.

Deleting the hook removes the whole class. The cost is that `tauri dev` no longer
starts Vite for you — run `npm --prefix frontend run dev` alongside it; `devUrl`
already points at `localhost:5173` — and that packaging a stale console is now
possible locally, where before it was merely likely. The failure mode is at least
legible: Tauri reports `Unable to find your web assets … frontendDist is set to
"../frontend/dist"` with the absolute path it resolved, rather than an `npm
ENOENT` for a directory nobody named.

## N connections, and no active one

`frontend/src/connections/registry.ts` holds a map of connections and
deliberately has **no "active connection"** field. Selecting a host in the UI is
a rendering choice, not a state change: every connection stays registered and
probed, so one host being unreachable reddens one row rather than blanking the
app.

That single-valued field is what stops comparable clients from holding more than
one host at a time, and it would be just as limiting above the seam as below it.
The Rust `ProxyRegistry` has the same shape for the same reason, and every
command takes an explicit `connection_id`.

Every browser-local key is namespaced by `(connection, company)` through
`scopedKey`. Company alone is wrong as soon as two hosts serve a company of the
same name; connection alone is wrong as soon as one host serves two companies.
Anything reading or writing that state must depend on **both** — a callback that
closes over the scope but depends only on the company will write under the host
the operator just switched away from.

### On the desktop a base url is absolute or it is nothing

A browser can be given anything, including the empty string, which means *same
origin* — that is how every web deployment finds its host, since
`opencompany serve` mounts the console at the origin serving the assets.

The desktop is the runtime with a rule. `ProxyTransport` hands the base url to
Rust, which joins it to a path by concatenation, so anything without an
authority yields a *relative* url and `reqwest` refuses it at `send`. The
request never reaches a socket, and the console reports "couldn't reach a
company host" about a host that was never addressed.
`isAddressableBaseUrl()` is the one place that says so — both the bootstrap add
in `App.tsx` and `restoreConnections()` ask it, and `ProxyRegistry::upsert`
enforces the same thing from below, at the last moment the caller is still on
the stack.

The empty string is the form
[#613](https://github.com/tinyhumansai/opencompany/issues/613) reported, and
only the shortest one: `/api` and `localhost:8080` fail identically, and the
second is what someone types into "Add a host". Parsing is not enough either —
`URL` accepts `tauri://localhost` and `file:///x`, and neither is a company
host — so the check is `http:` or `https:`.

Whether that host may then be *trusted with a secret* is a separate question,
answered separately — see "Where a credential may travel" below. Collapsing the
two would either forbid anonymous HTTP or permit credentialed HTTP.

Two consequences follow, and both are load-bearing
([#613](https://github.com/tinyhumansai/opencompany/issues/613)):

- **The desktop can hold zero connections.** The embedded host arrives over IPC
  and may never arrive at all. The rail therefore stays on screen at a count of
  zero — it holds the only "add a host" there is — and the console renders the
  absence rather than an empty pane.
- **Launch selection is stated, not sorted.** Restored hosts are added before
  the embedded one, so list order records when a host was learned about, not
  which one a person means. `App` selects the embedded host when nothing has
  been chosen.

Only the same-origin *default* is refused. A desktop pointed at a real host
through `?api=` or an injected `OPENCOMPANY_CONFIG` still gets its bootstrap
connection.

## The transport seam

`Transport` has two implementations, chosen at runtime by `isDesktopRuntime()`
so `frontend/` stays one build artifact:

- `BrowserTransport` — `fetch` and `EventSource`, a literal restatement of what
  the console did inline before. The browser build's behaviour is unchanged.
- `ProxyTransport` — every request and event stream through the app's own Rust
  core.

The desktop routes through Rust for three reasons, in the order they bite:

1. **CORS.** A webview origin is cross-origin with every host, so a direct fetch
   would need each operator to allow-list `tauri://localhost` before their
   desktop could connect — configuration standing in front of the headline
   feature. Requests made from Rust are not subject to CORS.
2. **The credential.** A device token attached in Rust never enters the webview.
3. **Streaming.** `EventSource` cannot set a request header, so it cannot carry
   the session header, and a `SameSite=Lax` cookie is never sent cross-site.

`src-tauri/tests/proxy_parity.rs` runs both transports against one real host and
compares, because the console's error handling reads the status, the body and a
response header — a transport that differed in any of them would produce
different `ApiError`s on the desktop for the same server behaviour.

### One reader of `window.__TAURI__`

`app.withGlobalTauri` assigns that global the whole `@tauri-apps/api` bundle, and
**v2 namespaces it by module**: the keys are `app`, `core`, `dpi`, `event`,
`image`, `menu`, `path`, `tray`, `webview`, `webviewWindow` and `window`, and
`invoke` and `Channel` are under `core`. The bare `__TAURI__.invoke` is the v1
shape and reads `undefined`.

`frontend/src/api/transport/bridge.ts` is the only file that touches the global.
Before [#616](https://github.com/tinyhumansai/opencompany/issues/616) two
transports read it separately and both read the v1 shape, so `bridge()` resolved
to `null`, `oc_connect` never ran, no connection was registered and the console
reported an unreachable host — a network-shaped symptom for a bug that never
opened a socket.

The unit tests could not catch it, because they asserted the same wrong shape:
every mock hand-wrote `{ invoke, Channel }` at the top level, and 82 desktop
tests passed against a fixture the runtime never produces. So
`test/unit/desktop-bridge.test.ts` now reads the shape off `@tauri-apps/api`
itself and asserts the v1 form is **refused** — a mock is evidence only if
something ties it to the real thing.

`isDesktopRuntime()` still probes for presence alone, deliberately: a `__TAURI__`
whose `core.invoke` does not resolve is a broken desktop rather than a browser,
and `ProxyTransport` throwing "the desktop bridge is unavailable" names that,
where falling back to `BrowserTransport` would bury it in a CORS failure against
every host.

### Registration precedes traffic

The core resolves a connection id against its own registry, so the console must
call `oc_connect` before any request for that id. `addConnection` is synchronous
(React renders off it) and `oc_connect` is not, so each registration is kept as
a promise and awaited by `ProxyTransport` before its first call. Firing and
forgetting loses the race on a fast probe, and the symptom — a host that is
unreachable once and fine on retry — reads like a network fault.

### What the proxy will not carry

Caller-supplied `x-opencompany-session`, `authorization`, `cookie` and
`proxy-authorization` headers are dropped before the connection's own credential
is attached. `RequestBuilder::header` appends and axum reads the *first* value,
so a header from the webview would otherwise be the one the host honoured.
Keeping the token out of the webview is worth little if the webview still
decides what a request authenticates as.

### Where a credential may travel

Addressability is not the only question a base url has to answer. A host can be
perfectly reachable over a wire anyone on the path can read, and the desktop's
credential is a device session — a person's standing authority on a company,
attached to every request and to the whole life of the event stream, and
replayable by whoever copies it down
([#731](https://github.com/tinyhumansai/opencompany/issues/731)).

So a second rule sits beside the first: **a credential travels over HTTPS, or to
a host on this machine, and nowhere else.** `may_carry_a_credential` in
`src-tauri/src/proxy/mod.rs` is the one that enforces it, with
`mayCarryACredential` in `frontend/src/api/transport/index.ts` as the console's
copy — the same arrangement as `isAddressableBaseUrl`, and for the same reason:
a check in the console alone is bypassed by anything reaching the proxy
directly, and a check in Rust alone arrives as an opaque IPC rejection that
`client.ts` flattens into "cannot reach the company host".

Loopback is exempt because `http://127.0.0.1:<port>` is how the embedded host is
reached, on a port that changes every launch and so can never carry a
certificate; `localhost` and its subdomains come with it, per RFC 6761. The
private ranges are deliberately **not** exempt — an office LAN is precisely
where someone else is on the path.

The rule turns on the credential rather than on the scheme, which is what keeps
a home-lab or staging host without a certificate usable: an anonymous connection
to one still registers and still reads, because nothing is exposed that a
passer-by could not have asked the host for themselves. Three surfaces apply it:

- `ProxyRegistry::upsert` refuses to register a credentialed connection to such
  a host, by name — `this host is not encrypted`, not `not an absolute host url`,
  because an operator told the second goes to debug a network that is working.
- `claim` in `commands.rs` refuses the pairing exchange before opening a socket.
  This is the one place a session token is *created* rather than replayed — the
  code goes out in the request and the token comes back in the response — and it
  never touches the registry, so `upsert`'s refusal does not cover it. Its client
  also refuses redirects: a 307 from an HTTPS base to an HTTP one would re-send
  the claim, body and all, over the wire the check just refused.
- `probe` in `registry.ts` marks such a connection `down` with the reason,
  before contacting it, so the row says what is wrong instead of blaming the
  network.

The webview also runs under a CSP (`src-tauri/tauri.conf.json`) whose
`connect-src` allows the IPC origin only. All host traffic goes through Rust and
needs nothing else.

## The embedded host

`src-tauri/src/embedded.rs` runs a real host in-process on `127.0.0.1:0`,
holding the data root's lock (see [`data-root.md`](data-root.md)). It becomes an
ordinary connection in the console, discovered through `oc_embedded` because
only the core knows which port the OS chose.

Loopback and never `0.0.0.0`: an embedded instance is this machine's, and
binding a routable address would publish someone's company to their network.

It binds a real socket rather than driving the `Router` in-process through
`tower::Service`. Skipping the socket would save microseconds and buy a second
code path through the auth extractors, the CORS branch and the error envelopes —
divergence in an auth extractor being precisely the class of bug that cannot be
afforded. With a real listener, every Playwright spec and every proxy test is
valid evidence about embedded mode too.

Not started when its root could not be taken — most often because another
process holds it. The console renders that as a row; the desktop still holds
remote hosts, which is the point of holding several.

## Several hosts on one machine

The desktop runs a roster of local hosts rather than exactly one, so an
operator can keep two companies side by side on one machine. How the roster is
stored, which empty root gets a starter company and which gets the first-run
wizard, and how to run the shell in development are in
[`desktop-instances.md`](desktop-instances.md).

### One row, however many launches

The ephemeral port is not free: it means the embedded host's *address* is
different on every launch, and `addConnection` recognises a host by address. So
each launch read as a first meeting — a new connection id, a new row, and the
previous launch's row left behind pointing at a closed port. They persist, so
they accumulated, and they all carry the same label; the sidebar filled with
indistinguishable "This computer" entries, all but one broken (issue #615).

Every local host therefore reports `instance_id`, read from the data root
(see [`instance.rs`](../../../src/app/instance.rs)) rather than derived from the
address, and the console registers them through `adoptLocalHosts` rather than
`addConnection`. That function matches each running instance on its identity,
re-points the remembered connection at the new port, and drops the profiles no
running instance claimed.

It takes the whole set in one call for a reason the single-host version could
not survive. `adoptEmbeddedHost` dropped *any other* embedded profile as last
launch's ghost — and with a roster, another embedded profile is ordinarily the
operator's second company. The prune has to see every live instance before it
removes anything, so it is one call rather than a loop of them.
`adoptEmbeddedHost` remains as the one-host wrapper for the degrade above.

Only **running** instances become connections. A stopped one has no address, so
a row for it could do nothing but fail its probe forever; it is visible — and
startable — in the rail's dialog instead.

Reusing the remembered id is what carries the tour state, the last-read channel
and the mail draft across a relaunch, all of them keyed by connection id. A
*different* `instance_id` — a second data root — is deliberately not adopted:
that is a different host, and merging its local state is the failure the
`(connection, company)` namespace exists to prevent.

Profiles written before the identity was reported carry neither it nor an
`origin` marker, so `embeddedProfiles` recognises them by the signature the bug
left: this client's own label for its host, at a loopback address. Narrow on
purpose — a host an operator added by hand is labelled by authority
(`127.0.0.1:8080`), never with that string.

## Authenticating as a person

A desktop cannot hold a session cookie: `SameSite=Lax` means the browser never
sends one cross-site, and a webview is cross-site with every server. The only
other header credential was the platform bearer, which maps to `actor: None` —
every write anonymous in the journal.

So a session has a second carrier and a way to get one. Both are documented in
[`users.md`](users.md) → "Two carriers, one session" and "Device pairing".

The token lives in the OS keychain (`src-tauri/src/keychain.rs`), and the
console never sees it. `oc_connect` takes no device material: the core resolves
a paired session by connection id. Pairing runs entirely in Rust —
`oc_pair_device` performs the claim, writes the result to the keychain, and
answers with the company, device id and expiry — so the token exists for one
HTTP response that the webview is not on the path of. That is the difference
between a design where the webview *should not* hold the credential and one
where it *cannot*. The claim itself is a plain `claim()` rather than command
logic, so the rules on it can be tested without starting a GUI — see "Where a
credential may travel" for the one it enforces.

The console's `Credential { kind: "device", ref }` is therefore a record that
this machine is paired, not something the core is told. `ref` is the host's
device id, useful when deciding what to revoke from the host's device list.

Backend selection, the test store, and the Linux session-keyring caveat (a
pairing there does not survive a logout) are documented in the module.

## ACP

`src-tauri/src/acp/` is the client half: it spawns a locally-installed harness
over stdio and serves the `fs/read_text_file` and `fs/write_text_file` methods
the agent calls back with. Path confinement is enforced in Rust, below the UI —
the console renders the permission prompt but must never be the thing that
enforces the answer. A renderer decides what a person sees; it must not decide
what a model can reach.

The server half (`src/server/acp/`, `src/harness/acp_run_turn.rs`) is behind the
`acp` feature and **not yet mounted on any router**. `/acp` is a reserved prefix
either way, so a build without it answers a protocol probe with a 404 rather
than the console shell with a `200`.
