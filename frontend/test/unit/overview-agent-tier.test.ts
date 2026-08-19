import { describe, expect, it } from "vitest";

import type { Person as HostPerson } from "@/api/auth";
import type { Task } from "@/api/tasks";
import type { TeamMemberDto } from "@/api/types";
import { fromDto, type TeamMember } from "@/lib/team";
import { adapt } from "@/views/overview/kg/adapter";
import { ownedBy } from "@/views/overview/pulse";

/**
 * The tier an agent draws with on the Overview graph (issue #643).
 *
 * The graph built every agent node with a literal `tier: "worker"`, so a
 * company declaring `[[agent]] tier = "orchestrator"` read back as `tier
 * worker` on its own graph. The root cause was a DTO gap, not a rendering bug:
 * the graph is built from the roster **list**, and only the single-agent detail
 * read carried a tier — the same gap #635 closed for tool grants and desks.
 *
 * Every failure here is silent on screen. A wrong tier draws a perfectly clean
 * node; it just describes a company that does not exist, and it contradicts the
 * agent detail card one click away.
 *
 * The two values are deliberately **two questions**, and each case below pins
 * one of them against the other:
 *
 * - `tier` is the declaration, verbatim — absent means the company declared
 *   none, which is not the same as any tier string.
 * - `isOrchestrator` is the host's roster rule (tagged tier first, else the
 *   first declared agent). The console must never re-derive it from `tier`:
 *   an untagged CEO is the orchestrator with no tier at all, and a *second*
 *   agent tagged with the orchestrator tier carries the tag without being one.
 */

const BASE = {
  tasks: [] as Task[],
  people: [] as HostPerson[],
  workflows: [],
  desks: [],
  ownedBy,
};

/** A roster member as the console holds it, with the tier fields under test. */
function member(id: string, over: Partial<TeamMember> = {}): TeamMember {
  return {
    id,
    name: id,
    role: "Worker",
    description: "",
    tone: "a",
    avatar: "a",
    inboxEnabled: false,
    effectiveTools: [],
    desks: [],
    ...over,
  };
}

/** The agents the graph would draw for this roster. */
function graphAgents(members: TeamMember[]) {
  return adapt({ ...BASE, members }).agents;
}

describe("the graph draws the tier the company declared", () => {
  it("carries a declared tier through to the node verbatim", () => {
    const agents = graphAgents([
      member("ceo", { tier: "orchestrator", isOrchestrator: true }),
      member("writer", { tier: "reasoning" }),
      member("intern"),
    ]);

    expect(agents.find((a) => a.id === "ceo")!.tier).toBe("orchestrator");
    expect(agents.find((a) => a.id === "writer")!.tier).toBe("reasoning");

    // The negative control: an undeclared tier stays undeclared. `null` is the
    // graph's "cannot say" — substituting anything here is the whole of #643.
    expect(agents.find((a) => a.id === "intern")!.tier).toBeNull();

    // The literal regression assertion. Nobody declared "worker", so no node
    // may carry it — this is the exact string the graph used to stamp on every
    // agent it drew.
    expect(agents.map((a) => a.tier)).not.toContain("worker");
  });

  it("reads the orchestrator marker from the host rather than from the tier", () => {
    // An untagged CEO: the host resolved it as the orchestrator by roster
    // position, and it declares no tier at all. A console deriving the marker
    // from the tier string would draw this one unmarked.
    const [ceo] = graphAgents([member("ceo", { isOrchestrator: true })]);
    expect(ceo.tier).toBeNull();
    expect(ceo.isOrchestrator).toBe(true);

    // The negative control, the other way round: a second agent tagged with the
    // orchestrator tier is not the orchestrator. The tag shows; the marker does
    // not. A tier-derived marker would draw two orchestrators on one company.
    const [, second] = graphAgents([
      member("ceo", { tier: "orchestrator", isOrchestrator: true }),
      member("deputy", { tier: "orchestrator", isOrchestrator: false }),
    ]);
    expect(second.tier).toBe("orchestrator");
    expect(second.isOrchestrator).toBe(false);
  });

  it("says nothing for a host that answers neither", () => {
    // A host predating #643 sends no tier and no marker. `fromDto` leaves both
    // undefined, and the graph draws "cannot say" — never a default tier and
    // never a marker guessed from the tier's absence.
    const old = fromDto({ id: "ada", role: "Engineer" } satisfies TeamMemberDto);
    const [agent] = graphAgents([old]);

    expect(agent.tier).toBeNull();
    expect(agent.tier).not.toBe("worker");
    expect(agent.isOrchestrator).toBe(false);
  });
});

describe("fromDto passes both fields straight through", () => {
  it("keeps an absent field absent rather than defaulting it", () => {
    const declared = fromDto({
      id: "ceo",
      role: "Chief Executive",
      tier: "orchestrator",
      isOrchestrator: true,
    });
    expect(declared.tier).toBe("orchestrator");
    expect(declared.isOrchestrator).toBe(true);

    // The negative control: `undefined` must survive the mapping as
    // `undefined`. The same rule `budgetUsdDaily` follows — a coalesced default
    // here would be indistinguishable from a host that answered.
    const silent = fromDto({ id: "intern", role: "Intern" });
    expect(silent.tier).toBeUndefined();
    expect(silent.isOrchestrator).toBeUndefined();

    // And a host that answers `false` is not the same as one that says nothing,
    // even though both draw unmarked.
    expect(fromDto({ id: "w", role: "W", isOrchestrator: false }).isOrchestrator).toBe(false);
  });
});
