import { describe, expect, it } from "vitest";

import { OpenCompanyClient } from "@/api/client";
import type { PatchTask, Task } from "@/api/tasks";
import type {
  StreamHandlers,
  Transport,
  TransportRequest,
  TransportResponse,
} from "@/api/transport";
import { ApiError, workflowRefusalProblems } from "@/api/types";
import { computeTaskPatch } from "@/lib/task-edit";
import { taskProposalDiff } from "@/lib/task-workflow-proposal";

/**
 * Issue #580 (Stage 2, frontend). Three decidable-without-a-host contracts:
 *
 * 1. the adapter that turns a card's STORED workflow proposal (a camelCase
 *    `WorkflowGraphSpec`) into the #415 review diff, or a stated refusal;
 * 2. the chat wire — a once-vs-workflow line omits `deliverable` for once and
 *    carries `"workflow"` when chosen, so an ordinary message is byte-identical
 *    to a pre-#580 one;
 * 3. the edit-dialog diff's deliverable arm — an untouched control emits no
 *    patch, a flip emits exactly `{ deliverable }`.
 *
 * Issue #1191 adds a fourth: the decision behind the panel's per-node breakdown
 * of a refused Apply. The markup that renders it is NOT covered — this runner is
 * for pure functions and the console has no component-test harness (no
 * testing-library, and `include` collects only `*.test.ts`). What is covered is
 * the branch that decides whether a breakdown exists at all, which is where the
 * mistakes live; the JSX beneath it mirrors `ProposalCard`'s block line for line.
 */

// ---------------------------------------------------------------------------
// 1. taskProposalDiff — the ops→diff adapter
// ---------------------------------------------------------------------------

/** A realistic built proposal: the camelCase graph the host stores on the card. */
function spec() {
  return {
    id: "weekly_summary",
    name: "Weekly summary",
    description: "Posts a weekly summary of dev activity.",
    nodes: [
      { id: "cron", kind: "trigger", name: "Weekly", schedule: "0 9 * * 1" },
      { id: "gather", kind: "agent", name: "Gather", agent: "analyst" },
      {
        id: "post",
        kind: "output",
        name: "Post",
        destination: { kind: "owner" },
        requiresApproval: true,
      },
    ],
    edges: [
      { from: "cron", to: "gather" },
      { from: "gather", to: "post" },
    ],
  };
}

describe("taskProposalDiff", () => {
  it("renders a realistic spec as an all-added diff with field payloads and edges", () => {
    const result = taskProposalDiff(spec());
    if ("reason" in result) throw new Error(`expected a diff, got: ${result.reason}`);
    const { diff } = result;

    // Every step is an addition — the proposal is a brand-new graph.
    expect(diff.nodes.map((n) => n.id).sort()).toEqual(["cron", "gather", "post"]);
    expect(diff.nodes.every((n) => n.change === "added")).toBe(true);

    // A new step carries its whole payload, not just its name (the #415
    // guarantee): the trigger's schedule and the output's approval flag are
    // reviewable rather than applied unseen.
    const cron = diff.nodes.find((n) => n.id === "cron")!;
    const cronFields = Object.fromEntries(cron.fields.map((f) => [f.field, f.after]));
    expect(cronFields).toMatchObject({ kind: "trigger", schedule: "0 9 * * 1" });

    const post = diff.nodes.find((n) => n.id === "post")!;
    const postFields = Object.fromEntries(post.fields.map((f) => [f.field, f.after]));
    expect(postFields).toMatchObject({ kind: "output", requiresApproval: "true" });

    // Both connections show as added.
    expect(diff.edges).toEqual([
      { from: "cron", to: "gather", change: "added" },
      { from: "gather", to: "post", change: "added" },
    ]);
  });

  it("refuses a spec with no steps rather than showing an empty diff", () => {
    const result = taskProposalDiff({ id: "x", name: "Y", nodes: [], edges: [] });
    expect(result).toHaveProperty("reason");
  });

  it("refuses a spec that reuses a step id", () => {
    const bad = {
      ...spec(),
      nodes: [
        // Coherent for its kind (issue #783 now checks that in the shared
        // `validateProposal`), so the id reuse is what this exercises — not a
        // missing `agent` on the first node.
        { id: "gather", kind: "agent", name: "Gather", agent: "analyst" },
        { id: "gather", kind: "output", name: "Duplicate" },
      ],
      edges: [],
    };
    const result = taskProposalDiff(bad);
    expect(result).toMatchObject({ reason: expect.stringContaining("gather") });
  });

  it("refuses an edge to a step that is not there", () => {
    const bad = {
      ...spec(),
      nodes: [{ id: "gather", kind: "agent", name: "Gather" }],
      edges: [{ from: "gather", to: "ghost" }],
    };
    const result = taskProposalDiff(bad);
    expect(result).toHaveProperty("reason");
  });

  it("refuses a step carrying a field the diff cannot render", () => {
    const bad = {
      ...spec(),
      nodes: [{ id: "gather", kind: "agent", name: "Gather", sudo: true }],
      edges: [],
    };
    const result = taskProposalDiff(bad);
    expect(result).toMatchObject({ reason: expect.stringContaining("sudo") });
  });

  it("refuses a non-object ops payload without crashing", () => {
    for (const junk of [null, undefined, "nope", 42, [1, 2, 3]]) {
      const result = taskProposalDiff(junk);
      expect(result).toHaveProperty("reason");
    }
  });
});

