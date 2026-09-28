/**
 * The company talking to itself: who may reach whom, who has, and who made whom.
 *
 * Pure — nodes and edges out of a roster, a desk list, and whatever the live
 * stream has said so far — so the interesting cases (a wildcard allowlist, a
 * hand-off whose target is redacted, two dispatches to one desk) are testable
 * without a browser or a host.
 *
 * # Why the edges come from four places
 *
 * There is no endpoint that answers "which agents talk to each other". The host
 * carries the pieces and nothing joins them:
 *
 * - **`delegates_to`** is the *address space* — the desks a teammate may hand
 *   work to. Declared, so it is drawable before a company has ever run. Without
 *   it a graph of a fresh company is a set of unconnected dots, which is not
 *   what its manifest says.
 * - **Desk membership** is who shares a room, and therefore who can answer whom.
 * - **`task_dispatched` → `desk_task_completed`** is a delegation that actually
 *   happened, and the only pair the journal already correlates.
 * - **`tool_call` frames** name the rest: `add_agent` (a spawn), `spawn_task`,
 *   `delegate_to_desk`, `delegate_to_teammate`.
 *
 * The last of those is the weakest and is treated as such. A tool call's
 * arguments reach the console **redacted** (`TurnStep.detail`), so the target may
 * simply not be readable — in which case the edge is recorded with an unknown
 * target rather than guessed at. A graph that invents an edge is worse than one
 * that admits a gap.
 */

/** What a node is. Desks and agents are different kinds of thing, not a tree. */
export type CommsNodeKind = "agent" | "desk";

export interface CommsNode {
  id: string;
  kind: CommsNodeKind;
  label: string;
  /** An agent's role, or a desk's seat count as a sentence. */
  detail: string;
  /** Whether this agent is the company's orchestrator. */
  orchestrator?: boolean;
  /** Set while this agent is taking a turn. */
  speaking?: boolean;
  /** Set when the console saw it created rather than declared. */
  spawned?: boolean;
}

/**
 * Edge kinds, in increasing order of how much they claim.
 *
 * `member` and `may-delegate` are **structure** — true because the manifest says
 * so. `handed-off`, `spawned` and `spoke` are **history** — true because the
 * console watched it happen. They are drawn differently for that reason: a company that
 * has never run should look connected but idle, not busy.
 */
export type CommsEdgeKind = "member" | "may-delegate" | "handed-off" | "spawned" | "spoke";

export interface CommsEdge {
  id: string;
  from: string;
  to: string;
  kind: CommsEdgeKind;
  /** How many times this has been observed. Structure edges stay at 0. */
  count: number;
  /** When it was last observed, for recency. */
  lastAtMillis?: number;
  /** Set when an endpoint is not on the roster or desk list yet. */
  provisional?: boolean;
  /** A short label — the task title, or the tool that made the edge. */
  label?: string;
}

export interface CommsGraph {
  nodes: CommsNode[];
  edges: CommsEdge[];
}

/** The roster shape this module needs. A subset of the console's `TeamMember`. */
export interface CommsAgent {
  id: string;
  name?: string;
  role?: string;
  isOrchestrator?: boolean;
  /** Desk ids this teammate may hand work to; `["*"]` means every desk. */
  delegatesTo?: string[];
}

/** The desk shape this module needs. */
export interface CommsDesk {
  id: string;
  name: string;
  members: string[];
}

/** `"*"` in a `delegates_to` list means every desk. */
export const DELEGATES_TO_WILDCARD = "*";

