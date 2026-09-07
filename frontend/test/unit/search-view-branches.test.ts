// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { SearchView } from "@/views/SearchView";

/**
 * `SearchView` is `HostingView`'s copy-paste sibling (codex review, #1785),
 * and carried the same authority gap: the provider picker, the API key field
 * and Save rendered enabled for a member, with nothing that read `canManage`
 * — the page's own footer sentence names the reason the choice is an
 * administrator's, and the form above it did not act on it. The host refuses
 * `PUT …/search` with a 403 whatever the console shows.
 */

const SEARCH_OK = {
  provider: "brave",
  effectiveProvider: "brave",
  apiKeyConfigured: true,
  endpoint: null,
  needsApiKey: false,
  needsEndpoint: false,
  granted: true,
  inBuild: true,
  supportedProviders: ["managed", "brave", "exa", "searxng"],
};

/**
 * A client answering the search read with a value or a rejection.
 *
 * `/auth/me` is answered separately, and as an admin by default — matching
 * `HostingView`'s own fixture — since this page resolves the viewer's role to
 * decide whether to offer the write form.
 */
function clientWith(answer: unknown, role: "admin" | "member" = "admin"): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/companies/acme",
    get: (path: string) =>
      path.endsWith("/auth/me")
        ? Promise.resolve({ id: "u1", email: "a@b.c", role, company: "acme", hasPassword: true })
        : answer instanceof Error
          ? Promise.reject(answer)
          : Promise.resolve(answer ?? SEARCH_OK),
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

async function show(client: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(SearchView, { client, company: "acme" }));
  });
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
  vi.restoreAllMocks();
});

describe("SearchView authority (issue #1785 copy-paste pair)", () => {
  it("offers a member the current provider, and no way to change it", async () => {
    await show(clientWith(SEARCH_OK, "member"));

    expect(at("search-read-only")?.textContent).toContain("Only an admin");
    expect(at("search-provider")).toBeNull();
    expect(at("search-api-key")).toBeNull();
    expect(at("search-endpoint")).toBeNull();
    expect(at("search-save")).toBeNull();
    expect(at("search-clear")).toBeNull();

    // Not a blank page: the effective provider is still on screen.
    expect(at("search-view")?.textContent).toContain("Brave Search");
  });

  it("offers an admin every control, with no read-only notice", async () => {
    await show(clientWith(SEARCH_OK, "admin"));

    expect(at("search-read-only")).toBeNull();
    expect(at("search-provider")).not.toBeNull();
    expect((at("search-save") as HTMLButtonElement | null)?.disabled).toBe(false);
  });

  it("hides the SearXNG endpoint field from a member too, same as the API key", async () => {
    await show(
      clientWith(
        { ...SEARCH_OK, provider: "searxng", effectiveProvider: "searxng", endpoint: "https://searx.acme.com" },
        "member",
      ),
    );

    expect(at("search-endpoint")).toBeNull();
    // The effective provider is still readable — a member is told what is
    // configured, just not handed the field that would change it.
    expect(at("search-view")?.textContent).toContain("SearXNG");
  });
});
