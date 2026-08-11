// SPDX-License-Identifier: GPL-3.0-or-later

// Our host's data, shaped into the knowledge graph's five-ring org model.
//
// ## Where every ring comes from
//
// The graph reads `company → department → the work → the worker who does it →
// that worker's tools`. Every one of those is now a value the host answers for
// (issue #601):
//
// | Edge                  | Source                                       |
// |-----------------------|----------------------------------------------|
// | teammate → department | the desks it sits on (`GET …/team`)          |
// | worker → tools        | its resolved grants (`GET …/team`)           |
// | task → worker         | `task.assignee` on the board                 |
// | department → workflow | the flow's own agent stages (see below)      |
// | workflow → stages     | the saved graph's nodes (`GET …/workflows/…`)|
// | stage → worker        | the `agent` node's own agent id              |
//
// This module used to invent three of those rings — dealing each teammate a
// slice of the company-wide `[tools] allow` list, keyword-matching roles into
// five hard-coded departments, and templating one routine per department —
// because the roster **list** carried none of it and fetching every agent's
// detail on page load was worse. That DTO gap is closed: a roster row now
// carries the same `tools` and `desks` the per-agent detail read serves, from
// the same host-side constructor, so the graph and the agent card cannot
// disagree about a teammate (the verification rule issue #264 established).
//
// One thing is still this console's reading rather than the company's
// declaration, and `DERIVED_NOTICE` says so: **where a workflow sits on the
// wheel**. The host scopes a flow to the company, not to a desk.

import type { Person as HostPerson } from "@/api/auth";
import type { Task } from "@/api/tasks";
import type { MemoryEntry } from "@/api/memory";
import type { DeskDto } from "@/api/types";
import type { WorkflowGraph } from "@/api/workflows";
import type { TeamMember } from "@/lib/team";
import { TASK_COLUMNS } from "@/lib/tasks-sample";
import type { BrainGraphEdge, BrainGraphNode, MemoryGraph } from "./memory-core";
import { distillMemoryGraph } from "./memory-core";
import { isOpen } from "../pulse";
import type { Agent, Department, Person, SopTask, Workflow, WorkflowStage } from "./schemas";

/**
 * The one caveat left on this surface.
 *
 * Everything the graph draws is now something the company declared — except
 * which pillar a workflow hangs off, because a workflow belongs to the company
 * rather than to a desk. It is drawn on the desk of the first teammate it runs
 * through, which is a real relationship (that teammate really does perform that
 * stage, and really does sit on that desk) read as a placement it was never
 * declared to be.
 */
export const DERIVED_NOTICE =
  "A workflow is drawn on the desk of the first teammate it runs through — the company scopes flows to itself, not to a desk. Everything else on this graph is declared.";

/**
 * Pillar tints, dealt in desk order.
 *
 * Presentation, not data: the desks are the company's, the palette is this
 * page's. A company with more desks than hues wraps rather than running out.
 */
const PILLAR_COLORS = ["#2a78d6", "#1baf7a", "#eb6834", "#4a3aa7", "#eda100"];

/**
 * The pillar for teammates and people who sit on no desk.
 *
 * A **stated** empty state, which is the point: a company that has declared no
 * desks gets one honestly-labelled group rather than five invented pillars with
 * its roster keyword-matched across them. Humans land here too — signing in
 * says who somebody is, not what they work on, and desks hold agents.
 *
 * The id cannot collide with a real desk: desk ids are slugified from their
 * names into `[a-z0-9-]`, so no desk can ever be called `__no_desk__`.
 */
export const NO_DESK_ID = "__no_desk__";

const NO_DESK: Department = {
  id: NO_DESK_ID,
  name: "No desk",
  slug: "no-desk",
  tagline: "Not on any desk",
  color: "#8b8b8b",
  // Last on the wheel, always: it is the remainder, not a peer.
  order: Number.MAX_SAFE_INTEGER,
};

/**
 * The company's desks, as the graph's department pillars.
 *
 * Host order is kept — manifest desks first, then operator-created ones — so
 * the wheel does not reshuffle when somebody adds a desk at runtime.
 */
export function departmentsFromDesks(desks: DeskDto[]): Department[] {
  return desks.map((desk, i) => ({
    id: desk.id,
    name: desk.name,
    slug: desk.id,
    tagline: desk.description ?? "",
    color: PILLAR_COLORS[i % PILLAR_COLORS.length],
    order: i,
  }));
}

/**
 * Which pillar a teammate stands on: its first desk, or the no-desk pillar.
 *
 * A teammate can sit on several desks and the wheel gives it one place, so the
 * first is taken — host order, which puts the blueprint's own desks ahead of
 * anything added at runtime. Its other desks are not lost, they are simply not
 * where its node is drawn.
 */
export function deskOf(member: TeamMember): string {
  return member.desks[0]?.id ?? NO_DESK_ID;
}

