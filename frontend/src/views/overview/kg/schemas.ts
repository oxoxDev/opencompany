// SPDX-License-Identifier: GPL-3.0-or-later
//
// The record shapes the knowledge graph is built from.
//
// The graph, its layout, and its detail cards were written against a five-ring
// org model — departments, written-out SOP tasks, the one worker who does each,
// and that worker's tools. `adapter.ts` maps the host's own shapes (desks,
// resolved tool grants, saved workflow graphs, board cards) onto it; these are
// the types both sides agree on.

export type AgentStatus = "active" | "idle" | "paused";

export interface Department {
  id: string;
  name: string;
  slug: string;
  tagline: string;
  color: string;
  order: number;
}

export interface Agent {
  id: string;
  departmentId: string;
  name: string;
  role: string;
  status: AgentStatus;
  /**
   * The cognition tier this company declared for the agent, verbatim, or `null`
   * when it declared none (issue #643).
   *
   * A free string rather than a closed union: the tier is the host's
   * vocabulary, not the graph's, and the previous `"lead" | "worker"` union was
   * a set the host never used — which is how every node came to be stamped
   * `worker`. `null` is "not declared" and must be drawn as such.
   */
  tier: string | null;
  /**
   * Whether the host resolved this agent as the company's orchestrator — a
   * separate question from {@link Agent.tier}, answered by the host's roster
   * rule. Never re-derive it from the tier string.
   */
  isOrchestrator: boolean;
  description: string;
  model: string;
  tools: string[];
  parentId: string | null;
  instance: string;
}

export interface Person {
  id: string;
  departmentId: string;
  name: string;
  role: string;
  tools: string[];
}

/**
 * One step of a workflow: a node of the company's saved graph.
 *
 * `agentId` is set only on the graph's `agent` nodes — the flow itself names
 * who performs that step, so nothing here has to guess. A `trigger`, an
 * `http_request` or an `output` node performs no agent's work and carries none.
 */
export interface WorkflowStage {
  name: string;
  agentId?: string;
}

export interface Workflow {
  id: string;
  departmentId: string;
  name: string;
  /** One line on what the flow does. */
  summary: string;
  /** The stages, in run order — the flow written out. */
  stages: WorkflowStage[];
}

export type SopAssigneeKind = "agent" | "person";

export interface SopTask {
  id: string;
  departmentId: string;
  /** The job, stated as work. */
  title: string;
  summary: string;
  /** The written-out checklist. */
  steps: string[];
  assigneeKind: SopAssigneeKind;
  assigneeId: string;
}

export interface AgentRun {
  id: string;
  agentId: string;
  startedAt: string;
  finishedAt: string;
  ok: boolean;
  summary: string;
  model?: string | null;
  tokensIn?: number | null;
  tokensOut?: number | null;
  costUsd?: number | null;
}
