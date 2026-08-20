import { Handle, type NodeProps, Position } from "@xyflow/react";

import {
  COLOR_CLASSES,
  RUN_STATE_CLASSES,
  type NodeRunState,
  type WorkflowNodeData,
} from "@/lib/workflow-sample";
import { cn } from "@/lib/utils";

/** The word shown on a node's run-state badge. */
const RUN_STATE_LABEL: Record<NodeRunState, string> = {
  running: "running",
  ok: "done",
  error: "failed",
  // Issue #881: the word an operator can act on. Not "failed" — nothing broke —
  // and not "done", which is precisely the claim this state exists to stop.
  blocked: "needs approval",
};

/** Badge tint per run state — read alongside the ring, never instead of it, so
 * the state does not rely on colour alone. */
const RUN_STATE_BADGE: Record<NodeRunState, string> = {
  running: "bg-status-running-soft text-status-running-text",
  ok: "bg-status-done-soft text-status-done-text",
  error: "bg-status-failed-soft text-status-failed-text",
  blocked: "bg-status-blocked-soft text-[var(--status-blocked-text)]",
};

/** A custom xyflow node: emoji + colored header, name, and a one-line summary.
 *
 * Issue #371 layers a run state on top: a ring around the card and a word in
 * the header, so an operator watching a run can see where it has got to. The
 * resting node — no run being shown — renders exactly as it did before. */
export function WorkflowNode({ data, selected }: NodeProps) {
  const d = data as WorkflowNodeData;
  const colors = COLOR_CLASSES[d.color];
  const runState = d.runState;
  return (
    <div
      className={cn(
        "min-w-[180px] max-w-[240px] rounded-xl border-2 bg-card shadow-sm transition-shadow",
        colors.border,
        runState && RUN_STATE_CLASSES[runState],
        selected && "ring-2 ring-primary/40",
      )}
      data-run-state={runState}
    >
      <Handle
        type="target"
        position={Position.Left}
        className="!size-2 !border-2 !bg-background"
      />
      <div
        className={cn(
          "flex items-center gap-2 rounded-t-[10px] px-3 py-2",
          colors.chip,
        )}
      >
        <span className="text-base leading-none" aria-hidden>
          {d.emoji}
        </span>
        <div className="min-w-0 flex-1 truncate text-sm font-semibold">
          {d.name}
        </div>
        {runState && (
          <span
            className={cn(
              "shrink-0 rounded px-1.5 py-0.5 text-3xs font-medium uppercase tracking-wide",
              RUN_STATE_BADGE[runState],
            )}
            // "running" is now reported by the engine (issue #382), not a
            // derived frontier — the node really is executing when this shows.
            // Issue #981: the last arm used to say "This node finished." on a
            // node whose report was refused, which is true and reads as a
            // promise it cannot make. It now says what it is actually a verdict
            // on, and the strip below says the rest.
            title={
              runState === "running"
                ? "This node is executing now."
                : runState === "error"
                  ? "This node failed."
                  : d.reportUndelivered
                    ? "This step ran. Its report did not go out."
                    : "This node finished."
            }
          >
            {RUN_STATE_LABEL[runState]}
          </span>
        )}
      </div>
      <div className="px-3 py-2 text-2xs leading-snug text-muted-foreground">
        {d.summary}
        {typeof d.elapsedMs === "number" && (
          <span className="ml-1 font-mono opacity-70">
            · {formatElapsed(d.elapsedMs)}
          </span>
        )}
      </div>
      {/* Issue #981. The complaint was a run reading `undelivered` whose output
          node painted DONE, green, and said nothing else — so the truth lived
          only in a banner somewhere off the canvas. The ring and the badge above
          are unchanged, because they answer whether the STEP ran and it did;
          this answers what became of its report, in its own words, on the same
          card. Read together they are two facts, not a contradiction. */}
      {d.reportUndelivered && (
        <div
          className="flex items-center gap-1.5 rounded-b-[10px] border-t border-status-failed/40 bg-status-failed-soft px-3 py-1 text-3xs font-medium text-[var(--status-failed-text)]"
          data-testid="workflow-node-undelivered"
          title="This step ran. Its report did not go out — open the run in History for the reason."
        >
          <span className="size-1.5 shrink-0 rounded-full bg-status-failed" />
          report not delivered
        </div>
      )}
      <Handle
        type="source"
        position={Position.Right}
        className="!size-2 !border-2 !bg-background"
      />
    </div>
  );
}

/** A compact duration: sub-second in ms, else seconds to one decimal. */
function formatElapsed(ms: number): string {
  return ms < 1000 ? `${ms}ms` : `${(ms / 1000).toFixed(1)}s`;
}
