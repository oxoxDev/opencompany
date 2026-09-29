// Per-tool permissions for an MCP server (issue #2373): what happens when an
// agent calls one remote tool.
//
// Two route families, one shape. A **declared** server (manifest / runtime /
// default) is addressed by its name through `…/mcp/servers/{name}/tools/policy`;
// a **directory install** by its stable install id through
// `…/mcp/registry/{serverId}/tools/policy`. Which one a row uses follows
// `McpServer.source` and nothing else — a reconciled row carries a `serverId`
// and is still a declared server (see `McpServer.source` in `api/types.ts`).
//
// The read is open; the writes are an admin's, and the host answers 403
// whatever the console thinks.

import type { OpenCompanyClient } from "./client";

/** What happens when an agent calls a tool. */
export type ApprovalMode = "always_allow" | "needs_approval" | "blocked";

/** The group a tool is defaulted under. */
export type ToolTier = "read_only" | "interactive" | "write_delete";

/** One remote tool, as the host resolves it. */
export interface ToolPolicyRow {
  tool: string;
  /** The tier this row is grouped and defaulted under. */
  effectiveTier: ToolTier;
  /**
   * What discovery suggested, when it reached this tool. May legitimately
   * disagree with `effectiveTier`: an operator can reclassify a row, and that
   * disagreement is the reclassification, not a fault.
   */
  suggestedTier?: ToolTier;
  mode: ApprovalMode;
  /** Whether an operator decided this row, as opposed to it inheriting. */
  isOverride: boolean;
}

/**
 * One tier's bulk default.
 *
 * `stored` is the difference between a decision and a nominal value. An unset
 * tier still reports a `mode` for reference, but the host does not apply it to
 * a tool whose tier only came from discovery — so a row under it reads "Asks"
 * while this reads "Runs". A control that renders `mode` alone invites an
 * operator to confirm what looks like the current value and grant a bulk allow
 * by doing so.
 */
export interface TierDefault {
  mode: ApprovalMode;
  stored: boolean;
}

/**
 * A server's whole resolved policy.
 *
 * `tierDefaults` is **total** — every tier is present — so the console renders
 * what the host decided instead of shipping a second copy of the fallbacks that
 * could drift from it.
 */
export interface ToolPolicyDocument {
  server: string;
  tierDefaults: Record<ToolTier, TierDefault>;
  tools: ToolPolicyRow[];
  /** When discovery last succeeded. `0` reads as never. */
  discoveredAtMillis: number;
}

/** One row of a patch. Naming neither field is how a row's override is cleared. */
export interface ToolPolicyPatchEntry {
  tool: string;
  tier?: ToolTier;
  mode?: ApprovalMode;
}

/**
 * A patch. Both fields are optional and only the present ones are applied — an
 * absent field is "leave this as it is", never "clear it", so a row is cleared
 * by sending it with neither field rather than by sending nulls.
 */
export interface ToolPolicyPatch {
  /** A tier named `null` is cleared back to unset. */
  tierDefaults?: Partial<Record<ToolTier, ApprovalMode | null>>;
  tools?: ToolPolicyPatchEntry[];
}

function declaredPath(client: OpenCompanyClient, company: string | null, name: string): string {
  return `${client.scopeFor(company)}/mcp/servers/${encodeURIComponent(name)}/tools/policy`;
}

function registryPath(client: OpenCompanyClient, company: string | null, serverId: string): string {
  return `${client.scopeFor(company)}/mcp/registry/${encodeURIComponent(serverId)}/tools/policy`;
}

/**
 * Where a row's policy lives.
 *
 * Built from `McpServer.source`, never from whether a `serverId` is present —
 * the rule the delete guard and the provenance badge already follow.
 */
export function policyTarget(
  server: { name: string; source: string; serverId?: string },
): { kind: "declared"; name: string } | { kind: "registry"; serverId: string } | null {
  if (server.source === "registry") {
    return server.serverId ? { kind: "registry", serverId: server.serverId } : null;
  }
  return { kind: "declared", name: server.name };
}

type Target = NonNullable<ReturnType<typeof policyTarget>>;

function pathFor(client: OpenCompanyClient, company: string | null, target: Target): string {
  return target.kind === "registry"
    ? registryPath(client, company, target.serverId)
    : declaredPath(client, company, target.name);
}

/** Read a server's resolved policy. 409 `policy_unreadable` when the stored document is damaged. */
export function readToolPolicy(
  client: OpenCompanyClient,
  company: string | null,
  target: Target,
): Promise<ToolPolicyDocument> {
  return client.get<ToolPolicyDocument>(pathFor(client, company, target));
}

/** Apply a patch and get the resulting document back. */
export function writeToolPolicy(
  client: OpenCompanyClient,
  company: string | null,
  target: Target,
  patch: ToolPolicyPatch,
): Promise<ToolPolicyDocument> {
  return client.put<ToolPolicyDocument>(pathFor(client, company, target), patch);
}

/** Drop the whole stored document, leaving the declaration as the policy. */
export function resetToolPolicy(
  client: OpenCompanyClient,
  company: string | null,
  target: Target,
): Promise<ToolPolicyDocument> {
  return client.del<ToolPolicyDocument>(pathFor(client, company, target));
}
