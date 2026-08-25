import { describe, expect, it } from "vitest";

import type { FsNode } from "@/api/workspace";
import { agentBadge } from "@/lib/workspace";
import { rosterNameMap } from "@/lib/roster-names";

/**
 * The workspace tree's agent-provenance badge (issue #1723).
 *
 * The chip marking an agent-authored node used to print the raw roster handle
 * (`analytics_analyst`) — the node's real folder name, but not what an
 * operator recognizes the teammate by. It now resolves through the same roster
 * lookup the row label uses, so it reads "Analytics Analyst", with the raw
 * handle kept on the tooltip. These tests are red against the pre-#1723 badge
 * that rendered `node.createdBy.id` verbatim.
 */

function agentNode(id: string): Pick<FsNode, "createdBy"> {
  return { createdBy: { kind: "agent", id } } as Pick<FsNode, "createdBy">;
}

const ROSTER = rosterNameMap([{ id: "analytics_analyst", name: "Analytics Analyst" }]);

describe("agentBadge", () => {
  it("resolves the raw handle to the roster display name", () => {
    const badge = agentBadge(agentNode("analytics_analyst"), "report.md", ROSTER);
    expect(badge).not.toBeNull();
    // The label is the human name — never the snake_case handle the id carries.
    expect(badge?.label).toBe("Analytics Analyst");
  });

  it("keeps the raw handle reachable for the tooltip", () => {
    // Disambiguation still needs the underlying id, so it rides `handle`
    // (which the row spends on the badge's `title`) even as the label reads
    // as a name.
    const badge = agentBadge(agentNode("analytics_analyst"), "report.md", ROSTER);
    expect(badge?.handle).toBe("analytics_analyst");
  });

  it("falls back to the id when the roster has not resolved it", () => {
    // The roster listing may not have loaded, or may not carry this id — the
    // badge must still render the handle rather than go blank, exactly as the
    // shared resolver does.
    const badge = agentBadge(agentNode("analytics_analyst"), "report.md", rosterNameMap([]));
    expect(badge?.label).toBe("analytics_analyst");
    expect(badge?.handle).toBe("analytics_analyst");
  });

  it("shows no badge for a node not authored by an agent", () => {
    const operator = { createdBy: { kind: "operator" } } as Pick<FsNode, "createdBy">;
    const seed = { createdBy: { kind: "seed" } } as Pick<FsNode, "createdBy">;
    expect(agentBadge(operator, "report.md", ROSTER)).toBeNull();
    expect(agentBadge(seed, "report.md", ROSTER)).toBeNull();
  });

  it("suppresses the badge when its name would only repeat the row label", () => {
    // An agent's own root folder is already labelled with the agent's name;
    // a chip repeating it beside the label says nothing new.
    expect(agentBadge(agentNode("analytics_analyst"), "Analytics Analyst", ROSTER)).toBeNull();
  });
});
