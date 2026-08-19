import { describe, expect, it } from "vitest";

import {
  focusLabelIds,
  LABEL_PRIORITY,
  planLabels,
  type LabelCandidate,
} from "@/views/overview/kg/label-plan";

/**
 * The Overview graph's label declutter (issue #1104).
 *
 * The rule this replaced was one boolean, and it failed at both ends: at rest
 * only the company and its departments were named, so every agent was an
 * anonymous circle; the moment a pillar was focused, every node in the tree was
 * named at once and the names smeared into each other.
 *
 * Two properties are worth guarding here, and neither is visible from a
 * screenshot:
 *
 * 1. **Priority decides who survives a collision.** Silent failure: the label
 *    you are pointing at loses to a sibling that happened to be nominated
 *    first, and the graph looks like hover simply does nothing.
 * 2. **The overlap is measured in SCREEN space.** Labels hold one on-screen
 *    size at every camera depth (`fixedLabel` counter-scales through
 *    `--kg-cam-k`), so graph units answer this question correctly at exactly
 *    one zoom level and wrongly everywhere else — and wrongly in the direction
 *    that matters, because zooming out is what packs nodes together. That
 *    regression would restore the pile-up this issue is about while every unit
 *    of the layout still looked right.
 */

const W = 880;

const cand = (over: Partial<LabelCandidate> & { id: string }): LabelCandidate => ({
  text: "AAAA",
  x: 0,
  y: 0,
  dy: 20,
  fontPx: 10,
  priority: LABEL_PRIORITY.worker,
  ...over,
});

describe("planLabels", () => {
  it("drops the lower-priority label of a colliding pair", () => {
    const kept = planLabels(
      [
        cand({ id: "quiet", x: 0, priority: LABEL_PRIORITY.worker }),
        cand({ id: "hovered", x: 28, priority: LABEL_PRIORITY.hovered }),
      ],
      { x: 0, y: 0, w: W },
      W,
    );
    expect([...kept]).toEqual(["hovered"]);
  });

  it("keeps both when the boxes clear each other", () => {
    const kept = planLabels(
      [
        cand({ id: "left", x: 0, priority: LABEL_PRIORITY.worker }),
        cand({ id: "right", x: 32, priority: LABEL_PRIORITY.hovered }),
      ],
      { x: 0, y: 0, w: W },
      W,
    );
    expect(kept).toEqual(new Set(["left", "right"]));
  });

  it("measures in screen space, so zooming out drops a label the same graph gap kept", () => {
    const pair = [
      cand({ id: "left", x: 0, priority: LABEL_PRIORITY.hovered }),
      cand({ id: "right", x: 32, priority: LABEL_PRIORITY.worker }),
    ];
    // camera width === canvas width: one graph unit is one px, both fit
    expect(planLabels(pair, { x: 0, y: 0, w: W }, W)).toEqual(new Set(["left", "right"]));
    // pulled back to half scale the nodes are 16px apart while the labels are
    // still 24px wide — in graph units nothing changed at all
    expect([...planLabels(pair, { x: 0, y: 0, w: W * 2 }, W)]).toEqual(["left"]);
  });

  it("panning the camera never changes the outcome", () => {
    const pair = [
      cand({ id: "left", x: 0, priority: LABEL_PRIORITY.hovered }),
      cand({ id: "right", x: 32, priority: LABEL_PRIORITY.worker }),
    ];
    expect(planLabels(pair, { x: -400, y: 250, w: W }, W)).toEqual(
      planLabels(pair, { x: 0, y: 0, w: W }, W),
    );
  });

  it("collides on the rendered width, so a long name costs its neighbour", () => {
    const neighbour = cand({ id: "neighbour", x: 100, priority: LABEL_PRIORITY.worker });
    const short = cand({ id: "named", x: 0, text: "A", priority: LABEL_PRIORITY.hovered });
    const long = { ...short, text: "A".repeat(40) };
    expect(planLabels([short, neighbour], { x: 0, y: 0, w: W }, W)).toEqual(
      new Set(["named", "neighbour"]),
    );
    expect([...planLabels([long, neighbour], { x: 0, y: 0, w: W }, W)]).toEqual(["named"]);
  });

  it("separates labels that share an x but sit on different rows", () => {
    const stacked = [
      cand({ id: "row-0", x: 0, dy: 20, priority: LABEL_PRIORITY.hovered }),
      cand({ id: "row-1", x: 0, dy: 40, priority: LABEL_PRIORITY.worker }),
    ];
    expect(planLabels(stacked, { x: 0, y: 0, w: W }, W)).toEqual(new Set(["row-0", "row-1"]));
  });
});

describe("focusLabelIds", () => {
  const branches = [
    { source: "self", target: "team" },
    { source: "team", target: "task-a" },
    { source: "team", target: "task-b" },
    { source: "task-a", target: "worker" },
    { source: "worker", target: "tool" },
  ];

  it("names the focused node and its direct children, and stops there", () => {
    expect(focusLabelIds(branches, "team")).toEqual(new Set(["team", "task-a", "task-b"]));
  });

  it("follows what was clicked: an agent names its tools, not its pillar's tasks", () => {
    expect(focusLabelIds(branches, "worker")).toEqual(new Set(["worker", "tool"]));
  });

  it("names nothing when nothing is focused", () => {
    expect(focusLabelIds(branches, null).size).toBe(0);
  });
});
