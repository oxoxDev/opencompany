// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { ConnectionScopeProvider } from "@/connections/ConnectionContext";
import { WorkspaceView } from "@/views/WorkspaceView";

/**
 * Issue #1472: the Workspace carried two "Discard" buttons that destroyed the
 * only copy of an operator's words on one unconfirmed click, both rendered as
 * the quietest control in their row.
 *
 * The migration banner's Discard called `clearLegacyLocal`, which removes the
 * scoped key *and* the pre-connection origin it was adopted from — so the notes
 * can never be re-offered. Meanwhile deleting a note the host still holds is
 * gated behind an AlertDialog saying "There is no undo". The strictly more
 * recoverable act was the guarded one.
 *
 * These tests pin the fix the way `workspace-delete-confirm.test.ts` pins
 * #1255: render the real view and assert the destructive effect does not happen
 * until a confirm is pressed — plus that "Not now" makes the banner stop asking
 * *without* destroying anything, which is the exit the banner never had.
 */

/** The scoped key `lib/workspace.ts` keeps this company's old scratchpad under. */
const SCRATCHPAD_KEY = "oc-workspace:c1::acme";
/** The pre-connection origin key that `clearLegacyLocal` also removes. */
const LEGACY_KEY = "oc-workspace:acme";
const DECLINED_KEY = "oc-workspace-migration-declined:c1::acme";

/** Two notes the operator typed into the retired client-side scratchpad. */
const SCRATCHPAD = JSON.stringify([
  {
    id: "fs-1",
    name: "Runbook.md",
    kind: "file",
    parentId: null,
    content: "steps",
    updatedAt: 1,
  },
  {
    id: "fs-2",
    name: "Ideas.md",
    kind: "file",
    parentId: null,
    content: "thoughts",
    updatedAt: 1,
  },
]);

function client(): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/company/acme",
    get: vi.fn().mockResolvedValue([]),
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

function button(label: string): HTMLButtonElement {
  const found = Array.from(document.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === label,
  );
  if (!found)
    throw new Error(`no “${label}” button in:\n${document.body.innerHTML}`);
  return found as HTMLButtonElement;
}

function banner(): Element | null {
  return document.querySelector('[data-testid="workspace-migration-banner"]');
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  // jsdom has no layout, so the tree's reveal effect (issue #1371) would throw
  // on a method that does not exist there.
  Element.prototype.scrollIntoView = vi.fn();
  localStorage.clear();
  localStorage.setItem(SCRATCHPAD_KEY, SCRATCHPAD);
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  localStorage.clear();
});

async function render() {
  await act(async () => {
    root.render(
      createElement(ConnectionScopeProvider, {
        scope: { connection: "c1", company: "acme" },
        children: createElement(WorkspaceView, {
          client: client(),
          company: "acme",
        }),
      }),
    );
  });
}

describe("the migration banner's Discard asks before it destroys (issue #1472)", () => {
  it("opens a confirm naming the count, and leaves the notes in the browser", async () => {
    await render();
    expect(banner()).not.toBeNull();

    await act(async () => {
      button("Discard").click();
    });

    // The reported defect: this click *was* the delete.
    expect(localStorage.getItem(SCRATCHPAD_KEY)).toBe(SCRATCHPAD);
    expect(document.body.textContent).toContain(
      "Delete 2 notes kept only in this browser?",
    );
    expect(
      document.querySelector('[data-testid="workspace-discard-confirm"]'),
    ).not.toBeNull();
  });

  it("Keep it dismisses the confirm without touching the scratchpad", async () => {
    await render();

    await act(async () => {
      button("Discard").click();
    });
    await act(async () => {
      button("Keep it").click();
    });

    expect(localStorage.getItem(SCRATCHPAD_KEY)).toBe(SCRATCHPAD);
    expect(banner()).not.toBeNull();
  });

  it("only the confirm's own action clears the key and the origin it came from", async () => {
    localStorage.setItem(LEGACY_KEY, SCRATCHPAD);
    await render();

    await act(async () => {
      button("Discard").click();
    });
    await act(async () => {
      button("Delete notes").click();
    });

    expect(localStorage.getItem(SCRATCHPAD_KEY)).toBeNull();
    expect(localStorage.getItem(LEGACY_KEY)).toBeNull();
    expect(banner()).toBeNull();
  });
});

