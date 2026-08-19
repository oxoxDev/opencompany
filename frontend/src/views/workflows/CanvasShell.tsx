// The workflow DETAIL view's column layout — and the convention for where a
// panel mounts (issue #1107).
//
// Scope first, because it is load-bearing since issue #1110: this is the shell
// for `#/workflows/<id>`, never for `#/workflows`. The index is a list of
// workflows, so it has no single run to show history for and no single graph to
// inspect — every slot below is per-workflow chrome. `WorkflowsView` therefore
// renders this on the detail side of its one branch and a plain column on the
// index side, which is what makes "no run chrome on the index" structural
// rather than a rule each panel has to remember.
//
// Before this, every panel the view grew was appended to one vertical stack
// under the canvas, and each one ate the dimension a workflow graph needs most.
// Run history was the worst case: tall rows in a `max-h-72` horizontal strip,
// showing two runs at a time while spending the full width of the screen.
//
// The detail view has three slots, and each one is a decision about the panel's
// LIFETIME, not about where there happens to be room:
//
//   left   — an in-flow rail. A browsing context the operator opened on
//            purpose and keeps open while they work: it shrinks the canvas,
//            because it is meant to be read alongside it. Run history is the
//            only occupant today. Single occupancy — a second rail must
//            replace this one rather than stack under it, the same way the
//            right slot already arbitrates between its two panels.
//
//   right  — a floating overlay, mounted `absolute right-3 top-3 bottom-3
//            z-10` INSIDE the canvas (see `CopilotPanel` and
//            `NodeDetailPanel`). A transient focus that covers the graph and
//            is dismissed: the canvas keeps its full width underneath, so
//            closing the panel restores it instantly. Also single occupancy,
//            and `WorkflowsView` arbitrates it with a ternary — opening the
//            copilot clears the selected node.
//
//   bottom — a full-width strip below BOTH canvas and rail, mounted as a
//            sibling of this component (and, like this component, only on the
//            detail side of the branch). The receipt for something that just
//            happened and is dismissed when read: `RunResultPanel` and
//            `RunFailurePanel`. Full width because it spans the whole view,
//            and below the rail because the rail answers "what has run
//            before" while the strip answers "what did the run I just started
//            do" — two questions, two places.
//
// The arithmetic that fixes the breakpoint: the rail is in-flow, so it costs
// the canvas real width, while the right overlays only cover it. A 320px rail
// plus a 384px overlay is 704px of chrome, which is most of a laptop window
// once the app's own nav sidebar is subtracted. So the rail is in-flow only at
// `xl` (≥1280px viewport, ≥~1024px of view) — below that it falls back to the
// bottom strip it has always been, which never competes with the right slot.

import type { ReactNode } from "react";

export function CanvasShell({
  rail,
  children,
}: {
  /**
   * The left rail, or nothing. Rendered AFTER the canvas in the DOM and moved
   * left with `order` at `xl`: keeping the canvas first means the narrow
   * layout's reading order is unchanged, and the rail names itself as a
   * landmark so assistive tech can reach it directly either way.
   */
  rail?: ReactNode;
  /** The canvas. Positioned, because the right slot mounts inside it. */
  children: ReactNode;
}) {
  return (
    <div className="flex min-h-0 flex-1 flex-col xl:flex-row">
      <div className="relative min-h-0 flex-1">{children}</div>
      {rail && <div className="shrink-0 xl:order-first xl:w-80">{rail}</div>}
    </div>
  );
}
