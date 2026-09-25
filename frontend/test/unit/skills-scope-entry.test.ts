// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { SkillsView } from "@/views/SkillsView";

/**
 * The two ways into a skill's detail panel, and the card label in front of them.
 *
 * The screen flow names both — a card click, and `Scope…` on the row menu — and
 * says there is no `Details` entry beside them because the card click is the way
 * in. They must open the **same** thing: two pieces of state here is how one
 * entry point comes to open a stale row while the other opens the live one.
 *
 * The card's reach label is asserted in the same file because it is the claim the
 * panel behind it can contradict. Before the host reported the scope the card
 * said "available for your agents to read" unconditionally, which is false for
 * exactly the companies that bothered to scope.
 */

const AGENTS = [
  { id: "ceo", state: "inherited" as const, holds: true },
  { id: "writer", state: "included" as const, holds: true },
  { id: "hermit", state: "excluded" as const, holds: false },
];

const INSTALLED = [
  {
    id: "brand-voice",
    name: "Brand Voice",
    description: "How we sound.",
    category: "Content",
    source: "custom",
    enabled: true,
    agents: AGENTS,
  },
];

const TEAM = AGENTS.map((agent) => ({
  id: agent.id,
  role: "Worker",
  skills: {
    requested:
      agent.state === "inherited"
        ? null
        : agent.state === "included"
          ? ["brand-voice"]
          : [],
    companyAvailable: ["brand-voice", "invoicing"],
    effective: agent.holds ? ["brand-voice"] : [],
    overridden: false,
  },
}));

function clientWith(installed: unknown[] = INSTALLED): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/companies/acme",
    listTeam: () => Promise.resolve(TEAM),
    updateAgent: vi.fn(() => Promise.resolve({})),
    get: (path: string) => {
      if (path.endsWith("/auth/me")) {
        return Promise.resolve({
          id: "u1",
          email: "a@b.c",
          role: "admin",
          company: "acme",
          hasPassword: true,
        });
      }
      if (path.endsWith("/skills/registry")) return Promise.resolve([]);
      if (path.endsWith("/skills")) return Promise.resolve(installed);
      return Promise.reject(new Error(`unexpected GET ${path}`));
    },
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

async function show(client: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(SkillsView, { client, company: "acme" }));
  });
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

/** The panel renders through a portal, so it lands on `document`. */
function anywhere(testid: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(`[data-testid="${testid}"]`);
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

beforeEach(() => {
  window.location.hash = "";
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
  vi.restoreAllMocks();
});

describe("opening a skill", () => {
  it("opens the panel from the card", async () => {
    await show(clientWith());
    expect(
      anywhere("skill-detail-panel"),
      "closed until something opens it",
    ).toBeNull();

    const card = container.querySelector('[data-testid="skill-card-open"]');
    expect(card, "the card's title region is a real button").not.toBeNull();
    expect(
      card?.tagName,
      "a real button, so focus, Enter and Space come free",
    ).toBe("BUTTON");
    await click(card!);

    expect(anywhere("skill-detail-name")?.textContent).toBe("Brand Voice");
  });

  it("opens the same panel from the row menu's Scope…", async () => {
    await show(clientWith());
    await click(container.querySelector('[data-testid="skill-row-menu"]')!);
    const entry = anywhere("skill-menu-scope");
    expect(entry, "`Scope…` is on the menu").not.toBeNull();
    await click(entry!);

    expect(anywhere("skill-detail-name")?.textContent).toBe("Brand Voice");
    // The panel lists the same roster either way in, because both entry points
    // set one piece of state.
    expect(anywhere("skill-detail-agents")).not.toBeNull();
  });

  it("offers no Details entry beside Scope…", async () => {
    // The screen flow is explicit: the card click is what opens the panel, and
    // scope lives there. A second entry named `Details` would be a second answer
    // to "how do I open this".
    await show(clientWith());
    await click(container.querySelector('[data-testid="skill-row-menu"]')!);
    expect(anywhere("skill-menu-details")).toBeNull();
  });
});

describe("the card's reach label", () => {
  it("counts the teammates that hold it rather than claiming every agent does", async () => {
    await show(clientWith());
    expect(
      container.querySelector('[data-testid="skill-reach"]')?.textContent,
    ).toContain("2 of 3 agents");
  });

  it("keeps the unconditional claim when the host does not report the scope", async () => {
    // Absent is not zero. A host that says nothing about the scope cannot be
    // quoted as saying nobody holds the skill.
    await show(clientWith([{ ...INSTALLED[0], agents: undefined }]));
    expect(
      container.querySelector('[data-testid="skill-reach"]')?.textContent,
    ).toContain("Available for your agents to read");
  });

  it("says hidden for a switched-off skill whatever the scope says", async () => {
    await show(clientWith([{ ...INSTALLED[0], enabled: false }]));
    expect(
      container.querySelector('[data-testid="skill-reach"]')?.textContent,
    ).toContain("Hidden from agents");
  });
});
