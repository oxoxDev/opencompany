// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { ChatMessage } from "@/lib/chat";
import { MessageTimeline } from "@/views/chat/MessageTimeline";
import {
  buildTimeline,
  buildTimelineItems,
  type Channel,
  type TimelineItem,
} from "@/views/chat/model";

/**
 * Where a channel opens, and what is allowed to move it (issue #757).
 *
 * The transcript used to animate to the bottom on arrival: one effect, always
 * `behavior: "smooth"`, firing on mount as well as on growth. Opening a channel
 * therefore painted an un-anchored position and then slid down, and the longer
 * the transcript the longer the slide.
 *
 * jsdom performs no layout, so `scrollHeight` and `clientHeight` are 0 and
 * `scrollTo` does not exist. The geometry below is stubbed on the prototype for
 * exactly that reason — these tests are about *which* anchoring call the
 * component makes and when, which is the part that was wrong. They cannot and
 * do not claim anything about real pixel positions.
 */

const CONTENT_HEIGHT = 4000;
const VIEWPORT_HEIGHT = 800;
/**
 * How tall the transcript is *right now* (issue #1224).
 *
 * A cold load renders this component before the history exists, so the box is
 * one screen tall and grows to the full transcript a beat later. The bug this
 * models is entirely about that growth, so the height has to be something a
 * test can move.
 */
let contentHeight = CONTENT_HEIGHT;
/** The largest `scrollTop` the current transcript allows — i.e. the bottom. */
const bottom = () => contentHeight - VIEWPORT_HEIGHT;
/** Every `scrollTo` the component made, in order. */
let calls: Array<{ top: number; behavior?: string }> = [];
let scrollTop = 0;
let container: HTMLDivElement;
let root: Root;

/** What each stubbed property looked like before, so it can be put back. */
const saved = new Map<string, PropertyDescriptor | undefined>();

const STUBS: Record<string, PropertyDescriptor> = {
  scrollHeight: { get: () => contentHeight, configurable: true },
  clientHeight: { get: () => VIEWPORT_HEIGHT, configurable: true },
  scrollTop: {
    get: () => scrollTop,
    // Clamped, as a browser clamps it. This is not decoration: issue #1224 is
    // entirely about `el.scrollTop = el.scrollHeight` landing somewhere other
    // than the bottom because the box was one screen tall at the time, and an
    // unclamped stub cannot express that at all — it would record 4000 on an
    // 800px box and every assertion about "did the anchor work" would pass
    // whether or not it did.
    set: (v: number) => {
      scrollTop = Math.max(0, Math.min(v, contentHeight - VIEWPORT_HEIGHT));
    },
    configurable: true,
  },
  scrollTo: {
    // A smooth scroll is an *animation*: it does not move `scrollTop`
    // synchronously, it eases toward the offset it captured over the following
    // frames. Modelling that matters for issue #1224, where the transcript
    // grows while such an animation is in flight — so the call is recorded and
    // the position is deliberately left where it was. An instant scroll still
    // lands immediately, as it does in a browser.
    value: (opts: { top: number; behavior?: string }) => {
      calls.push(opts);
      if (opts.behavior === "smooth") return;
      scrollTop = Math.max(0, Math.min(opts.top, contentHeight - VIEWPORT_HEIGHT));
    },
    configurable: true,
    writable: true,
  },
};

function stubGeometry() {
  for (const [prop, desc] of Object.entries(STUBS)) {
    saved.set(prop, Object.getOwnPropertyDescriptor(Element.prototype, prop));
    Object.defineProperty(Element.prototype, prop, desc);
  }
}

/**
 * Put the prototype back. Without this the stubs outlive the file inside a
 * shared worker, and the next suite to touch a scroll container inherits a
 * 4000px document it never asked for.
 */
function restoreGeometry() {
  for (const [prop, desc] of saved) {
    if (desc) Object.defineProperty(Element.prototype, prop, desc);
    else delete (Element.prototype as unknown as Record<string, unknown>)[prop];
  }
  saved.clear();
}

function channel(id: string): Channel {
  return { id, name: id, kind: "channel", purpose: "" };
}

