import { describe, expect, it } from "vitest";

import type { WorkflowGraph } from "@/api/workflows";
import {
  layout,
  minimapDimensions,
  minimapViewBox,
} from "@/views/workflows/graph";

/**
 * Issue #1259: the minimap's own container shrinks to match a linear graph's
 * aspect ratio, instead of always requesting React Flow's 200x150 default —
 * and, unlike React Flow's built-in `<MiniMap>`, the SCALE used to draw node
 * rects is computed from node positions alone, never the live pan/zoom
 * viewport.
 *
 * #1230 made the minimap draw real node `<rect>`s by giving every laid-out
 * node an `initialWidth`/`initialHeight` hint. But `layout()` places every
 * node's depth on the x-axis and sibling index on the y-axis, and every
 * shipped company workflow template is one node per depth layer — so the real
 * content bounding box for an 8-10 node pipeline is thousands of pixels wide
 * and one node (64px) tall. React Flow's built-in `<MiniMap>` "contain"-fits a
 * bounding box into whatever container it's given (`viewScale =
 * Math.max(scaledWidth, scaledHeight)`, not configurable via any prop), so fit
 * into the 200x150 default, the viewBox's height gets padded ~30-40x past the
 * real content — each node rect renders as an imperceptible sliver. The rects
 * existed (#1230), but were still invisible in practice.
 *
 * `minimapDimensions` shrinks the requested container height instead of
 * fighting React Flow's fit algorithm. That alone measured as NO improvement
 * in a real browser, though: the built-in `<MiniMap>`'s bounding box is the
 * union of node bounds and the CURRENT VIEWPORT, and for a wide graph
 * clamped at React Flow's default `minZoom`, the on-screen viewport is itself
 * far taller than any node — dominating the bounding box regardless of
 * container size. `minimapViewBox` (what `WorkflowMiniMap.tsx` actually
 * draws with) computes the same "contain" fit off `contentBounds` alone, so
 * it isn't subject to that coupling — these tests assert the property that
 * matters: a node's real height ends up a healthy fraction of the drawn
 * viewBox's height, not ~3% of one inflated by an unrelated viewport rect.
 *
 * A graph with real vertical spread (true siblings sharing a layer) needs no
 * shrink and keeps the previous 200x150 default untouched.
 */

function graph(nodes: { id: string; kind: string; name: string }[], edges: { from: string; to: string }[] = []): WorkflowGraph {
  return { id: "w", name: "W", version: "v1", nodes, edges } as unknown as WorkflowGraph;
}

// Nine nodes, purely linear — the pathological shape the issue calls out
// ("most shipped company templates, 8-10 nodes in a line").
const LINEAR_CHAIN = graph(
  [
    { id: "n0", kind: "trigger", name: "Start" },
    { id: "n1", kind: "agent", name: "Step 1" },
    { id: "n2", kind: "agent", name: "Step 2" },
    { id: "n3", kind: "agent", name: "Step 3" },
    { id: "n4", kind: "agent", name: "Step 4" },
    { id: "n5", kind: "agent", name: "Step 5" },
    { id: "n6", kind: "agent", name: "Step 6" },
    { id: "n7", kind: "agent", name: "Step 7" },
    { id: "n8", kind: "output", name: "Done" },
  ],
  [
    { from: "n0", to: "n1" },
    { from: "n1", to: "n2" },
    { from: "n2", to: "n3" },
    { from: "n3", to: "n4" },
    { from: "n4", to: "n5" },
    { from: "n5", to: "n6" },
    { from: "n6", to: "n7" },
    { from: "n7", to: "n8" },
  ],
);

// A diamond: `b` and `c` are true siblings — both depend on `a`, both feed
// `d` — so they land in the same depth layer at different rows, giving the
// graph real vertical spread. `layout()` puts a=(0,0), b=(300,0), c=(300,150),
// d=(600,0); bounds work out to 790x214, and 214 already needs the full
// default 150-tall container to keep a comfortable node-height fraction (the
// `minimapDimensions` formula's own ideal height for those bounds is ~217,
// above the 150 ceiling), so this graph should render with the untouched
// default.
const DIAMOND = graph(
  [
    { id: "a", kind: "trigger", name: "Start" },
    { id: "b", kind: "agent", name: "Branch B" },
    { id: "c", kind: "agent", name: "Branch C" },
    { id: "d", kind: "output", name: "Join" },
  ],
  [
    { from: "a", to: "b" },
    { from: "a", to: "c" },
    { from: "b", to: "d" },
    { from: "c", to: "d" },
  ],
);

const EMPTY = graph([]);

