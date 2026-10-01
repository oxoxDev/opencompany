// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { SkillsView } from "@/views/SkillsView";

/**
 * The Registry tab draws the same kind of object as the Installed tab, so it
 * offers the same cards/list switch — and keeps its own search, because a query
 * typed while browsing what could be added must not hide half of what already
 * is.
 */

const INSTALLED = [
  {
    id: "brand-voice",
    name: "Brand Voice",
    description: "How we sound.",
    category: "Content",
    source: "custom",
    enabled: true,
    agents: [],
  },
];

const REGISTRY = [
  {
    id: "api-design",
    name: "API Design",
    description: "Reviewing an interface before it ships.",
    category: "Engineering",
    publisher: "tinyhumans",
    version: "1.2.0",
  },
  {
    id: "brand-voice",
    name: "Brand Voice",
    description: "How we sound.",
    category: "Content",
    publisher: "tinyhumans",
    version: "1.0.0",
  },
];

function clientWith(role: "admin" | "member" = "admin"): OpenCompanyClient {
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

function anywhere(testid: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(`[data-testid="${testid}"]`);
}

function countOf(testid: string): number {
  return document.querySelectorAll(`[data-testid="${testid}"]`).length;
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

async function type(input: HTMLInputElement, value: string) {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(
      window.HTMLInputElement.prototype,
      "value",
    )?.set;
    setter?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function openRegistryTab() {
  const tab = Array.from(
    container.querySelectorAll<HTMLElement>('[role="tab"]'),
  ).find((t) => t.textContent?.includes("Registry"));
  await act(async () => {
    tab?.click();
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

describe("the registry tab's drawing", () => {
  it("opens on cards, the shape for browsing", async () => {
    await show(clientWith());
    await openRegistryTab();
    expect(countOf("registry-card")).toBe(REGISTRY.length);
    expect(countOf("registry-row"), "no table yet").toBe(0);
  });

  it("offers the same switch the installed set has", async () => {
    await show(clientWith());
    await openRegistryTab();
    expect(anywhere("skills-view-toggle")).not.toBeNull();
    expect(anywhere("skills-view-cards")?.getAttribute("aria-pressed")).toBe(
      "true",
    );
    expect(anywhere("skills-view-list")?.getAttribute("aria-pressed")).toBe(
      "false",
    );
  });

  it("swaps the cards for rows", async () => {
    await show(clientWith());
    await openRegistryTab();
    await click(anywhere("skills-view-list")!);

    expect(countOf("registry-row")).toBe(REGISTRY.length);
    expect(countOf("registry-card"), "one rendering at a time").toBe(0);
  });

  it("marks what is already installed in either drawing", async () => {
    await show(clientWith());
    await openRegistryTab();
    expect(document.body.textContent).toContain("Installed");

    await click(anywhere("skills-view-list")!);
    expect(document.body.textContent).toContain("Installed");
  });

  it("offers a member no way to install, in either drawing", async () => {
    await show(clientWith("member"));
    await openRegistryTab();
    // Not a substring match: the "Installed" tab is itself a button.
    const installs = () =>
      Array.from(document.querySelectorAll("button")).filter(
        (b) =>
          b.getAttribute("role") !== "tab" &&
          b.textContent?.trim() === "Install",
      ).length;
    expect(installs()).toBe(0);

    await click(anywhere("skills-view-list")!);
    expect(installs()).toBe(0);
  });
});

describe("the two searches", () => {
  it("narrows the registry without touching the installed set", async () => {
    await show(clientWith());
    await openRegistryTab();

    await type(anywhere("registry-search") as HTMLInputElement, "api");
    expect(countOf("registry-card")).toBe(1);
    expect(document.body.textContent).toContain("API Design");

    // Back to Installed: its own list is unchanged by a registry query.
    const installedTab = Array.from(
      container.querySelectorAll<HTMLElement>('[role="tab"]'),
    ).find((t) => t.textContent?.includes("Installed"));
    await act(async () => {
      installedTab?.click();
    });
    expect(countOf("installed-row")).toBe(INSTALLED.length);
  });
});
