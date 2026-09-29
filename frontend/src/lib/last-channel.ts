// Where the operator was last reading, per company (issue #412).
//
// The shell already knows which chat channel is on screen — `onChannelViewed`
// records it so unread counts clear and an unaddressed system line is addressed
// to the channel the operator was actually looking at (issue #368). That memory
// lives in a ref, so it dies with the tab: leaving Chat and coming back, or
// reloading, dropped the operator on whichever channel happened to sort first.
//
// This is the same fact, written somewhere it survives a reload. It is a
// *hint*, never authority: the hash still decides which channel renders (see
// `RoomView`), so a deep link outranks it and a remembered channel that no
// longer exists falls through to the same unknown-channel notice as any other
// stale id (issue #370).

import { type LocalScope, scopedKeyAdoptingLegacy } from "@/connections/types";
import { migrateLegacyGeneralId } from "@/lib/chat";

/** Namespaced so a host serving several companies remembers each separately. */
function keyFor(scope: LocalScope): string {
  return scopedKeyAdoptingLegacy(
    "oc.chat.last-channel",
    scope,
    // Note the different shape: this one used a `.` and `__default__`.
    `oc.chat.last-channel.${scope.company ?? "__default__"}`,
  );
}

/**
 * `localStorage`, or `null` where it isn't usable.
 *
 * Access itself can throw — Safari's private mode and a "block all cookies"
 * setting both make the property itself raise rather than return a dud object.
 * A console that cannot remember a channel must still render one.
 */
function storage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

/**
 * The channel this company was last read in, or `null` if nothing is
 * remembered. A `#general` remembered under a legacy id is rewritten to
 * `general` on the way out.
 */
export function readLastChannel(scope: LocalScope): string | null {
  const store = storage();
  if (!store) return null;
  try {
    const key = keyFor(scope);
    const value = store.getItem(key);
    if (!value) return null;
    const current = migrateLegacyGeneralId(value);
    if (current !== value) store.setItem(key, current);
    return current;
  } catch {
    return null;
  }
}

/**
 * Remember the channel on screen.
 *
 * Skips a write when the value is unchanged: the caller reports the open
 * channel again on every message that lands in it, and `localStorage` writes
 * are synchronous — re-writing the same string on every frame of a busy channel
 * would put disk I/O on the render path for no gain.
 */
export function writeLastChannel(scope: LocalScope, channelId: string): void {
  const store = storage();
  if (!store) return;
  try {
    const key = keyFor(scope);
    if (store.getItem(key) === channelId) return;
    store.setItem(key, channelId);
  } catch {
    // A full or read-only quota is not worth failing a render over.
  }
}
