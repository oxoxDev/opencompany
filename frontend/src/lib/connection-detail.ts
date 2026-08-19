// What a connection's detail view is allowed to claim (issues #404, #821).
//
// The issue's rule — "do not invent a number … say so on screen rather than
// rendering a plausible-looking zero" — is not only about usage. Two of the
// three facts the view states are unavailable for most connections, and each
// has a wrong answer that looks right:
//
//  - **when it was connected**: only Composio records it, so a blank cell reads
//    as "never" for the two systems that never had one to record;
//  - **what has gone through it**: metered per *provider*, never per account,
//    and an unmetered host is not a host where nothing happened.
//
// A remote MCP server (#821) adds a third, which is the one its own surface
// gets wrong today: it has no connection object at all, so "connected" has to
// be assembled from two facts that a single badge would collapse — whether the
// harness hands its tools out, and what the endpoint said the last time anyone
// asked. See [`mcpStanding`].
//
// These are the decisions, kept out of the component so they can be pinned
// without a DOM.

import type { McpHealth, McpServer, ProviderCallsDto } from "@/api/types";

/**
 * "connected 9 Aug 2026", or a statement that no date exists.
 *
 * Composio is the only one of the three connection systems that records one.
 * The native `oauth/{provider}` store keeps `{token, account}` and journals
 * nothing on connect; MCP has no such concept. So for those the date is not a
 * gap waiting on a timestamp column — it is a fact about the store, and saying
 * it plainly beats a blank the operator reads as "never".
 */
export function connectedOn(createdAt: string | undefined): string {
  if (createdAt === undefined || createdAt.trim() === "") {
    return "connection date not recorded";
  }
  const at = new Date(createdAt);
  // Composio's own field, forwarded verbatim by the host — which is exactly why
  // it is parsed defensively here. An unparseable string is not evidence of a
  // date, and `Invalid Date` on screen is worse than admitting there isn't one.
  if (Number.isNaN(at.getTime())) return "connection date not recorded";
  return `connected ${at.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  })}`;
}

/**
 * Successful calls counted against one Composio toolkit over the usage window.
 *
 * Matched on the bare slug. A remote MCP server's calls are recorded under
 * `mcp:<server>` (issue #698) precisely so the two namespaces cannot merge: a
 * company with a Composio `gmail` and an MCP server its operator also called
 * `gmail` would otherwise read one total as the other's. Prefix-matching here
 * would re-create by hand the collision that naming was chosen to prevent.
 *
 * Zero is a real answer, not a missing one — `composio_execute` meters every
 * successful call (`src/metering/oauth.rs`), so a provider with no row has had
 * no successful call in the window. The *unmetered* case is a failed usage read,
 * which the caller reports separately rather than flattening to zero here.
 */
export function callsForProvider(rows: readonly ProviderCallsDto[], slug: string): number {
  const want = slug.trim().toLowerCase();
  return rows.find((r) => r.provider.trim().toLowerCase() === want)?.calls ?? 0;
}

/** The namespace prefix the host records a remote MCP server's calls under. */
export const MCP_PROVIDER_PREFIX = "mcp:";

/**
 * The `byProvider` key a remote MCP server's calls land on, or `null` when the
 * server cannot be named (issue #821).
 *
 * The mirror of `mcp_provider` in `src/metering/oauth.rs`: trimmed, lowercased,
 * and prefixed. Feed it to {@link callsForProvider} rather than teaching that
 * function a prefix — the lookup stays one exact match on one namespaced key,
 * which is what keeps a Composio `gmail` and an MCP server called `gmail` from
 * reading as each other.
 *
 * An unnameable server returns `null`, where the host would have recorded a
 * bare `unknown`. That row is the host's *cannot-attribute* bucket, shared with
 * every other call whose provider was unknown — reading it as this server's
 * total would be inventing exactly the number the panel refuses to invent. The
 * caller reports "not attributable" instead. In practice unreachable: the host
 * rejects a server without a name, so nothing on the list can be in this state.
 */
export function mcpProviderSlug(server: string): string | null {
  const normalized = server.trim().toLowerCase();
  if (normalized === "") return null;
  return `${MCP_PROVIDER_PREFIX}${normalized}`;
}

/** What a remote MCP server is right now, in the two facts that decide it. */
export interface McpStanding {
  /**
   * Whether the harness hands this server's tools to agents at all.
   *
   * Not "is it healthy": a turned-off server whose endpoint answers perfectly
   * is still contributing nothing, and that is the fact an operator opened the
   * panel to learn.
   */
  live: boolean;
  /** Connected, and as what — the one line under the title. */
  summary: string;
  /** What the last probe found, or a statement that there has not been one. */
  probe: string;
}

/**
 * The standing of a remote MCP server (issue #821).
 *
 * MCP has no connection object — no account, no grant, no record of a connect.
 * What it has is two independent facts, and the wrong answer here is to collapse
 * them into one "connected" badge:
 *
 *  - **`enabled`** decides whether any agent receives the server's tools. A
 *    disabled server is unreachable by construction however well its endpoint
 *    answers.
 *  - **the last probe** says whether the endpoint answered when someone last
 *    asked, which is a different question and is often unanswered: a server
 *    nobody has pressed Test on has no `health` at all. "Never probed" is not
 *    "broken" and it is not "fine" — rendering it as either invents the probe
 *    that was never run.
 *
 * `authConfigured` is the "as what": a stored outbound credential, or none.
 * Never the credential, which the host does not serve.
 */
export function mcpStanding(server: McpServer, health: McpHealth | undefined): McpStanding {
  const summary = !server.enabled
    ? "turned off — no teammate receives its tools"
    : server.authConfigured
      ? "on, calling with a stored credential"
      : "on, calling with no credential";
  return { live: server.enabled, summary, probe: probeLine(health) };
}

/** The probe half of [`mcpStanding`]. */
function probeLine(health: McpHealth | undefined): string {
  // Never pressed Test, and never probed on an add. Distinct from every status
  // below, including `unknown` — which is a probe that ran and could not tell.
  if (health === undefined) return "this server has not been probed from here";
  switch (health.status) {
    case "ok":
      return `reachable — ${health.toolCount} tool${health.toolCount === 1 ? "" : "s"} on the last probe`;
    case "needs_config":
      return "the last probe was refused for want of a credential";
    case "error":
      return "the last probe did not reach it";
    default:
      return "the last probe could not tell whether it is reachable";
  }
}

/**
 * "last probed 13 Aug 2026, 14:22", or `null` when there is no probe to date.
 *
 * The counterpart of [`connectedOn`], and the reason MCP gets a timestamp at all:
 * the host *does* record when it last asked (`checkedAtMillis`), even though it
 * records nothing about when the server was added. Stating the one it has beside
 * the one it does not is what keeps "connection date not recorded" from reading
 * as "this panel knows no dates".
 *
 * A non-finite or non-positive value is treated as no probe rather than as the
 * epoch — the host omits `health` entirely when it has never probed, so a zero
 * here is a malformed wire value, and "last probed 1 Jan 1970" is the same class
 * of plausible-looking lie as a rendered zero.
 */
export function probedOn(atMillis: number | undefined): string | null {
  if (atMillis === undefined || !Number.isFinite(atMillis) || atMillis <= 0) return null;
  const at = new Date(atMillis);
  if (Number.isNaN(at.getTime())) return null;
  return `last probed ${at.toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  })}`;
}
