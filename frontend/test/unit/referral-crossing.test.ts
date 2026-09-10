// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { ReferralConversationDto } from "@/api/types";
import { ReferralChip, ReferralConversation } from "@/views/room/StepTimeline";

/**
 * **What a crossing looks like to the operator reading it.**
 *
 * The rows a crossing is made of are dropped from the asking desk — an agent
 * who does not work there did not speak there — so this component and the chip
 * beside it are the only account of what was actually asked and answered. The
 * asker's own report is a paraphrase, and paraphrases drift.
 */

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

function crossing(over: Partial<ReferralConversationDto> = {}): ReferralConversationDto {
  return {
    askerId: "exchanges",
    otherId: "triage",
    otherDeskId: "front_desk",
    otherDeskName: "Front Desk",
    direct: true,
    lines: [
      { authorId: "exchanges", authorLabel: "", text: "what is on order #W2378156?", outbound: true },
      { authorId: "triage", authorLabel: "triage", text: "five items, keyboard included.", outbound: false },
    ],
    ...over,
  };
}

describe("a crossing on the message that brought it home", () => {
  it("counts the exchange and names who was asked, without opening it", () => {
    act(() => {
      root.render(createElement(ReferralConversation, { crossing: crossing() }));
    });
    // Closed by default: the desk still reads as its own conversation, and how
    // much was said is visible without saying it.
    expect(container.textContent).toContain("asked @triage · 2 messages");
    expect(container.textContent).not.toContain("what is on order");
  });

  it("shows both sides, in order, once opened", () => {
    act(() => {
      root.render(createElement(ReferralConversation, { crossing: crossing() }));
    });
    act(() => {
      container.querySelector("button")?.click();
    });
    const text = container.textContent ?? "";
    expect(text).toContain("what is on order #W2378156?");
    expect(text).toContain("five items, keyboard included.");
    expect(text.indexOf("what is on order")).toBeLessThan(text.indexOf("five items"));
  });

  it("names a DESK with a #, because a room was asked rather than a person", () => {
    act(() => {
      root.render(
        createElement(ReferralConversation, {
          crossing: crossing({ direct: false, otherDeskId: "order_ops" }),
        }),
      );
    });
    expect(container.textContent).toContain("asked #order_ops · 2 messages");
    expect(container.textContent).not.toContain("@triage");
  });

  it("renders nothing at all when the crossing carried no lines", () => {
    act(() => {
      root.render(createElement(ReferralConversation, { crossing: crossing({ lines: [] }) }));
    });
    expect(container.textContent).toBe("");
  });
});

describe("the chip beside it", () => {
  const base = {
    deskId: "front_desk",
    deskName: "Front Desk",
    askerId: "triage",
    sequence: 42,
    direction: "answered" as const,
  };

  it("names the person on a direct crossing, and offers no link", () => {
    act(() => {
      root.render(createElement(ReferralChip, { ...base, direct: true }));
    });
    expect(container.textContent).toContain("Answered by @triage");
    // Their desk holds none of the exchange, so a link there would open an
    // unrelated conversation at a sequence that is not in it.
    expect(container.querySelector("a")).toBeNull();
  });

  it("names the desk on a desk crossing, and links to it", () => {
    act(() => {
      root.render(createElement(ReferralChip, { ...base, direct: false }));
    });
    expect(container.textContent).toContain("Answered by Front Desk");
    expect(container.querySelector("a")?.getAttribute("href")).toBe(
      "#/chat?desk=front_desk&at=42",
    );
  });
});
