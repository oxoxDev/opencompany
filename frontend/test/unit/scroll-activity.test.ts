// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { startScrollActivity } from "@/lib/scroll-activity";

/**
 * The scroll-activity mark (issue #1109).
 *
 * What is worth asserting here is not "it sets an attribute" — it is the three
 * properties the themed scrollbars actually rely on, each of which is invisible
 * in a screenshot until it is wrong:
 *
 *   - one listener covers containers it was never told about, because `scroll`
 *     is caught in the capture phase rather than by subscribing per view;
 *   - two panels scrolling at once keep independent idle timers, so the one
 *     that stopped first does not un-mark the one still moving;
 *   - the disposer leaves nothing behind — no timer, no stuck mark.
 */

let dispose: (() => void) | undefined;

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  dispose?.();
  dispose = undefined;
  document.body.innerHTML = "";
  vi.useRealTimers();
});

/** A scroll container the utility was never handed a reference to. */
function scroller(): HTMLElement {
  const el = document.createElement("div");
  document.body.append(el);
  return el;
}

describe("startScrollActivity", () => {
  it("marks whichever element scrolls, without being registered", () => {
    dispose = startScrollActivity();

    // Created *after* the listener was armed: the point of the capture-phase
    // listener is that a panel mounted later needs no wiring of its own.
    const panel = scroller();
    expect(panel.hasAttribute("data-scrolling")).toBe(false);

    panel.dispatchEvent(new Event("scroll"));
    expect(panel.hasAttribute("data-scrolling")).toBe(true);
  });

  it("clears the mark once the element has been still for the idle beat", () => {
    dispose = startScrollActivity(700);
    const panel = scroller();

    panel.dispatchEvent(new Event("scroll"));
    vi.advanceTimersByTime(699);
    expect(panel.hasAttribute("data-scrolling")).toBe(true);

    vi.advanceTimersByTime(1);
    expect(panel.hasAttribute("data-scrolling")).toBe(false);
  });

  it("keeps the mark through a continuous scroll, writing it once", () => {
    dispose = startScrollActivity(700);
    const panel = scroller();
    const write = vi.spyOn(panel, "setAttribute");

    // A flick of a trackpad is a burst of events, not one: each has to push the
    // idle deadline out rather than queue its own expiry.
    for (let i = 0; i < 10; i += 1) {
      panel.dispatchEvent(new Event("scroll"));
      vi.advanceTimersByTime(400);
      expect(panel.hasAttribute("data-scrolling")).toBe(true);
    }

    // Re-marking an already-marked element would invalidate its style on every
    // frame of the scroll to set the attribute to what it already is.
    expect(write).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(700);
    expect(panel.hasAttribute("data-scrolling")).toBe(false);
  });

  it("tracks two containers independently", () => {
    dispose = startScrollActivity(700);
    const thread = scroller();
    const drawer = scroller();

    thread.dispatchEvent(new Event("scroll"));
    vi.advanceTimersByTime(400);
    drawer.dispatchEvent(new Event("scroll"));

    // The thread's own deadline passes while the drawer is still moving.
    vi.advanceTimersByTime(300);
    expect(thread.hasAttribute("data-scrolling")).toBe(false);
    expect(drawer.hasAttribute("data-scrolling")).toBe(true);

    vi.advanceTimersByTime(400);
    expect(drawer.hasAttribute("data-scrolling")).toBe(false);
  });

  it("marks the root element when the page itself scrolls", () => {
    dispose = startScrollActivity();

    // The document is the target for the root scroller, and `html` is the only
    // thing a stylesheet can hang a scrollbar rule on.
    document.dispatchEvent(new Event("scroll"));
    expect(document.documentElement.hasAttribute("data-scrolling")).toBe(true);
  });

  it("stops listening and clears a live mark on dispose", () => {
    const stop = startScrollActivity(700);
    const panel = scroller();

    panel.dispatchEvent(new Event("scroll"));
    expect(panel.hasAttribute("data-scrolling")).toBe(true);

    stop();
    // Cleared immediately rather than left to expire: the timer is gone, so
    // nothing would ever come back for it.
    expect(panel.hasAttribute("data-scrolling")).toBe(false);
    expect(vi.getTimerCount()).toBe(0);

    panel.dispatchEvent(new Event("scroll"));
    expect(panel.hasAttribute("data-scrolling")).toBe(false);
  });
});
