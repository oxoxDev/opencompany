/**
 * A group ask shows everyone in the room, not just the first seat asked.
 *
 * The desk rows carry one `askee`: they were written when an ask could name
 * only one seat, and `ask_teammates` records the first it asked there
 * (`primary_askee`). The full membership is in the channel key the same row
 * carries, so the chip reads that instead — a three-way exchange that drew two
 * avatars and read "Ada asked Grace" was under-reporting who was in it.
 */
import { describe, expect, it } from "vitest";

import { conversationAskees } from "@/components/episode/ConversationChip";
import type { ConversationRecord } from "@/lib/episodes";

const record = (
  asker: string,
  askee: string,
  conversationId: string,
): ConversationRecord => ({
  root: 7,
  asker,
  askee,
  conversationId,
  openedAtMillis: 0,
  lines: [],
});

describe("conversationAskees", () => {
  it("reads both seats of a pair from the channel key", () => {
    expect(conversationAskees(record("ada", "grace", "dm:ada+grace"))).toEqual(["grace"]);
  });

  it("reads every seat of a group ask, not only the recorded askee", () => {
    const group = record("creative_director", "copywriter", "dm:analytics_analyst+copywriter+creative_director");
    expect(conversationAskees(group)).toEqual(["analytics_analyst", "copywriter"]);
  });

  it("falls back to the row's own askee when the key is not a channel it knows", () => {
    expect(conversationAskees(record("ada", "grace", "engineering"))).toEqual(["grace"]);
  });

  it("falls back rather than showing nobody when the key names only the asker", () => {
    expect(conversationAskees(record("ada", "grace", "dm:ada"))).toEqual(["grace"]);
  });
});
