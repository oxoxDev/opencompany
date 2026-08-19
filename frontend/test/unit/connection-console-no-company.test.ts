// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { AppSpec, CompanyStatus } from "@/api/types";
import { HostsProvider } from "@/connections/HostsContext";
import { ConnectionConsole } from "@/views/ConnectionConsole";

/**
 * The other half of the zero-company dead end fixed on #908 (see
 * `setup-wizard-finish-gate.test.ts` for the setup-wizard side): a
 * *configured* host (setup already complete) that still has no companies —
 * an operator deleted the only one, say — must offer a route back into
 * setup rather than a plain "can't connect" dead end with only a page
 * reload to offer.
 */

function spec(over: Partial<AppSpec> = {}): AppSpec {
  return {
    name: "opencompany",
    version: "0.0.0",
    api_url: "",
    cycles_available: false,
    setup_complete: true,
    ...over,
  } as AppSpec;
}

function client(over: {
  companies?: CompanyStatus[];
  setupComplete?: boolean;
}): OpenCompanyClient {
  return {
    spec: async () => spec({ setup_complete: over.setupComplete ?? true }),
    listCompanies: async () => over.companies ?? [],
    get: async () => ({
      complete: over.setupComplete ?? true,
      config_path: "/data/config.toml",
      fields: [],
      templates: [],
      auth_modes: ["email"],
      build: {
        acp_in_build: false,
        acp_transport_mounted: false,
        mcp_in_build: false,
        harness_in_build: false,
        oauth_in_build: false,
      },
      companies: (over.companies ?? []).map((c) => c.id),
      inference: { ready: false, provider: null, base_url: null },
    }),
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

/**
 * The console reads its host roster from context, because the switcher that
 * shows it lives in the sidebar header two layers down (#1142). An empty
 * roster draws no switcher at all, so these screens are exactly what they
 * were — which is what this wrapper is for: supplying the contract without
 * adding anything to the tree under test.
 */
async function show(c: OpenCompanyClient) {
  await act(async () => {
    root.render(
      createElement(HostsProvider, {
        value: {
          connections: [],
          selected: null,
          onSelect: () => {},
          onAdd: () => {},
          localInstances: [],
          hub: false,
        },
        children: createElement(ConnectionConsole, {
          connectionId: "test-connection",
          client: c,
          defaultCompany: null,
        }),
      }),
    );
  });
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

describe("a configured host with no companies", () => {
  it("offers a route back into setup instead of a dead-end error", async () => {
    await show(client({ companies: [] }));

    const noCompany = container.querySelector('[data-testid="no-company"]');
    expect(noCompany).toBeTruthy();
    // Never the generic connection-error screen, which only offers a reload.
    expect(container.querySelector('[data-testid="connection-error"]')).toBeNull();

    await act(async () => {
      (container.querySelector("button") as HTMLButtonElement).click();
    });

    expect(container.querySelector('[data-testid="setup-wizard"]')).toBeTruthy();
  });

  it("still shows the ordinary picker once a company exists", async () => {
    const company = (id: string, name: string): CompanyStatus => ({
      id,
      name,
      lifecycle: "running",
      pending_approvals: 0,
    });
    await show(
      client({
        companies: [company("acme", "Acme"), company("beta", "Beta")],
      }),
    );

    expect(container.querySelector('[data-testid="no-company"]')).toBeNull();
    // Prove the picker itself rendered, not merely the absence of the
    // no-company screen: the supplied companies are on screen.
    expect(container.textContent).toContain("Acme");
    expect(container.textContent).toContain("Beta");
    // Never the generic connection-error screen, which only offers a reload.
    expect(container.querySelector('[data-testid="connection-error"]')).toBeNull();
  });
});
