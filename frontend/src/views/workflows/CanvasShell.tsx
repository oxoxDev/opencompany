// The workflow DETAIL view's column layout — and the convention for where a
// panel mounts (issue #1107, extended by #1205).
//
// Scope first, because it is load-bearing since issue #1110: this is the shell
// for `#/workflows/<id>`, never for `#/workflows`. The index is a list of
// workflows, so it has no single run to show history for and no single graph to
// inspect — every slot below is per-workflow chrome. `WorkflowsView` therefore
// renders this on the detail side of its one branch and a plain column on the
// index side, which is what makes "no run chrome on the index" structural
// rather than a rule each panel has to remember.
//
// Before #1107, every panel the view grew was appended to one vertical stack
// under the canvas, and each one ate the dimension a workflow graph needs most.
// Run history was the worst case: tall rows in a `max-h-72` horizontal strip,
// showing two runs at a time while spending the full width of the screen. #1205
// found the run-result drawer in the same state and gave it the same fix.
//
// The detail view now has three slots — two rails and one overlay — and each
// is a decision about the panel's LIFETIME, not about where there happens to be
// room:
//
//   leftRail  — in-flow, shrinks the canvas. A browsing context the operator
//               opened on purpose and keeps open while they work: it is meant
//               to be read alongside the graph, not over it. Run history is the
//               only occupant today. Single occupancy — a second panel wanting
//               this rail must replace it rather than stack under it.
//
//   rightRail — in-flow, shrinks the canvas from the other side. Issue #1205's
//               answer for `RunResultPanel` and `RunFailurePanel`: a receipt
//               for something that just happened, dismissed when read, but
//               tall and vertically stacking (a delivery block, a Steps list,
//               one card per node) — exactly the shape `leftRail` already
//               proved a horizontal strip cannot show well. It is built the
//               same way as `leftRail`, mirrored: in-flow only at `xl`
//               (≥1280px viewport), collapsing to a full-width strip below the
//               canvas beneath that — see `RunResultPanel`/`RunFailurePanel`
//               for the class pattern, copied from `RunHistoryPanel`. Single
//               occupancy; the two panels are mutually exclusive by
//               construction in `WorkflowsView` (a run either produces a
//               result or a failure, never both), so no arbitration beyond
//               that is needed.
//
//   right overlay — a floating overlay, mounted `absolute right-3 top-3
//               bottom-3 z-10` INSIDE the canvas (see `CopilotPanel` and
//               `NodeDetailPanel`, unchanged by #1205). A transient focus that
//               covers the graph and is dismissed: the canvas keeps its full
//               width underneath, so closing the panel restores it instantly.
//               Single occupancy, arbitrated by `WorkflowsView` with a
//               ternary — opening the copilot clears the selected node.
//
// **Why the overlay and `rightRail` can never collide.** They are not siblings
// in the same box: the overlay is `absolute` against the canvas's own
// `relative` container (`children` below), not against this shell. When
// `rightRail` is present it takes real flex width, so the canvas container
// itself gets narrower — and the overlay's containing block shrinks with it.
// The overlay's right edge is always the (now-narrower) canvas's right edge,
// which is `rightRail`'s left edge, never underneath or past it. No z-index
// race, no arbitration code: the overlay simply has less canvas to sit over.
//
// The arithmetic that fixes the breakpoint: a rail is in-flow, so it costs the
// canvas real width, while the overlay only covers it. Two 320px rails (640px)
// plus the app's own 216px nav sidebar is 856px before the canvas or the
// overlay even enter into it — most of a laptop window. So both rails are
// in-flow only at `xl` (≥1280px viewport) — below that each falls back to the
// bottom strip it has always been, stacking canvas → leftRail's strip →
// rightRail's strip, which never competes with the overlay slot.

import type { ReactNode } from "react";

export function CanvasShell({
  leftRail,
  rightRail,
  children,
}: {
  /**
   * The left rail, or nothing. Rendered AFTER the canvas in the DOM and moved
   * left with `order` at `xl`: keeping the canvas first means the narrow
   * layout's reading order is unchanged, and the rail names itself as a
   * landmark so assistive tech can reach it directly either way.
   */
  leftRail?: ReactNode;
  /**
   * The right rail, or nothing (issue #1205). Rendered AFTER the canvas and
   * the left rail in the DOM, with NO `order` override: at `xl` (flex-row)
   * that natural position already reads as "on the right" (canvas, then the
   * reordered-first left rail, then this); below `xl` (flex-col) it naturally
   * stacks last, under both the canvas and the left rail's own strip.
   */
  rightRail?: ReactNode;
  /** The canvas. Positioned, because the right overlay slot mounts inside it. */
  children: ReactNode;
}) {
  return (
    <div className="flex min-h-0 flex-1 flex-col xl:flex-row">
      <div className="relative min-h-0 flex-1">{children}</div>
      {leftRail && (
        <div className="shrink-0 xl:order-first xl:w-80">{leftRail}</div>
      )}
      {rightRail && <div className="shrink-0 xl:w-80">{rightRail}</div>}
    </div>
  );
}
