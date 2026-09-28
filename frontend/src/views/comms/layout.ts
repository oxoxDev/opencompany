/**
 * Where the comms graph's nodes sit.
 *
 * Deterministic and pure, and both words are load-bearing.
 *
 * # Why not a force simulation
 *
 * The knowledge graph uses `d3-force` and is right to: it is a large, undirected
 * neighbourhood somebody explores. This graph is the opposite — small, directed,
 * layered, and **fed live**. A force simulation re-heats on every insertion, so
 * an `add_agent` frame arriving would drift every unrelated node on screen. "Who
 * spawned whom" is a claim about structure; a layout that moves when something
 * else happens destroys the spatial memory that makes the claim readable between
 * glances.
 *
 * So: two columns, stable ordering, and a node inserted where its id sorts. A
 * new agent appears in place and nothing else moves.
 */

import type { CommsGraph, CommsNode } from "@/views/comms/model";

export interface Placed {
  node: CommsNode;
  x: number;
  y: number;
}

export interface Layout {
  placed: Placed[];
  width: number;
  height: number;
}

/** Node box, and the gaps between. Named so the view and the maths agree. */
export const NODE_WIDTH = 180;
export const NODE_HEIGHT = 52;
const COLUMN_GAP = 220;
const ROW_GAP = 20;
const PADDING = 24;

/**
 * Two columns: agents on the left, desks on the right.
 *
 * Agents act and desks are acted upon — every `may-delegate` and `handed-off`
 * edge runs agent → desk — so left-to-right *is* the direction of the arrow, and
 * a reader does not have to follow an arrowhead to know which way work moved.
 *
 * A `spawned` or `spoke` edge can run agent → agent and so stays inside the
 * left column; it is drawn as a curve rather than a straight line for exactly
 * that reason.
 */
export function layoutComms(graph: CommsGraph): Layout {
  const agents = graph.nodes
    .filter((n) => n.kind === "agent")
    // Orchestrators first — a company's own line is where a reader starts — then
    // by id, so the order cannot change between renders.
    .sort((a, b) => {
      if (!!a.orchestrator !== !!b.orchestrator) return a.orchestrator ? -1 : 1;
      return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
    });
  const desks = graph.nodes
    .filter((n) => n.kind === "desk")
    .sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));

  const placed: Placed[] = [];
  const column = (nodes: CommsNode[], x: number) => {
    nodes.forEach((node, i) => {
      placed.push({ node, x, y: PADDING + i * (NODE_HEIGHT + ROW_GAP) });
    });
  };
  column(agents, PADDING);
  column(desks, PADDING + NODE_WIDTH + COLUMN_GAP);

  const rows = Math.max(agents.length, desks.length, 1);
  return {
    placed,
    width: PADDING * 2 + NODE_WIDTH * 2 + COLUMN_GAP,
    height: PADDING * 2 + rows * NODE_HEIGHT + (rows - 1) * ROW_GAP,
  };
}

/** The centre of a placed node, for drawing an edge to it. */
export function centreOf(placed: Placed): { x: number; y: number } {
  return { x: placed.x + NODE_WIDTH / 2, y: placed.y + NODE_HEIGHT / 2 };
}

/**
 * The path an edge takes.
 *
 * A cubic curve between columns and a wider bow inside one, so a spawn edge
 * (agent → agent, same column) does not lie on top of the nodes between its
 * endpoints.
 */
export function edgePath(
  from: { x: number; y: number },
  to: { x: number; y: number },
  sameColumn: boolean,
): string {
  if (sameColumn) {
    const bow = Math.max(60, Math.abs(to.y - from.y) / 2);
    return `M ${from.x} ${from.y} C ${from.x - bow} ${from.y}, ${to.x - bow} ${to.y}, ${to.x} ${to.y}`;
  }
  const mid = (from.x + to.x) / 2;
  return `M ${from.x} ${from.y} C ${mid} ${from.y}, ${mid} ${to.y}, ${to.x} ${to.y}`;
}

/**
 * How heavy an observed edge is drawn, from how often it has been seen.
 *
 * Log-scaled and capped: the difference between one hand-off and five is worth
 * seeing, the difference between fifty and five hundred is not, and a linear
 * scale makes a busy path swamp the diagram.
 */
export function edgeWeight(count: number): number {
  if (count <= 0) return 1;
  return Math.min(1 + Math.log2(count + 1), 5);
}
