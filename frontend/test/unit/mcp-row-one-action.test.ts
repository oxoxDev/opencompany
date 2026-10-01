// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { McpHealth, McpServer, McpSource } from "@/api/types";

/**
 * One row, one labelled action, and nothing destructive a mis-click away.
 *
 * The property worth pinning is not which icons are present but that exactly
 * one labelled control is, that it is the one this server's state calls for,
 * and that removal is behind the overflow and still asks before it happens.
 */

const api = vi.hoisted(() => ({
  listMcpServers: vi.fn(),
  testMcpServer: vi.fn(),
  discoverMcpTools: vi.fn(),
  addMcpServer: vi.fn(),
  removeMcpServer: vi.fn(),
  updateMcpServer: vi.fn(),
  startMcpOAuth: vi.fn(),
}));

const registryApi = vi.hoisted(() => ({
  connectMcpRegistryServer: vi.fn(),
  disconnectMcpRegistryServer: vi.fn(),
  getMcpRegistryEntry: vi.fn(),
  installMcpRegistryEntry: vi.fn(),
  searchMcpRegistry: vi.fn(),
  uninstallMcpRegistryServer: vi.fn(),
  updateMcpRegistryEnv: vi.fn(),
}));

const toasts = vi.hoisted(() => ({
  base: vi.fn(),
  success: vi.fn(),
  error: vi.fn(),
  message: vi.fn(),
  warning: vi.fn(),
  info: vi.fn(),
}));

vi.mock("@/api/mcp", () => api);
vi.mock("@/api/mcp-registry", () => registryApi);
vi.mock("sonner", () => ({
  toast: Object.assign(toasts.base, {
    success: toasts.success,
    error: toasts.error,
    message: toasts.message,
    warning: toasts.warning,
    info: toasts.info,
  }),
}));
vi.mock("@/views/connections/McpRegistryBrowser", () => ({
  McpDiscover: () => null,
}));
vi.mock("@/views/mcp/McpToolPermissions", () => ({
  McpToolPermissions: () => null,
}));
vi.mock("@/views/connections/connection-usage", () => ({
  UsageSection: () => null,
  useConnectionUsage: () => ({ load: "unavailable", calls: null, key: null }),
}));

const { McpServersSection } = await import(
  "@/views/connections/McpServersSection"
);

function row(over: Partial<McpServer> & { source: McpSource }): McpServer {
  return {
    name: "notion",
    endpoint: "https://mcp.notion.com/mcp",
    enabled: true,
    allowedTools: [],
    disallowedTools: [],
    readOnlyTools: [],
    timeoutSecs: 30,
    authConfigured: false,
    ...over,
  };
}

const OK: McpHealth = {
  status: "ok",
  message: "",
  toolCount: 16,
  checkedAtMillis: 1,
};

const client = {
  capabilityStatus: () => Promise.resolve({ mcpInBuild: true }),
} as unknown as OpenCompanyClient;

let container: HTMLDivElement;
let root: Root;

async function mount(servers: McpServer[], canManage = true) {
  api.listMcpServers.mockResolvedValue(servers);
  await act(async () => {
    root.render(
      createElement(McpServersSection, {
        client,
        company: "acme",
        canManage,
        chrome: "standalone" as const,
      }),
    );
  });
}

function all(selector: string): HTMLElement[] {
  return [...document.body.querySelectorAll<HTMLElement>(selector)];
}

