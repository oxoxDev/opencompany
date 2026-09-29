import { describe, expect, it } from "vitest";

import {
  applyObservations,
  boardObservations,
  neighbourhood,
  structuralGraph,
  type CommsObservation,
} from "@/views/comms/model";
import { edgeWeight, layoutComms } from "@/views/comms/layout";

/**
 * The company's own wiring, folded out of four unrelated sources.
 *
 * The failures worth guarding are both quiet: drawing an edge nobody has, and
 * dropping one everybody uses. The first invents a delegation path an operator
 * will then try to reason about; the second makes a busy company look idle.
 */

const AGENTS = [
  { id: "orchestrator", name: "Orchestrator", role: "Runs the company", isOrchestrator: true, delegatesTo: ["*"] },
  { id: "planner", name: "Planner", role: "Plans", delegatesTo: ["solvers"] },
  { id: "scribe", name: "Scribe", role: "Writes", delegatesTo: [] },
];
const DESKS = [
  { id: "solvers", name: "Solvers", members: ["planner", "scribe"] },
  { id: "records", name: "Records", members: ["scribe"] },
];

describe("structure, before anything has happened", () => {
  it("draws the desks a teammate may reach, not just the ones it has used", () => {
    // Without the declared allowlist a company that has never run draws as a
    // set of unconnected dots, which is not what its manifest says.
    const g = structuralGraph(AGENTS, DESKS);
    const may = g.edges.filter((e) => e.kind === "may-delegate");
    expect(may.some((e) => e.from === "agent:planner" && e.to === "desk:solvers")).toBe(true);
    expect(may.some((e) => e.from === "agent:scribe")).toBe(false);
  });

  it("expands a wildcard allowlist to every desk", () => {
    const g = structuralGraph(AGENTS, DESKS);
    const orchestrator = g.edges.filter(
      (e) => e.kind === "may-delegate" && e.from === "agent:orchestrator",
    );
    expect(orchestrator.map((e) => e.to).sort()).toEqual(["desk:records", "desk:solvers"]);
  });

  it("drops an allowlist entry naming a desk this company does not declare", () => {
    // Drawn, it would be an edge to a node that does not exist.
    const g = structuralGraph(
      [{ id: "a", delegatesTo: ["ghost"] }],
      DESKS,
    );
    expect(g.edges.filter((e) => e.kind === "may-delegate")).toEqual([]);
  });

  it("draws membership, which is who shares a room", () => {
    const g = structuralGraph(AGENTS, DESKS);
    const members = g.edges.filter((e) => e.kind === "member");
    expect(members).toHaveLength(3);
  });

  it("counts every observed edge as zero until something is observed", () => {
    const g = structuralGraph(AGENTS, DESKS);
    expect(g.edges.every((e) => e.count === 0)).toBe(true);
  });
});

describe("what the stream adds", () => {
  const base = structuralGraph(AGENTS, DESKS);

  it("merges repeat hand-offs into one weighted edge", () => {
    // Fifty parallel lines say nothing; one thick one says "this is the path
    // the company actually uses".
    const obs: CommsObservation[] = [
      { kind: "handed-off", from: "planner", to: "solvers", via: "delegate_to_desk", atMillis: 1 },
      { kind: "handed-off", from: "planner", to: "solvers", via: "delegate_to_desk", atMillis: 2 },
    ];
    const g = applyObservations(base, obs);
    const handed = g.edges.filter((e) => e.kind === "handed-off");
    expect(handed).toHaveLength(1);
    expect(handed[0].count).toBe(2);
    expect(handed[0].lastAtMillis).toBe(2);
  });

  it("draws no edge when the target was redacted", () => {
    // Tool-call arguments reach the console redacted, so the target may not be
    // readable. Guessing one is worse than admitting the gap.
    const g = applyObservations(base, [
      { kind: "handed-off", from: "planner", to: null, via: "delegate_to_desk", atMillis: 1 },
    ]);
    expect(g.edges.filter((e) => e.kind === "handed-off")).toEqual([]);
  });

  it("mints a spawned teammate the roster has not caught up with", () => {
    // The tool call is seen before the `/team` re-read lands.
    const g = applyObservations(base, [
      { kind: "spawned", by: "orchestrator", agentId: "newcomer", atMillis: 5 },
    ]);
    const node = g.nodes.find((n) => n.id === "agent:newcomer");
    expect(node?.spawned).toBe(true);
    expect(
      g.edges.some((e) => e.kind === "spawned" && e.to === "agent:newcomer"),
    ).toBe(true);
  });

  it("records a spawn with no known author as a node and no edge", () => {
    const g = applyObservations(base, [
      { kind: "spawned", by: null, agentId: "newcomer", atMillis: 5 },
    ]);
    expect(g.nodes.some((n) => n.id === "agent:newcomer")).toBe(true);
    expect(g.edges.filter((e) => e.kind === "spawned")).toEqual([]);
  });

  it("marks a speaking agent without adding an edge", () => {
    const g = applyObservations(base, [{ kind: "speaking", agentId: "planner" }]);
    expect(g.nodes.find((n) => n.id === "agent:planner")?.speaking).toBe(true);
    expect(g.edges).toHaveLength(base.edges.length);
  });

  it("draws who spoke to whom inside an episode as its own edge kind", () => {
    // A broadcast the router carried to a seat, a desk DM, a referral to a
    // desk: each is an agent reaching someone, watched happen, and merged by
    // endpoint pair like a hand-off — three DMs are one thicker edge.
    const g = applyObservations(base, [
      { kind: "spoke", from: "planner", to: "scribe", via: "dm", atMillis: 1 },
      { kind: "spoke", from: "planner", to: "scribe", via: "broadcast", atMillis: 2 },
      { kind: "spoke", from: "scribe", to: "records", via: "referral", atMillis: 3 },
    ]);
    const spoke = g.edges.filter((e) => e.kind === "spoke");
    expect(spoke.map((e) => `${e.from}->${e.to}:${e.count}:${e.label}`)).toEqual([
      "agent:planner->agent:scribe:2:broadcast",
      "agent:scribe->desk:records:1:referral",
    ]);
    expect(spoke.every((e) => !e.provisional)).toBe(true);
    // Structure is untouched: the member and may-delegate edges still count zero.
    expect(g.edges.filter((e) => e.kind !== "spoke").every((e) => e.count === 0)).toBe(true);
  });

  it("draws a seat talking to itself as nothing", () => {
    const g = applyObservations(base, [
      { kind: "spoke", from: "planner", to: "planner", via: "broadcast", atMillis: 1 },
    ]);
    expect(g.edges.filter((e) => e.kind === "spoke")).toEqual([]);
  });

  it("leaves the structural graph untouched", () => {
    // The fold must not mutate its input, or a re-render with the same base
    // would keep accumulating counts.
    const before = JSON.stringify(base);
    applyObservations(base, [
      { kind: "handed-off", from: "planner", to: "solvers", via: "x", atMillis: 1 },
    ]);
    expect(JSON.stringify(base)).toBe(before);
  });
});