/** One thing the live stream said. */
export type CommsObservation =
  | {
      kind: "handed-off";
      from: string;
      /** A desk id, a teammate id, or `null` when the target was redacted. */
      to: string | null;
      via: string;
      atMillis: number;
      label?: string;
    }
  | { kind: "spawned"; by: string | null; agentId: string; atMillis: number }
  | { kind: "speaking"; agentId: string }
  /**
   * One agent reached another inside an episode — a broadcast the router
   * carried to a seat, a desk DM, or a referral to another desk. Folded from
   * the episode frames by `lib/coordination.ts`. The most direct claim the
   * graph makes: not "may reach", not "handed a card", but "spoke to".
   */
  | { kind: "spoke"; from: string; to: string; via: "broadcast" | "dm" | "referral"; atMillis: number };

/** Build the structural graph — everything true before anybody does anything. */
export function structuralGraph(
  agents: CommsAgent[],
  desks: CommsDesk[],
): CommsGraph {
  const deskIds = desks.map((d) => d.id);
  const nodes: CommsNode[] = [
    ...agents.map((agent) => ({
      id: `agent:${agent.id}`,
      kind: "agent" as const,
      label: agent.name?.trim() || agent.id,
      detail: agent.role ?? "",
      orchestrator: agent.isOrchestrator === true,
    })),
    ...desks.map((desk) => ({
      id: `desk:${desk.id}`,
      kind: "desk" as const,
      label: desk.name,
      detail: desk.members.length === 1 ? "1 seat" : `${desk.members.length} seats`,
    })),
  ];

  const edges: CommsEdge[] = [];
  for (const desk of desks) {
    for (const member of desk.members) {
      edges.push({
        id: `member:${member}:${desk.id}`,
        from: `agent:${member}`,
        to: `desk:${desk.id}`,
        kind: "member",
        count: 0,
      });
    }
  }
  for (const agent of agents) {
    const declared = agent.delegatesTo ?? [];
    const targets = declared.includes(DELEGATES_TO_WILDCARD) ? deskIds : declared;
    for (const target of targets) {
      // A `delegates_to` naming a desk this company does not declare is dropped
      // rather than drawn to a node that does not exist.
      if (!deskIds.includes(target)) continue;
      edges.push({
        id: `may:${agent.id}:${target}`,
        from: `agent:${agent.id}`,
        to: `desk:${target}`,
        kind: "may-delegate",
        count: 0,
      });
    }
  }
  return { nodes, edges };
}

/**
 * Fold what the stream has said into the structural graph.
 *
 * Observations are **merged by endpoint pair**, not appended: a desk handed
 * fifty tasks is one thicker edge, not fifty edges. Weight is what says "this is
 * the path the company actually uses", and fifty parallel lines say nothing.
 */
