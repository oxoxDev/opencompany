import { describe, expect, it } from "vitest";

import type { ChatHistoryMessageDto } from "@/api/types";
import { fromHistory } from "@/lib/chat";

/**
 * Issue #966 — the console half of the host-authored notice.
 *
 * Three sites emit prose the runtime wrote itself: an approval-overflow notice,
 * the `"Acknowledged."` cycle fallback, and a failed-continuation report. They
 * used to journal under `"operator"`, which made a correct system row identical
 * on disk to a reply whose author the pre-#885 defect had overwritten. The host
 * now journals them under `SYSTEM_AUTHOR`, and this file pins what that buys on
 * the surface a reader actually looks at.
 *
 * **What is and is not covered here.** The `author === "system"` branch itself
 * predates this issue (#377 added it for the dispatch marker), so these are not
 * a regression guard for the host-side change — the marker tests already cover
 * that branch, and they only ever exercise it through a row carrying a card.
 * What was untested is this shape: a system-authored row with **no** `taskId`,
 * which is what all three notices are. Together with the host-side assertion
 * that `SYSTEM_AUTHOR` is literally `"system"`, this is the coupling that keeps
 * the two halves spelled the same.
 */

const AT = 1_700_000_000_000;

/** A notice as the host now journals it: authored by the runtime, no card. */
function notice(over: Partial<ChatHistoryMessageDto> = {}): ChatHistoryMessageDto {
  return {
    id: "512",
    channel: "system",
    author: "system",
    text: "Acknowledged.",
    atMillis: AT,
    mine: false,
    ...over,
  };
}

describe("a host-authored notice", () => {
  it("renders as a centred pill, not as the company speaking", () => {
    const [line] = fromHistory([notice()]);

    expect(line.from).toBe("system");
    expect(line.text).toBe("Acknowledged.");
  });

  /**
   * The notices carry no card. The marker tests only ever drive the system
   * branch through a row that has one, so this is the shape they leave open.
   */
  it("carries no card chip", () => {
    expect(fromHistory([notice()])[0].taskId).toBeUndefined();
  });

  /** All three notices, including the one that bypasses `OutboundMessage`. */
  it("reads the same for the failed-continuation report", () => {
    const [line] = fromHistory([
      notice({
        id: "513",
        text: "Your approval was recorded, but the agent could not pick the work back up.",
      }),
    ]);

    expect(line.from).toBe("system");
  });
});

describe("a notice journaled before the fix", () => {
  /**
   * The forward-only boundary, stated as a test rather than only in prose.
   *
   * A row already on disk keeps `"operator"` and is indistinguishable from a
   * reply whose author was overwritten, so it still reads as the company. That
   * is not a defect this change can repair — the distinguishing information was
   * never written down — and pinning it here keeps the limit visible to anyone
   * who later wonders why old transcripts still look wrong.
   */
  it("still reads as a company bubble, which no fix can undo", () => {
    const [line] = fromHistory([notice({ channel: "operator", author: "operator" })]);

    expect(line.from).toBe("company");
  });
});
