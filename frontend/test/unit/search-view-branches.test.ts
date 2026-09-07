// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "@/api/types";
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
 * decide whether to offer the write form. `session: "none"` rejects the way
 * the host's own `no_session()` does — a real `ApiError` from its
 * `{error, code}` envelope — for an unauthenticated or bearer-only caller.
 * `session: "error"` rejects with a plain network-style error: not a
 * confirmed absence of a session, so it must not be read as one.
 * `carriesPlatformBearer` defaults to `false`, matching a browser session
 * authenticating by cookie; a hub console can carry it alongside a real
 * session (`authHeaders`'s own doc comment), so the two are independent here.
 */
function clientWith(
  answer: unknown,
  session: "admin" | "member" | "none" | "error" = "admin",
  carriesPlatformBearer = false,
): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/companies/acme",
    carriesPlatformBearer,
    get: (path: string) => {
      if (path.endsWith("/auth/me")) {
        if (session === "none") return Promise.reject(new ApiError(401, "unauthorized", "not signed in", true));
        if (session === "error") return Promise.reject(new Error("network down"));
        return Promise.resolve({ id: "u1", email: "a@b.c", role: session, company: "acme", hasPassword: true });
      }
      return answer instanceof Error ? Promise.reject(answer) : Promise.resolve(answer ?? SEARCH_OK);
    },
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

  it("offers a bearer-only caller every control once /auth/me finds no session", async () => {
    await show(clientWith(SEARCH_OK, "none", true));

    expect(at("search-read-only")).toBeNull();
    expect(at("search-provider")).not.toBeNull();
  });

  it("stays read-only on an ambiguous /auth/me failure, even with a bearer present", async () => {
    // A network error, a timeout, or a 5xx is not a confirmed absence of a
    // session — a member's session could still be live and would still take
    // precedence on the host (coderabbit review).
    await show(clientWith(SEARCH_OK, "error", true));

    expect(at("search-read-only")?.textContent).toContain("Only an admin");
    expect(at("search-provider")).toBeNull();
  });

  it("defers to a member session even when a platform bearer is also present", async () => {
    // A hub console can carry both credentials at once (`authHeaders`), and
    // resolve_principal tries the session first — so a bearer must never
    // paper over a member's own 403 (codex review).
    await show(clientWith(SEARCH_OK, "member", true));

    expect(at("search-read-only")?.textContent).toContain("Only an admin");
    expect(at("search-provider")).toBeNull();
  });

  it("defers to an admin session when a platform bearer is also present", async () => {
    await show(clientWith(SEARCH_OK, "admin", true));

    expect(at("search-read-only")).toBeNull();
    expect(at("search-provider")).not.toBeNull();
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
