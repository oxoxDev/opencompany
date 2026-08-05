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
};

/** Badge tint per run state — read alongside the ring, never instead of it, so
 * the state does not rely on colour alone. */
const RUN_STATE_BADGE: Record<NodeRunState, string> = {
  running: "bg-sky-500/15 text-sky-700 dark:text-sky-300",
  ok: "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300",
  error: "bg-red-500/15 text-red-700 dark:text-red-300",
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
      <Handle type="target" position={Position.Left} className="!size-2 !border-2 !bg-background" />
      <div className={cn("flex items-center gap-2 rounded-t-[10px] px-3 py-2", colors.chip)}>
        <span className="text-base leading-none" aria-hidden>
          {d.emoji}
        </span>
        <div className="min-w-0 flex-1 truncate text-sm font-semibold">{d.name}</div>
        {runState && (
          <span
            className={cn(
              "shrink-0 rounded px-1.5 py-0.5 text-[9px] font-medium uppercase tracking-wide",
              RUN_STATE_BADGE[runState],
            )}
            // "running" is a derived frontier, not something the engine reports.
            // Saying so on hover is cheaper than being subtly wrong in silence.
            title={
              runState === "running"
                ? "Reached this node — the engine reports nodes only as they finish, so this is where the run should be now."
                : runState === "error"
                  ? "This node failed."
                  : "This node finished."
            }
          >
            {RUN_STATE_LABEL[runState]}
          </span>
        )}
      </div>
      <div className="px-3 py-2 text-[11px] leading-snug text-muted-foreground">
        {d.summary}
        {typeof d.elapsedMs === "number" && (
          <span className="ml-1 font-mono opacity-70">· {formatElapsed(d.elapsedMs)}</span>
        )}
      </div>
      <Handle type="source" position={Position.Right} className="!size-2 !border-2 !bg-background" />
    </div>
  );
}

/** A compact duration: sub-second in ms, else seconds to one decimal. */
function formatElapsed(ms: number): string {
  return ms < 1000 ? `${ms}ms` : `${(ms / 1000).toFixed(1)}s`;
}
