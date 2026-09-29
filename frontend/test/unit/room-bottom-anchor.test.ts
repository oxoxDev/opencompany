import { describe, expect, it } from "vitest";

import { BOTTOM_SLACK_PX, isAtBottom } from "@/views/room/bottomAnchor";

/**
 * The predicate every anchoring rule is gated on, and the negation of the
 * "jump to the end" control's visibility.
 *
 * Only the arithmetic is testable here — the rules that call it need a
 * document and live in `test/e2e/thread-scroll-anchor.spec.ts`. What this pins
 * is the slack: a strict test against zero reads a view that is visibly at the
 * bottom as scrolled away, which stops the transcript following for the rest
 * of the session.
 */
describe("isAtBottom", () => {
  it("is true when the view is exactly at the bottom", () => {
    expect(isAtBottom({ scrollHeight: 1200, scrollTop: 800, clientHeight: 400 })).toBe(true);
  });

  it("is true exactly at the slack boundary", () => {
    expect(
      isAtBottom({ scrollHeight: 1200, scrollTop: 800 - BOTTOM_SLACK_PX, clientHeight: 400 }),
    ).toBe(true);
  });

  it("is false one pixel past the slack boundary", () => {
    expect(
      isAtBottom({ scrollHeight: 1200, scrollTop: 800 - BOTTOM_SLACK_PX - 1, clientHeight: 400 }),
    ).toBe(false);
  });

  it("is false well up the transcript", () => {
    expect(isAtBottom({ scrollHeight: 1200, scrollTop: 0, clientHeight: 400 })).toBe(false);
  });

  it("is true on a fractional clientHeight that never lands on zero", () => {
    expect(
      isAtBottom({ scrollHeight: 1200.5, scrollTop: 800.25, clientHeight: 400.125 }),
    ).toBe(true);
  });

  it("is true for a box with no height yet, before any content has arrived", () => {
    expect(isAtBottom({ scrollHeight: 0, scrollTop: 0, clientHeight: 0 })).toBe(true);
  });
});
