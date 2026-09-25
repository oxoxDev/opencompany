// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { Task } from "@/api/tasks";
import type { TeamMemberDto } from "@/api/types";
import type { TaskColumn } from "@/lib/board-columns";

/**
 * A rename must reach `currentAgentNames` even when the renamed agent is not
 * in `members` (issue: roster read omitted the agent, or hasn't landed yet).
 *
 * `#/team/<agentId>` is unvalidated (`app-shell.tsx` documents this): the
 * detail page resolves the id against the host directly, so an operator can
 * land on and rename an agent this view's own roster read never returned. If
 * the rename only ever updates a `members` row, that agent's name in every
 * other session mention stays whatever `agentNames` said until the operator
 * switches company and the shell re-fetches its snapshot.
 */

const api = vi.hoisted(() => ({
  listTasks: vi.fn(),
  fetchBoardColumns: vi.fn(),
}));

vi.mock("@/api/tasks", () => ({ listTasks: api.listTasks }));
vi.mock("@/lib/board-columns", () => ({
  fetchBoardColumns: api.fetchBoardColumns,
  IN_FLIGHT_COLUMNS: ["planning", "in_progress"],
}));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn(), warning: vi.fn(), info: vi.fn() }),
}));

vi.mock("@/views/team/AgentDetailView", () => ({
  AgentDetailView: ({
    agentId,
    agentNames,
    onAgentNameChange,
  }: {
    agentId: string;
    agentNames?: Readonly<Record<string, string>>;
    onAgentNameChange?: (agentId: string, name: string) => void;
  }) =>
    createElement(
      "div",
      { "data-testid": "agent-detail" },
      createElement("span", { "data-testid": "agent-name" }, agentNames?.[agentId] ?? "(unknown)"),
      createElement("button", {
        "data-testid": "rename",
        onClick: () => onAgentNameChange?.(agentId, "Renamed Ghost"),
      }),
    ),
}));

const { TeamView } = await import("@/views/TeamView");

/** The roster the host answers with — the ghost agent is not in it. */
const ROSTER: TeamMemberDto[] = [
  { id: "maya", name: "Maya", role: "Research Lead", description: "Tracks competitor moves." },
];

const TASKS: Task[] = [];
const COLUMNS: TaskColumn[] = [
  { id: "pending", label: "Pending", closed: false },
  { id: "working", label: "Working", closed: false },
  { id: "done", label: "Done", closed: true },
];

function fakeClient(): OpenCompanyClient {
  return {
    scopeFor: (company: string | null) => `/api/v1/${company ?? "company"}`,
    listTeam: async () => ROSTER,
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  vi.clearAllMocks();
  api.listTasks.mockResolvedValue(TASKS);
  api.fetchBoardColumns.mockResolvedValue(COLUMNS);
});

afterEach(async () => {
  await act(async () => {
    root.unmount();
  });
  container.remove();
});

function render(client: OpenCompanyClient, company = "acme", refreshKey = 0) {
  return act(async () => {
    root.render(
      createElement(TeamView, {
        client,
        company,
        sub: "ghost-agent",
        agentNames: {},
        onOpenAgent: vi.fn(),
        refreshKey,
        onRunSetup: vi.fn(),
        onManageDesks: vi.fn(),
        onNavigateToDesk: vi.fn(),
      }),
    );
  });
}

describe("a rename reaches currentAgentNames even when members omits the agent", () => {
  it("updates the displayed name after a save on an agent the roster read never returned", async () => {
    const client = fakeClient();
    await render(client);

    expect(document.querySelector('[data-testid="agent-name"]')?.textContent).toBe("(unknown)");

    await act(async () => {
      document.querySelector<HTMLElement>('[data-testid="rename"]')?.click();
    });

    expect(document.querySelector('[data-testid="agent-name"]')?.textContent).toBe("Renamed Ghost");
  });

  it("keeps the rename through a same-company refresh and drops it on a company switch", async () => {
    const client = fakeClient();
    await render(client);
    await act(async () => {
      document.querySelector<HTMLElement>('[data-testid="rename"]')?.click();
    });

    await render(client, "acme", 1);
    expect(document.querySelector('[data-testid="agent-name"]')?.textContent).toBe("Renamed Ghost");

    await render(client, "globex", 1);
    expect(document.querySelector('[data-testid="agent-name"]')?.textContent).toBe("(unknown)");
  });
});
