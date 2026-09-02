// The live Composio API (issue #110, epic #26 Cell D): the console reads the
// company's Composio status and — when a company brings its own account — writes
// a per-company OAuth bearer token through the host's `.../composio` routes
// (REST, camelCase over the wire).
//
// On the hosted platform nothing is pasted: the instance authenticates with a
// platform-minted, audience-bound identity and Composio calls present that, which
// the read shape reports as `credentialSource: "attested"`. The write route is the
// per-company BYO override, and the only option when this repo is run standalone
// — which is UNSUPPORTED. Either way the token is WRITE-ONLY: sent on
// `PUT .../composio/token`, stored in the host's secret store, never returned. The
// read shape carries only `credentialSource` plus non-secret routing (backend URL,
// toolkit allowlist). Standalone functions over the shared client (mirrors
// `api/inference.ts`), so no change to `OpenCompanyClient` is needed.

import type { OpenCompanyClient, RequestOptions } from "./client";

/**
 * Where this company's Composio credential comes from.
 *
 * - `attested` — the instance's own platform identity; nothing is stored here.
 * - `company` — this company's **own** TinyHumans credential, set by its admin
 *   (issue #586). The backend derives the Composio entity from it, so a company
 *   with a key set connects providers as itself with no Composio token and no
 *   per-tenant provider app. Rotating that one key moves every brokered surface
 *   at once — see `api/credential.ts`.
 * - `static` — a Composio token this company pasted, or a static instance key.
 * - `none` — no credential can be obtained, so agents get no Composio tools.
 */
export type ComposioCredentialSource = "attested" | "company" | "static" | "none";

/**
 * One provider in the catalog the host offers, with the backend's own display
 * metadata (issue #600).
 *
 * Every field but `slug` is best-effort and may be empty — a manifest
 * allowlist, a degraded fallback, and a backend predating Composio's dynamic
 * catalog all yield slug-only entries. The console fills those in from
 * `@/lib/composio-catalog`, which is why that local typography table survives
 * rather than being deleted in favour of the backend's names.
 */
export interface ComposioToolkitEntry {
  /** Toolkit slug, e.g. `googlecalendar`. The key every host call is made with. */
  slug: string;
  /** Human-readable name, e.g. `Google Calendar`. Empty when unpublished. */
  name: string;
  /** One-line description. Empty when unpublished. Searched alongside the name. */
  description: string;
  /** Composio-hosted logo URL, or `null` when unpublished. */
  logo: string | null;
  /**
   * Composio's own free-form category names, e.g. `["productivity", "email"]`.
   *
   * Forwarded verbatim by the host and bucketed here by substring — which is
   * what means a Composio integration added tomorrow lands in the right group
   * with no code change on either side of the wire.
   */
  categories: string[];
}

/** The company's Composio status. Never carries the token. */
export interface ComposioStatus {
  /** Whether the `composio` feature is compiled into this build at all. */
  inBuild: boolean;
  /** Whether the company explicitly grants `composio` (a `*` wildcard does not count). */
  granted: boolean;
  /** Which credential this company's Composio calls present — never the credential itself. */
  credentialSource: ComposioCredentialSource;
  /** The effective Composio backend URL (non-secret). */
  backendUrl: string;
  /** The manifest toolkit allowlist verbatim (empty = defer to the backend allowlist). */
  toolkits: string[];
  /**
   * Whether this company is in **open mode** — an empty manifest allowlist,
   * which means the backend's own allowlist governs and every toolkit it
   * permits is reachable (issue #397).
   *
   * The host tells us rather than letting us infer it: an empty `toolkits`
   * means *allow everything*, which is the opposite of what an empty list
   * reads as. Gating provider rows on `toolkits.length > 0` was exactly that
   * misreading, and it left 19 of 20 shipped templates with nothing to click.
   */
  openMode: boolean;
  /**
   * The toolkits to render as provider rows — the manifest list when non-empty,
   * else the backend's live catalog. In open mode this is still not a hard
   * limit: any slug the backend permits can be authorized, which is what the
   * "other provider" field is for.
   */
  effectiveToolkits: string[];
  /**
   * The same providers as {@link effectiveToolkits}, in the same order, each
   * carrying whatever display metadata the backend published for it (issue
   * #600).
   *
   * This is what makes the panel browsable. Before it, the host reduced every
   * catalog entry to a bare slug one layer before the console, so 123 providers
   * could only be a flat list: there was nothing to group by, nothing to brand
   * with, and nothing to search but the slug.
   *
   * Additive rather than a replacement — {@link effectiveToolkits} is still the
   * slug contract, and it is still all an authorize call needs.
   */
  effectiveCatalog: ComposioToolkitEntry[];
  /**
   * Where {@link effectiveToolkits} came from (issue #397).
   *
   * - `manifest` — the company's own allowlist, offered verbatim. Not a
   *   degradation: the company chose this list.
   * - `backend` — Composio's live catalog. Authoritative.
   * - `fallback` — the catalog could not be fetched, so this is a built-in
   *   starter list that may be incomplete. Always paired with
   *   {@link catalogNotice}.
   *
   * Rendering a `fallback` the same way as a `backend` list would waste the
   * distinction: eight providers would look like the whole set, which is the
   * shape of the bug this issue was reopened for.
   */
  catalogSource: "manifest" | "backend" | "fallback";
  /** Why the list is a fallback, for the operator. `null` unless degraded. */
  catalogNotice: string | null;
}

