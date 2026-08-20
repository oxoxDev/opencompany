// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { DeskDto, TeamMemberDto } from "@/api/types";
import { OrgChartView } from "@/views/company/OrgChartView";
import { TeamView } from "@/views/TeamView";

/**
 * Issue #1207: the Company and Desks headers put their actions on a row of
 * their own, anchored to nothing — floating beneath a two-line title+
 * description block rather than sharing the heading's line.
 *
 * The fix puts the heading and its actions on one row, right-aligned against
 * the heading, with the description as its own line beneath — the shape
 * `workflow-toolbar-layout.test.ts` pins for the workflow detail toolbar
 * (#1135/#1138). These assertions read the rendered DOM because the property
 * under test — "the heading and the actions are on the same row, and the
 * description is not" — is a fact about layout structure that no pure helper
 * holds.
 */

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), {
    success: vi.fn(),
    error: vi.fn(),
    warning: vi.fn(),
    info: vi.fn(),
  }),
}));

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  window.location.hash = "";
});

const testId = (id: string) => container.querySelector<HTMLElement>(`[data-testid="${id}"]`);

/** The heading and the actions row share a parent, and the description does not. */
function assertHeadingActionsShareRowDescriptionBeneath(headerTestId: string, heading: string) {
  const row = testId(headerTestId);
  expect(row).not.toBeNull();

  const h1 = [...(row?.querySelectorAll("h1") ?? [])].find((el) => el.textContent === heading);
  expect(h1).toBeTruthy();
  expect(h1?.parentElement).toBe(row);

  // The actions live in the same row as the heading, as its sibling.
  const buttons = row?.querySelectorAll("button") ?? [];
  expect(buttons.length).toBeGreaterThan(0);
  for (const button of buttons) {
    expect(row?.contains(button)).toBe(true);
  }

  // The description is NOT inside the heading/actions row — it is a sibling
  // that follows it, so it reads as its own line beneath.
  const description = row?.parentElement?.querySelector("p");
  expect(description).toBeTruthy();
  expect(row?.contains(description ?? null)).toBe(false);
  expect(description?.previousElementSibling).toBe(row);
}

describe("Company header (#1207)", () => {
  function client(over: { team?: TeamMemberDto[] } = {}): OpenCompanyClient {
    return {
      scopeFor: () => "/api/v1/company/acme",
      get: vi.fn(async (path: string) => {
        if (path.endsWith("/auth/me")) return { id: "u1", email: "op@acme.test", role: "admin" };
        if (path.endsWith("/users")) return [];
        if (path.endsWith("/tasks")) return [];
        return [];
      }),
      listTeam: vi.fn().mockResolvedValue(over.team ?? []),
    } as unknown as OpenCompanyClient;
  }

  it("puts the heading and its actions on one row, with the description beneath", async () => {
    await act(async () => {
      root.render(
        createElement(TeamView, {
          client: client({
            team: [{ id: "maya", name: "Maya", role: "Analyst" } as TeamMemberDto],
          }),
          company: "acme",
          sub: null,
          onOpenAgent: vi.fn(),
          onManageDesks: vi.fn(),
        }),
      );
    });
    await act(async () => {});

    assertHeadingActionsShareRowDescriptionBeneath("company-header", "Company");
    expect(testId("company-manage-desks")).not.toBeNull();
  });
});

describe("Desks header (#1207)", () => {
  function client(over: { desks?: DeskDto[]; team?: TeamMemberDto[] } = {}): OpenCompanyClient {
    return {
      scopeFor: () => "/api/v1/company/acme",
      listDesks: vi.fn().mockResolvedValue(over.desks ?? []),
      listTeam: vi.fn().mockResolvedValue(over.team ?? []),
      get: vi.fn().mockResolvedValue([]),
      status: vi.fn().mockResolvedValue({ name: "Acme" }),
    } as unknown as OpenCompanyClient;
  }

  it("puts the heading and its actions on one row, with the description beneath", async () => {
    await act(async () => {
      root.render(createElement(OrgChartView, { client: client(), company: "acme" }));
    });
    await act(async () => {});

    assertHeadingActionsShareRowDescriptionBeneath("desks-header", "Desks");
  });
});
