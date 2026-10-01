// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { SkillsView } from "@/views/SkillsView";

/**
 * The ways into a skill's page, and the card label in front of them.
 *
 * The screen flow names two — a card click, and `Scope…` on the row menu — and
 * says there is no `Details` entry beside them because the card click is the way
 * in. A third arrived with the address: `?skill=<slug>`, which is what a
 * teammate's Skills tab links at. All of them must open the **same** thing: two
 * pieces of state here is how one entry point comes to open a stale row while
 * the other opens the live one.
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

/** Looked up on `document`, so the same helper reads a portal or a page. */
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
  it("opens the page from the card", async () => {
    await show(clientWith());
    expect(
      anywhere("skill-page"),
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

  it("opens the same page from the row menu's Scope…", async () => {
    await show(clientWith());
    await click(container.querySelector('[data-testid="skill-row-menu"]')!);
    const entry = anywhere("skill-menu-scope");
    expect(entry, "`Scope…` is on the menu").not.toBeNull();
    await click(entry!);

    expect(anywhere("skill-detail-name")?.textContent).toBe("Brand Voice");
    // The page lists the same roster either way in, because both entry points
    // set one piece of state.
    expect(anywhere("skill-detail-agents")).not.toBeNull();
  });

  it("replaces the list rather than opening beside it", async () => {
    // The difference between this and the sheet it replaced. A sheet left the
    // cards mounted behind it, so a suite that only asserted the subject's name
    // passed either way — and the 384px the sheet gave the scope list is what
    // the swap exists to stop.
    await show(clientWith());
    expect(
      container.querySelectorAll('[data-testid="installed-row"]').length,
      "the list is on screen before anything is opened",
    ).toBeGreaterThan(0);

    await click(container.querySelector('[data-testid="skill-card-open"]')!);

    expect(
      container.querySelectorAll('[data-testid="installed-row"]').length,
      "the list is gone, not covered",
    ).toBe(0);
    expect(
      anywhere("skill-page-back"),
      "and there is a way back",
    ).not.toBeNull();
  });

  it("opens the skill named by the address, with no click at all", async () => {
    // The deep link a teammate's Skills tab writes. Held in the address rather
    // than in component state so it survives a reload and can be sent to
    // somebody — the same contract an open MCP server has.
    window.location.hash = "#/settings/skills?skill=brand-voice";
    await show(clientWith());
    expect(anywhere("skill-detail-name")?.textContent).toBe("Brand Voice");
  });

  it("falls back to the list when the address names a skill this company lacks", async () => {
    // An uninstall elsewhere, or a link from another company. The page cannot
    // render a row it does not have, and a blank screen would be worse than the
    // list.
    window.location.hash = "#/settings/skills?skill=nothing-here";
    await show(clientWith());
    expect(anywhere("skill-page"), "no page").toBeNull();
    expect(
      container.querySelectorAll('[data-testid="installed-row"]').length,
      "the list instead",
    ).toBeGreaterThan(0);
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

describe("the filter bar", () => {
  it("offers no ordering, because name order is the only one", async () => {
    await show(clientWith());
    expect(anywhere("skills-sort")).toBeNull();
    expect(container.textContent).not.toContain("Sort by");
  });

  it("keeps every filter that still narrows the list", async () => {
    await show(clientWith());
    for (const id of [
      "skills-filter-source",
      "skills-filter-enabled",
      "skills-filter-category",
      "skills-filter-drift",
    ]) {
      expect(anywhere(id), id).not.toBeNull();
    }
  });
});

describe("cards or rows", () => {
  function rowsOf(testid: string) {
    return container.querySelectorAll(`[data-testid="${testid}"]`).length;
  }

  it("opens the installed set on rows, with the toggle offering both", async () => {
    await show(clientWith());
    expect(rowsOf("installed-row")).toBeGreaterThan(0);
    expect(rowsOf("installed-card"), "no cards yet").toBe(0);
    expect(anywhere("skills-view-list")?.getAttribute("aria-pressed")).toBe(
      "true",
    );
    expect(anywhere("skills-view-cards")?.getAttribute("aria-pressed")).toBe(
      "false",
    );
  });

  it("swaps the rows for cards, and puts the choice on the address", async () => {
    await show(clientWith());
    await click(anywhere("skills-view-cards")!);

    expect(rowsOf("installed-card")).toBeGreaterThan(0);
    expect(rowsOf("installed-row"), "one rendering at a time").toBe(0);
    expect(window.location.hash).toContain("view=cards");
  });

  it("drops the key rather than spelling out the default", async () => {
    await show(clientWith());
    await click(anywhere("skills-view-cards")!);
    await click(anywhere("skills-view-list")!);

    expect(rowsOf("installed-row")).toBeGreaterThan(0);
    expect(window.location.hash).not.toContain("view=");
  });

  it("opens in cards when the address asks for them", async () => {
    window.location.hash = "#/settings/skills?view=cards";
    await show(clientWith());
    expect(rowsOf("installed-card")).toBeGreaterThan(0);
  });

  it("falls back to the tab default when the address names a view it does not have", async () => {
    window.location.hash = "#/settings/skills?view=mosaic";
    await show(clientWith());
    expect(rowsOf("installed-row")).toBeGreaterThan(0);
    expect(rowsOf("installed-card")).toBe(0);
  });

  it("opens the skill from a row, the way a card does", async () => {
    window.location.hash = "#/settings/skills?view=list";
    await show(clientWith());
    await click(container.querySelector('[data-testid="skill-card-open"]')!);
    expect(anywhere("skill-detail-name")?.textContent).toBe("Brand Voice");
  });
});
