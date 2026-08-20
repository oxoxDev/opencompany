import { describe, expect, it } from "vitest";

import type { McpHealth, McpServer, McpSource } from "@/api/types";
import {
  mcpLifecycle,
  mcpProvenanceNote,
  mcpRemoval,
  mcpRemovalNote,
  mcpRowControls,
  mcpSourceBadge,
} from "@/lib/mcp-registry";

/**
 * Issue #1270. `GET …/mcp/servers` now returns two structurally different
 * things in one list: List A rows (manifest / default / operator-typed
 * runtime), addressed by `name` against a declaration the host resolves, and
 * directory installs, addressed by a `serverId` in the host's registry store.
 *
 * The row cannot tell them apart by looking at what it has, because a server
 * can be **both** — declared in `company.toml` *and* installed from the
 * directory — and the host reconciles those into one row that keeps List A's
 * provenance. That row carries a `serverId`. So "has a serverId" is not "is a
 * registry row", and reading it as one is exactly how a delete removes the
 * wrong thing: it would uninstall the directory copy of a manifest server,
 * report success, and leave the declared server exactly where it was.
 *
 * These pin the dispatch: provenance comes from `source`, and `source` decides
 * which half of the API the row may call at all.
 */

const health = (status: McpHealth["status"], authHint?: string): McpHealth => ({
  status,
  message: "",
  toolCount: 0,
  checkedAtMillis: 1,
  authHint,
});

/** A merged row. Defaults to a plain List A runtime server. */
function row(over: Partial<McpServer> & { source: McpSource }): McpServer {
  return {
    name: "notion",
    endpoint: "https://mcp.notion.com/mcp",
    enabled: true,
    allowedTools: [],
    disallowedTools: [],
    timeoutSecs: 30,
    authConfigured: false,
    ...over,
  };
}

describe("mcpRemoval — which route owns this row", () => {
  it("sends a registry row to the install route, keyed by serverId", () => {
    const server = row({ source: "registry", name: "org-git", serverId: "srv_9fa1" });

    // The `name` here is a display slug the host minted for the merged view. It
    // addresses no List A declaration, and on an unlucky collision would
    // address someone else's.
    expect(mcpRemoval(server)).toEqual({ kind: "install", serverId: "srv_9fa1" });
  });

  it("keeps a manifest server's delete guard even when it also has an install", () => {
    // The reconciliation case. The host adopted the install onto the manifest
    // row, so the row carries a `serverId` — and must still refuse a delete,
    // because the declaration in company.toml outlives any uninstall and the
    // next read merges it straight back.
    const reconciled = row({ source: "manifest", serverId: "srv_9fa1" });

    expect(mcpRemoval(reconciled)).toEqual({ kind: "none" });
    expect(mcpRemoval(row({ source: "manifest" }))).toEqual({ kind: "none" });
  });

  it("refuses a delete on an install-wide default, with or without an install", () => {
    expect(mcpRemoval(row({ source: "default" }))).toEqual({ kind: "none" });
    expect(mcpRemoval(row({ source: "default", serverId: "srv_9fa1" }))).toEqual({ kind: "none" });
  });

  it("deletes a reconciled runtime row by name, not by its install id", () => {
    // One call is the whole removal: the host drops the runtime index row and
    // uninstalls the directory half together. Routing this to the registry
    // instead would uninstall one half and leave the typed-in URL behind.
    const reconciled = row({ source: "runtime", name: "linear", serverId: "srv_44" });

    expect(mcpRemoval(reconciled)).toEqual({ kind: "index", name: "linear" });
  });

  it("offers no delete for a registry row the host gave no id", () => {
    // A delete with no key would remove nothing and report success.
    expect(mcpRemoval(row({ source: "registry" }))).toEqual({ kind: "none" });
  });
});

