import { describe, expect, it } from "vitest";

import { foldLiveFrame, type LiveRow } from "@/lib/live-frame";

/**
 * What a live row is allowed to lose on the way in from the wire.
 *
 * The answer is nothing. `StepTimeline` renders one set of rows whether they
 * arrived live or folded onto the reply, so a field the fold drops is a field
 * the timeline can only show *after* the turn ends — which for every state in
 * this file is after the point the operator could have acted on it.
 *
 * Three states were being dropped:
 *
 * 1. **`awaiting_approval`.** The fold tested `status === "error"` and sent
 *    everything else to `ok`, so a call parked on a sign-off rendered as one
 *    that had succeeded, for the whole time it sat waiting. It then flipped to
 *    "awaiting approval · didn't run" when the reply landed. `StepTimeline`
 *    has carried the entire parked treatment since #411 — the amber chip, the
 *    forced auto-expand, the honest duration — and none of it was reachable
 *    while it mattered.
 * 2. **`failure`.** On the wire since #411 and simply undeclared console-side,
 *    so a live row could not wear the chip naming *why* a call failed.
 * 3. **`truncated`.** Same, for #410's "result cut" marker.
 *
 * The fourth test covers a different loss: repeated thinking frames stacking
 * into a run of identical rows.
 */
describe("a completion's typed state survives the fold", () => {
  const started: LiveRow[] = [
    { kind: "tool_call", status: "running", label: "Fetch the roster", toolCallId: "c1" },
  ];

  it("keeps a parked call parked instead of reading as succeeded", () => {
    const rows = foldLiveFrame(started, {
      type: "tool_result",
      toolCallId: "c1",
      status: "awaiting_approval",
    });

    // The whole point: not `ok`. A row that says `ok` here is a row claiming a
    // gated call ran.
    expect(rows?.[0].status).toBe("awaiting_approval");
  });

  it("carries the typed failure so the live row can name the cause", () => {
    const rows = foldLiveFrame(started, {
      type: "tool_result",
      toolCallId: "c1",
      status: "error",
      failure: "declined",
    });

    expect(rows?.[0].status).toBe("error");
    expect(rows?.[0].failure).toBe("declined");
  });

  it("carries the cut marker", () => {
    const rows = foldLiveFrame(started, {
      type: "tool_result",
      toolCallId: "c1",
      status: "ok",
      truncated: true,
    });

    expect(rows?.[0].truncated).toBe(true);
  });

  it("sends an unknown status word to ok rather than throwing", () => {
    // A host newer than this console is a thing that happens. A row that reads
    // as finished is a better failure than a timeline that does not render.
    const rows = foldLiveFrame(started, {
      type: "tool_result",
      toolCallId: "c1",
      status: "something_new",
    });

    expect(rows?.[0].status).toBe("ok");
  });

  it("keeps the typed state on a completion with no observed start", () => {
    // The unpaired arm builds a fresh row rather than updating one, so it has
    // its own chance to drop these.
    const rows = foldLiveFrame([], {
      type: "tool_result",
      status: "awaiting_approval",
      label: "Send the invoice",
      truncated: true,
    });

    expect(rows?.[0].status).toBe("awaiting_approval");
    expect(rows?.[0].truncated).toBe(true);
  });
});

describe("thinking runs do not stack", () => {
  it("collapses a repeated thinking frame onto the trailing row", () => {
    const first = foldLiveFrame([], { type: "thinking" });
    expect(first).toHaveLength(1);

    // `null` is the fold's "drop this frame" answer, which lets the caller keep
    // the previous array identity and skip a re-render.
    expect(foldLiveFrame(first ?? [], { type: "thinking" })).toBeNull();
  });

  it("opens a second thinking row once a tool call separates the runs", () => {
    // A turn that thinks, calls a tool, then thinks again genuinely thought
    // twice — and the tool call in between is what says so.
    let rows = foldLiveFrame([], { type: "thinking" }) ?? [];
    rows = foldLiveFrame(rows, { type: "tool_call", toolCallId: "c1", label: "Search" }) ?? rows;
    rows = foldLiveFrame(rows, { type: "tool_result", toolCallId: "c1", status: "ok" }) ?? rows;
    rows = foldLiveFrame(rows, { type: "thinking" }) ?? rows;

    expect(rows.map((r) => r.kind)).toEqual(["thinking", "tool_call", "thinking"]);
  });
});
