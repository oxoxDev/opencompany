// Where a card's origin conversation lives, for the card → chat half of the
// #246 round trip.
//
// A card created out of a conversation carries `originChatId`, a **host thread
// id**. Chats are served by the Room tab, addressed `#/chat/<channelId>`, so
// the id on the card is not itself an address: it has to be resolved through
// the shell's thread → channel map first, and that resolution can come back
// empty (a thread whose desk is gone, a map not yet loaded). Deciding here
// rather than at the button keeps "is there somewhere to go" and "go there"
// from drifting apart.

import { hostMessageId } from "@/lib/chat";
import { channelForThread } from "@/views/room/model";

export type OriginConversation =
  /** The card was not opened from a conversation; the row renders nothing. */
  | { kind: "none" }
  /** It was, but no channel carries that thread — state the origin, offer no jump. */
  | { kind: "unreachable" }
  /**
   * It was, and the Room channel below renders it. `threadId` names the
   * message the card's thread panel should open to, when the card was raised
   * inside a thread rather than the channel's own timeline.
   */
  | { kind: "channel"; channelId: string; threadId?: string };

/**
 * Resolved through {@link channelForThread}, not a bare `map[originChatId]`,
 * so a `dm:`-prefixed origin resolves too. A card raised in `#general` has the
 * origin `general`.
 */
export function originConversation(
  originChatId: string | undefined | null,
  chatChannelByThread: Readonly<Record<string, string>> | undefined,
  originParent?: number,
): OriginConversation {
  if (!originChatId) return { kind: "none" };
  const channelId = chatChannelByThread
    ? channelForThread(chatChannelByThread, originChatId)
    : null;
  if (!channelId) return { kind: "unreachable" };
  return {
    kind: "channel",
    channelId,
    // `Room`'s transcript keys every host-journaled message as `h<seq>`
    // (`hostMessageId`) to keep it apart from a locally-minted optimistic id;
    // a bare seq here would never match a loaded message's `id`, and the
    // thread panel would silently fail to open.
    threadId: originParent != null ? hostMessageId(String(originParent)) : undefined,
  };
}