describe("mcpLifecycle — connect/disconnect belongs to registry rows only", () => {
  it("offers a disconnect only while a session is live", () => {
    const server = row({ source: "registry", serverId: "srv_1" });

    expect(mcpLifecycle(server, health("ok"))).toBe("disconnect");
  });

  it("offers a connect for every state that is not a live session", () => {
    const server = row({ source: "registry", serverId: "srv_1" });

    for (const state of [undefined, health("needs_config"), health("error"), health("unknown")]) {
      expect(mcpLifecycle(server, state)).toBe("connect");
    }
  });

  it("offers neither on a reconciled row, whose live half is List A's", () => {
    // The connect route would accept this `serverId`, which is the trap: what
    // the company's agents reach through this row is the List A half, so a
    // Disconnect here would drop a session the row does not represent while its
    // tools went on working. The row already governs its own half with the
    // Switch.
    expect(mcpLifecycle(row({ source: "manifest", serverId: "srv_1" }), health("ok"))).toBe("none");
    expect(mcpLifecycle(row({ source: "runtime", serverId: "srv_1" }), health("ok"))).toBe("none");
    expect(mcpLifecycle(row({ source: "runtime" }), health("ok"))).toBe("none");
  });
});

describe("mcpRowControls — which half of the API a row may call", () => {
  it("withholds List A's controls from a directory install", () => {
    // The Switch, Test and Tools all resolve the row's `name` against List A's
    // declarations. A directory install has none, so all three answer `no MCP
    // server named …` — a click spent to reach an error about a name the
    // operator never chose.
    const controls = mcpRowControls(
      row({ source: "registry", name: "org-git", serverId: "srv_9fa1" }),
      health("needs_config"),
    );

    expect(controls.toggle).toBe(false);
    expect(controls.probe).toBe(false);
    expect(controls.lifecycle).toBe("connect");
    expect(controls.removal).toEqual({ kind: "install", serverId: "srv_9fa1" });
  });

  it("leaves a reconciled manifest row with exactly the controls it always had", () => {
    const controls = mcpRowControls(
      row({ source: "manifest", serverId: "srv_9fa1" }),
      health("ok"),
    );

    expect(controls.toggle).toBe(true);
    expect(controls.probe).toBe(true);
    expect(controls.lifecycle).toBe("none");
    expect(controls.removal).toEqual({ kind: "none" });
  });

  it("leaves a plain runtime row unchanged", () => {
    const controls = mcpRowControls(row({ source: "runtime" }), undefined);

    expect(controls).toEqual({
      toggle: true,
      probe: true,
      lifecycle: "none",
      removal: { kind: "index", name: "notion" },
    });
  });
});

describe("mcpSourceBadge", () => {
  it("badges a directory install as its own provenance", () => {
    expect(mcpSourceBadge("registry").label).toBe("registry");
  });

  it("badges a reconciled row by its source, so provenance is never re-derived", () => {
    // The badge is a pure function of `source` — a row carrying a `serverId`
    // cannot reach this at all, which is the property that keeps a manifest
    // server from being relabelled by an install that landed on it.
    expect(mcpSourceBadge("manifest")).toEqual({ label: "manifest", variant: "secondary" });
  });

  it("gives every provenance a badge rather than defaulting a new one", () => {
    for (const source of ["manifest", "runtime", "default", "registry"] as const) {
      expect(mcpSourceBadge(source).label).toBe(source);
    }
  });
});

describe("detail-panel prose", () => {
  it("stops describing a directory install as console-added", () => {
    // The panel read `manifest` against everything-else, so a registry row was
    // told it "lives in this company's runtime store" — true of neither half.
    const note = mcpProvenanceNote("registry");

    expect(note).toContain("directory");
    expect(note).not.toContain("runtime store");
    expect(note).not.toBe(mcpProvenanceNote("runtime"));
  });

  it("gives each provenance its own removal sentence", () => {
    const notes = (["manifest", "runtime", "default", "registry"] as const).map(mcpRemovalNote);

    expect(new Set(notes).size).toBe(4);
    // The two undeletable provenances say so; the two deletable ones do not.
    expect(mcpRemovalNote("manifest")).toContain("cannot be removed");
    expect(mcpRemovalNote("default")).toContain("cannot be removed");
    expect(mcpRemovalNote("registry")).not.toContain("cannot be removed");
    expect(mcpRemovalNote("runtime")).not.toContain("cannot be removed");
  });
});
