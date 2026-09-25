// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { Skill } from "@/api/skills";
import { SkillsView } from "@/views/SkillsView";

/**
 * What the Skills list renders once the host reports where an install stands.
 *
 * `skills-list.test.ts` pins the rules as values in and values out. This file
 * pins the surface, because the two ways this goes wrong are invisible to a pure
 * unit test: a badge that never reaches the card, and a menu item whose disabled
 * state and stated reason disagree with each other.
 */

const REGISTRY = [
  {
    id: "cold-outreach",
    name: "Cold Outreach",
    description: "Open a conversation with a stranger.",
    category: "Marketing",
    publisher: "OpenCompany",
    version: "2.0.0",
  },
];

function skill(over: Partial<Skill> & { id: string }): Skill {
  return {
    name: "Cold Outreach",
    description: "Open a conversation with a stranger.",
    category: "Marketing",
    source: "registry",
    enabled: true,
    version: "1.0.0",
    updatedAtMillis: 1_759_000_000_000,
    modified: false,
    ...over,
  };
}

function clientWith(skills: Skill[], posted: { path: string; body: unknown }[] = []) {
  return {
    scopeFor: () => "/api/v1/companies/acme",
    carriesPlatformBearer: false,
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
      if (path.endsWith("/inference")) {
        return Promise.resolve({ designsProfiles: true, canRebuildInPlace: false });
      }
      if (path.endsWith("/skills/registry")) return Promise.resolve(REGISTRY);
      if (path.endsWith("/skills")) return Promise.resolve(skills);
      return Promise.reject(new Error(`unexpected GET ${path}`));
    },
    post: (path: string, body: unknown) => {
      posted.push({ path, body });
      return Promise.resolve(skill({ id: "cold-outreach", version: "2.0.0", modified: false }));
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

function click(el: Element | null | undefined) {
  return act(async () => {
    (el as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function testid(id: string): HTMLElement | null {
  return document.body.querySelector<HTMLElement>(`[data-testid="${id}"]`);
}

/** Opens the only row's ⋮ menu. */
async function openRowMenu() {
  const trigger = testid("skill-row-menu");
  if (!trigger) throw new Error("no row menu trigger");
  await click(trigger);
}

beforeEach(() => {
  window.location.hash = "";
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

describe("the drift badge", () => {
  it("names an available update on the row", async () => {
    await show(
      clientWith([
        skill({ id: "cold-outreach", updateAvailable: { from: "1.0.0", to: "2.0.0" } }),
      ]),
    );

    expect(testid("skill-update-available")?.textContent).toContain("Update available");
    expect(testid("skill-modified")).toBeNull();
  });

  it("says Modified instead when the stored copy was edited", async () => {
    await show(
      clientWith([
        skill({
          id: "cold-outreach",
          modified: true,
          updateAvailable: { from: "1.0.0", to: "2.0.0" },
        }),
      ]),
    );

    expect(testid("skill-modified")?.textContent).toContain("Modified");
    expect(testid("skill-update-available")).toBeNull();
  });

  it("shows neither on a row whose install has not drifted", async () => {
    await show(clientWith([skill({ id: "cold-outreach" })]));

    expect(testid("skill-update-available")).toBeNull();
    expect(testid("skill-modified")).toBeNull();
  });
});

describe("the row menu's Update", () => {
  it("is enabled when the library has moved and the copy is untouched", async () => {
    await show(
      clientWith([
        skill({ id: "cold-outreach", updateAvailable: { from: "1.0.0", to: "2.0.0" } }),
      ]),
    );
    await openRowMenu();

    const item = testid("skill-menu-update");
    expect(item).not.toBeNull();
    expect(item?.getAttribute("data-disabled")).toBeNull();
    expect(testid("skill-menu-update-reason")).toBeNull();
  });

  it("is disabled with the edit as the reason when the copy was changed", async () => {
    await show(
      clientWith([
        skill({
          id: "cold-outreach",
          modified: true,
          updateAvailable: { from: "1.0.0", to: "2.0.0" },
        }),
      ]),
    );
    await openRowMenu();

    expect(testid("skill-menu-update")?.getAttribute("data-disabled")).not.toBeNull();
    expect(testid("skill-menu-update-reason")?.textContent).toContain(
      "changed after it was installed",
    );
  });

  it("is disabled as already current when nothing has moved", async () => {
    await show(clientWith([skill({ id: "cold-outreach" })]));
    await openRowMenu();

    expect(testid("skill-menu-update")?.getAttribute("data-disabled")).not.toBeNull();
    expect(testid("skill-menu-update-reason")?.textContent).toContain(
      "already matches the registry",
    );
  });

  it("is disabled as not-from-the-registry on an authored skill", async () => {
    await show(clientWith([skill({ id: "cold-outreach", source: "custom", version: null })]));
    await openRowMenu();

    expect(testid("skill-menu-update")?.getAttribute("data-disabled")).not.toBeNull();
    expect(testid("skill-menu-update-reason")?.textContent).toContain(
      "installed from the registry",
    );
  });
});

describe("the update review", () => {
  it("shows the revision it would move from and to, and says the text is not shown", async () => {
    await show(
      clientWith([
        skill({ id: "cold-outreach", updateAvailable: { from: "1.0.0", to: "2.0.0" } }),
      ]),
    );
    await openRowMenu();
    await click(testid("skill-menu-update"));

    expect(testid("skill-update-versions")?.textContent).toContain("v1.0.0");
    expect(testid("skill-update-versions")?.textContent).toContain("v2.0.0");
    expect(testid("skill-update-live")?.textContent).toContain("OpenCompany v2.0.0");
    expect(testid("skill-update-no-body")?.textContent).toContain("full text isn't shown");
  });

  it("writes nothing when the operator keeps what they have", async () => {
    const posted: { path: string; body: unknown }[] = [];
    await show(
      clientWith(
        [skill({ id: "cold-outreach", updateAvailable: { from: "1.0.0", to: "2.0.0" } })],
        posted,
      ),
    );
    await openRowMenu();
    await click(testid("skill-menu-update"));
    await click(testid("skill-update-keep"));

    expect(posted).toHaveLength(0);
    expect(testid("skill-update-dialog")).toBeNull();
  });

  it("posts the update and clears the badge with the row the host answers", async () => {
    const posted: { path: string; body: unknown }[] = [];
    await show(
      clientWith(
        [skill({ id: "cold-outreach", updateAvailable: { from: "1.0.0", to: "2.0.0" } })],
        posted,
      ),
    );
    await openRowMenu();
    await click(testid("skill-menu-update"));
    await click(testid("skill-update-confirm"));

    expect(posted).toHaveLength(1);
    expect(posted[0].path).toContain("/skills/cold-outreach/update");
    expect(testid("skill-update-available")).toBeNull();
  });
});
