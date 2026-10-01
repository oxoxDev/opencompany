// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { McpHealth, McpServer, McpSource } from "@/api/types";

/**
 * The MCP row's controls, asserted against the DOM.
 *
 * `mcp-credential-affordance.test.ts` and `mcp-registry-row.test.ts` pin the
 * deciders, and both were green while the console offered an Authorization
 * field on a row whose own message said a pasted token would not work: nothing
 * asserted that the decider's answer was the control the row actually rendered.
 * `mcp-sign-in`, `mcp-rotate-env`, `mcp-add-token` and `mcp-env-save` had no
 * test between them.
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

// The directory browser runs its own search on mount and owns none of the
// behaviour under test here.
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
vi.mock("@/views/connections/ProviderDetail", () => ({
  ProviderDetail: () => null,
}));

const { McpServersSection } = await import("@/views/connections/McpServersSection");

const health = (status: McpHealth["status"], authHint?: string, message = ""): McpHealth => ({
  status,
  message,
  toolCount: 0,
  checkedAtMillis: 1,
  authHint,
});

function row(over: Partial<McpServer> & { source: McpSource }): McpServer {
  return {
    name: "org-git",
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

/** Dialogs portal to the body, so controls are looked up page-wide. */
function control(testId: string): HTMLElement | null {
  return document.body.querySelector(`[data-testid="${testId}"]`);
}

/** Mount the section over `servers` and let its two mount reads settle. */
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

describe("a directory install that wants a browser sign-in", () => {
  const server = row({ source: "registry", serverId: "srv_9fa1" });
  const refused = health(
    "needs_config",
    "oauth_required",
    "This server needs a browser sign-in — a pasted token will not work.",
  );

  it("renders no credential control, and says why on the server's page", async () => {
    await mount([{ ...server, health: refused }]);

    // The bug: the row's own message said a token would not work, and the row
    // below it offered a token field with a Save button.
    expect(control("mcp-rotate-env")).toBeNull();
    expect(control("mcp-add-token")).toBeNull();
    expect(control("mcp-sign-in")).toBeNull();
    expect(control("mcp-env-inline")).toBeNull();

    await act(async () => (control("mcp-server-open") as HTMLButtonElement).click());

    expect(control("mcp-server-page")).not.toBeNull();
    expect(control("mcp-page-primary")).toBeNull();
    const notice = control("mcp-no-credential-control");
    expect(notice).not.toBeNull();
    expect(notice?.textContent).toContain("can't be authorised here");
    // The host's own sentence is kept, not replaced.
    expect(document.body.textContent).toContain("a pasted token will not work");
  });

  it("withholds Connect, which the host can only refuse again", async () => {
    await mount([{ ...server, health: refused }]);

    expect(control("mcp-lifecycle")).toBeNull();
    expect(registryApi.connectMcpRegistryServer).not.toHaveBeenCalled();
  });

  it("still offers the env form to an install refused for a value", async () => {
    // The narrowing has to stay narrow: an install that wants named env values
    // is the case `rotate_env` exists for.
    await mount([{ ...server, health: health("needs_config") }]);

    expect(control("mcp-rotate-env")).not.toBeNull();
    // Connect is real for this row, and it is one press behind the overflow: a
    // row carries one labelled action, and this row's is the credential.
    expect(control("mcp-row-overflow")).not.toBeNull();
    expect(control("mcp-no-credential-control")).toBeNull();
  });
});

describe("a List A row whose stored credential was refused", () => {
  it("offers the token field the message has been telling the operator to use", async () => {
    await mount([
      {
        ...row({ source: "runtime", name: "notion", authConfigured: true }),
        health: health("needs_config", "token_rejected", "That token was rejected. Update it and Test again."),
      },
    ]);

    const button = control("mcp-add-token");
    expect(button).not.toBeNull();
    expect(button?.getAttribute("aria-label")).toContain("Replace notion's API token");

    act(() => (button as HTMLButtonElement).click());
    const dialog = control("mcp-connect-dialog");
    expect(dialog).not.toBeNull();
    expect(dialog?.querySelector('[data-testid="mcp-token-inline"]')).not.toBeNull();
  });
});

describe("saving a directory install's credentials", () => {
  const server = row({
    source: "registry",
    serverId: "srv_9fa1",
    qualifiedName: "@acme/git",
  });

  /** Open the env form and fill its one declared key. */
  async function openForm() {
    await mount([{ ...server, health: health("needs_config") }]);
    registryApi.getMcpRegistryEntry.mockResolvedValue({
      qualifiedName: "@acme/git",
      displayName: "Git",
      source: "mcp_official",
      requiredEnvKeys: ["GIT_TOKEN"],
      installable: true,
    });

    await act(async () => (control("mcp-rotate-env") as HTMLButtonElement).click());

    const field = document.body.querySelector<HTMLInputElement>("#mcp-env-org-git-GIT_TOKEN");
    expect(field).not.toBeNull();
    const setter = Object.getOwnPropertyDescriptor(
      window.HTMLInputElement.prototype,
      "value",
    )?.set;
    act(() => {
      setter?.call(field, "ghp_wrong");
      field?.dispatchEvent(new Event("input", { bubbles: true }));
    });
  }

  it("surfaces a refused reconnection and keeps the form open", async () => {
    await openForm();
    registryApi.updateMcpRegistryEnv.mockResolvedValue({
      note: "Saved.",
      test: health("needs_config", "token_rejected", "That credential was rejected."),
    });

    await act(async () => (control("mcp-env-save") as HTMLButtonElement).click());

    // Before this, `res.test` was stored and the form closed regardless — a
    // Save that looked identical whether the credential worked or not.
    expect(control("mcp-env-inline")).not.toBeNull();
    expect(document.body.textContent).toContain("That credential was rejected.");
  });

  it("reports a refusal the host sent with no sentence of its own", async () => {
    await openForm();
    registryApi.updateMcpRegistryEnv.mockResolvedValue({
      note: "Saved.",
      test: health("error", undefined, "   "),
    });

    await act(async () => (control("mcp-env-save") as HTMLButtonElement).click());

    expect(control("mcp-env-inline")).not.toBeNull();
    expect(document.body.textContent).toContain("still isn't connected");
  });

  it("closes the form and confirms the connection when the credentials connect", async () => {
    await openForm();
    registryApi.updateMcpRegistryEnv.mockResolvedValue({
      note: "Saved.",
      test: { ...health("ok"), toolCount: 4 },
    });

    await act(async () => (control("mcp-env-save") as HTMLButtonElement).click());

    expect(control("mcp-env-inline")).toBeNull();
    expect(control("mcp-connect-done")?.textContent).toContain("Connected · 4 tools");
  });
});
