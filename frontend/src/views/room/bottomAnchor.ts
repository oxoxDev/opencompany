/**
 * The bottom-anchoring predicate, shared by every scrolling transcript.
 *
 * Pure on purpose — no React, no document. It is the one part of the anchoring
 * machinery a unit test can reach, and it is also the condition a "jump to the
 * end" control's visibility is the negation of, so both readings come from one
 * definition instead of two that can drift.
 */

/**
 * How close to the bottom still counts as "parked at the bottom", in CSS
 * pixels. Sub-pixel layout and a fractional `clientHeight` mean the arithmetic
 * rarely lands on exactly zero, so a strict test would read a view that is
 * visibly at the bottom as scrolled away and stop following.
 */
export const BOTTOM_SLACK_PX = 32;

/** The three numbers a scroller reports about where it is parked. */
export interface ScrollMetrics {
  scrollHeight: number;
  scrollTop: number;
  clientHeight: number;
}

/** Whether a scroller at these metrics counts as parked at the bottom. */
export function isAtBottom({ scrollHeight, scrollTop, clientHeight }: ScrollMetrics): boolean {
  return scrollHeight - scrollTop - clientHeight <= BOTTOM_SLACK_PX;
}
