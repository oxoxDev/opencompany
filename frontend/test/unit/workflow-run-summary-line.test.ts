import { describe, expect, it } from "vitest";

import type { DeliveryReport, WorkflowRunOutcome } from "@/api/workflows";
import { runSummaryLine, runTone } from "@/views/workflows/run-health";

/**
 * The workflow list's status column, as a string (issue #1136).
 *
 * The column is a fixed 13rem — that is what puts the state dots in a vertical
 * line down the page — so everything the card says beside its dot has to fit in
 * one truncating sentence instead of a row of badges that cannot shrink. These
 * pin what that sentence is allowed to lose and what it must never repeat.
 */

/** A settled manual run with nothing wrong with it. */
function run(over: Partial<WorkflowRunOutcome> = {}): WorkflowRunOutcome {
  return {
    seq: 1,
    atMillis: 1_700_000_000_000,
    workflowId: "feature_pipeline",
    scheduled: false,
    runId: "run-1",
    deliveries: [],
    pendingApprovals: [],
    ...over,
  };
}

/** One delivery row. `status` is widened because `pending` reaches the console
 * from the host before this union names it — see `PENDING_STATUS`. */
function delivery(status: string, kind: string): DeliveryReport {
  return { node: "publish", kind, status: status as DeliveryReport["status"], detail: "", reason: "" };
}

/** The line as the row builds it: `runTone`'s label, and its failed node. */
function line(r: WorkflowRunOutcome, failedNode?: string | null): string {
  return runSummaryLine(r, runTone(r).label, failedNode);
}

describe("runSummaryLine", () => {
  it("names who started the run, and how it went", () => {
    expect(line(run())).toBe("Manual run ok");
    expect(line(run({ scheduled: true }))).toBe("Scheduled run ok");
  });

  it("names the node a failed run stopped at", () => {
    expect(line(run({ error: "boom" }), "draft")).toBe(
      "Manual run failed at “draft”",
    );
  });

  it("carries a failure with no known node without an empty quote", () => {
    expect(line(run({ error: "boom" }), null)).toBe("Manual run failed");
  });

  // The whole reason this is a function and not a template in the row: the
  // naive join reads "not delivered · 2 not delivered", because `runTone`
  // already reports the undelivered reports AS the state.
  it("brackets the count when the state is the thing being counted", () => {
    const r = run({
      deliveries: [
        delivery("failed", "email"),
        delivery("failed", "slack"),
      ],
    });
    expect(runTone(r).label).toBe("not delivered");
    expect(line(r)).toBe("Manual run not delivered (2)");
  });

  it("brackets a waiting-approval count the same way", () => {
    const r = run({ deliveries: [delivery("pending", "email")] });
    expect(runTone(r).label).toBe("awaiting approval");
    expect(line(r)).toBe("Manual run awaiting approval (1)");
  });

  // A failure outranks both delivery readings in `runTone`, so here the counts
  // are facts the state does NOT carry — and they get spelled out.
  it("spells the counts out when the state is something else", () => {
    const r = run({
      error: "boom",
      deliveries: [
        delivery("failed", "email"),
        delivery("pending", "slack"),
      ],
    });
    expect(line(r, "draft")).toBe(
      "Manual run failed at “draft” · 1 not delivered · 1 awaiting approval",
    );
  });

  it("says nothing about deliveries that landed", () => {
    const r = run({
      deliveries: [
        delivery("sent", "email"),
        delivery("sent", "slack"),
      ],
    });
    expect(line(r)).toBe("Manual run ok");
  });
});
