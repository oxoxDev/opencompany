import { describe, expect, it } from "vitest";

import type { Person as HostPerson } from "@/api/auth";
import type { WorkflowGraph } from "@/api/workflows";
import type { DeskDto } from "@/api/types";
import type { TeamMember } from "@/lib/team";
import {
  adapt,
  departmentsFromDesks,
  deskOf,
  DERIVED_NOTICE,
  NO_DESK_ID,
  orderStages,
} from "@/views/overview/kg/adapter";
import { buildKnowledgeGraph } from "@/views/overview/kg/model";
import { ownedBy } from "@/views/overview/pulse";

/**
 * The overview graph, drawn from the host rather than from a plausible story
 * (issues #601, #365).
 *
 * Three of the five rings used to be invented — a tool shelf dealt out of the
 * company-wide allow-list, five hard-coded departments keyword-matched from
 * role text, and one templated routine per department. Every assertion here
 * exists because the honest value and the invented one were indistinguishable
 * on screen: the graph looked exactly as convincing when it was making things
 * up, which is why it stayed wrong long enough to need an issue.
 */

function member(over: Partial<TeamMember> & Pick<TeamMember, "id" | "name">): TeamMember {
  return {
    role: "Engineer",
    description: "",
    tone: "sky",
    inboxEnabled: false,
    effectiveTools: [],
    desks: [],
    ...over,
  };
}

function desk(id: string, name: string): DeskDto {
  return { id, name, members: [] };
}

function flow(over: Partial<WorkflowGraph> & Pick<WorkflowGraph, "id" | "name">): WorkflowGraph {
  return { nodes: [], edges: [], ...over };
}

function person(id: string, email: string, role: "admin" | "member"): HostPerson {
  return {
    id,
    email,
    role,
    status: "active",
    hasPassword: true,
    mustChangePassword: false,
    createdAtMillis: 0,
  };
}

const NO_TASKS = { tasks: [], ownedBy };

describe("a teammate's tools", () => {
  it("are the grants the host resolved, verbatim", () => {
    const roster = [
      member({
        id: "ada",
        name: "Ada",
        effectiveTools: ["workspace.read", "composio"],
        desks: [{ id: "eng", name: "Engineering", lead: true }],
      }),
    ];

    const { agents } = adapt({
      members: roster,
      desks: [desk("eng", "Engineering")],
      workflows: [],
      people: [],
      ...NO_TASKS,
    });

    // Not a slice, not a deal, not a reordering: the same list, in the same
    // order, that `GET .../team/{id}` shows on the agent's own card.
    expect(agents[0].tools).toEqual(["workspace.read", "composio"]);
  });

  it("are empty when the host granted none, rather than filled from somewhere", () => {
    const { agents, toolLabels } = adapt({
      members: [member({ id: "ada", name: "Ada", effectiveTools: [] })],
      desks: [],
      workflows: [],
      people: [],
      ...NO_TASKS,
    });

    expect(agents[0].tools).toEqual([]);
    expect(toolLabels).toEqual({});
  });

  it("label themselves with the literal grant, so an operator can grep for it", () => {
    const { toolLabels } = adapt({
      members: [member({ id: "ada", name: "Ada", effectiveTools: ["workspace.*", "*"] })],
      desks: [],
      workflows: [],
      people: [],
      ...NO_TASKS,
    });

    expect(toolLabels).toEqual({ "workspace.*": "workspace.*", "*": "*" });
  });

  it("never reach a human — the host resolves grants per agent", () => {
    const { people } = adapt({
      members: [member({ id: "ada", name: "Ada", effectiveTools: ["composio"] })],
      desks: [],
      workflows: [],
      people: [person("u1", "sam@example.com", "admin")],
      ...NO_TASKS,
    });

    expect(people[0].tools).toEqual([]);
  });
});

