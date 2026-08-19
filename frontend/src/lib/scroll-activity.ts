/**
 * Marks the element the operator is currently scrolling (issue #1109).
 *
 * The console's scrollbars are themed in `index.css`, and they rest at a low
 * weight so that a panel at rest is not fenced in by a bright grey bar. Lifting
 * them while the content is actually moving is the other half of that, and CSS
 * cannot do it: there is no `:scrolling` selector, and the two states CSS *can*
 * see are both wrong for this.
 *
 * `:hover` is the usual substitute, and it matches every ancestor of the
 * pointer — `html` included. A hover reveal is therefore really an "is the
 * pointer inside the window" reveal, lighting every nested scroller at once,
 * which is louder than the always-on bar it replaced. `:focus-within` has the
 * same shape. So the signal comes from here instead: one listener, the element
 * that actually emitted a `scroll` event, and an idle timer.
 *
 * ## One listener for the whole document
 *
 * `scroll` does not bubble from an element, but it still runs the capture phase
 * down from the document, so a single capturing listener sees every scroller in
 * the app — the ones that exist today and the ones added next month, with no
 * per-view wiring and nothing for a new panel to forget. That is the entire
 * reason this is a document-level utility and not a hook a view opts into.
 *
 * The element is marked with `data-scrolling`, an attribute React does not
 * manage and therefore does not clobber on re-render, and it is cleared once
 * the element has been still for `idleMs`. Each element carries its own timer:
 * two panels can scroll at once (a chat thread under a copilot drawer), and a
 * shared timer would clear the mark on whichever one stopped first.
 *
 * Deliberately framework-free, in the same spirit as `visible-poll.ts`: no React
 * import, no hook rules, callable from `main.tsx` before anything mounts and
 * from a plain test with fake timers.
 *
 * @param idleMs How long an element must be still before the mark is dropped.
 *               The default is long enough to cover the pause between two flicks
 *               of a trackpad, short enough that the bar is gone before the
 *               reader has finished the line they scrolled to.
 * @returns A disposer that removes the listener, cancels every pending timer and
 *          clears every mark it left behind. The console calls this once for the
 *          life of the document and never disposes; tests do.
 */
export function startScrollActivity(idleMs = 700): () => void {
  // Strong refs, not a WeakMap: the disposer has to be able to find every mark
  // it left in order to clear it, and an entry only lives for `idleMs` after its
  // element last moved. An element ripped out of the DOM mid-scroll drops out of
  // here on the next tick like any other.
  const pending = new Map<Element, number>();

  const clear = (el: Element) => {
    pending.delete(el);
    el.removeAttribute("data-scrolling");
  };

  const onScroll = (event: Event) => {
    // The root scroller reports the document as its target, and the styling hook
    // for it is the `html` element.
    const target = event.target;
    const el =
      target instanceof Document
        ? target.documentElement
        : target instanceof Element
          ? target
          : null;
    if (!el) return;

    // A pending timer and the attribute are set and cleared together, so the
    // timer's presence is what says the element is already marked. Writing the
    // attribute again on every frame of a scroll would invalidate style for the
    // element dozens of times a second to set it to the value it already has.
    const running = pending.get(el);
    if (running === undefined) el.setAttribute("data-scrolling", "");
    else window.clearTimeout(running);

    pending.set(
      el,
      window.setTimeout(() => clear(el), idleMs),
    );
  };

  // `passive` because this handler never calls `preventDefault`, and telling the
  // browser so up front keeps it off the critical path of the scroll it is
  // watching. `capture` is what makes one listener enough — see above.
  document.addEventListener("scroll", onScroll, { capture: true, passive: true });

  return () => {
    document.removeEventListener("scroll", onScroll, { capture: true });
    for (const [el, timer] of pending) {
      window.clearTimeout(timer);
      el.removeAttribute("data-scrolling");
    }
    pending.clear();
  };
}
