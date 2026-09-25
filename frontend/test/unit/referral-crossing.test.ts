// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { AgentConversationDto, ReferralConversationDto } from "@/api/types";
import { AgentConversation, ReferralChip, ReferralConversation } from "@/views/room/StepTimeline";

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

const agentNames = { exchanges: "Order Desk", triage: "Triage Team" };

describe("a crossing on the message that brought it home", () => {
  it("counts the exchange and names who was asked, by roster name, without opening it", () => {
    act(() => {
      root.render(createElement(ReferralConversation, { crossing: crossing(), agentNames }));
    });
    // Closed by default: the desk still reads as its own conversation, and how
    // much was said is visible without saying it.
    expect(container.textContent).toContain("asked @Triage Team · 2 messages");
    expect(container.textContent).not.toContain("what is on order");
    expect(container.textContent).not.toContain("triage");
  });

  it("falls back to 'a teammate' rather than the raw id when the roster has no name for it", () => {
    act(() => {
      root.render(createElement(ReferralConversation, { crossing: crossing() }));
    });
    expect(container.textContent).toContain("asked @a teammate · 2 messages");
    expect(container.textContent).not.toContain("triage");
  });

  it("shows both sides, in order, once opened, naming the asker by roster rather than by id", () => {
    act(() => {
      root.render(createElement(ReferralConversation, { crossing: crossing(), agentNames }));
    });
    act(() => {
      container.querySelector("button")?.click();
    });
    const text = container.textContent ?? "";
    expect(text).toContain("what is on order #W2378156?");
    expect(text).toContain("five items, keyboard included.");
    expect(text.indexOf("what is on order")).toBeLessThan(text.indexOf("five items"));
    // The outbound line has no `authorLabel` (it's this desk's own asker), so
    // it must fall back to the roster rather than the raw id it did before.
    expect(text).toContain("Order Desk");
    expect(text).not.toContain("exchanges");
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

  it("names the person on a direct crossing by roster name, and offers no link", () => {
    act(() => {
      root.render(createElement(ReferralChip, { ...base, direct: true, agentNames }));
    });
    expect(container.textContent).toContain("Answered by @Triage Team");
    expect(container.textContent).not.toContain("triage");
    // Their desk holds none of the exchange, so a link there would open an
    // unrelated conversation at a sequence that is not in it.
    expect(container.querySelector("a")).toBeNull();
  });

  it("falls back to 'a teammate' rather than the raw id when the roster has no name for the asker", () => {
    act(() => {
      root.render(createElement(ReferralChip, { ...base, direct: true }));
    });
    expect(container.textContent).toContain("Answered by @a teammate");
    expect(container.textContent).not.toContain("triage");
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

function exchange(over: Partial<AgentConversationDto> = {}): AgentConversationDto {
  return {
    root: 1,
    askerId: "exchanges",
    askeeId: "triage",
    conversationId: "dm:exchanges+triage",
    concluded: false,
    forced: false,
    lines: [
      { authorId: "exchanges", authorLabel: "", text: "what's the lag budget?", outbound: true },
      { authorId: "triage", authorLabel: "triage", text: "50ms.", outbound: false },
    ],
    ...over,
  };
}

describe("an agent-to-agent exchange kept in the pair channel", () => {
  it("names both seats by roster, live, without opening it", () => {
    act(() => {
      root.render(createElement(AgentConversation, { exchange: exchange(), agentNames }));
    });
    expect(container.textContent).toContain("Order Desk is talking to @Triage Team");
    expect(container.textContent).not.toContain("exchanges");
  });

  it("falls back to 'a teammate' rather than the raw id when the roster has no name for it", () => {
    act(() => {
      root.render(createElement(AgentConversation, { exchange: exchange() }));
    });
    expect(container.textContent).toContain("a teammate is talking to @a teammate");
    expect(container.textContent).not.toContain("exchanges");
    expect(container.textContent).not.toContain("triage");
  });

  it("names the concluded asker by roster once opened", () => {
    act(() => {
      root.render(
        createElement(AgentConversation, {
          exchange: exchange({ concluded: true }),
          agentNames,
        }),
      );
    });
    expect(container.textContent).toContain("asked @Triage Team");
    act(() => {
      container.querySelector("button")?.click();
    });
    const text = container.textContent ?? "";
    expect(text).toContain("what's the lag budget?");
    expect(text).toContain("50ms.");
    // The outbound line has no `authorLabel`, so it must fall back to the
    // roster rather than the raw asker id it did before.
    expect(text).toContain("Order Desk");
    expect(text).not.toContain("exchanges");
  });
});