describe("layout stability", () => {
  it("does not move existing nodes when an unrelated one arrives", () => {
    // The reason this is not a force simulation: a spawn frame must not drift
    // every node on screen, or "who spawned whom" stops being readable between
    // glances.
    const before = layoutComms(structuralGraph(AGENTS, DESKS));
    const after = layoutComms(
      applyObservations(structuralGraph(AGENTS, DESKS), [
        { kind: "spawned", by: "orchestrator", agentId: "zzz-newcomer", atMillis: 1 },
      ]),
    );
    for (const placed of before.placed) {
      const same = after.placed.find((p) => p.node.id === placed.node.id);
      expect(same, placed.node.id).toBeTruthy();
      expect(same!.x).toBe(placed.x);
      expect(same!.y).toBe(placed.y);
    }
  });

  it("puts the orchestrator first and is otherwise stable by id", () => {
    const { placed } = layoutComms(structuralGraph(AGENTS, DESKS));
    const agents = placed.filter((p) => p.node.kind === "agent");
    expect(agents[0].node.id).toBe("agent:orchestrator");
    expect(agents.map((p) => p.node.id).slice(1)).toEqual([
      "agent:planner",
      "agent:scribe",
    ]);
  });

  it("caps edge weight so a busy path does not swamp the diagram", () => {
    expect(edgeWeight(0)).toBe(1);
    expect(edgeWeight(1)).toBeGreaterThan(1);
    expect(edgeWeight(5)).toBeLessThan(edgeWeight(50));
    expect(edgeWeight(5_000)).toBeLessThanOrEqual(5);
  });
});

describe("the neighbourhood filter", () => {
  it("keeps only what touches the selection", () => {
    const g = structuralGraph(AGENTS, DESKS);
    const n = neighbourhood(g, "desk:records");
    // The orchestrator's `["*"]` allowlist genuinely reaches this desk, so it
    // belongs in the neighbourhood — the point of drawing the address space is
    // that it shows reach nobody has exercised yet.
    expect(n.nodes.map((x) => x.id).sort()).toEqual([
      "agent:orchestrator",
      "agent:scribe",
      "desk:records",
    ]);
    expect(n.edges.every((e) => e.from === "desk:records" || e.to === "desk:records")).toBe(true);
  });
});

describe("the board as a record of hand-offs", () => {
  it("reads a card's origin desk and its assignee as an edge", () => {
    // A card IS one part of the company handing work to another, and unlike a
    // tool-call frame it survives a reload and is not redacted.
    expect(
      boardObservations([
        { originChatId: "solvers", assignee: "records", updatedAt: 7 },
      ]),
    ).toEqual([
      { kind: "handed-off", from: "solvers", to: "records", via: "task", atMillis: 7 },
    ]);
  });

  it("skips a card nobody delegated", () => {
    // The board's own `+` button has no origin: an operator adding a card is not
    // the company delegating, and drawing it as one would inflate every graph.
    expect(boardObservations([{ assignee: "records" }])).toEqual([]);
    expect(boardObservations([{ originChatId: "solvers" }])).toEqual([]);
  });

  it("skips a card that landed where it was raised", () => {
    // A desk opening its own card is work, not a hand-off — and a self-loop is
    // an edge no layout can draw usefully.
    expect(
      boardObservations([{ originChatId: "solvers", assignee: "solvers" }]),
    ).toEqual([]);
  });

  it("draws a desk-to-desk hand-off between two desk nodes", () => {
    // Both endpoints resolve desk-first: guessing "agent" for a bare id would
    // draw an edge to a node that does not exist.
    const g = applyObservations(
      structuralGraph(AGENTS, DESKS),
      boardObservations([{ originChatId: "solvers", assignee: "records", updatedAt: 1 }]),
    );
    const handed = g.edges.find((e) => e.kind === "handed-off");
    expect(handed?.from).toBe("desk:solvers");
    expect(handed?.to).toBe("desk:records");
  });

  it("resolves an assignee that is a teammate, not a desk", () => {
    const g = applyObservations(
      structuralGraph(AGENTS, DESKS),
      boardObservations([{ originChatId: "solvers", assignee: "planner", updatedAt: 1 }]),
    );
    const handed = g.edges.find((e) => e.kind === "handed-off");
    expect(handed?.to).toBe("agent:planner");
  });
});
