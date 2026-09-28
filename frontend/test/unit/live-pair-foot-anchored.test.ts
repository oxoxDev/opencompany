// Where the live pair renders, and why it is not negotiable.
//
// Source-shape assertions, in the idiom this suite uses for wiring no rendered
// snapshot can show (`raw-turns-toggle.test.ts` is the nearest precedent, and
// `assert-design-tokens.sh` the one it cites).
//
// **Position is chronology. Ownership is a label.** A transcript is a timeline,
// so where a row sits says when it happened. The live pair says "happening
// now", which means it belongs at the now end — the foot of the pane — and
// nowhere else. `messageSeq` decides which *bucket* its frames fold into and
// which pane owns them; it never decides a y-position.
//
// The pair used to render under the asking message, which is invisible as a
// mistake on a single-responder turn: nothing is journaled between the question
// and the reply, so "under the query" and "at the foot" are the same pixel. It
// only diverges once something lands in between — and a hive episode journals a
// seat's line per turn, so by convergence the pulsing row sat several messages
// up while everything below it had already happened.
//
// These assertions are cheap and the bug they guard is not: reintroducing the
// under-message render is a one-line prop away, and it reads as an improvement
// ("show the steps next to the question that asked for them") right up until a
// room deliberates.

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const messageRow = readFileSync("src/views/room/MessageRow.tsx", "utf8");
const messageTimeline = readFileSync("src/views/room/MessageTimeline.tsx", "utf8");

describe("the live pair is pinned to the foot", () => {
  /**
   * A message row renders what was said, not what is happening. Giving it live
   * rows is what put a "now" element at a past position.
   */
  it("does not hand live rows to a message row", () => {
    expect(messageRow).not.toContain("liveSteps");
    expect(messageTimeline).not.toContain("liveSteps={liveStepsByMessage");
  });

  /**
   * Both halves of the pair read the same resolved rows. They were split — the
   * receipt on the thread bucket, the steps on the query bucket — and each
   * showed half a turn: the receipt stuck on "Picked up by <name>" because its
   * bucket was empty, while the rows surfaced somewhere else entirely.
   */
  it("feeds both foot rows from one resolved set of rows", () => {
    expect(messageTimeline).toContain("const openTurnSteps");
    expect(messageTimeline).toContain("steps={openTurnSteps ?? []}");
    // Neither foot row may go back to reading one bucket directly.
    expect(messageTimeline).not.toContain("steps={liveSteps ?? []}");
  });

  /**
   * The union is only safe because the two maps are filled exclusively — a
   * frame carrying `messageSeq` files under its query, one without files under
   * the thread, never both. If that ever stops holding, reading both would
   * double-count, so the reasoning has to stay next to the code.
   */
  it("says why reading both buckets cannot double-count", () => {
    expect(messageTimeline).toContain("exclusively");
  });
});
