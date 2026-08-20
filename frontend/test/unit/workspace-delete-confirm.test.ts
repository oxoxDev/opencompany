// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { ConnectionScopeProvider } from "@/connections/ConnectionContext";
import { WorkspaceView } from "@/views/WorkspaceView";

/**
 * Issue #1255: deleting a note or folder via the Workspace explorer's Actions
 * menu used to call the delete API on the first click, no confirmation, no
 * undo — and for a folder that one click took its entire nested subtree with
 * it. This pins the fix the same way `ledger-retire-confirm.test.ts` pins
 * #1216 on the Ledgers screen: render the real view against a fake host and
 * assert the delete API is not called until a confirm button is pressed.
 */

/** A minimal `FsNode` off the wire — only the fields the tree reads. */
function node(over: {
  id: string;
  name: string;
  kind: "folder" | "file";
  parentId?: string;
}) {
  return { ...over, updatedAt: 1 };
}

/** A fake host: `get` answers the workspace tree read, `del` the delete call. */
function client(
  tree: ReturnType<typeof node>[],
  del: (path: string) => Promise<unknown>,
): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/company/acme",
    get: vi.fn().mockResolvedValue(tree),
    del: vi.fn(del),
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

// Both the dropdown's Actions menu and the AlertDialog render through a
// portal onto `document.body`, not inside `container` — so lookups search the
// whole document, the same way `ledger-retire-confirm.test.ts` reaches
// `ledger-retire-confirm` and `workflow-index-first.test.ts` reaches
// `workflow-delete-confirm`.
function button(label: string): HTMLButtonElement {
  const found = Array.from(document.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === label,
  );
  if (!found) throw new Error(`no “${label}” button in:\n${document.body.innerHTML}`);
  return found as HTMLButtonElement;
}

function maybeButton(label: string): HTMLButtonElement | undefined {
  return Array.from(document.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === label,
  ) as HTMLButtonElement | undefined;
}

// `DropdownMenuItem` (base-ui `Menu.Item`) renders as `role="menuitem"`, not a
// `<button>` — a separate lookup from the plain `<button>`s the AlertDialog
// (and everything else in the tree) uses.
function menuItem(label: string): HTMLElement {
  const found = Array.from(document.querySelectorAll('[role="menuitem"]')).find(
    (el) => el.textContent?.trim() === label,
  );
  if (!found) throw new Error(`no “${label}” menu item in:\n${document.body.innerHTML}`);
  return found as HTMLElement;
}

// Each tree row is exactly one `div.group` (see `TreeRow` in WorkspaceView.tsx),
// holding the row's own name button and its own Actions dropdown trigger — so
// matching on that class picks the one row rather than an ancestor container
// that happens to also contain the name text and *some* Actions button.
function actionsButtonFor(name: string): HTMLButtonElement {
  const row = Array.from(container.querySelectorAll("div.group")).find((d) =>
    d.textContent?.includes(name),
  );
  const found = row?.querySelector('[aria-label="Actions"]');
  if (!found) throw new Error(`no Actions button for “${name}” in:\n${container.innerHTML}`);
  return found as HTMLButtonElement;
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

async function render(host: OpenCompanyClient) {
  await act(async () => {
    root.render(
      createElement(ConnectionScopeProvider, {
        scope: { connection: "c1", company: "acme" },
        children: createElement(WorkspaceView, { client: host, company: "acme" }),
      }),
    );
  });
}

/** Open the row's `…` Actions menu, then click its "Delete" item. */
async function clickDelete(name: string) {
  await act(async () => {
    actionsButtonFor(name).click();
  });
  await act(async () => {
    menuItem("Delete").click();
  });
}

describe("deleting a note or folder asks before it deletes (issue #1255)", () => {
  it("opens a confirm dialog on a file's Delete and does not call the delete API", async () => {
    const tree = [node({ id: "note-1", name: "Plan.md", kind: "file" })];
    const del = vi.fn(async (_path: string) => undefined);
    await render(client(tree, del));

    await clickDelete("Plan");

    expect(del).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain("Delete “Plan”?");
    expect(document.querySelector('[data-testid="workspace-delete-confirm"]')).not.toBeNull();
  });

  it("Keep it dismisses the dialog without ever calling the delete API", async () => {
    const tree = [node({ id: "note-1", name: "Plan.md", kind: "file" })];
    const del = vi.fn(async (_path: string) => undefined);
    await render(client(tree, del));

    await clickDelete("Plan");
    expect(maybeButton("Keep it")).toBeDefined();

    await act(async () => {
      button("Keep it").click();
    });

    expect(del).not.toHaveBeenCalled();
    expect(document.querySelector('[data-testid="workspace-delete-confirm"]')).toBeNull();
  });

  it("names the nested note and deletes only the folder id once confirmed", async () => {
    const tree = [
      node({ id: "folder-1", name: "Campaigns", kind: "folder" }),
      node({ id: "note-1", name: "Q3.md", kind: "file", parentId: "folder-1" }),
    ];
    const del = vi.fn(async (_path: string) => undefined);
    await render(client(tree, del));

    // The folder's own row (and its Actions button) is present whether or not
    // it is expanded — subtreeCounts reads the flat node list, not the UI's
    // expanded set — so deleting a collapsed folder still names its contents.
    await clickDelete("Campaigns");

    expect(document.body.textContent).toContain("1 note");
    expect(del).not.toHaveBeenCalled();

    await act(async () => {
      button("Delete folder").click();
    });

    expect(del).toHaveBeenCalledTimes(1);
    expect((del.mock.calls[0] as [string])[0]).toContain("folder-1");
    expect((del.mock.calls[0] as [string])[0]).not.toContain("note-1");
  });
});
