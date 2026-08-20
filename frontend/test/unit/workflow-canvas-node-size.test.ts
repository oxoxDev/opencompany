import { describe, expect, it } from "vitest";

import type { WorkflowGraph } from "@/api/workflows";
import { layout } from "@/views/workflows/graph";

/**
 * Issue #1230: the laid-out nodes carry their own size.
 *
 * React Flow's minimap reads `node.internals.userNode` — the object `layout`
 * returns — and skips any node whose `measured ?? width ?? initialWidth` is
 * `undefined` (`nodeHasDimensions` in `@xyflow/system`). This layout is a
 * `useMemo` that rebuilds every node on every run-state repaint, so React
 * Flow's own measurement never survives onto that object: the minimap painted a
 * blank box in every workflow while `fitView` worked fine, because the fit
 * reads `node.internals` instead.
 *
 * A pure assertion on the layout, which is where the omission was — not a
 * render test of the minimap. The thing that broke is a missing field on a
 * plain object.
 */

function graph(nodes: { id: string; kind: string; name: string }[], edges: { from: string; to: string }[] = []): WorkflowGraph {
  return { id: "w", name: "W", version: "v1", nodes, edges } as unknown as WorkflowGraph;
}

const CHAIN = graph(
  [
    { id: "start", kind: "trigger", name: "On escalation" },
    { id: "triage", kind: "agent", name: "Triage the report" },
    { id: "out", kind: "output", name: "Escalation summary" },
  ],
  [
    { from: "start", to: "triage" },
    { from: "triage", to: "out" },
  ],
);

describe("the laid-out canvas nodes", () => {
  it("carry a size hint, so a consumer reading the node object has dimensions", () => {
    const { nodes } = layout(CHAIN);
    expect(nodes).toHaveLength(3);
    for (const node of nodes) {
      expect(node.initialWidth, `${node.id} has no width hint`).toBeGreaterThan(0);
      expect(node.initialHeight, `${node.id} has no height hint`).toBeGreaterThan(0);
    }
  });

  it("keeps the hint on every repaint, which is when the array is rebuilt", () => {
    // The memo re-runs whenever a node's run state changes; that is exactly the
    // moment React Flow's measurement was being discarded.
    const painted = layout(CHAIN, { triage: "running" }, { triage: 42 }, new Set(["out"]));
    for (const node of painted.nodes) {
      expect(node.initialWidth).toBeGreaterThan(0);
      expect(node.initialHeight).toBeGreaterThan(0);
    }
  });

  it("leaves the real geometry to React Flow", () => {
    // A hint, not an override: `measured` wins for layout and hit-testing, so
    // setting `width`/`height` here instead would freeze every node at one size
    // regardless of whether it carries a summary line.
    const { nodes } = layout(CHAIN);
    for (const node of nodes) {
      expect(node.width).toBeUndefined();
      expect(node.height).toBeUndefined();
    }
  });

  it("still lays the chain out left to right", () => {
    // The hint must not disturb positioning — the same columns as before.
    const { nodes } = layout(CHAIN);
    const x = Object.fromEntries(nodes.map((n) => [n.id, n.position.x]));
    expect(x.start).toBeLessThan(x.triage);
    expect(x.triage).toBeLessThan(x.out);
  });
});
