// The one address the retired Tasks page left behind (issue #1140).
//
// `#/tasks/<id>` is the card detail — the timeline, the plan brief, the
// discussion, the attempts, the steer controls — and it is linked from chat,
// from an approval card, from a workflow run's rows and from every card on the
// board. The *page* those links used to land inside is gone; the address is
// not, so reading a card id out of it stopped being that page's private helper
// and became the router's.

/**
 * The card id in `#/tasks/<id>`, given the address's second segment.
 *
 * Returns `null` for an address that names no card, which is the whole reason
 * this is a function rather than a `decodeURIComponent` at the call site:
 *
 * * `#/tasks` — the retired board page. The shell rewrites it to the board's
 *   home in Ledgers rather than rendering a detail screen with nothing to show.
 * * `#/tasks/%` — malformed percent-encoding. `decodeURIComponent` throws
 *   `URIError` on it, and an address bar is operator input: a typo must read as
 *   "no card", not end the render. This guard came from the deleted screen,
 *   where it covered the same two callers for the same reason.
 */
export function taskIdFromSegment(segment: string | null): string | null {
  if (!segment) return null;
  try {
    const id = decodeURIComponent(segment).trim();
    return id || null;
  } catch {
    return null;
  }
}
