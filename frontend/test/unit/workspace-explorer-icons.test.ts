// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { ConnectionScopeProvider } from "@/connections/ConnectionContext";
import { WorkspaceView } from "@/views/WorkspaceView";

/**
 * Issue #1378: the explorer header is six identical icon-only ghost buttons in
 * one undifferentiated row, two of which remove folders. Their labels existed
 * only as `aria-label` — nothing a sighted operator ever meets — and the
 * sweep's confirm-and-destroy button rendered as a pale tint rather than the
 * solid red every other confirm-and-destroy in the console wears.
 */

const HEADER_LABELS = [
  "Refresh",
  "New file",
  "New folder",
  "Upload",
  "Tidy empty agent folders",
  "Repair duplicate folders",
];

let container: HTMLDivElement;
let root: Root;

function host(): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/company/acme",
    get: vi.fn().mockResolvedValue([
      { id: "agents", name: "Agents", kind: "folder", updatedAt: 1 },
      {
        id: "empty-1",
        name: "01JQ",
        kind: "folder",
        parentId: "agents",
        updatedAt: 1,
      },
    ]),
    post: vi
      .fn()
      .mockResolvedValue({ wouldRemove: [{ id: "empty-1", name: "01JQ" }] }),
  } as unknown as OpenCompanyClient;
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  Element.prototype.scrollIntoView = vi.fn();
  localStorage.clear();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

async function render() {
  await act(async () => {
    root.render(
      createElement(ConnectionScopeProvider, {
        scope: { connection: "c1", company: "acme" },
        children: createElement(WorkspaceView, {
          client: host(),
          company: "acme",
        }),
      }),
    );
  });
}

describe("every explorer icon says what it is (issue #1378)", () => {
  it("wires each of the six header buttons to a tooltip carrying its label", async () => {
    await render();

    for (const label of HEADER_LABELS) {
      const button = container.querySelector(`[aria-label="${label}"]`);
      expect(button, `no button labelled “${label}”`).not.toBeNull();
      // The defect: the label was `aria-label` and nothing else, so a sighted
      // operator met six identical glyphs.
      expect(button?.getAttribute("data-slot"), label).toBe("tooltip-trigger");
    }
  });

  it("separates the two folder-removing controls from the ones that make things", async () => {
    await render();

    const buttons = Array.from(container.querySelectorAll("[aria-label]"));
    const tidy = buttons.findIndex(
      (b) => b.getAttribute("aria-label") === "Tidy empty agent folders",
    );
    const upload = buttons.findIndex(
      (b) => b.getAttribute("aria-label") === "Upload",
    );
    expect(upload).toBeGreaterThanOrEqual(0);
    expect(tidy).toBe(upload + 1);

    // A divider sits between the make-something group and the repair group, so
    // the row is not six identical glyphs with two mines in it.
    const row = container.querySelector('[aria-label="Upload"]')?.parentElement;
    expect(row?.querySelector("span[aria-hidden].w-px")).not.toBeNull();
  });
});

describe("the sweep's confirm wears the destroy weight (issue #1378)", () => {
  it("renders Remove with the solid destructive class, not the pale tint", async () => {
    await render();

    await act(async () => {
      (
        container.querySelector(
          '[data-testid="workspace-sweep"]',
        ) as HTMLButtonElement
      ).click();
    });

    const confirm = document.querySelector(
      '[data-testid="workspace-sweep-confirm"]',
    );
    expect(confirm).not.toBeNull();
    // The same override `DeleteDialog` uses — the codebase's one weight for
    // "this button destroys something".
    expect(confirm?.className).toContain("bg-destructive");
    expect(confirm?.className).toContain("text-white");
  });
});
