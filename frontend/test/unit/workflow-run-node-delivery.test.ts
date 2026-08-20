// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type {
  DeliveryReport,
  WorkflowGraph,
  WorkflowRunOutcome,
  WorkflowRunResult,
} from "@/api/workflows";
import { layout } from "@/views/workflows/graph";
import {
  isUndelivered,
  undeliveredCount,
  undeliveredNodes,
} from "@/views/workflows/run-health";
import { RunHistoryPanel } from "@/views/workflows/RunHistoryPanel";
import { RunResultPanel } from "@/views/workflows/RunResultPanel";

/**
 * Issue #981, the half PR #1165 left open: a run whose report was dropped reads
 * `undelivered` at the run level, and the node the operator clicks into still
 * claimed `ok` — two readings contradicting each other on one screen.
 *
 * The fix is NOT a fourth node status. `nodes[].status` answers "did the engine
 * run this step?" and for a dropped report the honest answer is `ok`: delivery
 * is post-engine, the node ran, its work stands. So the node cell stops being
 * ALONE — every surface that renders a node renders the delivery beside it,
 * explicitly labelled. These tests pin BOTH halves: the `ok` that must not move,
 * and the delivery marker that must appear next to it.
 *
 * They also pin the second half of the issue: two `skipped` reasons stop
 * counting as undelivered, and the third deliberately does not.
 */

function delivery(over: Partial<DeliveryReport> = {}): DeliveryReport {
  return {
    node: "report",
    kind: "channel",
    target: "engineering",
    status: "failed",
    detail: "the destination channel is not wired on this runtime",
    reason: "channel-not-wired",
    ...over,
  };
}

const DROPPED = delivery();
const DRY = delivery({ status: "skipped", reason: "dry-run" });
const AGAIN = delivery({ status: "skipped", reason: "already-delivered" });
const NOWHERE = delivery({
  status: "skipped",
  kind: "none",
  target: undefined,
  reason: "no-destination-configured",
});

describe("what counts as a report that did not go out", () => {
  it("excuses a test run — nothing was attempted, on purpose", () => {
    expect(isUndelivered(DRY)).toBe(false);
    expect(undeliveredCount([DRY])).toBe(0);
    expect(undeliveredNodes([DRY]).size).toBe(0);
  });

  it("excuses a report an earlier run in the lineage already sent", () => {
    expect(isUndelivered(AGAIN)).toBe(false);
    expect(undeliveredCount([AGAIN])).toBe(0);
  });

  // The deliberate NON-move, and the reason the other two could move at all:
  // this report was produced and then lost, with nothing accounting for it.
  // Issue #925 added the row so that case stops being indistinguishable from a
  // graph that routed nothing on purpose.
  it("still counts an output node with nowhere to send", () => {
    expect(isUndelivered(NOWHERE)).toBe(true);
    expect(undeliveredCount([NOWHERE])).toBe(1);
  });

  it("counts a row whose reason the host never recorded", () => {
    // A host predating issue #248 sends no `reason` at all. An unreadable
    // reason must not excuse a report from the number an operator acts on.
    expect(isUndelivered(delivery({ status: "skipped", reason: undefined }))).toBe(
      true,
    );
  });

  it("scopes the two exemptions to `skipped`", () => {
    for (const status of ["failed", "denied"] as const) {
      expect(isUndelivered(delivery({ status, reason: "dry-run" }))).toBe(true);
      expect(
        isUndelivered(delivery({ status, reason: "already-delivered" })),
      ).toBe(true);
    }
  });

  it("never counts a sent or a parked report, whatever its reason", () => {
    expect(isUndelivered(delivery({ status: "sent", reason: "channel-posted" }))).toBe(
      false,
    );
    expect(
      isUndelivered(delivery({ status: "pending", reason: "parked-for-approval" })),
    ).toBe(false);
  });

  it("joins the dropped rows to the nodes that produced them", () => {
    const nodes = undeliveredNodes([
      DROPPED,
      delivery({ node: "second", status: "sent", reason: "channel-posted" }),
      delivery({ node: "third", status: "skipped", reason: "dry-run" }),
    ]);
    expect([...nodes]).toEqual(["report"]);
  });
});

const GRAPH: WorkflowGraph = {
  id: "digest",
  name: "Digest",
  version: "v1",
  nodes: [
    { id: "draft", kind: "agent", name: "Draft" },
    { id: "report", kind: "output", name: "Report" },
  ],
  edges: [{ from: "draft", to: "report" }],
};

