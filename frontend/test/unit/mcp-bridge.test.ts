import { describe, expect, it } from "vitest";

import { mcpAddedMessage, mcpBridgeState } from "@/lib/mcp-bridge";
import type { CapabilityStatusDto } from "@/api/types";

/**
 * The MCP tab's build-state signal (issue #567).
 *
 * The management routes are ungated, so an operator on a build without the `mcp`
 * feature adds a server, stores a token, watches it probe healthy — and no agent
 * ever receives a tool from it. What makes this worth a test rather than an
 * inline ternary is the third state: `mcpInBuild` is optional on the wire, and
 * treating a host that never sends it as "the bridge is missing" would replace
 * one false claim with another, on every older host.
 */

function status(over: Partial<CapabilityStatusDto> = {}): CapabilityStatusDto {
  return { configured: false, ...over };
}

describe("mcpBridgeState", () => {
  it("reads an explicit true as the bridge being present", () => {
    expect(mcpBridgeState(status({ mcpInBuild: true }))).toBe("present");
  });

  it("reads an explicit false as the bridge being absent", () => {
    expect(mcpBridgeState(status({ mcpInBuild: false }))).toBe("absent");
  });

  it("says nothing when the host omits the field", () => {
    // An older host that predates the flag. Rendering this as "absent" would
    // tell every such operator their servers are dead when they may be fine.
    expect(mcpBridgeState(status())).toBe("unknown");
  });

  it("says nothing when the capability read failed", () => {
    expect(mcpBridgeState(null)).toBe("unknown");
    expect(mcpBridgeState(undefined)).toBe("unknown");
  });

  it("does not treat a non-boolean as an answer", () => {
    // A host that sends `null` (or any other shape) for the field has not said
    // the bridge is missing — the same silence as omitting it.
    const malformed = { configured: false, mcpInBuild: null } as unknown as CapabilityStatusDto;
    expect(mcpBridgeState(malformed)).toBe("unknown");
  });
});

/**
 * The add-success message, which is where the screen's other claim used to
 * live: "Teammates pick it up on the next rebuild" — fired at the moment the
 * operator acts, and false twice over on a build with no bridge (nothing picks
 * it up, and since #566 a rebuild is not how pickup happens either).
 */
describe("mcpAddedMessage", () => {
  it("confirms the add without promising pickup when the bridge is missing", () => {
    const message = mcpAddedMessage("notion", "absent");
    expect(message).toContain("notion");
    // Still a success: the server IS stored, and survives the rebuild that adds
    // the feature. Saying otherwise would report a working add as a failure.
    expect(message).toMatch(/stored/i);
    expect(message).not.toMatch(/teammates pick it up/i);
  });

  it("promises next-turn pickup when the bridge is present", () => {
    const message = mcpAddedMessage("notion", "present");
    expect(message).toMatch(/next turn/i);
    // #566: pickup no longer needs a restart, and the console said it did long
    // after the host stopped saying so.
    expect(message).not.toMatch(/rebuild|restart/i);
  });

  it("treats an unanswering host as ordinary, not as broken", () => {
    // Unknown is not absence: claiming no agent can use the server, on a host
    // that never said so, is the same class of lie in the other direction.
    expect(mcpAddedMessage("notion", "unknown")).toBe(mcpAddedMessage("notion", "present"));
  });
});
