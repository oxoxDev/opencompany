import { migrateLegacyGeneralId, type ChatMessage } from "@/lib/chat";
import type { ReadMarker } from "@/api/types";

/**
 * How the console decides what is unread (issue #755).
 *
 * Both rules live here rather than inline in the shell so a test can exercise
 * *this* code instead of a copy of it. A private re-implementation in a test
 * file passes whether or not the shipped rule still agrees with it, which is
 * the failure mode this module exists to remove.
 */

/**
 * Merge the read floors the host remembers into the ones this tab has observed,
 * taking the later of each pair.
 *
 * Never an assignment. The host's markers arrive asynchronously while the
 * console is already usable, so a channel opened in that window has a *fresher*
 * floor than anything stored — overwriting it would re-raise a badge the
 * operator just cleared.
 *
 * A channel the host said nothing about keeps whatever this tab had, and a
 * channel this tab has never opened adopts the stored floor outright. A floor
 * stored under a legacy `#general` id (`main`, `General`) is read as `general`'s.
 */
export function mergeReadFloors(
  viewed: Readonly<Record<string, number>>,
  markers: readonly ReadMarker[],
): Record<string, number> {
  const merged = { ...viewed };
  for (const m of markers) {
    const channelId = migrateLegacyGeneralId(m.channelId);
    merged[channelId] = Math.max(merged[channelId] ?? 0, m.lastReadAt);
  }
  return merged;
}

/**
 * How many messages in a channel count as unread against a floor.
 *
 * Your own lines never count — a badge for what you just typed is noise. The
 * comparison is strictly `>`, so a message landing on the same millisecond as
 * the floor reads as *seen*: the floor is stamped when the channel is viewed,
 * and a row already on screen at that instant has been.
 */
export function unreadCount(
  messages: readonly Pick<ChatMessage, "from" | "at">[],
  floor: number,
): number {
  return messages.filter((m) => m.from !== "you" && m.at > floor).length;
}
