// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { ConnectionScopeProvider } from "@/connections/ConnectionContext";
import { WorkspaceView } from "@/views/WorkspaceView";

/**
 * Issue #1469: the commonest repair outcome opened a dialog that denied itself.
 *
 * `duplicate_folder_plan` leaves any group holding a *file* entirely alone and
 * reports every member as `fileSharesTheName` — so a note and a folder both
 * called `Specs` yields **zero** folds and two residuals. The dialog then
 * announced "0 folders share a name", drew an empty fold list, filed the
 * residuals under "These will be left for you" as if they were the leftovers of
 * work that had happened, and offered a permanently disabled "Merge 0".
 *
 * These pin the branch: the residual-only outcome is now the thing the dialog
 * is about, every row carries its real kind and its path, and the footer has an
 * action that can actually be pressed.
 */

const TREE = [
  { id: "eng", name: "Engineering", kind: "folder", updatedAt: 1 },
  {
    id: "specs-folder",
    name: "Specs",
    kind: "folder",
    parentId: "eng",
    updatedAt: 1,
  },
  {
    id: "specs-note",
    name: "Specs",
    kind: "file",
    parentId: "eng",
    updatedAt: 1,
  },
];

const RESIDUAL_ONLY = {
  residuals: [
    {
      id: "specs-folder",
      name: "Specs",
      parentId: "eng",
      cause: "fileSharesTheName",
    },
    {
      id: "specs-note",
      name: "Specs",
      parentId: "eng",
      cause: "fileSharesTheName",
    },
  ],
};

/** The one host each test drives, so its write methods can be asserted on. */
function host() {
  return {
    scopeFor: () => "/api/v1/company/acme",
    get: vi.fn().mockResolvedValue(TREE),
    post: vi.fn().mockResolvedValue(RESIDUAL_ONLY),
    patch: vi.fn(),
    del: vi.fn(),
  };
}

let client: ReturnType<typeof host>;

let container: HTMLDivElement;
let root: Root;

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

async function openRepair() {
  client = host();
  await act(async () => {
    root.render(
      createElement(ConnectionScopeProvider, {
        scope: { connection: "c1", company: "acme" },
        children: createElement(WorkspaceView, {
          client: client as unknown as OpenCompanyClient,
          company: "acme",
        }),
      }),
    );
  });
  await act(async () => {
    (
      container.querySelector(
        '[data-testid="workspace-repair"]',
      ) as HTMLButtonElement
    ).click();
  });
}

function rows(): HTMLElement[] {
  return Array.from(
    document.querySelectorAll('[data-testid="workspace-repair-residual"]'),
  ) as HTMLElement[];
}

describe("a residual-only repair is about the residuals (issue #1469)", () => {
  it("never claims 0 folders share a name", async () => {
    await openRepair();
    const dialog = document.querySelector('[role="dialog"]');
    expect(dialog).not.toBeNull();

    expect(dialog?.textContent).toContain("Two things share a name");
    // The reported defect, verbatim.
    expect(dialog?.textContent).not.toContain("0 folders share a name");
    expect(dialog?.textContent).not.toContain("Merge 0");
  });

  it("does not frame the residuals as the leftovers of work that happened", async () => {
    await openRepair();
    const dialog = document.querySelector('[role="dialog"]');

    expect(dialog?.textContent).not.toContain("These will be left for you");
    expect(dialog?.textContent).toContain(
      "Nothing here can be merged automatically",
    );
  });

  it("offers an action that can be pressed", async () => {
    await openRepair();
    const done = Array.from(document.querySelectorAll("button")).find(
      (b) => b.textContent?.trim() === "Done",
    );
    expect(done).toBeDefined();
    expect(done?.disabled).toBe(false);

    await act(async () => {
      done?.click();
    });
    expect(
      document.querySelector('[data-testid="workspace-repair-residual"]'),
    ).toBeNull();
  });
});

describe("each residual says which one it is (issue #1469)", () => {
  it("draws the folder half as a folder, not as a second note", async () => {
    await openRepair();
    const [first, second] = rows();

    // Both were `FileText` — under an instruction reading "rename or remove one
    // of them", which is unfollowable when both look like the same kind.
    const icons = [first, second].map(
      (r) => r.querySelector("svg")?.getAttribute("class") ?? "",
    );
    expect(icons.some((c) => c.includes("lucide-folder"))).toBe(true);
    expect(icons.some((c) => c.includes("lucide-file-text"))).toBe(true);
  });

  it("says where each one lives", async () => {
    await openRepair();
    for (const row of rows())
      expect(row.textContent).toContain("in Engineering");
  });

  it("reveals the node in the tree without writing anything", async () => {
    await openRepair();
    const postsBefore = client.post.mock.calls.length;
    await act(async () => {
      rows()[0].click();
    });

    // The dialog closes onto the tree; nothing was renamed, moved or deleted.
    // Revealing must never be a write — the operator has not decided anything.
    expect(
      document.querySelector('[data-testid="workspace-repair-residual"]'),
    ).toBeNull();
    expect(client.del).not.toHaveBeenCalled();
    expect(client.patch).not.toHaveBeenCalled();
    expect(client.post.mock.calls.length).toBe(postsBefore);
  });
});