/**
 * A saved workflow graph, written out as an ordered list of stages.
 *
 * The nodes are sorted into run order (Kahn's algorithm over the graph's own
 * edges, ties broken by declared order) rather than taken as declared: the file
 * lists nodes, the edges say what follows what, and a flow read out of order is
 * a flow described wrongly. A cycle — or a node no edge reaches — cannot be
 * ordered that way, so anything left over is appended in declared order rather
 * than dropped.
 *
 * `agentId` rides along from the graph's own `agent` nodes, so the stage → who
 * performs it edge is the flow's answer and not a deal across the department.
 */
export function orderStages(graph: Pick<WorkflowGraph, "nodes" | "edges">): WorkflowStage[] {
  const rank = new Map(graph.nodes.map((n, i) => [n.id, i]));
  const indegree = new Map(graph.nodes.map((n) => [n.id, 0]));
  const next = new Map<string, string[]>();
  // Deduplicated: two edges between the same pair would raise the in-degree
  // twice and strand the target forever.
  const seen = new Set<string>();
  for (const edge of graph.edges) {
    if (!rank.has(edge.from) || !rank.has(edge.to)) continue;
    const key = `${edge.from}\u0000${edge.to}`;
    if (seen.has(key)) continue;
    seen.add(key);
    indegree.set(edge.to, (indegree.get(edge.to) ?? 0) + 1);
    next.set(edge.from, [...(next.get(edge.from) ?? []), edge.to]);
  }

  const ready = graph.nodes.filter((n) => indegree.get(n.id) === 0).map((n) => n.id);
  const order: string[] = [];
  const placed = new Set<string>();
  while (ready.length) {
    // Lowest declared index first, so a fan-out reads in file order and the
    // same graph always produces the same list.
    ready.sort((a, b) => (rank.get(a) ?? 0) - (rank.get(b) ?? 0));
    const id = ready.shift()!;
    if (placed.has(id)) continue;
    placed.add(id);
    order.push(id);
    for (const to of next.get(id) ?? []) {
      const left = (indegree.get(to) ?? 0) - 1;
      indegree.set(to, left);
      if (left === 0) ready.push(to);
    }
  }
  for (const node of graph.nodes) if (!placed.has(node.id)) order.push(node.id);

  const byId = new Map(graph.nodes.map((n) => [n.id, n]));
  return order.map((id) => {
    const node = byId.get(id)!;
    return {
      name: node.name,
      // Only an `agent` node names a teammate; every other kind performs no
      // agent's work and must not be attached to one.
      ...(node.kind === "agent" && node.agent ? { agentId: node.agent } : {}),
    };
  });
}

export interface AdaptInput {
  members: TeamMember[];
  tasks: Task[];
  /** The company's desks — the graph's department pillars. */
  desks: DeskDto[];
  /** The company's saved workflow graphs, whole. */
  workflows: WorkflowGraph[];
  /** The humans who can sign in to this company. */
  people: HostPerson[];
  /** Matches a board card to a roster member; the one real assignment edge. */
  ownedBy: (task: Task, member: TeamMember) => boolean;
}

export interface Adapted {
  departments: Department[];
  agents: Agent[];
  people: Person[];
  workflows: Workflow[];
  tasks: SopTask[];
  /** Tool slug → display label, for the detail cards. */
  toolLabels: Record<string, string>;
}

