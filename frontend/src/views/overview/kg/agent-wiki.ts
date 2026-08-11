// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * The detail card shown when you click a tool on the graph.
 *
 * A tool on the outer ring is a **tool-grant glob**: one entry of what the host
 * says an agent actually holds, resolved from the agent's own `[[agent]].tools`
 * line against the company's `[tools] allow` ceiling. `search`, `composio`,
 * `workspace.*` and the catch-all `*` are all grants; so is anything else an
 * operator wrote into that list.
 *
 * The slug is therefore the truest label there is — it is the literal string in
 * `company.toml`, which is what an operator greps for when they want to change
 * it. Nothing here invents a friendlier name for it (issue #601).
 */

/** Grants in the MCP namespace, which the card flags with its own mark. */
const MCP_PREFIXES = ['mcp', 'mcp_', 'mcp:', 'mcp.', 'mcp-'];

/** The grant that holds everything the company allows. */
const CATCH_ALL = '*';

export type ToolWiki = {
  slug: string;
  name: string;
  /** True when the grant names the MCP tool namespace. */
  mcp: boolean;
  /** Human-readable origin. */
  kind: string;
  /** Where this tool is managed. */
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
 * Matched on the namespace rather than an exact string because a grant may be
 * the family (`mcp`), a glob over it (`mcp_*`), or one tool inside it
 * (`mcp_registry_list_tools`) — the same boundary rule the host's own
 * `grant_matches` applies.
 */
export function isMcpSlug(slug: string): boolean {
  return slug === 'mcp' || MCP_PREFIXES.some((p) => p !== 'mcp' && slug.startsWith(p));
}

/**
 * Build a tool's card.
 *
 * `labels` is the adapter's slug → display-name map. For a grant it maps the
 * slug to itself, so the card shows the literal `company.toml` entry; the
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
