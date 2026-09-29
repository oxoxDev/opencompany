import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

import { isAtBottom } from "./bottomAnchor";

/**
 * Bottom-anchoring for a scrolling transcript.
 *
 * Four rules, accreted one issue at a time in `MessageTimeline` and lifted here
 * unchanged so a second transcript — the thread panel — gets all four rather
 * than a partial copy. Their comments came with them: each records the case
 * that made the rule necessary, and a pane carrying three of the four is a pane
 * that anchors on open and then slides behind its own composer.
 *
 * Wire the returned refs to the scroller and to a plain wrapper around its
 * content, and `onScroll` to the scroller's scroll event.
 */
export interface BottomAnchorOptions {
  /**
   * What "a different transcript" means here — the channel id, the thread's
   * parent id. Rule 1 re-anchors when it changes; a count would not, because
   * two transcripts can hold the same number of rows.
   */
  key: string;
  /** This transcript's history has not arrived yet. Rules 1 and 2 turn on it. */
  pending: boolean;
  /** The values whose change means the transcript grew. Spread into rule 2. */
  growth: readonly unknown[];
}

export function useBottomAnchor({ key, pending, growth }: BottomAnchorOptions) {
  const scroller = useRef<HTMLDivElement>(null);
  /** The inner column whose own height rule 2b's `ResizeObserver` watches. */
  const content = useRef<HTMLDivElement>(null);
  /**
   * Is the view parked at the bottom, and therefore still following?
   *
   * A ref rather than state on purpose: it is read inside effects and written
   * from a scroll handler that fires at frame rate. Making it state would
   * re-render the whole transcript on every wheel tick to compute a value no
   * rendered output depends on.
   */
  const following = useRef(true);
  /** The channel the growth effect has already settled on. See rule 2. */
  const settledOn = useRef<string | null>(null);
  /**
   * The same answer as {@link following}, for rendered output — a control that
   * only exists while the reader has scrolled away.
   *
   * Two holders rather than one because the ref's reason above still stands: a
   * scroll handler runs at frame rate, and re-rendering the transcript on every
   * wheel tick is what the ref avoids. So the state is written only when the
   * answer *crosses* the threshold, which happens once per gesture. The mirror
   * ref is what makes that test free of the handler's own render cycle.
   */
  const [atBottom, setAtBottom] = useState(true);
  const shown = useRef(true);

  const settle = useCallback((next: boolean) => {
    following.current = next;
    if (next === shown.current) return;
    shown.current = next;
    setAtBottom(next);
  }, []);

  const onScroll = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    settle(isAtBottom(el));
  }, [settle]);

  /** Resumes following and travels to the newest row. */
  const jumpToLatest = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    settle(true);
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  }, [settle]);

  // Rule 1 — arriving at a channel. `useLayoutEffect` so the jump happens
  // before paint: with `useEffect` the browser paints the un-anchored position
  // first, which is the flash this issue is about. `channel.id` is the
  // dependency, not `items.length` — two channels can hold the same number of
  // rows, and an effect keyed on the count would not fire for that switch at
  // all, leaving the new channel wearing the old one's scroll offset.
  //
  // `historyPending` is the second dependency, and it is what makes the rule
  // true rather than merely well-intentioned (issue #1224). A cold load mounts
  // this component *before* the transcript exists: history is still on the wire
  // (`historyPending`), the box is one screen tall, and "scroll to the bottom"
  // is a no-op against content that has not arrived. Keyed on the channel
  // alone, this effect then never ran again, and the operator was left at the
  // top of a transcript that appeared under them a hundred milliseconds later.
  // Re-anchoring as the history lands is the same jump, against the real
  // transcript this time.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    // Not `following.current = true`: programmatic scrolling emits no event on
    // a transcript that does not overflow, so a pane switched to from a
    // scrolled-away one would keep showing the control it no longer needs.
    settle(true);
  }, [key, pending, settle]);

  // Rule 2 — growth while the channel is open. Each new tool row grows the
  // block at the bottom, so the scroll has to follow it as the turn works, not
  // only when the reply lands. A card arriving counts too — it is the thing the
  // operator has to act on. Skipped entirely when they have scrolled away.
  //
  // `channel.id` is a dependency so the first pass after a switch can *defer*:
  // the layout effect above has already anchored this channel, and animating on
  // top of that is the very travel rule 1 removes.
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    if (settledOn.current !== key) {
      settledOn.current = key;
      return;
    }
    // Nothing to follow while the transcript is still on the wire (#1224).
    // `scrollTo` captures a **pixel offset**, not the idea of "the bottom", so
    // an animation started against a one-screen box eases to a number the
    // arriving history makes meaningless — and the scroll events it emits on
    // the way there are indistinguishable from a person scrolling, so
    // `trackFollowing` reads the grown transcript as "they scrolled away" and
    // the channel stops following for the rest of the session. Rule 1 above
    // owns the anchor until the history has landed.
    if (pending) return;
    if (!following.current) return;
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    // `growth` is spread, which the rule cannot verify. The dependency list is
    // the contract above, and it belongs to the caller that knows what growing
    // means for its own rows — not to whatever satisfies the linter.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, pending, ...growth]);

  // Rule 3 — the *viewport* shrinking underneath (issue #1325).
  //
  // Rules 1 and 2 both watch the content. Neither watches the box, and the box
  // moves: the composer below this pane grows with the draft (`field-sizing-
  // content`, up to `max-h-48`), which takes its height out of this scroller's
  // `clientHeight`. `scrollTop` is untouched by that, so the transcript slides
  // up behind the composer — measured at 96px on a two-line draft and up to
  // ~150px at the cap, which is often the very message being replied to,
  // hidden for exactly as long as the draft is long.
  //
  // It could not be fixed by adding a dependency to rule 2: the composer is a
  // sibling component and its height is not a value this one is given. The
  // element's own size is, through `ResizeObserver` — and observing the box
  // covers the window resizing and the thread panel opening as well, which want
  // the same answer.
  //
  // `following.current` is the same gate rule 2 uses, so a reader who has
  // deliberately scrolled up is left alone. Instant rather than smooth,
  // unlike rule 2: this fires as the composer grows a line at a time, and an
  // animation per keystroke would be a permanent wobble rather than a glide.
  // Setting `scrollTop` does not resize anything, so there is no feedback loop.
  useEffect(() => {
    const el = scroller.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (!following.current) return;
      el.scrollTop = el.scrollHeight;
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  // Rule 2b — content that grows without moving any of rule 2's dependencies
  // (issue #1935 review, coderabbit 3892517543). `ChatLiveReceipt`'s 30s
  // "still waiting" note is timed by a clock entirely internal to that
  // component: nothing here re-renders when it appears, so rule 2 never fires
  // and the note can land under the fold with no follow-scroll to reveal it.
  // A live receipt is the concrete case, but the same gap exists for any
  // in-place child growth this component was not told about.
  //
  // Rule 3's `ResizeObserver` cannot double as this one — it watches the
  // *scroller's own border box*, which content overflowing inside an
  // `overflow-y-auto` container never changes; that is the whole reason the
  // container scrolls instead of growing. This one watches the *content*
  // column instead — the inner wrapper whose height the rows and receipt
  // actually determine — so it fires on exactly the growth rule 3 cannot see,
  // and stays silent on the box-only resizes (composer growing, window
  // resizing) rule 3 exists for, which do not move this column's own height.
  useEffect(() => {
    const contentEl = content.current;
    const scrollerEl = scroller.current;
    if (!contentEl || !scrollerEl || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      // Nothing to follow while the transcript is still on the wire, same as
      // rule 2 — a cold load's content grows repeatedly as history lands, and
      // rule 1 owns the anchor until it has.
      if (pending || !following.current) return;
      scrollerEl.scrollTo({ top: scrollerEl.scrollHeight, behavior: "smooth" });
    });
    observer.observe(contentEl);
    return () => observer.disconnect();
  }, [pending]);

  return { scroller, content, onScroll, following, atBottom, jumpToLatest };
}
