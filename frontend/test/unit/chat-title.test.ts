import { describe, expect, it } from "vitest";

import { titleFromMessage } from "@/lib/chat";

/**
 * The board-card title derived from a chat message (issue #246).
 *
 * A truncation is the textbook silent-wrong-answer: it always returns a string,
 * it always looks plausible, and the two ways it is usually wrong — overshooting
 * the cap by the ellipsis, and cutting a surrogate pair in half — produce output
 * that no type and no render test objects to. The second one puts a `�` on
 * a card.
 */

const CAP = 80;

describe("titleFromMessage", () => {
  it("takes the first non-blank line, so a multi-line ask reads as a headline", () => {
    expect(titleFromMessage("  \n\n  Draft the launch post  \nwith a link at the end")).toBe(
      "Draft the launch post",
    );
  });

  it("returns empty for a message with nothing in it, so the caller can refuse", () => {
    expect(titleFromMessage("")).toBe("");
    expect(titleFromMessage("   \n\t\n  ")).toBe("");
  });

  it("leaves a title at exactly the cap alone", () => {
    const exact = "a".repeat(CAP);
    expect(titleFromMessage(exact)).toBe(exact);
    expect(Array.from(titleFromMessage(exact))).toHaveLength(CAP);
  });

  it("counts the ellipsis inside the cap rather than overshooting by one", () => {
    // The off-by-one that shows up as a title one character over budget.
    const long = "b".repeat(CAP + 40);
    const title = titleFromMessage(long);
    expect(Array.from(title)).toHaveLength(CAP);
    expect(title.endsWith("…")).toBe(true);
  });

  it("never cuts an emoji in half", () => {
    // Astral-plane code points are two UTF-16 units each. Slicing on `.length`
    // instead of code points ends the title on a lone surrogate, which renders
    // as a replacement character.
    const emoji = "🚀".repeat(CAP + 10);
    const title = titleFromMessage(emoji);

    expect(title).not.toContain("�");
    // No LONE surrogate survived the cut. Iterating a string yields whole code
    // points, so a well-formed 🚀 comes through as U+1F680 and only an
    // unpaired half lands in the surrogate range itself.
    for (const point of title) {
      const code = point.codePointAt(0)!;
      expect(code >= 0xd800 && code <= 0xdfff).toBe(false);
    }
    expect(Array.from(title)).toHaveLength(CAP);
  });
});
