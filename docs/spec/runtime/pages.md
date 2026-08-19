# Agent-authored internal dashboard pages

A company can hand an agent the ability to build its own small internal
dashboard pages — a metrics view, a pipeline board, a status page — as real
React, rendered inside the operator console. This is not a template or a
markdown render: the agent writes TSX, the server compiles it, and the
console runs the result. That means running agent-authored code in the
browser, which is the one thing this repository had, until now, no
infrastructure for at all: `GET …/workspace/blob/{id}`
([`workspace.rs`](../../../src/server/ops/workspace.rs)) *actively refuses*
to render anything inline for exactly this reason (issue #667). Pages answers
that refusal with real isolation rather than routing around it — see
"The security model, in two halves" below.

## Storage: `Pages/<slug>/` in the existing workspace tree

No new port. A page lives at `Pages/<slug>/` in the company's
[`WorkspaceStore`](../../../src/ports/workspace.rs), the same store
`Agents/<agent-id>/` and the rest of the shared note tree already use:

```text
Pages/<slug>/
  page.toml          # title, description, icon, nav_visible — the manifest
  Page.tsx            # the agent-authored source, a text node
  Page.compiled.mjs   # server-compiled output, a binary node, mime application/javascript
```

`Page.tsx` is an ordinary text node; `Page.compiled.mjs` is a binary node —
mime, size and sha256 computed by the store, same as any upload. Both ride
the workspace's existing `[workspace] max_blob_mb` / `tree_quota_gb` quotas;
no new limit exists for pages specifically.

`slug` is restricted to `^[a-z0-9][a-z0-9-]*$` — narrower than an ordinary
workspace path segment, because a slug is also a URL path segment
(`GET …/pages/{slug}`) and has to survive that role without escaping or
ambiguity. The layout constants (`Pages`, `page.toml`, `Page.tsx`,
`Page.compiled.mjs`, the compiled mime) live in
[`company::workspace_scaffold`](../../../src/company/workspace_scaffold.rs)
rather than only in the harness tool module, because the HTTP routes that
serve a page must exist — and 404 correctly — in a build compiled without the
`openhuman` feature, which is the only build the harness tools compile under.

## The tool namespace: `pages`

[`harness::pages_tools`](../../../src/harness/pages_tools.rs) exposes four
tools, mirroring the shape of `harness::workspace_tools`:

- `pages_list` — every page's slug and manifest.
- `pages_read` — one page's manifest and `Page.tsx` source.
- `pages_write` — create or update a page's manifest and/or source.
- `pages_delete` — remove a page's whole bundle.

Unlike `workspace`, there is no `pages.write` split behind a separate,
explicit grant: `pages` rides the default `"*"` grant whole, the same as
`files`/`docs`/`shell`/`code`. A company that has not deliberately withheld
tools gets all four the moment it names an agent for the job — the global
`page_builder` agent (`globals/agents/page_builder.toml`) is exactly that.
`pages` is also **not** in `GATEABLE_NAMESPACES`
(`src/company/types.rs`), for the same reason `workspace`/`docs`/`files`
are not: an agent should not lose the ability to fix a broken page under
token-budget pressure.

## The compile contract

`pages_write` compiles `Page.tsx` synchronously, whenever `source` is given,
using [`swc_core`](https://github.com/swc-project/swc) — a pure-Rust
TypeScript/JSX compiler, chosen specifically because the runtime image has no
Node (`Dockerfile`'s builder stage; only the separate frontend Docker build
stage does). Compilation therefore has to be a Rust-native step, done inside
this binary, at request time.

The pipeline, in [`pages_tools::compile_page`](../../../src/harness/pages_tools.rs):

1. **Parse** as TSX (`Syntax::Typescript { tsx: true, .. }`).
2. **Check the import allow-list**, on the freshly parsed AST, before any
   transform runs. Every specifier a page references must name exactly one
   of: `"react"`, `"react-dom/client"`, `"react/jsx-runtime"`,
   `"@opencompany/site"`. The check is a full AST walk, so it covers all
   three forms that carry a module specifier — a static `import`, a
   re-export (`export * from "…"` / `export { x } from "…"`), and a dynamic
   `import("…")` — not just the top-level `import` statements (a page could
   otherwise smuggle a bare specifier through a form the allow-list never
   looked at, and the browser would fetch it outside the served import map).
   Anything else — `"node:fs"`, a bare npm package, a relative import —
   fails the whole call with a diagnostic naming the disallowed specifier.
   This is a compile-time allow-list check, not a sandbox: the runtime
   isolation is the sandboxed iframe (see below), and the allow-list exists
   so a page cannot even *reference* something the pages SDK does not intend
   to serve, catching a mistake at write time instead of at render time.
3. **Strip TypeScript** types (`ecma_transforms_typescript::strip`).
4. **Transform JSX** via the automatic runtime
   (`ecma_transforms_react::react` with `Runtime::Automatic`), which rewrites
   JSX elements into `jsx`/`jsxs` calls importing from `"react/jsx-runtime"`
   — no `React` import is needed in page source.
5. **Render** the transformed AST back to JS text (`ecma_codegen`).

A parse error, a rejected import, or a codegen failure returns the
diagnostic as the tool's error result — the same ergonomics as a failing
`cargo build` — and **writes nothing**: neither `Page.tsx` nor
`Page.compiled.mjs` changes until a call compiles cleanly. `pages_write`
also carries a required `expected_updated_at` compare-and-swap token
whenever it overwrites a page that already has a `Page.tsx`, the same
read-before-write invariant `workspace_write` enforces.

## The HTTP routes

[`server::ops::pages`](../../../src/server/ops/pages.rs) serves three routes,
scoped and authenticated exactly like every other console route (this is an
internal dashboard page, not a public site):

| Route | Serves |
| --- | --- |
| `GET {scope}/pages` | Every page's manifest as JSON — `[{ "slug", "title", "description", "icon", "navVisible" }]` — for the console nav. |
| `GET {scope}/pages/{slug}` | A fixed HTML shell: an import map pointing `react` / `react-dom/client` / `react/jsx-runtime` at `/pages-sdk/react.mjs` and `@opencompany/site` at `/pages-sdk/index.mjs`, plus a `<script type="module">` that imports `./{slug}/bundle.mjs` — a path relative to the shell's own URL at `…/pages/{slug}` — and mounts it with `ReactDOM.createRoot`. |
| `GET {scope}/pages/{slug}/bundle.mjs` | The page's `Page.compiled.mjs`, streamed with `Content-Type: application/javascript` and `Content-Disposition: inline`. |

All three set:

```text
Content-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; connect-src 'none'; frame-ancestors 'self'
X-Content-Type-Options: nosniff
Cache-Control: no-store
```

`Cache-Control: no-store` on every route: the shell, manifest and bundle are
authenticated, company-specific content, so a browser — or an intermediary —
must never serve a cached copy of one company's (or one session's) page to
another request.

as defense in depth, on top of the iframe sandbox described below, which is
the boundary that actually holds.

### Why `Content-Disposition: inline` is the right call here, and wrong at `workspace/blob/{id}`

`workspace/blob/{id}` forces `attachment` and a fixed allow-list of *safe to
render* image/PDF types, because the bytes behind it are an **untrusted
upload** — anything a caller sent, of any claimed mime, with no verification
that the bytes match the claim. Rendering that inline on the console's own
origin would hand a malicious upload the operator's session cookie.

`Page.compiled.mjs` is a different kind of bytes: it is not upload input, it
is the **validated output of the compile step** in the previous section — a
source that already passed TSX parsing, the import allow-list, and a
successful codegen. Serving it as `application/javascript` with `inline` is
serving trusted output, not routing around the blob route's caution; the
blob route's refusal and this route's `inline` are the same policy applied
to two different inputs.

The HTML shell in the middle route is not agent content at all — it is a
fixed Rust format string this route builds itself, with the slug
(pre-validated as `^[a-z0-9][a-z0-9-]*$`) as its only interpolated value —
so there is no injection surface there either.

## The security model, in two halves

**Server half (this document).** CSP headers, a validated slug, and a
compiled bundle that already passed an import allow-list before it was ever
written. None of this is the actual isolation boundary — it is defense in
depth around a payload that is trusted because of how it was produced, not
because the server contained it after the fact.

**Client half (frontend, a separate concern from this doc).** The console
embeds a page in a sandboxed iframe — `sandbox="allow-scripts"`, deliberately
**without** `allow-same-origin` — so the frame is opaque-origin: it cannot
read `document.cookie`, cannot reach the parent frame's DOM, and cannot ride
the operator's session on a credentialed request of its own. Live data
reaches the page through a postMessage bridge to the parent console tab
instead of a credential handed into the frame; the parent is the only party
that holds the operator's authenticated session, and it executes the page's
requests on the page's behalf after verifying the message's `source` is that
exact iframe's `contentWindow`. That bridge forwards full GraphQL — queries
**and** mutations — so a page can read and write company data with the same
authority the operator's own session has; the sandbox stops the page from
touching cookies, the parent DOM, or making its own credentialed requests,
but it does not limit what an authorized request can *do* once it crosses
the bridge. The iframe embedding, the bridge, and the nav view that lists
pages are frontend concerns and are not described further here.


**Normative: pages require a same-origin console.** The page shell and its
`bundle.mjs` are loaded by an iframe `src` navigation, which can only attach the
credentials a browser attaches to a same-origin request — the operator's
HttpOnly session cookie. It cannot attach the API client's `authorization` or
`x-opencompany-session` header, so `pages` MUST only be served to a console that
is same-origin with the host. A cross-origin console gets no pages: its shell
request is unauthenticated.

**Normative: the bridge's residual privilege.** A page that the console's parent
frame loads through the bridge described above MUST be assumed able to perform
every query and mutation the operator's session authorizes, unless the console
imposes an operation allow-list at the point the bridge forwards a request.
Verifying `event.source` against `iframe.contentWindow` authenticates the
caller's window identity but does NOT restrict the scope of operations an
authorized message can request. The operational consequence feature
(`pages_write`, `pages_delete`) gates *persisting* a page — approval is
single-use and covers only that one storage operation; every later GraphQL
request the rendered page fires through the bridge is ungated. This is the
deliberate trade-off described in the client half above: the sandbox protects
the operator's *session credential* from the page, but does not protect the
operator's *authority* from what a page asks to do with it.