/** A mutating response: the resulting status plus a plain-language note. */
export interface ComposioMutation {
  status: ComposioStatus;
  note: string;
}

/** The `POST …/composio/authorize` response: the hosted connect URL to open. */
export interface ComposioAuthorize {
  /** Composio-hosted OAuth URL the operator opens in a new browser tab. */
  connectUrl: string;
}

/**
 * One connected account inside a {@link ComposioConnection} (issue #404).
 *
 * A non-secret projection of the host's `ConnectedAccountDto` — the id is
 * Composio's connection id, which is safe to hold here because it is the path
 * segment `DELETE …/composio/connections/{id}` takes and carries no credential.
 */
export interface ComposioConnectedAccount {
  /** Composio's connection id — what a disconnect is addressed to. */
  id: string;
  /**
   * Composio's raw status string (`ACTIVE`, `INITIATED`, `EXPIRED`, …),
   * forwarded verbatim so the console can tell "never set up" from "set up and
   * since expired" rather than flattening both to "not connected".
   */
  status: string;
  /** Whether this individual account is usable. */
  connected: boolean;
  /**
   * When Composio recorded the connection. Absent when Composio did not say —
   * and absent for every other connection system, none of which records one.
   * Rendered as "not recorded" rather than as a blank that reads like "never".
   */
  createdAt?: string;
  /**
   * The account label the provider published (`ops@acme.test`). Omitted rather
   * than guessed: an account the provider did not name has no honest label, and
   * inventing one from the toolkit is how two Gmail accounts become
   * indistinguishable at exactly the moment the operator has to pick one.
   */
  account?: string;
  /**
   * Whether this is the account the company chose to act as for the toolkit
   * (issue #820). False on every account until somebody chooses — nothing is
   * defaulted implicitly, because a default the harness does not honour reads
   * as a guarantee.
   *
   * Optional on the wire for the same reason {@link ComposioConnection.accounts}
   * is: a host predating #820 answers without it, and absent must read as "no
   * choice", not as "this one".
   */
  isDefault?: boolean;
}

/** One toolkit's connected state, as returned by `GET …/composio/connections`. */
export interface ComposioConnection {
  /** Toolkit slug, e.g. `gmail`. */
  toolkit: string;
  /** Whether the company has at least one active connection for this toolkit. */
  connected: boolean;
  /**
   * Every connection this company holds for the toolkit, oldest id first
   * (issue #404).
   *
   * Usually one. Composio permits several accounts per toolkit and a company
   * that connected Gmail twice needs to see which is which before revoking one
   * — `connected: boolean` alone cannot back a disconnect, because it names
   * nothing to disconnect.
   *
   * Optional on the wire rather than required: the field was added additively
   * to a route this console already called, and a host predating it answers
   * without one. Callers treat a missing list as "this host cannot open a
   * provider", not as "no accounts".
   */
  accounts?: ComposioConnectedAccount[];
  /**
   * The account the company chose for this toolkit (issue #820), or absent.
   *
   * **Absent is the ordinary state and means nothing is chosen** — Composio
   * resolves the account itself, exactly as it did before a company could
   * express a preference. The console must not fill this in from the account
   * list: a default the harness does not honour reads as a guarantee.
   */
  defaultConnectionId?: string;
}

/**
 * The host's own budget for the upstream catalog fetch behind `GET …/composio`
 * — `composio_toolkits::FETCH_TIMEOUT` in `src/server/ops/composio_toolkits.rs`.
 *
 * Declared here so {@link CATALOG_READ_TIMEOUT_MS} can be checked against it.
 * A change on the host that is not mirrored here breaks the invariant test
 * rather than the Apps page.
 */
export const SERVER_FETCH_TIMEOUT_MS = 5_000;

/**
 * How long the console waits on `GET …/composio` before giving up.
 *
 * Must strictly dominate {@link SERVER_FETCH_TIMEOUT_MS}: on a cold catalog the
 * host spends up to that budget upstream and then answers with a flagged
 * fallback, so a client deadline at or below it cancels the read at exactly the
 * moment the host is about to explain itself, and the fallback can never be
 * rendered. Below the client's default `GET` deadline, because this read blocks a
 * view and wants a tighter bound than the shared default.
 */
