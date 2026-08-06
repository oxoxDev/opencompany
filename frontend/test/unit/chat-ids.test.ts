import { describe, expect, it } from "vitest";

import {
  hostMessageId,
  isHostMessageId,
  reconcileIds,
  toHostMessageId,
  type ChatMessage,
} from "@/lib/chat";

/**
 * Message-id reconciliation (issue #364) — the helper issue #434 was filed
 * over: it was correctly isolated for testing and then could not be tested,
 * because the console had no runner.
 *
 * Everything here is invisible when it goes wrong. A send renders optimistically
 * under a browser-local id and the host answers with a durable one; if the swap
 * drops a reply's parent link, the reply does not error — it silently leaves the
 * thread and reappears in the channel, which reads as the console having lost
 * it. No type catches that, and a browser walk that caught it would report it as
 * "the chat looked wrong".
 */

function message(over: Partial<ChatMessage> & Pick<ChatMessage, "id">): ChatMessage {
  return { from: "you", text: "…", at: 1_000, ...over };
}

describe("the two id namespaces", () => {
  it("marks a host id so it is distinguishable from a browser-local one", () => {
    expect(hostMessageId("42")).toBe("h42");
    expect(isHostMessageId("h42")).toBe(true);
    // `m<n>` is the local counter and means nothing outside this tab.
    expect(isHostMessageId("m7")).toBe(false);
    expect(isHostMessageId(undefined)).toBe(false);
  });

  it("round-trips a host id back to the sequence the host journals under", () => {
    expect(toHostMessageId(hostMessageId("42"))).toBe("42");
    // A local id names nothing on the host, and must not be sent as if it did.
    expect(toHostMessageId("m7")).toBeNull();
    expect(toHostMessageId(undefined)).toBeNull();
  });
});

describe("reconcileIds", () => {
  it("replaces the optimistic id with the durable one", () => {
    const before = [message({ id: "m1", text: "ship it" })];
    const after = reconcileIds(before, "m1", "42");
    expect(after[0].id).toBe("h42");
  });

  it("re-parents a reply that already pointed at the optimistic id", () => {
    // The race this exists for: the operator opened a thread on their own
    // bubble and replied to it before the POST resolved.
    const before = [
      message({ id: "m1", text: "ship it" }),
      message({ id: "m2", text: "on it", parentId: "m1" }),
    ];

    const after = reconcileIds(before, "m1", "42");

    expect(after[0].id).toBe("h42");
    expect(after[1].parentId).toBe("h42");
    // The reply keeps its own identity — only its parent moved.
    expect(after[1].id).toBe("m2");
  });

  it("leaves replies to other messages alone", () => {
    const before = [
      message({ id: "m1" }),
      message({ id: "m2", parentId: "m9" }),
    ];
    const after = reconcileIds(before, "m1", "42");
    expect(after[1].parentId).toBe("m9");
  });

  it("returns the same array when nothing matches, so React sees no new list", () => {
    // Identity, not equality: a fresh array on every reconcile would re-render
    // the whole transcript for a message this list does not contain.
    const before = [message({ id: "m1" })];
    expect(reconcileIds(before, "m-unknown", "42")).toBe(before);
  });

  it("returns the same array when the id is already the durable one", () => {
    const before = [message({ id: "h42" })];
    expect(reconcileIds(before, "h42", "42")).toBe(before);
  });
});
