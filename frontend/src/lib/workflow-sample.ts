// Presentation metadata for the workflow canvas, keyed by the tinyflows node
// kinds (the six originals — trigger / agent / tool_call / http_request /
// condition / output — plus the P2 catalog: switch / merge / split_out /
// transform / output_parser / sub_workflow). The live graph comes from the host
// (`@/api/workflows`); this module only maps each kind to its emoji + accent so
// `WorkflowsView` and `WorkflowNode` render it.

export type NodeColor = "primary" | "sage" | "amber" | "coral" | "neutral";

export interface WorkflowNodeData extends Record<string, unknown> {
  kind: string;
  name: string;
  summary: string;
  emoji: string;
  color: NodeColor;
  /**
   * How this node fared in the run being shown (issue #371) — the live one, or
   * a past run overlaid from the history panel. Absent when no run is being
   * shown, which is the resting state and must look exactly as it did before.
   */
  runState?: NodeRunState;
  /** Wall-clock duration of the node's execution, once it has finished. */
  elapsedMs?: number;
  /**
   * Whether the report this `output` node produced did NOT go out (issue #981).
   *
   * A field of its own rather than a fourth {@link NodeRunState}, because it is
   * not the same question. `runState` is the engine's verdict on the step, and
   * for a dropped report the honest answer is `ok`: delivery is post-engine, the
   * node ran, and its work stands. Widening the run-state union would have made
   * one ring colour encode two subsystems' verdicts — and, since
   * `nodeStateFrom()` fail-safes an unrecognised status to `error`, would have
   * made every console predating this paint a node that ran perfectly as failed.
   *
   * So the card carries both: the green ring and `DONE` for the step, and a
   * labelled strip for the report. Absent on every node of a run that delivered
   * what it routed, which is the resting case.
   */
  reportUndelivered?: boolean;
}

/**
 * A node's state within one run (issue #371, #382).
 *
 * All three are **reported** by the engine now. `running` used to be *derived* —
 * the observer had only an `on_step_finish` hook, so the console guessed a
 * frontier by marking a finished node's successors — but issue #382 added
 * `on_step_start`, so the host emits a `workflow_node_started` frame and the
 * console lights the node up because it was told to, not because it inferred it.
 * `ok` and `error` come from the finish frame, exact as before.
 */
export type NodeRunState = "running" | "ok" | "error" | "blocked";

/**
 * Ring + glow per run state, layered over the node's own kind accent.
 *
 * The status vocabulary, so a ring here means what the dot means everywhere
 * else. It reads clearly over the kind accent precisely because that accent
 * comes from the identity palette — the two never reach for the same hue.
 */
export const RUN_STATE_CLASSES: Record<NodeRunState, string> = {
  running:
    "ring-2 ring-status-running/70 shadow-status-running/20 animate-pulse",
  ok: "ring-2 ring-status-done/60",
  error: "ring-2 ring-status-failed/80",
  // Issue #881. Deliberately the blocked token rather than the failed one: the
  // node did not break, it is parked until a person decides. Red would send an
  // operator looking for a bug when the fix is a click in Approvals — and it
  // would put a run that is merely waiting into the same visual bucket as one
  // that fell over.
  blocked: "ring-2 ring-status-blocked/80",
};

/** Per-kind emoji + accent, mirroring OpenHuman's node-kind metadata. */
export const NODE_KIND_META: Record<
  string,
  { emoji: string; color: NodeColor }
> = {
  trigger: { emoji: "⚡", color: "sage" },
  agent: { emoji: "🤖", color: "primary" },
  tool_call: { emoji: "🔧", color: "amber" },
  http_request: { emoji: "🌐", color: "coral" },
  condition: { emoji: "🔀", color: "primary" },
  output: { emoji: "📋", color: "amber" },
  // P2 catalog.
  switch: { emoji: "🔱", color: "primary" },
  merge: { emoji: "🔗", color: "sage" },
  split_out: { emoji: "✂️", color: "coral" },
  transform: { emoji: "🧬", color: "amber" },
  output_parser: { emoji: "🧾", color: "amber" },
  sub_workflow: { emoji: "📦", color: "primary" },
};

/**
 * Tailwind classes per accent, so light/dark theming comes from tokens.
 *
 * The identity palette: a node's accent says what kind of step it is, not how
 * the run is going. The status hues stay free for the run badge that sits on
 * top of these nodes (`RUN_STATE_BADGE` in `workflow-node.tsx`) — which is the
 * whole reason a node tinted sage must not be the same green as a node that
 * finished.
 *
 * The key names are the accents the sample data already refers to; they name a
 * slot, not a colour.
 */
export const COLOR_CLASSES: Record<
  NodeColor,
  { border: string; chip: string }
> = {
  primary: { border: "border-primary/40", chip: "bg-primary/10" },
  sage: { border: "border-tone-3/40", chip: "bg-tone-3/10" },
  amber: { border: "border-tone-5/40", chip: "bg-tone-5/10" },
  coral: { border: "border-tone-4/40", chip: "bg-tone-4/10" },
  neutral: { border: "border-border", chip: "bg-muted" },
};

/** Emoji + accent for a node kind, falling back for an unknown kind. */
export function nodeKindMeta(kind: string): {
  emoji: string;
  color: NodeColor;
} {
  return NODE_KIND_META[kind] ?? { emoji: "•", color: "neutral" };
}
