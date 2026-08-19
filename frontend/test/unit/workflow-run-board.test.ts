// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { WorkflowRunResult } from "@/api/workflows";
import { RunResultPanel } from "@/views/workflows/RunResultPanel";

/**
 * The board block on the run drawer (issue #1014 PR-A).
 *
 * A run's `board` rows are shipped on the wire — on the synchronous run
 * response and on every history row — but the drawer never rendered them, so
 * "this run opened card X" was invisible. This suite is the one exception the
 * unit runner earns the same way `provider-detail-render` does: the thing under
 * test IS what reaches the operator's eye, and a spawned card's link to its
 * board card cannot be asserted anywhere but a render.
 */

let container: HTMLDivElement;
let root: Root;

function renderPanel(result: WorkflowRunResult): void {
  act(() => {
    root.render(
      createElement(RunResultPanel, {
        result,
        graph: null,
        request: "",
        onClose: () => {},
      }),
    );
  });
}

const BASE: WorkflowRunResult = {
  output: {},
  pendingApprovals: [],
};

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

describe("run drawer — board rows", () => {
  it("renders a spawned row and links its taskId to the card", () => {
    renderPanel({
      ...BASE,
      board: [{ action: "spawned", taskId: "t_1", title: "Draft" }],
    });
    const text = document.body.textContent ?? "";
    expect(text).toContain("Draft");
    // The card link is the canonical hash route the whole console uses.
    const link = document.querySelector('a[href="#/tasks/t_1"]');
    expect(link).not.toBeNull();
    expect(link?.textContent ?? "").toContain("Draft");
  });

  it("renders a spawnFailed row's attempted title with no card link", () => {
    renderPanel({
      ...BASE,
      board: [{ action: "spawnFailed", title: "Broken Draft" }],
    });
    const text = document.body.textContent ?? "";
    // The attempted title is the only thing that explains the failure — a
    // spawnFailed row carries no taskId, so there is no card to point at.
    expect(text).toContain("Broken Draft");
    expect(document.querySelector('a[href^="#/tasks/"]')).toBeNull();
  });
});
