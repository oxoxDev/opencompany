// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { SkillsView } from "@/views/SkillsView";

/**
 * Enabling, installing, uninstalling and adding a custom skill are admin-only
 * on the host (`src/server/ops/skills.rs`); the two reads — the installed list
 * and the registry browse — stay member-level. This pins the console's own
 * half of that split: a member sees the same lists an admin does, with the
 * four write controls withheld and a notice in their place, matching the
 * established pattern (`HostingView`, `TeamView`).
 */

const INSTALLED = [
  {
    id: "press-outreach",
    name: "Press Outreach",
    description: "Pitch journalists a story.",
    category: "Marketing",
    source: "registry",
    enabled: true,
  },
];

const REGISTRY = [
  {
    id: "customer-followup",
    name: "Customer Follow-up",
    description: "Check back in after a sale.",
    category: "Ops",
    publisher: "OpenCompany",
  },
];

function clientAs(role: "admin" | "member"): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/companies/acme",
    listTeam: () => Promise.resolve([]),
    get: (path: string) => {
      if (path.endsWith("/auth/me")) {
        return Promise.resolve({
          id: "u1",
          email: "a@b.c",
          role,
          company: "acme",
          hasPassword: true,
        });
      }
      if (path.endsWith("/skills/registry")) return Promise.resolve(REGISTRY);
      if (path.endsWith("/skills")) return Promise.resolve(INSTALLED);
      return Promise.reject(new Error(`unexpected GET ${path}`));
    },
  } as unknown as OpenCompanyClient;
}

/** A client whose `/auth/me` never settles, so a test can prove a reset
 *  happened before the new scope's role is known, rather than because it
 *  happened to arrive quickly. */
function clientWithHungAuth(company = "beta"): OpenCompanyClient {
  return {
    scopeFor: () => `/api/v1/companies/${company}`,
    listTeam: () => Promise.resolve([]),
    get: (path: string) => {
      if (path.endsWith("/auth/me")) return new Promise(() => {});
      if (path.endsWith("/skills/registry")) return Promise.resolve([]);
      if (path.endsWith("/skills")) return Promise.resolve([]);
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
  // Let the two independent list reads and the role read settle.
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

function at(testid: string): HTMLElement | null {
  return container.querySelector<HTMLElement>(`[data-testid="${testid}"]`);
}

function buttons(): string[] {
  return [...container.querySelectorAll("button")].map(
    (b) => b.textContent ?? "",
  );
}

beforeEach(() => {
  // The tab a page opens on rides the address (`useHashTab`), so a test that
  // opened one leaves it set for the next. Reset it here rather than in each
  // test: the leak is invisible — the page renders, just on the wrong tab.
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

describe("SkillsView authority", () => {
  it("offers a member the installed list, with every write withheld", async () => {
    await show(clientAs("member"));

    expect(at("skills-admin-only")?.textContent).toContain("Only an admin");

    // Read content survives — a member is not shown a blank tab.
    expect(container.textContent).toContain("Press Outreach");

    // No way to flip, uninstall, install or add one.
    const toggle = container.querySelector('[aria-label="Enable skill"]');
    expect(toggle?.hasAttribute("data-disabled")).toBe(true);
    expect(container.querySelector('[aria-label="Uninstall"]')).toBeNull();
    expect(buttons().some((t) => t.includes("Add skill"))).toBe(false);
  });

  it("withholds the registry's Install button from a member, once the tab is open", async () => {
    await show(clientAs("member"));
    const registryTab = [...container.querySelectorAll('[role="tab"]')].find(
      (t) => t.textContent?.includes("Registry"),
    ) as HTMLElement;
    await act(async () => {
      registryTab.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    expect(container.textContent).toContain("Customer Follow-up");
    // Word-boundary, not a substring match: the Installed tab's own trigger
    // text ("Installed (1)") contains "Install" as a prefix.
    expect(buttons().some((t) => /\bInstall\b/.test(t))).toBe(false);
  });

  it("offers an admin every write control, with no admin-only notice", async () => {
    await show(clientAs("admin"));

    expect(at("skills-admin-only")).toBeNull();

    const toggle = container.querySelector('[aria-label="Enable skill"]');
    expect(toggle?.hasAttribute("data-disabled")).toBe(false);
    expect(buttons().some((t) => t.includes("Add skill"))).toBe(true);
  });

  it("offers an admin the registry's Install button, once the tab is open", async () => {
    await show(clientAs("admin"));
    const registryTab = [...container.querySelectorAll('[role="tab"]')].find(
      (t) => t.textContent?.includes("Registry"),
    ) as HTMLElement;
    await act(async () => {
      registryTab.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    expect(buttons().some((t) => /\bInstall\b/.test(t))).toBe(true);
  });

  it("closes the write surface the instant the scope changes, before the new role is known", async () => {
    await show(clientAs("admin"));

    // The header is one menu now: the trigger opens it, and the item inside
    // opens the dialog.
    await act(async () => {
      const addSkill = [...container.querySelectorAll("button")].find((b) =>
        b.textContent?.includes("Add skill"),
      ) as HTMLElement;
      addSkill.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await act(async () => {
      document.body
        .querySelector<HTMLElement>('[data-testid="skills-add-write"]')!
        .dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    // The dialog is portaled onto `document.body`, not into `container`.
    expect(document.body.textContent).toContain("Add a skill");

    // The new scope's `/auth/me` never answers, so anything visible after this
    // render can only be explained by the reset the scope-change effect runs
    // up front — not by a fresh admin result arriving quickly.
    await act(async () => {
      root.render(
        createElement(SkillsView, {
          client: clientWithHungAuth(),
          company: "beta",
        }),
      );
    });

    expect(document.body.textContent).not.toContain("Add a skill");
    expect(at("skills-admin-only")?.textContent).toContain("Only an admin");
    expect(buttons().some((t) => t.includes("Add skill"))).toBe(false);
  });

  it("closes the write surface on a host reseat, even though `company` stays the same", async () => {
    await show(clientAs("admin"));

    expect(buttons().some((t) => t.includes("Add skill"))).toBe(true);

    // Same company as `show()` used ("acme"), but a different client — the
    // reseat a host swap produces. The new host's `/auth/me` never answers,
    // so anything closed after this render can only be explained by keying
    // the reset on `client`, not by a fresh (non-)admin result racing in.
    await act(async () => {
      root.render(
        createElement(SkillsView, {
          client: clientWithHungAuth("acme"),
          company: "acme",
        }),
      );
    });

    expect(at("skills-admin-only")?.textContent).toContain("Only an admin");
    expect(buttons().some((t) => t.includes("Add skill"))).toBe(false);
  });
});