/**
 * `n` message rows, built through the real timeline constructors so the rows
 * render as the app renders them. Only the count matters to the effects under
 * test, but a hand-rolled row shape would drift from `TimelineEntry` and fail
 * inside `MessageRow` rather than in an assertion.
 */
function items(n: number, ch: Channel): TimelineItem[] {
  const messages: ChatMessage[] = Array.from({ length: n }, (_, i) => ({
    id: `m${i}`,
    from: "you",
    text: `line ${i}`,
    at: 1_700_000_000_000 + i * 1_000,
  }));
  return buildTimelineItems(buildTimeline(messages, ch, []), []);
}

// `createElement` rather than JSX because the unit suite's vitest `include` is
// `*.test.ts` — a `.tsx` file is silently not collected, which reads as a
// passing suite.
function render(ch: Channel, rows: TimelineItem[], historyPending = false) {
  act(() => {
    root.render(
      createElement(MessageTimeline, {
        channel: ch,
        items: rows,
        historyPending,
        openThreadId: null,
        typing: false,
        onOpenThread: () => {},
        onReact: () => {},
        onDismissCard: () => {},
        dismissingCardId: null,
      }),
    );
  });
}

/** The scrolling body is the component's outermost element. */
function scroller(): HTMLElement {
  return container.firstElementChild as HTMLElement;
}

beforeEach(() => {
  // React only treats `act` as a real boundary when this is set; without it
  // effects still run but React warns, and the warning is the honest signal
  // that the flush is not being awaited the way the app flushes it.
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  calls = [];
  scrollTop = 0;
  contentHeight = CONTENT_HEIGHT;
  stubGeometry();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  restoreGeometry();
});

describe("channel arrival", () => {
  it("anchors instantly and never animates on the way in", () => {
    const ch = channel("engineering");
    render(ch, items(40, ch));

    // The jump is a direct `scrollTop` write, not an animated `scrollTo`.
    expect(scrollTop).toBe(bottom());
    expect(calls).toHaveLength(0);
  });

  it("re-anchors on a switch between channels holding the same number of rows", () => {
    const ch = channel("engineering");
    render(ch, items(40, ch));
    // Simulate the operator having scrolled up in the first channel.
    scrollTop = 0;

    // Same row count on purpose: the old effect keyed on `items.length` alone,
    // so this switch produced no effect at all and the new channel inherited
    // the previous scroll offset.
    const next = channel("product-design");
    render(next, items(40, next));

    expect(scrollTop).toBe(bottom());
  });
});

describe("growth while the channel is open", () => {
  it("follows a new row when the operator is parked at the bottom", () => {
    const ch = channel("engineering");
    render(ch, items(40, ch));
    calls = [];

    render(ch, items(41, ch));

    expect(calls).toEqual([{ top: CONTENT_HEIGHT, behavior: "smooth" }]);
  });

  it("leaves the viewport alone when the operator has scrolled up to read", () => {
    const ch = channel("engineering");
    render(ch, items(40, ch));
    calls = [];

    // Scrolled well away from the bottom, and the component told so by the
    // scroll event its own handler listens for.
    scrollTop = 100;
    act(() => {
      scroller().dispatchEvent(new Event("scroll", { bubbles: true }));
    });

    render(ch, items(41, ch));

    expect(calls).toHaveLength(0);
    expect(scrollTop).toBe(100);
  });

  it("resumes following once the operator returns to the bottom", () => {
    const ch = channel("engineering");
    render(ch, items(40, ch));

    scrollTop = 100;
    act(() => {
      scroller().dispatchEvent(new Event("scroll", { bubbles: true }));
    });
    render(ch, items(41, ch));
    expect(calls).toHaveLength(0);

    // Back to the bottom. `scrollHeight - scrollTop - clientHeight` is 0 here,
    // comfortably inside the slack the component allows.
    scrollTop = CONTENT_HEIGHT - VIEWPORT_HEIGHT;
    act(() => {
      scroller().dispatchEvent(new Event("scroll", { bubbles: true }));
    });
    render(ch, items(42, ch));

    expect(calls).toEqual([{ top: CONTENT_HEIGHT, behavior: "smooth" }]);
  });

  it("still follows when the view is a few pixels short of the bottom", () => {
    const ch = channel("engineering");
    render(ch, items(40, ch));
    calls = [];

    // Sub-pixel layout leaves a small remainder in a real browser; a strict
    // equality test would read this as "scrolled away" and stop following.
    scrollTop = CONTENT_HEIGHT - VIEWPORT_HEIGHT - 8;
    act(() => {
      scroller().dispatchEvent(new Event("scroll", { bubbles: true }));
    });
    render(ch, items(41, ch));

    expect(calls).toEqual([{ top: CONTENT_HEIGHT, behavior: "smooth" }]);
  });
});

