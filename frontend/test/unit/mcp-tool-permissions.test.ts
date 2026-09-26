// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { ApiError, type McpServer, type McpSource } from "@/api/types";
import type { ToolPolicyDocument } from "@/api/mcp-tool-policy";

/**
 * The console's per-tool permissions panel (issue #2373).
 *
 * Two properties are worth pinning, and neither is visible from the component's
 * props. The first is which half of the API a row may call: a server can be
 * both a declaration and a directory install, and the host reconciles those
 * into one row that keeps the declaration's provenance *and* carries a
 * `serverId` — so "has a serverId" is not "is a registry row" (#1270). The
 * second is that what the panel renders is the document the host echoed back,
 * never the value that was clicked: the host resolves a mode out of the
 * override, the tier's bulk default and the declaration, so a console that
 * rendered the click would be carrying a second copy of that ladder and would
 * eventually disagree with the gate.
 */

const api = vi.hoisted(() => ({
  readToolPolicy: vi.fn(),
  writeToolPolicy: vi.fn(),
  resetToolPolicy: vi.fn(),
}));

vi.mock("@/api/mcp-tool-policy", async () => {
  const actual = await vi.importActual<typeof import("@/api/mcp-tool-policy")>(
    "@/api/mcp-tool-policy",
  );
  return { ...actual, ...api };
});

const { policyTarget } = await import("@/api/mcp-tool-policy");
const { McpToolPermissions, tierPatch } = await import("@/views/mcp/McpToolPermissions");

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

function doc(over: Partial<ToolPolicyDocument> = {}): ToolPolicyDocument {
  return {
    server: "notion",
    tierDefaults: {
      read_only: { mode: "always_allow", stored: false },
      interactive: { mode: "needs_approval", stored: false },
      write_delete: { mode: "needs_approval", stored: false },
    },
    tools: [],
    discoveredAtMillis: 1,
    ...over,
  };
}

describe("which routes a row's permissions live behind", () => {
  it("sends a directory install to the registry route, keyed by its install id", () => {
    expect(policyTarget(row({ source: "registry", serverId: "srv_9fa1" }))).toEqual({
      kind: "registry",
      serverId: "srv_9fa1",
    });
  });

  it("keeps a reconciled row on the declared route despite its serverId", () => {
    // Declared in company.toml AND installed from the directory. The registry
    // route would address the install and leave the declaration's own policy —
    // the one the `mcp:<name>` bridge actually enforces — untouched.
    expect(policyTarget(row({ source: "manifest", serverId: "srv_9fa1" }))).toEqual({
      kind: "declared",
      name: "notion",
    });
  });

  it("refuses to guess for a registry row with no install id", () => {
    expect(policyTarget(row({ source: "registry" }))).toBeNull();
  });
});

const client = {} as unknown as OpenCompanyClient;

let container: HTMLDivElement;
let root: Root;

function el(testId: string): HTMLElement | null {
  return container.querySelector(`[data-testid="${testId}"]`);
}

