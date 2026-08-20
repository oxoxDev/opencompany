import { describe, expect, it } from "vitest";

import { clearTaskCard, type ChatMessage } from "@/lib/chat";
import { clearTaskCardEverywhere } from "@/views/chat/model";

/**
 * Dropping a dismissed card's chip from a transcript (issue #984).
 *
 * The subtle half is that this is keyed on the CARD, not on the row the
 * operator clicked. One card can be named by several lines — a turn journals
 * the id onto its reply, and "Add to board" writes it onto the operator's own
 * message — so clearing only the clicked bubble leaves the other chips linking
 * to a card the host no longer has. Nothing throws when that happens; the chip
 * simply stays on screen and navigates to a 404, which reads as the delete
 * having failed. Only an assertion catches it.
 */

const T0 = new Date("2026-03-02T10:00:00Z").getTime();

function message(over: Partial<ChatMessage> & Pick<ChatMessage, "id">): ChatMessage {
  return { from: "company", text: "…", at: T0, ...over };
}

describe("clearTaskCard", () => {
  it("drops the card from every line that names it, not just the first", () => {
    const rows = [
      message({ id: "a", taskId: "card-1" }),
      message({ id: "b", taskId: "card-2" }),
      message({ id: "c", taskId: "card-1" }),
    ];

    const next = clearTaskCard(rows, "card-1");

    expect(next.map((m) => m.taskId)).toEqual([undefined, "card-2", undefined]);
  });

  it("removes the key rather than leaving it present and undefined", () => {
    const next = clearTaskCard([message({ id: "a", taskId: "card-1" })], "card-1");

    // `"taskId" in row` is the assertion that matters: a present-and-undefined
    // key passes a truthiness test at every render site, but is not the same
    // thing to `Object.keys`, a spread, or a deep-equality assertion — and an
    // absent key is what "this line has no card" means everywhere else in the
    // module. (Not a persistence argument: these rows are never serialised.
    // The persisted copy is the host's, and the host is what stops the chip
    // coming back on a reload.)
    expect("taskId" in next[0]).toBe(false);
  });

  it("leaves every other field of a cleared line intact", () => {
    const rows = [
      message({ id: "a", from: "you", text: "ship it", at: 42, taskId: "card-1" }),
    ];

    const [row] = clearTaskCard(rows, "card-1");

    // `toStrictEqual`, not `toEqual`: `toEqual` treats a present-but-undefined
    // `taskId` as absent, so it would pass against the very implementation
    // this function exists instead of.
    expect(row).toStrictEqual({ id: "a", from: "you", text: "ship it", at: 42 });
  });

  it("returns the same array when no line names the card, so React sees no change", () => {
    const rows = [message({ id: "a", taskId: "card-1" })];

    // Identity, not equality. A fresh array here would re-render the whole
    // transcript on every dismissal of a card that is not on this channel.
    expect(clearTaskCard(rows, "card-nope")).toBe(rows);
    expect(clearTaskCard([], "card-1")).toHaveLength(0);
  });

  it("leaves lines that never had a card alone", () => {
    const rows = [message({ id: "a" }), message({ id: "b", taskId: "card-1" })];

    const next = clearTaskCard(rows, "card-1");

    expect(next[0]).toBe(rows[0]);
    expect("taskId" in next[1]).toBe(false);
  });
});

describe("clearTaskCardEverywhere", () => {
  const transcripts = () => ({
    general: [message({ id: "a", taskId: "card-1" }), message({ id: "b" })],
    // A dispatch marker lands in the origin thread's channel, which is not
    // necessarily the one the operator is looking at.
    engineering: [message({ id: "c", taskId: "card-1" })],
    design: [message({ id: "d", taskId: "card-2" })],
  });

  it("clears the card from every channel, not just one", () => {
    const next = clearTaskCardEverywhere(transcripts(), "card-1");

    expect(next.general.map((m) => m.taskId)).toEqual([undefined, undefined]);
    expect(next.engineering.map((m) => m.taskId)).toEqual([undefined]);
  });

  it("leaves a channel that never named the card alone, by identity", () => {
    const before = transcripts();

    const next = clearTaskCardEverywhere(before, "card-1");

    // Identity, not deep equality: a new array for an untouched channel is a
    // needless re-render of a transcript that did not change.
    expect(next.design).toBe(before.design);
    expect(next.design[0].taskId).toBe("card-2");
  });

  it("returns the same object when no channel names the card", () => {
    const before = transcripts();

    expect(clearTaskCardEverywhere(before, "card-404")).toBe(before);
  });

  it("handles a console with no transcripts yet", () => {
    expect(clearTaskCardEverywhere({}, "card-1")).toEqual({});
  });
});