/** Shape the host's data into the graph's org model. */
export function adapt(input: AdaptInput): Adapted {
  const agents: Agent[] = input.members.map((member) => ({
    id: member.id,
    departmentId: deskOf(member),
    name: member.name,
    role: member.role,
    status: "active",
    tier: "worker",
    description: member.description,
    model: "—",
    // What the host says this teammate actually holds, straight through. The
    // agent detail view renders the same list from the same server-side
    // resolution, which is the whole point of #601.
    tools: member.effectiveTools,
    parentId: null,
    instance: "builtin",
  }));

  // A tool slug is now a grant glob — the literal `[tools] allow` entry — so it
  // is its own best label. The map is kept because the detail card takes one.
  const toolLabels: Record<string, string> = {};
  for (const agent of agents) for (const grant of agent.tools) toolLabels[grant] = grant;

  // A board card becomes an SOP task owned by the teammate it is assigned to —
  // the one edge here that the host actually records. Cards nobody owns are
  // dropped rather than parked under an invented owner, and a closed one
  // (`pulse.ts`'s `isOpen`) is dropped too: the graph reads as work owed, not
  // an archive, so a card already in Done should not still show as active.
  const tasks: SopTask[] = [];
  for (const task of input.tasks) {
    if (!isOpen(task)) continue;
    const member = input.members.find((m) => input.ownedBy(task, m));
    if (!member) continue;
    tasks.push({
      id: task.id,
      departmentId: deskOf(member),
      title: task.title,
      summary: task.note ?? "",
      steps: [
        `Column: ${TASK_COLUMNS.find((c) => c.id === task.column)?.label ?? task.column}`,
        `Priority: ${task.priority}`,
        `Owner: ${task.assignee}`,
      ],
      assigneeKind: "agent",
      assigneeId: member.id,
    });
  }

  const people: Person[] = input.people.map((p) => ({
    id: p.id,
    // Every human, on the no-desk pillar. Desks hold agents, and signing in
    // says who somebody is rather than what they work on — so this is the true
    // statement, not a spread across pillars they were never placed on.
    departmentId: NO_DESK_ID,
    // Falling back to the local part of the address keeps a real name off the
    // graph only when nobody has set one.
    name: p.displayName?.trim() || p.email.split("@")[0],
    role: p.role === "admin" ? "Admin" : "Member",
    // Humans get no tool shelf: an operator's tools are their browser, and the
    // grants the host resolves are per **agent**.
    tools: [],
  }));

  // Every workflow the company has saved, written out. Placement is the one
  // reading here (see `DERIVED_NOTICE`): a flow hangs off the desk of the first
  // teammate it runs through, and off the no-desk pillar when it runs through
  // nobody on a desk — a pure HTTP or tool flow, say.
  const deskOfAgent = new Map(agents.map((a) => [a.id, a.departmentId]));
  const workflows: Workflow[] = input.workflows.map((graph) => {
    const stages = orderStages(graph);
    const firstSeated = stages
      .map((stage) => (stage.agentId ? deskOfAgent.get(stage.agentId) : undefined))
      .find((desk) => desk !== undefined && desk !== NO_DESK_ID);
    return {
      id: graph.id,
      departmentId: firstSeated ?? NO_DESK_ID,
      name: graph.name,
      summary: graph.description ?? "",
      stages,
    };
  });

  // Only pillars that ended up with somebody standing on them. A workflow does
  // not keep a pillar alive on its own: an empty desk with a flow pointed at it
  // would be a department with no department in it.
  const used = new Set([
    ...agents.map((a) => a.departmentId),
    ...people.map((p) => p.departmentId),
  ]);
  const departments = [...departmentsFromDesks(input.desks), NO_DESK].filter((d) =>
    used.has(d.id),
  );

  return { departments, agents, people, workflows, tasks, toolLabels };
}

/**
 * Which folder a memory row hangs off.
 *
 * An operator-authored fact carries its own taxonomy (`preference`, `person`,
 * …); an agent's runtime chunk carries none, so it is bucketed by where it came
 * from instead. Mirrors `MemoryView`'s type filter, so the graph and the Brain
 * page group the same rows the same way.
 */
function folderOf(entry: MemoryEntry): string {
  return entry.origin === "fact" ? (entry.kind ?? "fact") : entry.origin;
}

/**
 * The memory constellation, in the shape the core distils from.
 *
 * Each entry is a page, each memory kind is its folder hub. Entries of a kind
 * are linked to each other so the force layout has structure to pull on —
 * a `similar` edge here means "same kind", which is the only similarity this
 * console can honestly claim.
 */
export function buildMemoryGraph(entries: MemoryEntry[]): MemoryGraph {
  const nodes: BrainGraphNode[] = [];
  const edges: BrainGraphEdge[] = [];
  const kinds = [...new Set(entries.map(folderOf))];

  kinds.forEach((kind, k) => {
    const angle = (k / Math.max(1, kinds.length)) * Math.PI * 2;
    nodes.push({
      id: `folder:${kind}`,
      type: "folder",
      label: kind,
      folder: kind,
      kind,
      excerpt: "",
      wordCount: 0,
      tags: [],
      agents: [],
      vx: Math.cos(angle) * 0.4,
      vy: Math.sin(angle) * 0.4,
      vector: [],
      chunks: 0,
    });
  });

  const byKind = new Map<string, string[]>();
  entries.forEach((entry) => {
    const folder = folderOf(entry);
    const k = kinds.indexOf(folder);
    const angle = (k / Math.max(1, kinds.length)) * Math.PI * 2;
    // Seed each page near its folder, jittered deterministically by id so the
    // layout starts spread rather than stacked.
    const jitter = hash(entry.id) % 1000;
    const spread = 0.18 + (jitter / 1000) * 0.22;
    const spin = ((jitter % 360) * Math.PI) / 180;
    nodes.push({
      id: entry.id,
      type: "page",
      label: entry.title,
      folder,
      kind: folder,
      excerpt: entry.body,
      wordCount: entry.body.split(/\s+/).filter(Boolean).length,
      tags: [entry.source],
      agents: [],
      vx: clamp(Math.cos(angle) * 0.4 + Math.cos(spin) * spread),
      vy: clamp(Math.sin(angle) * 0.4 + Math.sin(spin) * spread),
      vector: [],
      chunks: 1,
    });
    edges.push({ source: entry.id, target: `folder:${folder}`, type: "member" });
    const siblings = byKind.get(folder);
    if (siblings) {
      edges.push({ source: entry.id, target: siblings[siblings.length - 1], type: "similar" });
      siblings.push(entry.id);
    } else {
      byKind.set(folder, [entry.id]);
    }
  });

  return distillMemoryGraph({ nodes, edges });
}

const clamp = (n: number) => Math.max(-1, Math.min(1, n));

function hash(s: string): number {
  let h = 0;
  for (const ch of s) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return h;
}