/**
 * A cold load: the component mounts before the transcript exists (issue #1224).
 *
 * `AppShell` hydrates each channel's history asynchronously, so the first
 * commit this component sees has no rows and a box one screen tall. The
 * transcript appears about eighty milliseconds later. Traced in a real browser,
 * the sequence was:
 *
 * ```
 * t=66   top=0   sh=709   ch=709    empty — history still on the wire
 * t=79   top=0   sh=750   ch=709    the channel intro; max scrollTop is now 41
 * t=104  top=4   sh=750   ch=709    a smooth scrollTo is under way, target 41
 * t=141  top=25  sh=4188  ch=709    history lands. The target is still 41.
 * t=178  top=41  sh=4188  ch=709    3438px from the bottom, and stuck there.
 * ```
 *
 * The steps below are that trace, in the units this harness works in.
 */
describe("a transcript that arrives after mount", () => {
  /** The box before anything has hydrated: one screen, nothing to scroll. */
  const EMPTY_HEIGHT = VIEWPORT_HEIGHT;
  /** The channel intro renders before the transcript and grows the box a little. */
  const INTRO_HEIGHT = VIEWPORT_HEIGHT + 41;

  /** Mount empty, render the intro, then land the history — the trace above. */
  function coldLoad(ch: Channel) {
    contentHeight = EMPTY_HEIGHT;
    render(ch, [], true);
    contentHeight = INTRO_HEIGHT;
    render(ch, items(1, ch), true);
    contentHeight = CONTENT_HEIGHT;
    render(ch, items(40, ch), false);
  }

  it("anchors against the real transcript, not the one-screen box it mounted on", () => {
    const ch = channel("engineering");
    coldLoad(ch);

    // Without re-anchoring this is 41 — the offset the pre-hydration box
    // allowed — which is the top of a 4000px transcript.
    expect(scrollTop).toBe(bottom());
  });

  it("starts no animation while the history is still on the wire", () => {
    const ch = channel("engineering");
    contentHeight = EMPTY_HEIGHT;
    render(ch, [], true);
    calls = [];

    contentHeight = INTRO_HEIGHT;
    render(ch, items(1, ch), true);

    // `scrollTo` captures a pixel offset, not the idea of "the bottom". Any
    // animation started here is aimed at 41 and the arriving history cannot
    // redirect it.
    expect(calls).toHaveLength(0);
  });

  it("does not read the arriving transcript as the operator scrolling away", () => {
    const ch = channel("engineering");
    coldLoad(ch);

    // In a browser the growth lands mid-animation and the animation's own
    // frames emit scroll events, which `trackFollowing` cannot tell from a
    // wheel. This is one of those frames.
    act(() => {
      scroller().dispatchEvent(new Event("scroll", { bubbles: true }));
    });
    calls = [];

    // A message arrives a moment later. This is the half that made the bug
    // permanent — the channel went deaf for the rest of the session.
    render(ch, items(41, ch), false);

    expect(calls).toEqual([{ top: CONTENT_HEIGHT, behavior: "smooth" }]);
  });

  it("still leaves a reader alone once they have genuinely scrolled up", () => {
    const ch = channel("engineering");
    coldLoad(ch);

    scrollTop = 100;
    act(() => {
      scroller().dispatchEvent(new Event("scroll", { bubbles: true }));
    });
    calls = [];

    render(ch, items(41, ch), false);

    expect(calls).toHaveLength(0);
    expect(scrollTop).toBe(100);
  });
});
