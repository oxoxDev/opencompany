import { describe, expect, it } from "vitest";

import { fromHistory, makeMessage } from "@/lib/chat";

/**
 * The episode fields ride a message the same way `referralConversation` does:
 * straight off the host on both the live and the rehydrated path, never
 * inferred, and absent on every row the host did not mark.
 */

describe("episode fields on a chat line", () => {
  it("rehydrates a row's episode and audience from history", () => {
    const [row] = fromHistory([
      {
        id: "12",
        channel: "ceo",
        author: "CEO",
        text: "Own the checklist?",
        atMillis: 5,
        mine: false,
        audience: ["engineer"],
        episode: {
          id: "ep-1",
          revision: 1,
          kind: "dm",
          to: ["engineer"],
          routedBy: { plan: { kind: "one", primaryId: "engineer" }, router: "explicit" },
        },
      },
    ]);
    expect(row.episode).toEqual({
      id: "ep-1",
      revision: 1,
      kind: "dm",
      to: ["engineer"],
      routedBy: { plan: { kind: "one", primaryId: "engineer" }, router: "explicit" },
    });
    expect(row.audience).toEqual(["engineer"]);
  });

  it("carries nothing for a row from a host predating episodes, or with an empty audience", () => {
    const [plain, empty] = fromHistory([
      { id: "1", channel: "ceo", author: "CEO", text: "hi", atMillis: 1, mine: false },
      { id: "2", channel: "ceo", author: "CEO", text: "hi", atMillis: 2, mine: false, audience: [] },
    ]);
    expect(plain.episode).toBeUndefined();
    expect(plain.audience).toBeUndefined();
    expect(empty.audience).toBeUndefined();
  });

  it("stamps a live reply the same way, so the two fold into one round", () => {
    const live = makeMessage("company", "Own the checklist?", {
      channel: "ceo",
      messageId: "12",
      episode: { id: "ep-1", revision: 1, kind: "dm", to: ["engineer"] },
      audience: ["engineer"],
    });
    expect(live.id).toBe("h12");
    expect(live.episode?.kind).toBe("dm");
    expect(live.audience).toEqual(["engineer"]);
    expect(makeMessage("company", "x", { audience: [] }).audience).toBeUndefined();
  });
});