export function applyObservations(
  base: CommsGraph,
  observations: CommsObservation[],
): CommsGraph {
  const nodes = base.nodes.map((n) => ({ ...n }));
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const edges = base.edges.map((e) => ({ ...e }));
  const edgeByKey = new Map(edges.map((e) => [`${e.kind}:${e.from}->${e.to}`, e]));

  for (const ob of observations) {
    if (ob.kind === "speaking") {
      const node = byId.get(`agent:${ob.agentId}`);
      if (node) node.speaking = true;
      continue;
    }

    if (ob.kind === "spawned") {
      // A spawned teammate may not be on the roster read yet — the tool call is
      // seen before the re-read lands — so the node is minted here and replaced
      // the moment `/team` confirms it.
      const id = `agent:${ob.agentId}`;
      let target = byId.get(id);
      if (!target) {
        target = { id, kind: "agent", label: ob.agentId, detail: "" };
        byId.set(id, target);
        nodes.push(target);
      }
      target.spawned = true;
      if (!ob.by) continue;
      const key = `spawned:agent:${ob.by}->${id}`;
      const held = edgeByKey.get(key);
      if (held) {
        held.count += 1;
        held.lastAtMillis = ob.atMillis;
        continue;
      }
      const edge: CommsEdge = {
        id: key,
        from: `agent:${ob.by}`,
        to: id,
        kind: "spawned",
        count: 1,
        lastAtMillis: ob.atMillis,
      };
      edges.push(edge);
      edgeByKey.set(key, edge);
      continue;
    }

    if (ob.kind === "spoke") {
      const from = resolveNode(byId, ob.from);
      const to = resolveNode(byId, ob.to);
      if (from === to) continue;
      const key = `spoke:${from}->${to}`;
      const held = edgeByKey.get(key);
      if (held) {
        held.count += 1;
        held.lastAtMillis = ob.atMillis;
        held.label = ob.via;
        continue;
      }
      const edge: CommsEdge = {
        id: key,
        from,
        to,
        kind: "spoke",
        count: 1,
        lastAtMillis: ob.atMillis,
        label: ob.via,
        provisional: !byId.has(to) || !byId.has(from),
      };
      edges.push(edge);
      edgeByKey.set(key, edge);
      continue;
    }

    // A hand-off whose target was redacted names no edge. Dropped rather than
    // drawn to a guessed destination.
    if (!ob.to) continue;
    // Either endpoint may be a desk or a teammate — work is raised on a desk and
    // lands on a desk or a seat — so both are resolved the same way, desk first.
    // A bare id is ambiguous otherwise, and guessing "agent" would draw an edge
    // to a node that does not exist.
    const from = resolveNode(byId, ob.from);
    const to = resolveNode(byId, ob.to);
    if (from === to) continue;
    const key = `handed-off:${from}->${to}`;
    const held = edgeByKey.get(key);
    if (held) {
      held.count += 1;
      held.lastAtMillis = ob.atMillis;
      if (ob.label) held.label = ob.label;
      continue;
    }
    const edge: CommsEdge = {
      id: key,
      from,
      to,
      kind: "handed-off",
      count: 1,
      lastAtMillis: ob.atMillis,
      label: ob.label,
      provisional: !byId.has(to),
    };
    edges.push(edge);
    edgeByKey.set(key, edge);
  }

  return { nodes, edges };
}

/**
 * The node a bare id names.
 *
 * Desk first: desk ids and roster ids share one namespace on the wire, and a
 * desk is the more likely endpoint of a hand-off. An id matching neither still
 * resolves to an agent, so an endpoint the roster has not caught up with is
 * drawn rather than dropped.
 */
function resolveNode(byId: Map<string, CommsNode>, id: string): string {
  return byId.has(`desk:${id}`) ? `desk:${id}` : `agent:${id}`;
}

/**
 * Fold the board into observations: who raised work, and where it landed.
 *
 * A durable read rather than the live stream, deliberately. The host emits no
 * event naming a delegation's two ends, so the alternative is joining tool-call
 * frames — which arrive redacted and vanish on reload. The board survives both:
 * `originChatId` is the desk the raising turn was on and `assignee` is what owns
 * the card, and a card is exactly a piece of work one part of the company handed
 * another.
 *
 * Cards with no origin (the board's own `+` button) name no hand-off and are
 * skipped: an operator adding a card is not the company delegating.
 */
export function boardObservations(
  cards: { assignee?: string; originChatId?: string; updatedAt?: number }[],
): CommsObservation[] {
  const out: CommsObservation[] = [];
  for (const card of cards) {
    if (!card.originChatId || !card.assignee) continue;
    if (card.originChatId === card.assignee) continue;
    out.push({
      kind: "handed-off",
      from: card.originChatId,
      to: card.assignee,
      via: "task",
      atMillis: card.updatedAt ?? 0,
    });
  }
  return out;
}

/** Everything reachable from `id` in one hop, for the neighbourhood filter. */
export function neighbourhood(graph: CommsGraph, id: string): CommsGraph {
  const touching = graph.edges.filter((e) => e.from === id || e.to === id);
  const keep = new Set<string>([id]);
  for (const edge of touching) {
    keep.add(edge.from);
    keep.add(edge.to);
  }
  return {
    nodes: graph.nodes.filter((n) => keep.has(n.id)),
    edges: touching,
  };
}
