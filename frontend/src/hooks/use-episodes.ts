/**
 * The episodes of one channel, memoised over its transcript and the live
 * frames.
 *
 * A thin hook on purpose: `foldEpisodes` is pure and tested on its own, and
 * this exists only so `RoomView` recomputes the fold when — and only when —
 * the transcript, the frames or the channel change.
 */

import { useMemo } from "react";

import type { ChatMessage } from "@/lib/chat";
import type { EpisodeFrames } from "@/lib/episode-frames";
import { foldEpisodes, type Episode } from "@/lib/episodes";

export function useEpisodes(
  messages: ChatMessage[],
  frames: EpisodeFrames | undefined,
  chatId: string | undefined,
): Episode[] {
  return useMemo(() => foldEpisodes(messages, frames, chatId), [messages, frames, chatId]);
}