export const CATALOG_READ_TIMEOUT_MS = 15_000;

/**
 * The company's Composio status.
 *
 * `options` reaches the client's own deadline and cancellation: this route
 * fetches a live catalog from the platform, so a caller that blocks a view on
 * it wants a bound of its own and a way to drop a superseded read.
 */
export function getComposioStatus(
  client: OpenCompanyClient,
  company: string | null,
  options?: RequestOptions,
): Promise<ComposioStatus> {
  return client.get<ComposioStatus>(`${client.scopeFor(company)}/composio`, options);
}

/**
 * Set / rotate / clear this company's own Composio token. A non-empty value
 * rotates it; an empty string clears it, reverting to the instance's identity
 * where there is one.
 */
export function setComposioToken(
  client: OpenCompanyClient,
  company: string | null,
  token: string,
): Promise<ComposioMutation> {
  return client.put<ComposioMutation>(`${client.scopeFor(company)}/composio/token`, { token });
}

/**
 * Begin a per-provider OAuth handoff for `toolkit` (e.g. `gmail`). Returns the
 * Composio-hosted connect URL the console opens in a new tab. Composio runs the
 * OAuth itself — there is no local callback — so the console then polls
 * {@link listComposioConnections} until the toolkit reports connected. 409 when
 * the feature is not in the build, or no per-tenant token is configured yet.
 */
export function startComposioAuthorize(
  client: OpenCompanyClient,
  company: string | null,
  toolkit: string,
): Promise<ComposioAuthorize> {
  return client.post<ComposioAuthorize>(`${client.scopeFor(company)}/composio/authorize`, {
    toolkit,
  });
}

/**
 * The company's per-toolkit connected state — one row per toolkit that has at
 * least one connection. The console cross-references this against the granted
 * `toolkits` to render each provider row's connected / sign-in state. 409 when
 * the feature is not in the build, or no per-tenant token is configured yet.
 */
export function listComposioConnections(
  client: OpenCompanyClient,
  company: string | null,
): Promise<ComposioConnection[]> {
  return client.get<ComposioConnection[]>(`${client.scopeFor(company)}/composio/connections`);
}

/** What the host says it revoked, in its own words. */
export interface ComposioDisconnect {
  note: string;
}

/** The `…/default` response: what the company now acts as, and a sentence. */
export interface ComposioDefaultMutation {
  /** The toolkit the change applied to. Empty on a clear. */
  toolkit: string;
  /** The account now acting for that toolkit — absent after a clear. */
  connectionId?: string;
  /** Plain-language confirmation, in the host's own words. */
  note: string;
}

/**
 * Revoke one connected account (issue #404).
 *
 * Addressed by Composio's connection id, not by toolkit: a company with two
 * Gmail accounts has two ids and exactly one of them is being released.
 *
 * This is **not** interchangeable with `client.disconnectConnection`, which
 * posts to `…/connections/{provider}/disconnect` and blanks the host's own
 * `oauth/{provider}` secret. That route has never touched Composio, so calling
 * it for a Composio-connected provider reports success and leaves the account
 * connected on the next refresh. 404 when the id is not this company's; 409
 * when Composio is not in the build.
 */
export function disconnectComposioConnection(
  client: OpenCompanyClient,
  company: string | null,
  connectionId: string,
): Promise<ComposioDisconnect> {
  return client.del<ComposioDisconnect>(
    `${client.scopeFor(company)}/composio/connections/${encodeURIComponent(connectionId)}`,
  );
}

/**
 * Make `connectionId` the account this company's agents act as for its toolkit
 * (issue #820). Admin-only; 404 when the id names no connection this company
 * holds, or names one that is connected but not usable.
 *
 * The toolkit is not passed: it is a property of the connection, and asking the
 * caller to repeat it would only let the two disagree.
 */
export function setComposioDefaultAccount(
  client: OpenCompanyClient,
  company: string | null,
  connectionId: string,
): Promise<ComposioDefaultMutation> {
  return client.put<ComposioDefaultMutation>(
    `${client.scopeFor(company)}/composio/connections/${encodeURIComponent(connectionId)}/default`,
    {},
  );
}

/**
 * Stop naming an account for that connection's toolkit — Composio resolves it
 * again, as it did before. Needs no live provider, so it still works when the
 * account is gone or the backend is unreachable.
 */
export function clearComposioDefaultAccount(
  client: OpenCompanyClient,
  company: string | null,
  connectionId: string,
): Promise<ComposioDefaultMutation> {
  return client.del<ComposioDefaultMutation>(
    `${client.scopeFor(company)}/composio/connections/${encodeURIComponent(connectionId)}/default`,
  );
}
