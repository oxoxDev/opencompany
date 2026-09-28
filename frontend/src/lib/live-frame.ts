import type { TurnStep, TurnStepFailure, TurnStepStatus } from "@/api/types";

/** A live row: a {@link TurnStep} plus the transient key that pairs a result to
 * its call so the row flips `running → ok/error` in place. */
export type LiveRow = TurnStep & { toolCallId?: string };

/** The live-frame shape this fold reads — the fields common to `tool_call`,
 * `tool_result` and `thinking` on {@link CompanyStreamEvent}. */
export interface LiveFrame {
  type: "tool_call" | "tool_result" | "thinking";
  toolCallId?: string;
  label?: string;
  detail?: string;
  result?: string;
  /**
   * The typed reason the call did not succeed (issue #411), carried so a live
   * row can wear the same chip the folded step does. A live row that omitted it
   * announced the failure only once the reply landed — which is after the point
   * the operator could have acted on it.
   */
  failure?: TurnStepFailure;
  /** The result was cut before the agent could read all of it (issue #410). */
  truncated?: boolean;
  status?: string;
  elapsedMs?: number;
}

/**
 * The status word a completion reports, as the typed state it names.
 *
 * `awaiting_approval` is the reason this is a lookup rather than an
 * `=== "error"` test. The host has published that word since #411 —
 * `TurnStepStatus::wire_word` is the single source both the live frame and the
 * folded step read — and collapsing it to `ok` made a call **parked on a
 * sign-off render as one that succeeded**, for the whole time it sat waiting.
 * It then flipped to "awaiting approval · didn't run" when the reply landed, so
 * the one state the operator could have acted on was the one state the live
 * timeline could not show.
 *
 * Unknown words fall to `ok` rather than throwing: a host newer than this
 * console is a thing that happens, and a row that reads as finished is a better
 * failure than a row that takes the timeline down.
 */
function completedStatus(word: string | undefined): TurnStepStatus {
  switch (word) {
    case "error":
      return "error";
    case "awaiting_approval":
      return "awaiting_approval";
    default:
      return "ok";
  }
}

/**
 * Folds one live frame into a turn's rows, or returns `null` to drop it.
 *
 * Extracted from `AppShell.onTurnEvent` so the two maps that hold live rows —
 * `liveStepsByThread` and `liveStepsByMessage` — fold identically. A second
 * copy is how the two would drift, and a drifted fold is invisible: both keep
 * rendering, just differently.
 *
 * It also exists so the test can call the rule instead of restating it. The
 * review on #2068 caught exactly that: `keyFor` duplicated `onTurnEvent`'s
 * conditional, so it would have kept passing through a regression in the branch
 * the shell actually runs.
 *
 * `null` rather than an unchanged array is the "drop this frame" answer, so a
 * caller can bail out of its `setState` with the previous object identity and
 * let React skip the re-render.
 */
export function foldLiveFrame(rows: readonly LiveRow[], frame: LiveFrame): LiveRow[] | null {
  const next = [...rows];
  if (frame.type === "tool_call") {
    const idx = frame.toolCallId
      ? next.findIndex((r) => r.toolCallId === frame.toolCallId)
      : -1;
    const row = {
      kind: "tool_call" as const,
      status: "running" as const,
      label: frame.label ?? "Working",
      toolCallId: frame.toolCallId,
    };
    if (idx >= 0) next[idx] = { ...next[idx], ...row };
    else next.push(row);
    return next;
  }
  if (frame.type === "tool_result") {
    let idx = frame.toolCallId
      ? next.findIndex((r) => r.toolCallId === frame.toolCallId)
      : -1;
    // A result whose call is not in these rows belongs to another bucket.
    // Dropping it is deliberate: adopting it would invent a row with no start,
    // and pairing it with an unrelated `running` row would mark the wrong call
    // finished. The keying above is what keeps this from happening.
    if (idx < 0 && frame.toolCallId) return null;
    if (idx < 0) idx = next.findIndex((r) => r.status === "running");
    const status = completedStatus(frame.status);
    if (idx >= 0) {
      next[idx] = {
        ...next[idx],
        status,
        // The typed state and the cut marker ride along with the status, for
        // the reason `result` does: the live row and the folded step that
        // replaces it must not say different amounts about one call.
        failure: frame.failure ?? next[idx].failure,
        truncated: frame.truncated ?? next[idx].truncated,
        detail: frame.detail ?? next[idx].detail,
        // `result` is what came back — the summary `StepTimeline` renders under
        // the label. Carried for the same reason `detail` is: the live row and
        // the folded step it is replaced by should not say different amounts
        // about the same call. It was dropped while only the built-in harness
        // streamed (its rows lean on `detail`, derived from the arguments); an
        // ACP tool call carries its summary in `result` and nothing else, so a
        // dropped `result` is the whole of what the row could have said.
        result: frame.result ?? next[idx].result,
        elapsedMs: frame.elapsedMs,
      };
    } else {
      next.push({
        kind: "tool_call",
        status,
        label: frame.label ?? "Working",
        detail: frame.detail,
        result: frame.result,
        failure: frame.failure,
        truncated: frame.truncated,
        elapsedMs: frame.elapsedMs,
        toolCallId: frame.toolCallId,
      });
    }
    return next;
  }
  // A thinking run, coalesced by the backend into one frame per run.
  //
  // Collapsed onto a trailing thinking row rather than always appended. The
  // host coalesces *within* a run, but a turn that thinks, calls a tool and
  // thinks again emits one frame per run — and a reconnecting `EventSource`
  // can replay frames it already delivered. Appending unconditionally turned
  // both into a stack of identical "Thinking" entries with nothing between
  // them, which says a turn thought four times where it thought once.
  //
  // Only a *trailing* row collapses: a thinking run separated from this one by
  // a tool call is a genuinely distinct row, and the tool call in between is
  // what says so.
  const last = next[next.length - 1];
  if (last?.kind === "thinking") return null;
  next.push({ kind: "thinking", status: "ok", label: "Thinking" });
  return next;
}
