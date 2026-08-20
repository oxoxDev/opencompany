// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { ApprovalSummary, Verdict } from "@/api/types";
import { MessageTimeline } from "@/views/chat/MessageTimeline";
import { buildTimelineItems, type Channel } from "@/views/chat/model";

/**
 * One card is not disabled by another card's decision (#842, over #373).
 *
 * The shell keeps **one** in-flight map for the whole console, keyed by
 * approval id, because two decisions genuinely can be in flight at once — the
 * host serialises them behind its per-company lock and the console must not
 * pretend otherwise. `MessageTimeline` narrows that map to each card's own
 * items before handing it down; the card then reads `deciding.size > 0` as "I
 * am busy".
 *
 * Without the narrowing that read is wrong in the one direction that matters.
 * Any approval being decided anywhere — another channel, another turn, the
 * Approvals page in the same tab — would make every batch card in the
 * transcript report itself busy and grey out its buttons. That is issue #373's
 * bug exactly, one surface up: a single in-flight slot freezing rows it has
 * nothing to do with.
 *
 * **This suite exists because a test of the narrowing helper alone would not
 * catch it.** The helper is pure and easy to test in isolation, and it would go
 * on passing if someone deleted the call and passed the shell-wide map straight
 * through — which is the actual regression. So the assertion has to go through
 * the component that does the wiring.
 */

const T0 = new Date("2026-03-02T10:00:00Z").getTime();

const CHANNEL: Channel = {
  id: "marketing",
  name: "marketing",
  voice: "Marketing",
  kind: "channel",
  purpose: "",
};

function approval(id: string, batch: string, url: string): ApprovalSummary {
  return {
    id,
    kind: "web_fetch",
    amount_usd: null,
    at_millis: T0,
    agent: "seo",
    thread: CHANNEL.id,
    batch,
    payload: { url },
  };
}

/** One turn's batch of two, and a second, unrelated turn's single call. */
const ESPN = approval("a1", "turn-1", "https://espn.com/nba");
const BBC = approval("a2", "turn-1", "https://bbc.com/sport");
const OTHER_TURN = approval("b1", "turn-2", "https://crates.io/crates/serde");

let container: HTMLDivElement;
let root: Root;

async function render(deciding: ReadonlyMap<string, Verdict>) {
  const items = buildTimelineItems([], [ESPN, BBC, OTHER_TURN]);
  await act(async () => {
    root.render(
      createElement(MessageTimeline, {
        channel: CHANNEL,
        items,
        openThreadId: null,
        typing: false,
        onOpenThread: () => {},
        onReact: () => {},
        onDismissCard: () => {},
        dismissingCardId: null,
        now: T0 + 60_000,
        askerNames: new Map([["seo", "SEO Specialist"]]),
        decidingApprovals: deciding,
        onDecideApproval: () => {},
      }),
    );
  });
}

/** The card whose first item is `approvalId`, as the DOM exposes it. */
function card(approvalId: string): HTMLElement {
  const el = container.querySelector<HTMLElement>(`[data-approval-id="${approvalId}"]`);
  if (!el) throw new Error(`no card for ${approvalId}: ${container.textContent}`);
  return el;
}

function buttons(el: HTMLElement): HTMLButtonElement[] {
  return [...el.querySelectorAll("button")];
}

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

describe("in-flight decisions are scoped to the card they belong to", () => {
  it("leaves one turn's card live while a different turn's decision is in flight", async () => {
    // `b1` belongs to turn-2. Turn-1's card has nothing to do with it and must
    // stay decidable — the operator can answer two conversations' requests
    // without the first one freezing the second.
    await render(new Map([["b1", "approve"]]));

    const turnOne = buttons(card("a1"));
    expect(turnOne).toHaveLength(2);
    expect(turnOne.every((b) => b.disabled)).toBe(false);
    // Nor is it spinning: a card with no decision of its own in flight is idle,
    // and showing it as working would be the same lie in a quieter register.
    expect(card("a1").querySelectorAll(".animate-spin")).toHaveLength(0);
  });

  it("does disable the card whose own item is being decided", async () => {
    // The other direction, so the test above cannot pass by the narrowing
    // simply dropping everything on the floor.
    await render(new Map([["a1", "approve"]]));

    expect(buttons(card("a1")).every((b) => b.disabled)).toBe(true);
    expect(buttons(card("b1")).every((b) => b.disabled)).toBe(false);
  });

  it("disables a card when any one of its own items is in flight", async () => {
    // One click fans out to one resolve per item, so mid-flight only some of a
    // card's ids are in the map. The card is still busy.
    await render(new Map([["a2", "approve"]]));

    expect(buttons(card("a1")).every((b) => b.disabled)).toBe(true);
  });
});
