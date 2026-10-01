// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { SkillsView } from "@/views/SkillsView";

/**
 * `canManage` must not survive a company switch.
 *
 * `SettingsSection` mounts `SkillsView` with `key={company ?? "self"}`, the
 * same remount rule `HostingView`/`SearchView` already carry (codeRabbit
 * review — the key was missing here). Without it, an admin's `canManage` from
 * the company they just left stays `true` in state until the new company's
 * `/auth/me` settles, and during that window Add/Install/enable/Uninstall
 * render enabled for whatever role the operator actually holds in the company
 * now addressed. Driven at the view level, keyed the way the section keys it
 * — matching `finance-company-switch.test.ts`'s own reasoning for testing
 * composition rather than `SettingsSection` itself.
 */

function clientFor(company: string, role: "admin" | "member"): OpenCompanyClient {
  return {
    scopeFor: () => `/api/v1/companies/${company}`,
    listTeam: () => Promise.resolve([]),
    carriesPlatformBearer: false,
    get: (path: string) => {
      if (path.endsWith("/auth/me")) {
        return Promise.resolve({ id: "u1", email: "a@b.c", role, company, hasPassword: true });
      }
      return Promise.resolve([]);
    },
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

/** Renders the page exactly as `SettingsSection` does: keyed by company. */
async function showSkills(company: string, role: "admin" | "member") {
  await act(async () => {
    root.render(
      createElement(SkillsView, {
        key: company,
        client: clientFor(company, role),
        company,
      }),
    );
  });
  // canManage resolves through its own /auth/me round trip, a tick after render.
  await act(async () => {});
}

function at(testid: string): HTMLElement | null {
  return container.querySelector<HTMLElement>(`[data-testid="${testid}"]`);
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

describe("SkillsView company switch", () => {
  it("does not carry an admin's authority into a company where the operator is a member", async () => {
    await showSkills("acme", "admin");
    expect(at("skills-admin-only")).toBeNull();
    expect(
      Array.from(container.querySelectorAll("button")).some((b) => b.textContent?.includes("Add skill")),
    ).toBe(true);

    await showSkills("globex", "member");
    expect(at("skills-admin-only")?.textContent).toContain("Only an admin");
    expect(
      Array.from(container.querySelectorAll("button")).some((b) => b.textContent?.includes("Add skill")),
    ).toBe(false);
  });
});
