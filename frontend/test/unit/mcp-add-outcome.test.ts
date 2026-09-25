// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { ApiError, type McpHealth, type McpServer, type McpSource } from "@/api/types";

/**
 * What the add form says happened, and what removing a server takes with it.
 *
 * The add path has two outcomes that shared one banner: the host refusing the
 * write, and the host accepting it and then failing to reach the endpoint. Only
 * the first is a failure. The second leaves a server saved, enabled and
 * attached to every agent that reaches it — so titling it "Couldn't add the
 * server" tells the operator the opposite of what happened and invites a second
 * add of a server that is already there.
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
  McpRegistryBrowser: () => null,
}));
vi.mock("@/views/connections/ProviderDetail", () => ({
  ProviderDetail: () => null,
}));

const { McpServersSection } = await import("@/views/connections/McpServersSection");

const UNREACHABLE: McpHealth = {
  status: "error",
  message: "MCP server 'deadsrv' couldn't be used: mcp transport failure.",
  toolCount: 0,
  checkedAtMillis: 1,
};

function row(over: Partial<McpServer> & { source: McpSource }): McpServer {
  return {
    name: "deadsrv",
    endpoint: "https://mcp.example.com/mcp",
    enabled: true,
    allowedTools: [],
    disallowedTools: [],
    readOnlyTools: [],
    timeoutSecs: 30,
    authConfigured: false,
    ...over,
  };
}

const client = {
  capabilityStatus: () => Promise.resolve({ mcpInBuild: true }),
} as unknown as OpenCompanyClient;

let container: HTMLDivElement;
let root: Root;

async function mount(servers: McpServer[]) {
  api.listMcpServers.mockResolvedValue(servers);
  await act(async () => {
    root.render(
      createElement(McpServersSection, {
        client,
        company: "acme",
        canManage: true,
        chrome: "standalone" as const,
      }),
    );
  });
}

function field(id: string): HTMLInputElement {
  const el = container.querySelector<HTMLInputElement>(`#${id}`);
  if (!el) throw new Error(`no #${id}`);
  return el;
}

/** Type into a controlled input the way React reads it. */
async function type(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    window.HTMLInputElement.prototype,
    "value",
  )?.set;
  await act(async () => {
    setter?.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function submitAdd() {
  const button = [...container.querySelectorAll("button")].find((b) =>
    b.textContent?.includes("Add"),
  );
  if (!button) throw new Error("no Add button");
  await act(async () => {
    button.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.clearAllMocks();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("an add the host accepted but could not reach", () => {
  beforeEach(async () => {
    await mount([]);
    api.addMcpServer.mockResolvedValue({
      server: row({ source: "runtime" }),
      note: "Agents pick up this change on their next turn.",
      test: UNREACHABLE,
    });
    api.listMcpServers.mockResolvedValue([row({ source: "runtime" })]);
    await type(field("mcp-name"), "deadsrv");
    await type(field("mcp-endpoint"), "https://mcp.example.com/mcp");
    await submitAdd();
  });

  it("does not call it a failure to add", () => {
    expect(container.textContent).not.toContain("Couldn't add the server");
  });

  it("says it was added and could not be reached", () => {
    expect(container.textContent).toContain("Added, but it could not be reached");
  });

  it("says where it went, so nobody adds it twice", () => {
    expect(container.textContent).toContain("saved and listed above");
  });

  it("still surfaces the probe's own words", () => {
    expect(container.textContent).toContain("mcp transport failure");
  });
});

describe("an add the host refused", () => {
  it("is the one that reads as a failure to add", async () => {
    await mount([]);
    api.addMcpServer.mockRejectedValue(
      new ApiError(409, "conflict", "an MCP server named `deadsrv` already exists.", true),
    );
    await type(field("mcp-name"), "deadsrv");
    await type(field("mcp-endpoint"), "https://mcp.example.com/mcp");
    await submitAdd();

    expect(container.textContent).toContain("Couldn't add the server");
    expect(container.textContent).toContain("already exists");
    expect(container.textContent).not.toContain("saved and listed above");
  });
});

describe("removing a server", () => {
  it("asks before doing it, because the credential goes too", async () => {
    await mount([row({ source: "runtime" })]);

    const trash = container.querySelector<HTMLElement>('[data-testid="mcp-remove"]');
    expect(trash).not.toBeNull();
    await act(async () => {
      trash?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    expect(api.removeMcpServer).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain("Remove deadsrv?");
  });

  it("names what goes with it", async () => {
    await mount([row({ source: "runtime" })]);
    const trash = container.querySelector<HTMLElement>('[data-testid="mcp-remove"]');
    await act(async () => {
      trash?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    expect(document.body.textContent).toContain("stored credential");
    expect(document.body.textContent).toContain("per-tool permissions");
  });
});

describe("the banner about a server that was removed", () => {
  it("goes with it, so it stops describing something that is no longer there", async () => {
    await mount([]);
    api.addMcpServer.mockResolvedValue({
      server: row({ source: "runtime" }),
      note: "Agents pick up this change on their next turn.",
      test: UNREACHABLE,
    });
    api.listMcpServers.mockResolvedValue([row({ source: "runtime" })]);
    await type(field("mcp-name"), "deadsrv");
    await type(field("mcp-endpoint"), "https://mcp.example.com/mcp");
    await submitAdd();
    expect(container.textContent).toContain("Added, but it could not be reached");

    api.removeMcpServer.mockResolvedValue(undefined);
    api.listMcpServers.mockResolvedValue([]);
    const trash = container.querySelector<HTMLElement>('[data-testid="mcp-remove"]');
    await act(async () => {
      trash?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    const confirm = [...document.body.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "Remove",
    );
    expect(confirm).toBeDefined();
    await act(async () => {
      confirm?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    expect(api.removeMcpServer).toHaveBeenCalledWith(client, "acme", "deadsrv");
    expect(container.textContent).not.toContain("Added, but it could not be reached");
  });
});

describe("the banner about a server that was NOT removed", () => {
  it("stays, because that server is still saved and still unreachable", async () => {
    const other = row({ source: "runtime", name: "livesrv" });
    await mount([other]);
    api.addMcpServer.mockResolvedValue({
      server: row({ source: "runtime" }),
      note: "Agents pick up this change on their next turn.",
      test: UNREACHABLE,
    });
    api.listMcpServers.mockResolvedValue([other, row({ source: "runtime" })]);
    await type(field("mcp-name"), "deadsrv");
    await type(field("mcp-endpoint"), "https://mcp.example.com/mcp");
    await submitAdd();
    expect(container.textContent).toContain("Added, but it could not be reached");

    api.removeMcpServer.mockResolvedValue(undefined);
    api.listMcpServers.mockResolvedValue([row({ source: "runtime" })]);
    const trash = [...container.querySelectorAll<HTMLElement>('[data-testid="mcp-remove"]')].find(
      (b) => b.getAttribute("aria-label")?.includes("livesrv"),
    );
    expect(trash).toBeDefined();
    await act(async () => {
      trash?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    const confirm = [...document.body.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "Remove",
    );
    await act(async () => {
      confirm?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    expect(api.removeMcpServer).toHaveBeenCalledWith(client, "acme", "livesrv");
    // deadsrv is still there and still unreachable; the banner is the only
    // thing on screen saying so.
    expect(container.textContent).toContain("Added, but it could not be reached");
  });
});

describe("a banner about an add that was refused", () => {
  it("survives an unrelated removal, because nothing about it was answered", async () => {
    const other = row({ source: "runtime", name: "livesrv" });
    await mount([other]);
    api.addMcpServer.mockRejectedValue(
      new ApiError(409, "conflict", "an MCP server named `deadsrv` already exists.", true),
    );
    await type(field("mcp-name"), "deadsrv");
    await type(field("mcp-endpoint"), "https://mcp.example.com/mcp");
    await submitAdd();
    expect(container.textContent).toContain("Couldn't add the server");

    api.removeMcpServer.mockResolvedValue(undefined);
    api.listMcpServers.mockResolvedValue([]);
    const trash = container.querySelector<HTMLElement>('[data-testid="mcp-remove"]');
    await act(async () => {
      trash?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    const confirm = [...document.body.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "Remove",
    );
    await act(async () => {
      confirm?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    expect(api.removeMcpServer).toHaveBeenCalledWith(client, "acme", "livesrv");
    expect(container.textContent).toContain("Couldn't add the server");
  });

  it("clears on the next add attempt, which is what answers it", async () => {
    await mount([]);
    api.addMcpServer.mockRejectedValue(
      new ApiError(409, "conflict", "an MCP server named `deadsrv` already exists.", true),
    );
    await type(field("mcp-name"), "deadsrv");
    await type(field("mcp-endpoint"), "https://mcp.example.com/mcp");
    await submitAdd();
    expect(container.textContent).toContain("Couldn't add the server");

    api.addMcpServer.mockResolvedValue({
      server: row({ source: "runtime", name: "goodsrv" }),
      note: "Agents pick up this change on their next turn.",
    });
    api.listMcpServers.mockResolvedValue([row({ source: "runtime", name: "goodsrv" })]);
    await type(field("mcp-name"), "goodsrv");
    await type(field("mcp-endpoint"), "https://mcp.example.com/ok");
    await submitAdd();

    expect(container.textContent).not.toContain("Couldn't add the server");
  });
});