describe("the canvas card", () => {
  it("marks the dropped node and nothing else, without touching its run state", () => {
    const { nodes } = layout(
      GRAPH,
      { draft: "ok", report: "ok" },
      { draft: 120, report: 0 },
      undeliveredNodes([DROPPED]),
    );
    const byId = new Map(nodes.map((n) => [n.id, n.data]));
    // The step ran, and the card still says so.
    expect(byId.get("report")?.runState).toBe("ok");
    expect(byId.get("report")?.reportUndelivered).toBe(true);
    // A node with no dropped report is byte-for-byte what it was before.
    expect(byId.get("draft")?.reportUndelivered).toBeUndefined();
  });

  it("marks nothing for a test run", () => {
    const { nodes } = layout(GRAPH, { report: "ok" }, {}, undeliveredNodes([DRY]));
    expect(
      nodes.every((n) => n.data.reportUndelivered === undefined),
    ).toBe(true);
  });
});

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

const RUN: WorkflowRunOutcome = {
  seq: 1,
  atMillis: 1_700_000_000_000,
  workflowId: "digest",
  scheduled: false,
  runId: "run-1",
  deliveries: [DROPPED],
  pendingApprovals: [],
  verdict: "undelivered",
  nodes: [
    { nodeId: "draft", status: "ok", elapsedMs: 120 },
    { nodeId: "report", status: "ok", elapsedMs: 0 },
  ],
};

describe("the history panel's node chips", () => {
  it("keeps the node's `ok` AND says the report did not go out", () => {
    act(() => {
      root.render(
        createElement(RunHistoryPanel, {
          runs: [RUN],
          graph: GRAPH,
          workflowName: "Digest",
          onClose: () => {},
          selectedRunSeq: null,
          onSelectRun: () => {},
        }),
      );
    });
    // The fact that must NOT have moved: the engine ran both nodes.
    expect(
      container.querySelectorAll('[data-testid="workflow-run-node-ok"]'),
    ).toHaveLength(2);
    expect(
      container.querySelector('[data-testid="workflow-run-node-error"]'),
    ).toBeNull();
    // The fact that was missing: exactly one of them lost its report.
    const marks = container.querySelectorAll(
      '[data-testid="workflow-run-node-undelivered"]',
    );
    expect(marks).toHaveLength(1);
    expect(marks[0]?.textContent).toContain("not delivered");
  });

  it("marks no chip when the run delivered what it routed", () => {
    act(() => {
      root.render(
        createElement(RunHistoryPanel, {
          runs: [
            {
              ...RUN,
              verdict: "ok",
              deliveries: [
                delivery({ status: "sent", reason: "channel-posted" }),
              ],
            },
          ],
          graph: GRAPH,
          workflowName: "Digest",
          onClose: () => {},
          selectedRunSeq: null,
          onSelectRun: () => {},
        }),
      );
    });
    expect(
      container.querySelector('[data-testid="workflow-run-node-undelivered"]'),
    ).toBeNull();
  });
});

const RESULT: WorkflowRunResult = {
  output: {},
  pendingApprovals: [],
  runId: "run-1",
  deliveries: [DROPPED],
  nodes: [
    { nodeId: "draft", status: "ok", elapsedMs: 120 },
    { nodeId: "report", status: "ok", elapsedMs: 0 },
  ],
};

describe("the run drawer's Steps trail", () => {
  it("badges the dropped step beside its `ok`, not instead of it", () => {
    act(() => {
      root.render(
        createElement(RunResultPanel, {
          result: RESULT,
          graph: GRAPH,
          request: "",
          onClose: () => {},
        }),
      );
    });
    const timeline = container.querySelector(
      '[data-testid="workflow-run-node-timeline"]',
    );
    expect(timeline?.textContent).toContain("ok");
    const marks = container.querySelectorAll(
      '[data-testid="workflow-run-step-undelivered"]',
    );
    expect(marks).toHaveLength(1);
  });

  it("badges nothing on a test run, whose rows were never attempted", () => {
    act(() => {
      root.render(
        createElement(RunResultPanel, {
          result: { ...RESULT, dryRun: true, deliveries: [DRY] },
          graph: GRAPH,
          request: "",
          onClose: () => {},
        }),
      );
    });
    expect(
      container.querySelector('[data-testid="workflow-run-step-undelivered"]'),
    ).toBeNull();
  });
});