// ---------------------------------------------------------------------------
// 2. the chat wire — deliverable is omitted for once, carried for workflow
// ---------------------------------------------------------------------------

/** Records every request body so the test can read what reached the wire. */
class RecordingTransport implements Transport {
  readonly seen: TransportRequest[] = [];
  async request(req: TransportRequest): Promise<TransportResponse> {
    this.seen.push(req);
    return {
      status: 200,
      statusText: "",
      url: req.url,
      text: '{"responses":[]}',
      header: () => null,
    };
  }
  subscribe(_url: string, _handlers: StreamHandlers): () => void {
    return () => {};
  }
}

function bodyOf(req: TransportRequest): Record<string, unknown> {
  return req.body ? (JSON.parse(req.body) as Record<string, unknown>) : {};
}

describe("client.chat deliverable", () => {
  function clientOn(t: Transport) {
    return new OpenCompanyClient(
      { baseUrl: "", company: null, operatorToken: null, sessionHeader: null },
      t,
    );
  }

  it("omits deliverable for an ordinary (once) message, keeping the pre-#580 shape", async () => {
    const t = new RecordingTransport();
    await clientOn(t).chat("just a message");
    const body = bodyOf(t.seen[0]);
    expect(body).toEqual({ text: "just a message" });
    expect("deliverable" in body).toBe(false);
  });

  it("omits deliverable when the choice is explicitly once", async () => {
    const t = new RecordingTransport();
    await clientOn(t).chat("do it once", null, null, null, "once");
    expect("deliverable" in bodyOf(t.seen[0])).toBe(false);
  });

  it("carries deliverable:'workflow' when the operator chose to build one", async () => {
    const t = new RecordingTransport();
    await clientOn(t).chat("build me a weekly summary workflow", null, "main", null, "workflow");
    const body = bodyOf(t.seen[0]);
    expect(body.deliverable).toBe("workflow");
    // The other fields still ride the same body — the deliverable is additive.
    expect(body.chat).toBe("main");
    expect(body.text).toBe("build me a weekly summary workflow");
  });
});

// ---------------------------------------------------------------------------
// 3. the edit-dialog diff — deliverable normalization
// ---------------------------------------------------------------------------

function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "t1",
    title: "Weekly summary",
    column: "todo",
    priority: "medium",
    assignee: "",
    updatedAt: 0,
    ...overrides,
  };
}

/** The fully-seeded draft the dialog builds when a card opens (absent → once). */
function seededDraft(current: Task): PatchTask {
  return {
    title: current.title,
    note: current.note ?? "",
    column: current.column,
    priority: current.priority,
    assignee: current.assignee,
    deliverable: current.deliverable ?? "once",
  };
}

describe("computeTaskPatch deliverable", () => {
  it("emits no patch when the untouched draft matches the card", () => {
    const current = task({ deliverable: "workflow" });
    expect(computeTaskPatch(seededDraft(current), current)).toEqual({});
  });

  it("does not patch deliverable for a card with no stored value left untouched", () => {
    // task.deliverable is absent (== once); the seeded draft is normalized to
    // "once", so the two agree and nothing is sent.
    const current = task();
    expect("deliverable" in computeTaskPatch(seededDraft(current), current)).toBe(false);
  });

  it("patches exactly { deliverable } when flipped once → workflow", () => {
    const current = task(); // no stored deliverable → normalized "once"
    const draft = { ...seededDraft(current), deliverable: "workflow" as const };
    expect(computeTaskPatch(draft, current)).toEqual({ deliverable: "workflow" });
  });

  it("patches exactly { deliverable } when flipped workflow → once", () => {
    const current = task({ deliverable: "workflow" });
    const draft = { ...seededDraft(current), deliverable: "once" as const };
    expect(computeTaskPatch(draft, current)).toEqual({ deliverable: "once" });
  });
});

// ---------------------------------------------------------------------------
// 4. the refused-Apply breakdown — which refusals carry one (issue #1191)
// ---------------------------------------------------------------------------

describe("workflowRefusalProblems", () => {
  const problem = {
    node_id: "post_summary",
    field: "destination.target",
    message: "`engineering-desk` is not a workflow delivery channel",
  };

  it("returns the host's breakdown when the refusal carries one", () => {
    const e = new ApiError(400, "workflow_invalid", "…", true);
    e.problems = [problem];
    expect(workflowRefusalProblems(e)).toEqual([problem]);
  });

  it("returns null for a refusal the host sent no breakdown for", () => {
    // Every refusal that is not a workflow refusal: a 409 name clash, a 404.
    const e = new ApiError(409, "conflict", "A workflow named `x` already exists.", true);
    expect(workflowRefusalProblems(e)).toBeNull();
  });

  it("treats an empty array as no breakdown", () => {
    // An empty list would render as an empty bullet list under the sentence,
    // which reads as a rendering bug rather than as "nothing more to say".
    const e = new ApiError(400, "workflow_invalid", "…", true);
    e.problems = [];
    expect(workflowRefusalProblems(e)).toBeNull();
  });

  it("returns null when the failure never reached the host", () => {
    expect(workflowRefusalProblems(new Error("Failed to fetch"))).toBeNull();
    expect(workflowRefusalProblems(null)).toBeNull();
  });
});
