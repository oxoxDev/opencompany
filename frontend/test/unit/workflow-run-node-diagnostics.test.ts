// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { WorkflowRunResult } from "@/api/workflows";
import { RunResultPanel } from "@/views/workflows/RunResultPanel";

/**
 * The per-node timeline surfaces the engine's own broken-wiring list (issue
 * #1014).
 *
 * `ExecutionStep.diagnostics` records every config `=`-expression a node
 * resolved to `null` — the exact unresolved wiring behind a bad step — and the
 * host now carries it, paths only, onto each run node row. The drawer's step
 * list is where an operator reads a run, so this pins that the paths render
 * there, and — the load-bearing half — that only the config *path* renders,
 * never the expression text or any resolved value.
 *
 * A jsdom render rather than a pure test, for the same reason the sibling
 * `workflow-run-approvals` suite earns it: the claim is about what the drawer
 * puts on screen, which a pure function cannot see.
 */

const GRAPH = {
  id: "greet",
  name: "Greet",
  version: null,
  nodes: [{ id: "ceo", kind: "agent", name: "Draft the note", agent: "writer" }],
  edges: [],
};

function result(over: Partial<WorkflowRunResult> = {}): WorkflowRunResult {
  return {
    output: {},
    pendingApprovals: [],
    runId: "run-1",
    nodes: [
      {
        nodeId: "ceo",
        status: "ok",
        elapsedMs: 12,
        diagnostics: ["recipient"],
      },
    ],
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root;

async function render(run: WorkflowRunResult) {
  await act(async () => {
    root.render(
      createElement(RunResultPanel, {
        result: run,
        graph: GRAPH,
        request: "",
        onClose: () => {},
      }),
    );
  });
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

/** The diagnostics line rendered under a node's step row, if any. */
function diagnosticsLine(): HTMLElement | null {
  return container.querySelector<HTMLElement>(
    '[data-testid="workflow-run-node-diagnostics"]',
  );
}

describe("the step timeline surfaces a node's null-resolved config paths", () => {
  it("renders the config path under the node that came up empty", async () => {
    await render(result());

    const line = diagnosticsLine();
    expect(line).not.toBeNull();
    expect(line?.textContent).toContain("unresolved wiring:");
    expect(line?.textContent).toContain("recipient");
  });

  it("lists every path, joined, for a node with more than one miss", async () => {
    await render(
      result({
        nodes: [
          {
            nodeId: "ceo",
            status: "ok",
            elapsedMs: 8,
            diagnostics: ["recipient", "cc.0"],
          },
        ],
      }),
    );

    expect(diagnosticsLine()?.textContent).toContain("recipient, cc.0");
  });

  it("shows nothing when a node has no unresolved wiring", async () => {
    await render(
      result({
        nodes: [{ nodeId: "ceo", status: "ok", elapsedMs: 5 }],
      }),
    );

    expect(diagnosticsLine()).toBeNull();
  });
});
