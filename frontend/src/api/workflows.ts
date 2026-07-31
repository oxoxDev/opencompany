// The live workflow API: the console's Workflows canvas reads the company's
// saved graphs through the host's `…/workflows` routes (REST, camelCase over
// the wire) and runs one via `…/workflows/{wid}/run`. Replaces the client-side
// `workflow-sample` illustrative data.

import type { OpenCompanyClient } from "./client";

/** A one-line workflow entry, as the picker lists it. */
export interface WorkflowSummary {
  id: string;
  name: string;
  description?: string;
}

/** A single graph node. `kind` is one of the tinyflows node kinds. */
export interface WorkflowNode {
  id: string;
  /**
   * `trigger` | `agent` | `tool_call` | `http_request` | `condition` |
   * `output` | `switch` | `merge` | `split_out` | `transform` |
   * `output_parser` | `sub_workflow`.
   */
  kind: string;
  name: string;
  summary?: string;
  /** The roster agent id — only present on `agent` nodes. */
  agent?: string;
  /**
   * A standard 5-field cron saying when the workflow starts on its own — only
   * present on `trigger` nodes, and always interpreted in **UTC** (issue #169).
   * Absent means the workflow only runs when something starts it (the Run
   * button, the run route, or another workflow).
   */
  schedule?: string;
  /** Kind-specific configuration (a slug, URL, case labels, schema, …). */
  config?: unknown;
  /** How the engine handles an error on this node, when set. */
  onError?: string;
  /** The node's retry policy, when set. */
  retry?: {
    maxAttempts?: number;
    backoffMs?: number;
    backoff?: string;
  };
  /** Whether the node pauses for a human approval before proceeding. */
  requiresApproval?: boolean;
  /** Where an `output` node's report goes when the run finishes. */
  destination?: WorkflowDestination;
}

/**
 * Where a terminal `output` node routes its report once the run completes.
 *
 * `owner` is resolved server-side from the company's admins and carries no
 * target — the graph names nobody, which is what keeps it safe by construction.
 * `email` names an address and only sends when the company grants `email` AND
 * the recipient has already written in. `channel` must name a channel the
 * deployment already wired.
 */
export interface WorkflowDestination {
  kind: "owner" | "email" | "channel";
  /** Required for `email` (an address) and `channel` (an id); absent for `owner`. */
  target?: string;
}

/** The destination kinds the creator's picker offers, with prosumer labels. */
export const DESTINATION_KINDS: { value: WorkflowDestination["kind"]; label: string }[] = [
  { value: "owner", label: "Owner — the company's admins" },
  { value: "email", label: "Email — a specific address" },
  { value: "channel", label: "Channel — a wired chat channel" },
];

/** A directed edge between two node ids, with an optional branch label. */
export interface WorkflowEdge {
  from: string;
  to: string;
  label?: string;
}

/** The full graph the canvas renders. */
export interface WorkflowGraph {
  id: string;
  name: string;
  description?: string;
  nodes: WorkflowNode[];
  edges: WorkflowEdge[];
}

/** What became of one attempt to deliver an output node's report. */
export type DeliveryStatus = "sent" | "skipped" | "denied" | "failed";

/**
 * One attempt to route a reached `output` node's report to its destination.
 *
 * This is the ONLY place an operator learns a report was not delivered: a
 * delivery failure never fails the run, so it has nowhere else to surface. An
 * output node the run never reached contributes no row at all.
 */
export interface DeliveryReport {
  /** The output node whose report this was. */
  node: string;
  /** The destination kind as authored. */
  kind: string;
  /** The address or channel actually addressed, when there was one. */
  target?: string;
  status: DeliveryStatus;
  /** An operator-readable reason — populated even on success. */
  detail: string;
}

/** The result of a run: the engine's final state and any pending approvals. */
export interface WorkflowRunResult {
  /** The engine's final run state — a nested JSON payload. */
  output: unknown;
  /** Node ids left waiting on a human approval, if any. */
  pendingApprovals: string[];
  /**
   * One row per report-delivery attempt. Optional on the type (not the wire)
   * so a response from a host predating issue #170 still parses.
   */
  deliveries?: DeliveryReport[];
}

export function listWorkflows(
  client: OpenCompanyClient,
  company: string | null,
): Promise<WorkflowSummary[]> {
  return client.get<WorkflowSummary[]>(`${client.scopeFor(company)}/workflows`);
}

export function getWorkflow(
  client: OpenCompanyClient,
  company: string | null,
  wid: string,
): Promise<WorkflowGraph> {
  return client.get<WorkflowGraph>(
    `${client.scopeFor(company)}/workflows/${encodeURIComponent(wid)}`,
  );
}

export function runWorkflow(
  client: OpenCompanyClient,
  company: string | null,
  wid: string,
  input?: unknown,
): Promise<WorkflowRunResult> {
  return client.post<WorkflowRunResult>(
    `${client.scopeFor(company)}/workflows/${encodeURIComponent(wid)}/run`,
    { input: input ?? {} },
  );
}

/**
 * Authors a new workflow graph (issues #69, #168): the console's form creator
 * posts the same shape `getWorkflow` returns, and the host persists it on the
 * company record — so this works on every deployment, including a hosted tenant
 * whose company source tree is a read-only mount. Rejections carry a
 * prosumer-language `ApiError` message (bad id, duplicate id or name, an edge or
 * `agent` node the graph can't support).
 */
export function createWorkflow(
  client: OpenCompanyClient,
  company: string | null,
  graph: WorkflowGraph,
): Promise<WorkflowGraph> {
  return client.post<WorkflowGraph>(`${client.scopeFor(company)}/workflows`, graph);
}

/**
 * The node kinds the form creator's palette offers. These are the kinds that
 * are meaningful to author from a bare form — no per-node config required to do
 * something useful: `merge` fans several inputs into one stream and `transform`
 * passes items through (a config-less `set` is an identity pass-through).
 *
 * Deliberately withheld until the P4 config forms land: `tool_call`,
 * `http_request`, `switch`, `output_parser`, and `sub_workflow` all need config
 * (a slug, a URL, case labels, a schema, a `workflow_id`) to run — creating one
 * from a bare palette would silently produce a node that errors at run time — so
 * the creator doesn't offer them yet. All of these kinds still render on the
 * canvas and can be authored by hand in `workflows/<id>.toml`.
 */
export const CREATABLE_NODE_KINDS: { value: string; label: string }[] = [
  { value: "trigger", label: "Trigger — starts the workflow" },
  { value: "agent", label: "Agent — a teammate performs a step" },
  { value: "condition", label: "Condition — branches on something" },
  { value: "merge", label: "Merge — combines several inputs into one" },
  { value: "transform", label: "Transform — reshapes the data" },
  { value: "output", label: "Output — reports the result back" },
];
