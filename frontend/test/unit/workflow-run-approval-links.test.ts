// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GrantScope, Verdict, ApprovalSummary } from "@/api/types";
import type {
  WorkflowGraph,
  WorkflowRunOutcome,
  WorkflowRunResult,
} from "@/api/workflows";
import type { DecidedApproval } from "@/views/chat/model";
import { RunHistoryPanel } from "@/views/workflows/RunHistoryPanel";
import { RunResultPanel } from "@/views/workflows/RunResultPanel";

/**
 * The render half of issue #1014 (PR-B): a blocked run names the tools it gated
 * and links each parked card to the Approvals queue.
 *
 * Before this the run drawer said "decide it in Approvals" and pointed nowhere —
 * `WorkflowBlockedNode.tools` and `.approvalIds` rode the wire and were only
 * COUNTED, never rendered. Each test below fails on that code: there is no
 * gated-tool line and no link at all.
 *
 * A jsdom render rather than a pure test, because the claim is about what the
 * drawer paints — the tool text and an anchor per approval id — which a pure
 * test of the data cannot see.
 */

const GRAPH: WorkflowGraph = {
  id: "feature_pipeline",
  name: "Feature pipeline",
  version: null,
  nodes: [{ id: "spec", kind: "agent", name: "Draft the spec", agent: "writer" }],
  edges: [],
};

/** A settled run whose "spec" node blocked on two gated `publish_artifact` calls. */
function blockedOutcome(
  over: Partial<WorkflowRunOutcome> = {},
): WorkflowRunOutcome {
  return {
    seq: 1,
    atMillis: 1_700_000_000_000,
    workflowId: "feature_pipeline",
    scheduled: false,
    runId: "run-1",
    deliveries: [],
    pendingApprovals: ["spec"],
    nodes: [{ nodeId: "spec", status: "blocked", elapsedMs: 42_000 }],
    blockedNodes: [
      {
        nodeId: "spec",
        tools: ["publish_artifact", "web_search"],
        approvalIds: ["appr-1", "appr-2"],
      },
    ],
    approvals: [
      { nodeId: "spec", tool: "publish_artifact", outcome: "parked", approvalId: "appr-1" },
      { nodeId: "spec", tool: "web_search", outcome: "parked", approvalId: "appr-2" },
    ],
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root;

async function renderHistory(run: WorkflowRunOutcome) {
  await act(async () => {
    root.render(
      createElement(RunHistoryPanel, {
        runs: [run],
        graph: GRAPH,
        workflowName: "Feature pipeline",
        onClose: () => {},
        selectedRunSeq: null,
        onSelectRun: () => {},
      }),
    );
  });
}

function resultFrom(run: WorkflowRunOutcome): WorkflowRunResult {
  return {
    output: {},
    pendingApprovals: run.pendingApprovals,
    runId: run.runId,
    deliveries: run.deliveries,
    nodes: run.nodes,
    blockedNodes: run.blockedNodes,
    approvals: run.approvals,
  };
}

async function renderResult(run: WorkflowRunOutcome) {
  await act(async () => {
    root.render(
      createElement(RunResultPanel, {
        result: resultFrom(run),
        graph: GRAPH,
        request: "",
        onClose: () => {},
        // Empty live queue: the inline decidable section (#1002) does not render,
        // so this isolates PR-B's tool-name line + links.
        approvals: [] as ApprovalSummary[],
        now: 1_700_000_060_000,
        askerNames: new Map<string, string>(),
        deciding: new Map<string, Verdict>(),
        decided: {} as Record<string, DecidedApproval>,
        failed: {} as Record<string, string>,
        onDecide: (_a: ApprovalSummary, _v: Verdict, _s: GrantScope) => {},
      }),
    );
  });
}

function toolLine(): HTMLElement | null {
  return container.querySelector<HTMLElement>(
    '[data-testid="workflow-blocked-node-tools"]',
  );
}

function approvalLinks(): HTMLAnchorElement[] {
  return [
    ...container.querySelectorAll<HTMLAnchorElement>(
      '[data-testid="workflow-blocked-approval-link"]',
    ),
  ];
}

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("the run history drawer's blocked node", () => {
  it("names the tools the blocked node gated", async () => {
    await renderHistory(blockedOutcome());
    const line = toolLine();
    expect(line).not.toBeNull();
    expect(line?.textContent).toContain("publish_artifact");
    expect(line?.textContent).toContain("web_search");
  });

  it("turns each approval id into a link to the Approvals queue", async () => {
    await renderHistory(blockedOutcome());
    const links = approvalLinks();
    // Two parked cards → two links, each to the canonical queue route.
    expect(links).toHaveLength(2);
    for (const a of links) {
      expect(a.getAttribute("href")).toBe("#/approvals");
    }
    // Labelled by the tool that opened each card, from the run's receipt (#880).
    expect(links.map((a) => a.textContent)).toEqual(["publish_artifact", "web_search"]);
  });

  it("still links every approval id when an old host sent no per-call rows", async () => {
    // A host predating the receipt sends `approvalIds` but no `approvals` rows.
    // The links must still appear — one per id — with a neutral label.
    await renderHistory(
      blockedOutcome({
        approvals: undefined,
        blockedNodes: [
          { nodeId: "spec", tools: ["publish_artifact"], approvalIds: ["appr-1", "appr-2"] },
        ],
      }),
    );
    const links = approvalLinks();
    expect(links).toHaveLength(2);
    for (const a of links) expect(a.getAttribute("href")).toBe("#/approvals");
  });

  it("renders no blocked-node line for an ordinary run", async () => {
    await renderHistory(
      blockedOutcome({
        pendingApprovals: [],
        nodes: [{ nodeId: "spec", status: "ok", elapsedMs: 10 }],
        blockedNodes: undefined,
        approvals: undefined,
      }),
    );
    expect(toolLine()).toBeNull();
    expect(approvalLinks()).toHaveLength(0);
  });
});

describe("the synchronous run result panel", () => {
  it("renders the same tool names and approval links, for consistency", async () => {
    await renderResult(blockedOutcome());
    const line = toolLine();
    expect(line).not.toBeNull();
    expect(line?.textContent).toContain("publish_artifact");
    const links = approvalLinks();
    expect(links).toHaveLength(2);
    for (const a of links) expect(a.getAttribute("href")).toBe("#/approvals");
  });
});