describe("departments come from desks", () => {
  it("keeps host order and tints each pillar", () => {
    const departments = departmentsFromDesks([desk("eng", "Engineering"), desk("gtm", "Go to market")]);

    expect(departments.map((d) => [d.id, d.name, d.order])).toEqual([
      ["eng", "Engineering", 0],
      ["gtm", "Go to market", 1],
    ]);
    expect(new Set(departments.map((d) => d.color)).size).toBe(2);
  });

  it("stands a teammate on its first desk", () => {
    const ada = member({
      id: "ada",
      name: "Ada",
      desks: [
        { id: "eng", name: "Engineering", lead: true },
        { id: "gtm", name: "Go to market", lead: false },
      ],
    });
    expect(deskOf(ada)).toBe("eng");
  });

  it("draws only the desks somebody actually sits on", () => {
    const { departments } = adapt({
      members: [
        member({ id: "ada", name: "Ada", desks: [{ id: "eng", name: "Engineering", lead: true }] }),
      ],
      desks: [desk("eng", "Engineering"), desk("gtm", "Go to market")],
      workflows: [],
      people: [],
      ...NO_TASKS,
    });

    expect(departments.map((d) => d.id)).toEqual(["eng"]);
  });

  it("does not read anything into a role string", () => {
    // Under the old keyword matcher this roster split across four invented
    // pillars — Design, Engineering, Growth, Operations — none of which the
    // company had ever declared.
    const roster = [
      member({ id: "a", name: "A", role: "Product Designer" }),
      member({ id: "b", name: "B", role: "Backend Engineer" }),
      member({ id: "c", name: "C", role: "Growth Marketer" }),
      member({ id: "d", name: "D", role: "Support Specialist" }),
    ];

    const { departments, agents } = adapt({
      members: roster,
      desks: [],
      workflows: [],
      people: [],
      ...NO_TASKS,
    });

    expect(departments.map((d) => d.id)).toEqual([NO_DESK_ID]);
    expect(new Set(agents.map((a) => a.departmentId))).toEqual(new Set([NO_DESK_ID]));
  });

  it("gives a company with no desks one stated pillar rather than invented ones", () => {
    const { departments } = adapt({
      members: [member({ id: "ada", name: "Ada" })],
      desks: [],
      workflows: [],
      people: [],
      ...NO_TASKS,
    });

    expect(departments).toHaveLength(1);
    expect(departments[0].id).toBe(NO_DESK_ID);
    expect(departments[0].name).toBe("No desk");
  });

  it("puts every human on the no-desk pillar, admin or not", () => {
    const { people, departments } = adapt({
      members: [member({ id: "ada", name: "Ada", desks: [{ id: "eng", name: "Eng", lead: true }] })],
      desks: [desk("eng", "Eng")],
      workflows: [],
      people: [person("u1", "sam@example.com", "admin"), person("u2", "rio@example.com", "member")],
      ...NO_TASKS,
    });

    // Signing in says who somebody is, not what they work on — so both land in
    // the same honest group instead of being spread across the desks.
    expect(people.map((p) => p.departmentId)).toEqual([NO_DESK_ID, NO_DESK_ID]);
    expect(departments.map((d) => d.id)).toEqual(["eng", NO_DESK_ID]);
  });

  it("files a card under its owner's desk", () => {
    const ada = member({
      id: "ada",
      name: "Ada",
      desks: [{ id: "eng", name: "Engineering", lead: true }],
    });

    const { tasks } = adapt({
      members: [ada],
      desks: [desk("eng", "Engineering")],
      workflows: [],
      people: [],
      ownedBy,
      tasks: [
        {
          id: "t1",
          title: "Ship it",
          column: "doing",
          priority: "high",
          assignee: "ada",
          updatedAt: 0,
        },
      ],
    });

    expect(tasks).toHaveLength(1);
    expect(tasks[0].departmentId).toBe("eng");
  });
});

describe("orderStages", () => {
  it("reads the flow in edge order, not declaration order", () => {
    const stages = orderStages({
      nodes: [
        { id: "c", kind: "output", name: "Report" },
        { id: "a", kind: "trigger", name: "Every morning" },
        { id: "b", kind: "agent", name: "Draft", agent: "ada" },
      ],
      edges: [
        { from: "a", to: "b" },
        { from: "b", to: "c" },
      ],
    });

    expect(stages.map((s) => s.name)).toEqual(["Every morning", "Draft", "Report"]);
  });

  it("names the agent the flow itself names, and nobody else", () => {
    const stages = orderStages({
      nodes: [
        { id: "a", kind: "trigger", name: "Every morning" },
        { id: "b", kind: "agent", name: "Draft", agent: "ada" },
        // An `agent` field on a non-agent node is not a performer.
        { id: "c", kind: "http_request", name: "Post it", agent: "grace" },
      ],
      edges: [
        { from: "a", to: "b" },
        { from: "b", to: "c" },
      ],
    });

    expect(stages.map((s) => s.agentId)).toEqual([undefined, "ada", undefined]);
  });

  it("breaks a fan-out tie by declared order, so the same graph always reads the same", () => {
    const stages = orderStages({
      nodes: [
        { id: "root", kind: "trigger", name: "Start" },
        { id: "x", kind: "agent", name: "X", agent: "ada" },
        { id: "y", kind: "agent", name: "Y", agent: "grace" },
      ],
      edges: [
        { from: "root", to: "y" },
        { from: "root", to: "x" },
      ],
    });

    expect(stages.map((s) => s.name)).toEqual(["Start", "X", "Y"]);
  });

  it("keeps every node when the graph has a cycle", () => {
    const stages = orderStages({
      nodes: [
        { id: "a", kind: "agent", name: "A" },
        { id: "b", kind: "agent", name: "B" },
      ],
      edges: [
        { from: "a", to: "b" },
        { from: "b", to: "a" },
      ],
    });

    expect(stages.map((s) => s.name).sort()).toEqual(["A", "B"]);
  });

  it("is not confused by a repeated edge", () => {
    const stages = orderStages({
      nodes: [
        { id: "a", kind: "trigger", name: "A" },
        { id: "b", kind: "agent", name: "B" },
      ],
      edges: [
        { from: "a", to: "b" },
        { from: "a", to: "b" },
      ],
    });

    expect(stages.map((s) => s.name)).toEqual(["A", "B"]);
  });
});