const SINGLE = graph([{ id: "only", kind: "trigger", name: "Just one" }]);

describe("minimapDimensions", () => {
  it("shrinks the container for a long linear chain, instead of the fixed 150 default", () => {
    const { nodes } = layout(LINEAR_CHAIN);
    const { width, height } = minimapDimensions(nodes);

    expect(width).toBe(200);
    // Strictly below the old always-used default — the whole point of the fix.
    expect(height).toBeLessThan(150);
    // Never below the usable-strip floor.
    expect(height).toBeGreaterThanOrEqual(32);

    // The real content bounds for this 9-node chain: 8 columns of 300px plus
    // one node's own width, 1 node's height (every node sits at y=0).
    const contentWidth = 8 * 300 + 190;
    const contentHeight = 64;

    // At the returned height, a node's real height now occupies a real,
    // perceptible fraction of the minimap — not the ~3.3% it would occupy at
    // the old fixed 150 (64 / (viewScale-inflated ~2112), per the issue).
    // React Flow's own fit multiplies whichever axis needs more zoom-out by
    // Math.max(scaledWidth, scaledHeight); for this wide-vs-short content that
    // is always the width axis, so the resulting node-height fraction of the
    // rendered minimap is contentHeight / ((contentWidth / width) * height).
    const viewScale = contentWidth / width;
    const fraction = contentHeight / (viewScale * height);
    expect(fraction).toBeGreaterThan(0.1);
  });

  it("keeps the default 200x150 for a graph with real vertical spread", () => {
    const { nodes } = layout(DIAMOND);
    const { width, height } = minimapDimensions(nodes);
    expect(width).toBe(200);
    expect(height).toBe(150);
  });

  it("keeps the default for a single node — not itself a pathological case", () => {
    const { nodes } = layout(SINGLE);
    const { width, height } = minimapDimensions(nodes);
    expect(width).toBe(200);
    expect(height).toBe(150);
  });

  it("returns finite, sane defaults for an empty graph, never NaN or Infinity", () => {
    const { nodes } = layout(EMPTY);
    expect(nodes).toHaveLength(0);
    const { width, height } = minimapDimensions(nodes);
    expect(width).toBe(200);
    expect(height).toBe(150);
    expect(Number.isFinite(width)).toBe(true);
    expect(Number.isFinite(height)).toBe(true);
  });
});

describe("minimapViewBox", () => {
  it("keeps a linear chain's node height a healthy fraction of the drawn box — not ~3%", () => {
    const { nodes } = layout(LINEAR_CHAIN);
    const { height: containerHeight } = minimapDimensions(nodes);
    const box = minimapViewBox(nodes);

    // Same content bounds as the minimapDimensions test above.
    const contentHeight = 64;

    // This is the actual number `WorkflowMiniMap.tsx` draws with — unlike
    // React Flow's built-in `<MiniMap>`, nothing about the current pan/zoom
    // viewport feeds into it, so this fraction is deterministic from the
    // graph alone and doesn't depend on window size, zoom level, or whether
    // the canvas has even mounted yet.
    const fraction = contentHeight / box.height;
    expect(fraction).toBeGreaterThan(0.1);

    // The box's own height should be a small multiple of the container it's
    // fit into (React Flow's default offsetScale padding aside) — not the
    // 30-40x inflation the issue reported against the built-in component's
    // viewport-unioned bounding box.
    expect(box.height).toBeLessThan(containerHeight * 20);
  });

  it("returns a finite box for an empty graph, never NaN", () => {
    const { nodes } = layout(EMPTY);
    const box = minimapViewBox(nodes);
    expect(Number.isFinite(box.x)).toBe(true);
    expect(Number.isFinite(box.y)).toBe(true);
    expect(Number.isFinite(box.width)).toBe(true);
    expect(Number.isFinite(box.height)).toBe(true);
    expect(box.width).toBeGreaterThan(0);
    expect(box.height).toBeGreaterThan(0);
  });

  it("scales a single node's own bounds (190x64) into the default 200x150 container", () => {
    const { nodes } = layout(SINGLE);
    const box = minimapViewBox(nodes);
    // Width is the tighter axis (190/200 = 0.95 > 64/150 = 0.4267), so it
    // determines viewScale; the box is padded by 2x the default offsetScale
    // (5 * viewScale) on each axis, same as React Flow's built-in component.
    const viewScale = 190 / 200;
    const offset = 5 * viewScale;
    expect(box.width).toBeCloseTo(viewScale * 200 + offset * 2, 6);
    expect(box.height).toBeCloseTo(viewScale * 150 + offset * 2, 6);
  });
});
