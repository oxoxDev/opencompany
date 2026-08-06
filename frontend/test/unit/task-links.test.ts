import { describe, expect, it } from "vitest";

import { extraOutputCount, primaryLink, readTaskFocus } from "@/lib/task-output";
import type { Task, TaskOutput } from "@/api/tasks";

/**
 * Link precedence on a task card (issue #339).
 *
 * The precedence — artifact, else workflow, else the trace — is derived on
 * every render rather than stored, so getting it wrong shows up as a card
 * pointing at the *wrong* thing rather than at nothing. A card that opens a run
 * trace when it should open the deliverable looks completely normal.
 */

function task(over: Partial<Task> = {}): Task {
  return {
    id: "t-1",
    title: "Ship it",
    column: "done",
    priority: "medium",
    assignee: "ada",
    updatedAt: 0,
    ...over,
  };
}

function output(over: Partial<TaskOutput> = {}): TaskOutput {
  return { runId: "run-9", atMillis: 0, ...over };
}

describe("primaryLink", () => {
  it("prefers an artifact over everything else", () => {
    const link = primaryLink(
      task({
        output: output({
          artifacts: [{ artifactId: "a-1", version: 3, title: "Launch post", kind: "markdown" }],
          workflows: [{ workflowId: "w-1", action: "ran", runId: "run-9" }],
        }),
      }),
    );

    expect(link.kind).toBe("artifact");
    // Pinned at the revision this run wrote, not "latest".
    expect(link.href).toBe("#/tasks/t-1?artifact=a-1&v=3");
  });

  it("falls to the workflow when there is no artifact", () => {
    const link = primaryLink(
      task({ output: output({ workflows: [{ workflowId: "w-1", action: "ran", runId: "run-9" }] }) }),
    );
    expect(link.kind).toBe("workflow");
    expect(link.href).toBe("#/workflows/w-1?run=run-9");
  });

  it("opens the workflow with no run overlay when it was authored, not executed", () => {
    const link = primaryLink(
      task({ output: output({ workflows: [{ workflowId: "w-1", action: "created" }] }) }),
    );
    expect(link.href).toBe("#/workflows/w-1");
  });

  it("falls to the run trace when the attempt produced no deliverable", () => {
    // Not an absence — for a message sent or a question answered, the trace IS
    // the deliverable. This is what stops "no artifact" degrading to "no link".
    const link = primaryLink(task({ output: output({ attempt: 2 }) }));
    expect(link.kind).toBe("trace");
    expect(link.href).toBe("#/tasks/t-1?run=run-9");
    expect(link.label).toContain("attempt 2");
  });

  it("falls back to the card itself when nothing recorded an attempt", () => {
    const link = primaryLink(task());
    expect(link.kind).toBe("card");
    expect(link.href).toBe("#/tasks/t-1");
  });

  it("escapes a task id that would otherwise break the address", () => {
    const link = primaryLink(task({ id: "t/1 2" }));
    expect(link.href).toBe("#/tasks/t%2F1%202");
  });
});

describe("extraOutputCount", () => {
  it("never counts the trace, which every stamped card has", () => {
    // Counting it would put "+1 more" on every card and mean nothing.
    expect(extraOutputCount(task({ output: output() }))).toBe(0);
  });

  it("counts only deliverables beyond the primary one", () => {
    const two = task({
      output: output({
        artifacts: [
          { artifactId: "a-1", version: 1, title: "One", kind: "markdown" },
          { artifactId: "a-2", version: 1, title: "Two", kind: "markdown" },
        ],
        workflows: [{ workflowId: "w-1", action: "ran" }],
      }),
    });
    expect(extraOutputCount(two)).toBe(2);
  });

  it("is zero for a card with no stamp", () => {
    expect(extraOutputCount(task())).toBe(0);
  });
});

describe("readTaskFocus", () => {
  it("reads an artifact and its pinned revision", () => {
    expect(readTaskFocus("#/tasks/t-1?artifact=a-1&v=3")).toEqual({
      artifactId: "a-1",
      version: 3,
    });
  });

  it("drops a version that is not a positive integer rather than guessing", () => {
    // Landing on the newest revision is the same place no pin at all would
    // land, which is the sensible destination for a stale link.
    expect(readTaskFocus("#/tasks/t-1?artifact=a-1&v=nonsense")).toEqual({ artifactId: "a-1" });
    expect(readTaskFocus("#/tasks/t-1?artifact=a-1&v=0")).toEqual({ artifactId: "a-1" });
    expect(readTaskFocus("#/tasks/t-1?artifact=a-1&v=-2")).toEqual({ artifactId: "a-1" });
  });

  it("reads a run focus", () => {
    expect(readTaskFocus("#/tasks/t-1?run=run-9")).toEqual({ runId: "run-9" });
  });

  it("yields an empty focus for an address that asks for nothing", () => {
    expect(readTaskFocus("#/tasks/t-1")).toEqual({});
    expect(readTaskFocus("#/tasks/t-1?")).toEqual({});
  });
});