describe("declining the offer is not the same as destroying it (issue #1472)", () => {
  it("Not now stops the banner and keeps every note", async () => {
    await render();

    await act(async () => {
      button("Not now").click();
    });

    // The whole point of the third exit: the banner is gone and the notes are
    // not. Before #1472 the only way to stop being asked was the delete.
    expect(banner()).toBeNull();
    expect(localStorage.getItem(SCRATCHPAD_KEY)).toBe(SCRATCHPAD);
    expect(localStorage.getItem(DECLINED_KEY)).not.toBeNull();
  });

  it("stays declined across a remount, still without destroying anything", async () => {
    await render();
    await act(async () => {
      button("Not now").click();
    });

    await act(() => root.unmount());
    root = createRoot(container);
    await render();

    expect(banner()).toBeNull();
    expect(localStorage.getItem(SCRATCHPAD_KEY)).toBe(SCRATCHPAD);
  });
});

/**
 * The second Discard: the banner holding words rescued out of a note that was
 * deleted while the operator was writing in it. Its own comment in the view
 * says this is "the last copy of a paragraph" — and it was wired straight to
 * `setRescued(null)` on a bare ghost button.
 */
describe("the rescued-text Discard asks before it destroys (issue #1472)", () => {
  /** A host whose tree empties once `gone` flips — the deletion the frame reports. */
  function vanishingClient(gone: { now: boolean }): OpenCompanyClient {
    return {
      scopeFor: () => "/api/v1/company/acme",
      get: vi.fn(async (path: string) => {
        if (path.includes("/workspace/file/")) {
          return {
            id: "note-1",
            name: "Plan.md",
            content: "saved body",
            backlinks: [],
            updatedAt: 1,
          };
        }
        if (path.endsWith("/workspace")) {
          return gone.now
            ? []
            : [{ id: "note-1", name: "Plan.md", kind: "file", updatedAt: 1 }];
        }
        return [];
      }),
    } as unknown as OpenCompanyClient;
  }

  async function rescueBanner() {
    const gone = { now: false };
    const host = vanishingClient(gone);
    // No scratchpad here — the migration banner would own the word "Discard".
    localStorage.clear();

    const paint = (event?: {
      tick: number;
      nodeId: string;
      change: "removed";
    }) =>
      act(async () => {
        root.render(
          createElement(ConnectionScopeProvider, {
            scope: { connection: "c1", company: "acme" },
            children: createElement(WorkspaceView, {
              client: host,
              company: "acme",
              event,
            }),
          }),
        );
      });

    await paint();
    await act(async () => {
      (
        Array.from(container.querySelectorAll("button")).find((b) =>
          b.textContent?.includes("Plan"),
        ) as HTMLButtonElement
      ).click();
    });
    await act(async () => {
      (
        Array.from(document.querySelectorAll('[role="tab"]')).find(
          (t) => t.textContent?.trim() === "Edit",
        ) as HTMLElement
      ).click();
    });

    const editor = document.querySelector(
      '[data-testid="workspace-editor"]',
    ) as HTMLTextAreaElement;
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(
        HTMLTextAreaElement.prototype,
        "value",
      )?.set;
      setter?.call(editor, "a paragraph nobody else has");
      editor.dispatchEvent(new Event("input", { bubbles: true }));
    });

    gone.now = true;
    await paint({ tick: 1, nodeId: "note-1", change: "removed" });
  }

  it("keeps the text on screen when Discard is clicked, and asks first", async () => {
    await rescueBanner();
    expect(
      document.querySelector('[data-testid="workspace-rescued-banner"]'),
    ).not.toBeNull();

    await act(async () => {
      button("Discard").click();
    });

    // The defect: this click was the discard. The words must still be here.
    expect(
      document.querySelector('[data-testid="workspace-rescued-banner"]'),
    ).not.toBeNull();
    expect(
      (
        document.querySelector(
          '[data-testid="workspace-rescued-text"]',
        ) as HTMLTextAreaElement
      ).value,
    ).toBe("a paragraph nobody else has");
    expect(document.body.textContent).toContain("Discard your unsaved text?");
  });

  it("drops the text only once the confirm's own action is pressed", async () => {
    await rescueBanner();

    await act(async () => {
      button("Discard").click();
    });
    await act(async () => {
      button("Keep it").click();
    });
    expect(
      document.querySelector('[data-testid="workspace-rescued-banner"]'),
    ).not.toBeNull();

    await act(async () => {
      button("Discard").click();
    });
    await act(async () => {
      button("Discard text").click();
    });
    expect(
      document.querySelector('[data-testid="workspace-rescued-banner"]'),
    ).toBeNull();
  });
});

describe("the banner says what Import will do (issue #1472)", () => {
  it("names the destination folder and that the browser copy is removed", async () => {
    await render();
    const text = banner()?.textContent ?? "";

    expect(text).toContain("Imported from this browser");
    expect(text).toContain("removes this browser's copy");
    expect(text).toContain("moves them rather than copying them");
  });
});
