// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * The detail card shown when you click a tool on the graph.
 *
 * A tool on the outer ring is a **tool-grant glob**: one entry of what the host
 * says an agent actually holds, resolved from that agent's own `[[agent]].tools`
 * line against the company's `[tools] allow` ceiling (issue #601). `composio`,
 * `mcp:*`, `workspace.*` and the catch-all `*` are all grants; so is anything
 * else an operator wrote into that list.
 *
 * It used to be one of two records instead — a skill this company defined
 * (`skill-<id>`) or a single tool advertised by a connected MCP server
 * (`mcp-<serverId>-<toolName>`) — because the ring was dealt out of the
 * company's catalogue rather than read from the agent. Those slugs no longer
 * reach this card.
 *
 * The slug is therefore the truest label there is: it is the literal string in
 * `company.toml`, which is what an operator greps for when they want to change
 * it. Nothing here invents a friendlier name for it.
 */

/** Grant spellings that name the MCP tool namespace, which the card flags. */
const MCP_PREFIXES = ['mcp_', 'mcp:', 'mcp.', 'mcp-'];

/** The grant that holds everything the company allows. */
const CATCH_ALL = '*';

export type ToolWiki = {
  slug: string;
  name: string;
  /** True when the grant names the MCP tool namespace. */
  mcp: boolean;
  /** Human-readable origin. */
  kind: string;
  /** Where this grant is managed. */
  path: string;
  summary: string;
  usedBy: string[];
};

/** 'design-review' → 'Design Review' — the fallback when nothing named it. */
export function prettifySlug(slug: string): string {
  return slug
    .split(/[-_]/)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(' ');
}

/**
 * Whether a grant names the MCP tool namespace.
 *
 * Matched on the namespace rather than one exact string, because a grant may be
 * the bare family (`mcp`), a glob over it (`mcp:*`), or a single tool inside it
 * (`mcp_registry_list_tools`).
 *
 * This is a **display** hint only — it decides whether the card shows an MCP
 * mark. It is deliberately not a membership test: deciding which tools a grant
 * actually covers is the host's `grant_matches`, and re-implementing that here
 * would be the second copy issue #264 exists to prevent.
 */
export function isMcpSlug(slug: string): boolean {
  return slug === 'mcp' || MCP_PREFIXES.some((p) => slug.startsWith(p));
}

/**
 * Build a tool's card.
 *
 * `labels` is the adapter's slug → display-name map, which for a grant maps the
 * slug to itself so the card shows the literal `company.toml` entry. The
 * prettified fallback is only reached for a slug no adapter described.
 */
export function buildToolWiki(
  slug: string,
  usedBy: string[] = [],
  labels: Record<string, string> = {},
): ToolWiki {
  const mcp = isMcpSlug(slug);
  const name = labels[slug] ?? prettifySlug(slug);

  return {
    slug,
    name,
    mcp,
    kind: 'tool grant',
    path: 'company.toml → [tools] allow',
    summary:
      slug === CATCH_ALL
        ? 'Every tool this company allows — the catch-all grant.'
        : mcp
          ? `${name} — a grant over tools served by this company's MCP servers.`
          : `${name} — a tool family this company grants.`,
    usedBy,
  };
}