async function mount(server: McpServer, canManage = true, reloadKey = 0) {
  await act(async () => {
    root.render(
      createElement(McpToolPermissions, {
        client,
        company: "acme",
        server,
        canManage,
        reloadKey,
        onClose: () => {},
      }),
    );
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

describe("a damaged policy document", () => {
  it("offers the repair instead of rendering permissions nobody chose", async () => {
    api.readToolPolicy.mockRejectedValue(
      new ApiError(409, "policy_unreadable", "the stored tool permissions cannot be read.", true),
    );

    await mount(row({ source: "runtime" }));

    expect(el("mcp-permissions-unreadable")).not.toBeNull();
    expect(el("mcp-permissions-clear")).not.toBeNull();
    // An empty policy would read as "every tool runs on whatever the tier says",
    // and an edit saved from that view would make it true.
    expect(container.querySelectorAll('[data-testid="mcp-permission-row"]')).toHaveLength(0);
  });

  it("does not offer the repair to someone who cannot write", async () => {
    api.readToolPolicy.mockRejectedValue(
      new ApiError(409, "policy_unreadable", "the stored tool permissions cannot be read.", true),
    );

    await mount(row({ source: "runtime" }), false);

    expect(el("mcp-permissions-unreadable")).not.toBeNull();
    expect(el("mcp-permissions-clear")).toBeNull();
  });
});

describe("what the panel renders after a write", () => {
  const before = doc({
    tools: [
      {
        tool: "delete_page",
        effectiveTier: "read_only",
        suggestedTier: "write_delete",
        mode: "always_allow",
        isOverride: true,
      },
    ],
  });

  it("renders the host's answer, not the row as it was clicked", async () => {
    api.readToolPolicy.mockResolvedValue(before);
    // Clearing the override drops the operator's `read_only` reclassification,
    // so the row comes back under discovery's suggestion and the tier's own
    // default. Nothing in the console could have computed that.
    api.writeToolPolicy.mockResolvedValue(
      doc({
        tools: [
          {
            tool: "delete_page",
            effectiveTier: "write_delete",
            suggestedTier: "write_delete",
            mode: "needs_approval",
            isOverride: false,
          },
        ],
      }),
    );

    await mount(row({ source: "runtime" }));
    expect(el("mcp-permission-clear-row")).not.toBeNull();

    await act(async () => {
      el("mcp-permission-clear-row")?.click();
    });

    expect(api.writeToolPolicy).toHaveBeenCalledWith(client, "acme", { kind: "declared", name: "notion" }, {
      tools: [{ tool: "delete_page" }],
    });
    // The clear control is gone because the echoed row is no longer an
    // override — the panel re-derived from the response.
    expect(el("mcp-permission-clear-row")).toBeNull();
  });

  it("keeps the panel standing when a write is refused", async () => {
    api.readToolPolicy.mockResolvedValue(before);
    api.writeToolPolicy.mockRejectedValue(new ApiError(403, "forbidden", "admins only", true));

    await mount(row({ source: "runtime" }));
    await act(async () => {
      el("mcp-permission-clear-row")?.click();
    });

    expect(el("mcp-permissions-write-error")?.textContent).toContain("admins only");
    // The refused row still reads as the override it still is.
    expect(el("mcp-permission-clear-row")).not.toBeNull();
  });
});

describe("a server nothing has discovered yet", () => {
  it("says the tier defaults still apply rather than showing an empty list", async () => {
    api.readToolPolicy.mockResolvedValue(doc({ discoveredAtMillis: 0 }));

    await mount(row({ source: "runtime" }));

    expect(el("mcp-permissions-empty")).not.toBeNull();
  });
});

describe("what the per-tier control says is set", () => {
  it("reads as unset when the host stored nothing for that tier", async () => {
    // The bug this pins: the control rendered the tier's nominal mode, so a
    // fresh server showed "Read-only: Runs" while every read-only row showed
    // "Asks" — and choosing the value already on screen granted a bulk allow.
    api.readToolPolicy.mockResolvedValue(doc());

    await mount(row({ source: "runtime" }));

    const trigger = container.querySelector<HTMLElement>("#tier-read_only");
    expect(trigger?.textContent).toContain("Not set");
    expect(trigger?.textContent).not.toContain("Runs");
  });

  it("reads as the stored mode once an operator has written one", async () => {
    api.readToolPolicy.mockResolvedValue(
      doc({
        tierDefaults: {
          read_only: { mode: "always_allow", stored: true },
          interactive: { mode: "needs_approval", stored: false },
          write_delete: { mode: "needs_approval", stored: false },
        },
      }),
    );

    await mount(row({ source: "runtime" }));

    expect(container.querySelector("#tier-read_only")?.textContent).toContain("Runs");
  });
});

describe("a tool the allow and deny lists keep from being sent", () => {
  it("says so on the row, because its mode will never be consulted", async () => {
    api.readToolPolicy.mockResolvedValue(
      doc({
        tools: [
          {
            tool: "place_order",
            effectiveTier: "interactive",
            mode: "needs_approval",
            isOverride: false,
          },
        ],
      }),
    );

    await mount(row({ source: "runtime", disallowedTools: ["place_order"] }));

    expect(container.textContent).toContain("Not sent");
  });

  it("says nothing when the lists let every discovered tool through", async () => {
    api.readToolPolicy.mockResolvedValue(
      doc({
        tools: [
          {
            tool: "place_order",
            effectiveTier: "interactive",
            mode: "needs_approval",
            isOverride: false,
          },
        ],
      }),
    );

    await mount(row({ source: "runtime" }));

    expect(container.textContent).not.toContain("Not sent");
  });
});

describe("what a choice in the tier control means on the wire", () => {
  it("sends the mode when one was chosen", () => {
    expect(tierPatch("read_only", "always_allow")).toEqual({
      tierDefaults: { read_only: "always_allow" },
    });
  });

  it("sends null when the tier was put back to unset", () => {
    // Not an omitted key: the host reads a missing tier as "leave it alone",
    // so an omission would leave the bulk allow in force and the control
    // would read as cleared while the gate went on letting tools through.
    expect(tierPatch("read_only", "unset")).toEqual({
      tierDefaults: { read_only: null },
    });
  });

  it("clears the tier the operator named, not some other one", () => {
    expect(tierPatch("write_delete", "unset")).toEqual({
      tierDefaults: { write_delete: null },
    });
  });
});

describe("a probe that ran while the panel was open", () => {
  it("re-reads the policy, because the probe rewrote what it resolves against", async () => {
    api.readToolPolicy.mockResolvedValue(doc({ discoveredAtMillis: 0 }));

    const server = row({ source: "runtime" });
    await mount(server);
    expect(el("mcp-permissions-empty")).not.toBeNull();
    expect(api.readToolPolicy).toHaveBeenCalledTimes(1);

    api.readToolPolicy.mockResolvedValue(
      doc({
        discoveredAtMillis: 2,
        tools: [
          {
            tool: "search_pages",
            effectiveTier: "read_only",
            suggestedTier: "read_only",
            mode: "needs_approval",
            isOverride: false,
          },
        ],
      }),
    );
    await mount(server, true, 1);

    expect(api.readToolPolicy).toHaveBeenCalledTimes(2);
    expect(el("mcp-permissions-empty")).toBeNull();
    expect(container.textContent).toContain("search_pages");
  });

  it("does not re-read when nothing probed it", async () => {
    api.readToolPolicy.mockResolvedValue(doc());

    const server = row({ source: "runtime" });
    await mount(server);
    await mount(server);

    expect(api.readToolPolicy).toHaveBeenCalledTimes(1);
  });
});
