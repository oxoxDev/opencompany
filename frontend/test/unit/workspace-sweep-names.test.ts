// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { TeamMemberDto } from "@/api/types";
import { ConnectionScopeProvider } from "@/connections/ConnectionContext";
import { WorkspaceView } from "@/views/WorkspaceView";

/**
 * Issue #1479: the tidy's confirm asked an operator to approve removing folders
 * it identified by raw ULID.
 *
 * A swept folder's `name` *is* a roster id — that is what `Agents/<id>/`
 * folders are called — and this view already resolves those ids in the tree
 * sitting behind the modal (issue #973, `rosterDisplayName`). The dialog exists
 * because "17 empty folders" is a number nobody can verify; seven opaque ids
 * are exactly as unverifiable.
 */

function member(id: string, name: string): TeamMemberDto {
  return { id, name, role: name };
}

const SWEEPABLE = [
  { id: "f-1", name: "01JQZY8T7K" },
  { id: "f-2", name: "01JQZY8T7A" },
  { id: "f-3", name: "01JQZY8T7Z" },
];

function client(team: TeamMemberDto[]): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/company/acme",
    get: vi
      .fn()
      .mockResolvedValue([
        { id: "agents", name: "Agents", kind: "folder", updatedAt: 1 },
      ]),
    post: vi.fn().mockResolvedValue({ wouldRemove: SWEEPABLE }),
    listTeam: vi.fn().mockResolvedValue(team),
  } as unknown as OpenCompanyClient;
}

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

async function openSweep(team: TeamMemberDto[]) {
  await act(async () => {
    root.render(
      createElement(ConnectionScopeProvider, {
        scope: { connection: "c1", company: "acme" },
        children: createElement(WorkspaceView, {
          client: client(team),
          company: "acme",
        }),
      }),
    );
  });
  await act(async () => {
    (
      container.querySelector(
        '[data-testid="workspace-sweep"]',
      ) as HTMLButtonElement
    ).click();
  });
  const list = document.querySelector(
    '[data-testid="workspace-sweep-folders"]',
  );
  if (!list) throw new Error(`no sweep list in:\n${document.body.innerHTML}`);
  return Array.from(list.querySelectorAll("li")).map(
    (li) => li.textContent?.trim() ?? "",
  );
}

describe("the tidy names the teammate, not the id (issue #1479)", () => {
  it("resolves each folder through the roster the tree behind it already uses", async () => {
    const rows = await openSweep([
      member("01JQZY8T7K", "Nadia"),
      member("01JQZY8T7A", "Bruno"),
      member("01JQZY8T7Z", "Zora"),
    ]);

    // The reported defect: these were three ULIDs.
    expect(rows.join(" ")).toContain("Nadia");
    expect(rows.join(" ")).toContain("Bruno");
    expect(rows.join(" ")).not.toContain("01JQZY8T7K");
  });

  it("sorts by the name on screen, not by whatever order the host returned", async () => {
    const rows = await openSweep([
      member("01JQZY8T7K", "Nadia"),
      member("01JQZY8T7A", "Bruno"),
      member("01JQZY8T7Z", "Zora"),
    ]);

    expect(rows.map((r) => r.split(" ")[0])).toEqual([
      "Bruno",
      "Nadia",
      "Zora",
    ]);
  });

  it("says an unresolved id is no longer on the roster, rather than showing a bare ULID", async () => {
    const rows = await openSweep([member("01JQZY8T7K", "Nadia")]);

    const orphan = rows.find((r) => r.startsWith("01JQZY8T7A"));
    expect(orphan).toBeDefined();
    expect(orphan).toContain("no longer on the roster");
    // A resolved row makes no such claim.
    expect(rows.find((r) => r.startsWith("Nadia"))).not.toContain(
      "no longer on the roster",
    );
  });
});