describe("workflows", () => {
  it("are the company's saved graphs, not templates", () => {
    const { workflows } = adapt({
      members: [
        member({ id: "ada", name: "Ada", desks: [{ id: "eng", name: "Eng", lead: true }] }),
      ],
      desks: [desk("eng", "Eng")],
      workflows: [
        flow({
          id: "wf-nightly",
          name: "Nightly digest",
          description: "Summarise the day.",
          nodes: [
            { id: "t", kind: "trigger", name: "22:00 UTC" },
            { id: "w", kind: "agent", name: "Write it", agent: "ada" },
          ],
          edges: [{ from: "t", to: "w" }],
        }),
      ],
      people: [],
      ...NO_TASKS,
    });

    expect(workflows).toHaveLength(1);
    expect(workflows[0].name).toBe("Nightly digest");
    expect(workflows[0].summary).toBe("Summarise the day.");
    expect(workflows[0].stages.map((s) => s.name)).toEqual(["22:00 UTC", "Write it"]);
    // Placement — the one thing this console decides. `ada` sits on `eng`.
    expect(workflows[0].departmentId).toBe("eng");
  });

  it("hang off the no-desk pillar when they run through nobody seated", () => {
    const { workflows } = adapt({
      members: [member({ id: "ada", name: "Ada" })],
      desks: [],
      workflows: [
        flow({
          id: "wf-ping",
          name: "Ping",
          nodes: [{ id: "h", kind: "http_request", name: "Call the API" }],
        }),
      ],
      people: [],
      ...NO_TASKS,
    });

    expect(workflows[0].departmentId).toBe(NO_DESK_ID);
  });

  it("draw a stage only to the teammate the flow names", () => {
    const agents = [
      { id: "ada", departmentId: "eng", name: "Ada", role: "Eng", status: "active" as const, tier: "worker" as const, description: "", model: "—", tools: [], parentId: null, instance: "builtin" },
      { id: "grace", departmentId: "eng", name: "Grace", role: "Eng", status: "active" as const, tier: "worker" as const, description: "", model: "—", tools: [], parentId: null, instance: "builtin" },
    ];
    const graph = buildKnowledgeGraph(
      agents,
      [{ id: "eng", name: "Eng", slug: "eng", tagline: "", color: "#000", order: 0 }],
      [],
      [],
      [
        {
          id: "wf",
          departmentId: "eng",
          name: "Flow",
          summary: "",
          stages: [
            { name: "Start" },
            { name: "Draft", agentId: "ada" },
            // Names a teammate who is not on the roster: no edge, rather than a
            // link to a node that was never created.
            { name: "Ghost step", agentId: "nobody" },
          ],
        },
      ],
    );

    const runs = graph.edges.filter((e) => e.kind === "runs");
    // Under the old round-robin every stage got an edge, dealt across the
    // department — including the two the flow never handed to anyone.
    expect(runs).toEqual([{ source: "step:wf:1", target: "emp:ada", kind: "runs" }]);
    expect(runs.some((e) => e.target === "emp:grace")).toBe(false);
  });
});

describe("DERIVED_NOTICE", () => {
  it("names flow placement and no longer claims departments or tools are invented", () => {
    expect(DERIVED_NOTICE).toMatch(/workflow/i);
    // The acceptance criterion for #601, asserted rather than assumed: the
    // notice must not still be telling operators that rings the host now
    // answers for are placeholders.
    expect(DERIVED_NOTICE).not.toMatch(/placeholder/i);
    expect(DERIVED_NOTICE).not.toMatch(/tool assignments/i);
    expect(DERIVED_NOTICE).not.toMatch(/departments,/i);
  });
});