async function click(node: Element | null | undefined) {
  if (!node) throw new Error("nothing to click");
  await act(async () => {
    node.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.clearAllMocks();
  window.location.hash = "";
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("how many labelled actions a row carries", () => {
  it("carries none on a server with nothing to fix", async () => {
    await mount([{ ...row({ source: "runtime" }), health: OK }]);

    expect(all("[data-mcp-primary]")).toHaveLength(0);
    // The overflow is still there: re-check, tools, permissions and removal all
    // live behind it.
    expect(all('[data-testid="mcp-row-overflow"]')).toHaveLength(1);
  });

  it("carries exactly the one the server's state asks for", async () => {
    await mount([
      {
        ...row({ source: "runtime" }),
        health: {
          ...OK,
          status: "needs_config",
          authHint: "oauth_required",
          message: "needs a browser sign-in",
        },
      },
    ]);

    const primary = all("[data-mcp-primary]");
    expect(primary).toHaveLength(1);
    expect(primary[0]?.getAttribute("aria-label")).toBe("Sign in to notion");
    expect(primary[0]?.getAttribute("data-testid")).toBe("mcp-sign-in");
  });

  it("offers a member no action at all, and still lets them read the row", async () => {
    await mount(
      [
        {
          ...row({ source: "runtime" }),
          health: { ...OK, status: "needs_config", authHint: "token_rejected" },
        },
      ],
      false,
    );

    expect(all("[data-mcp-primary]")).toHaveLength(0);
    expect(all('[data-testid="mcp-server-row"]')).toHaveLength(1);
  });
});

describe("the name, and the way into the server", () => {
  it("is the link, so no row carries a View button", async () => {
    await mount([{ ...row({ source: "runtime" }), health: OK }]);

    const open = document.body.querySelector('[data-testid="mcp-server-open"]');
    expect(open?.getAttribute("aria-label")).toBe("Open notion");
    expect(
      all("button").filter((b) => b.textContent?.trim() === "View"),
    ).toHaveLength(0);
  });

  it("opens the server's page from anywhere on the row, with no expander", async () => {
    await mount([{ ...row({ source: "runtime" }), health: OK }]);

    expect(all('[data-testid="mcp-row-expander"]')).toHaveLength(0);
    await click(document.body.querySelector('[data-testid="mcp-source-badge"]'));
    expect(all('[data-testid="mcp-server-page"]')).toHaveLength(1);
    expect(window.location.hash).toContain("server=notion");
  });

  it("does not open the page from the row's own controls", async () => {
    await mount([{ ...row({ source: "runtime" }), health: OK }]);

    await click(document.body.querySelector('[data-testid="mcp-row-overflow"]'));
    expect(all('[data-testid="mcp-server-page"]')).toHaveLength(0);
    await click(document.body.querySelector('[data-testid="mcp-permissions"]'));
    expect(window.location.hash).toContain("permissions=notion");
    expect(window.location.hash).not.toContain("server=");
  });
});

describe("removing a server", () => {
  it("is behind the overflow rather than a glyph beside the name", async () => {
    await mount([{ ...row({ source: "runtime" }), health: OK }]);

    expect(all('[data-testid="mcp-remove"]')).toHaveLength(0);
    await click(document.body.querySelector('[data-testid="mcp-row-overflow"]'));
    expect(all('[data-testid="mcp-remove"]')).toHaveLength(1);
  });

  it("asks before doing it, because the credential goes too", async () => {
    await mount([{ ...row({ source: "runtime" }), health: OK }]);

    await click(document.body.querySelector('[data-testid="mcp-row-overflow"]'));
    await click(document.body.querySelector('[data-testid="mcp-remove"]'));

    expect(api.removeMcpServer).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain("Remove notion?");
    expect(document.body.textContent).toContain(
      "the stored credential goes with it",
    );
    expect(document.body.textContent).toContain(
      "Its per-tool permissions are removed too.",
    );
  });

  it("offers no removal for a manifest server, not a control that refuses", async () => {
    await mount([{ ...row({ source: "manifest" }), health: OK }]);

    await click(document.body.querySelector('[data-testid="mcp-row-overflow"]'));
    expect(all('[data-testid="mcp-remove"]')).toHaveLength(0);
  });
});

describe("a company with no servers", () => {
  const pressed = (id: string) =>
    document.body.querySelector(`[data-testid="${id}"]`)?.getAttribute("aria-pressed");

  it("opens on Discover", async () => {
    await mount([]);

    expect(pressed("mcp-mode-discover")).toBe("true");
    expect(document.body.querySelector('[data-testid="mcp-discover-search"]')).not.toBeNull();
    expect(document.body.querySelector('[data-testid="mcp-search"]')).toBeNull();
  });

  it("has Browse the directory open Discover, not focus a field", async () => {
    await mount([]);
    await click(document.body.querySelector('[data-testid="mcp-mode-yours"]'));
    expect(pressed("mcp-mode-yours")).toBe("true");

    await click(document.body.querySelector('[data-testid="mcp-browse-directory"]'));

    expect(pressed("mcp-mode-discover")).toBe("true");
    expect(window.location.hash).toContain("view=discover");
  });
});
