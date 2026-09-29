// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { ChatMessage } from "@/lib/chat";
import { MessageTimeline } from "@/views/room/MessageTimeline";
import {
  buildTimeline,
  buildTimelineItems,
  QUICK_REACTIONS,
  type Channel,
} from "@/views/room/model";

/**
 * The hover toolbar offers every quick reaction and the way into a thread, and
 * a reaction chip toggles unless its line is not yet stored.
 */

/** An ordinary, writable channel — the control for every assertion below. */
const ENGINEERING: Channel = {
  id: "engineering",
  name: "engineering",
  kind: "channel",
  purpose: "",
};

const T0 = Date.UTC(2026, 8, 1, 9, 0, 0);

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

/**
 * One channel's transcript, rendered through `MessageTimeline`.
 *
 * `createElement` rather than JSX because the unit suite's vitest `include` is
 * `*.test.ts` — a `.tsx` file is silently not collected, which reads as a
 * passing suite.
 */
function render(channel: Channel, messages: ChatMessage[]) {
  const items = buildTimelineItems(buildTimeline(messages, channel, []), []);
  act(() => {
    root.render(
      createElement(MessageTimeline, {
        channel,
        items,
        historyPending: false,
        openThreadId: null,
        typing: false,
        onOpenThread: () => {},
        onReact: () => {},
        onDismissCard: () => {},
        dismissingCardId: null,
      }),
    );
  });
}

/**
 * A durable line — an `h`-prefixed id, the shape `isHostMessageId` accepts.
 *
 * It has to be durable or `actionsUnavailableFor` disables the whole bar
 * ("not saved yet").
 */
function report(over: Partial<ChatMessage> = {}): ChatMessage {
  return {
    id: "h1",
    from: "company",
    text: "Automation **weekly digest** finished.",
    at: T0,
    ...over,
  };
}

/** The hover toolbar's quick-reaction buttons. */
function quickReactions(): HTMLButtonElement[] {
  return Array.from(container.querySelectorAll('button[aria-label^="React with "]'));
}

/** The chips under a line: one per emoji somebody has already used. */
function reactionChipButtons(): HTMLButtonElement[] {
  return Array.from(container.querySelectorAll("button[aria-pressed]")).filter(
    (button): button is HTMLButtonElement =>
      !(button.getAttribute("aria-label") ?? "").startsWith("React with "),
  );
}

describe("the hover reaction toolbar", () => {
  it("offers every quick reaction", () => {
    render(ENGINEERING, [report()]);
    expect(quickReactions().map((button) => button.getAttribute("aria-label"))).toEqual(
      QUICK_REACTIONS.map((emoji) => `React with ${emoji}`),
    );
  });

  it("offers the way into a thread", () => {
    render(ENGINEERING, [report()]);
    expect(container.querySelector('button[aria-label="Reply in thread"]')).not.toBeNull();
  });
});

describe("reactions that are already there", () => {
  it("stay toggleable", () => {
    render(ENGINEERING, [report({ reactions: [{ emoji: "👍", by: "Mithil", mine: false }] })]);
    const chips = reactionChipButtons();
    expect(chips).toHaveLength(1);
    expect(chips[0].disabled).toBe(false);
    expect(chips[0].title).toBe("Mithil reacted with 👍");
  });

  it("say why they do not toggle on an unsaved line", () => {
    render(ENGINEERING, [
      report({ id: "local-7", reactions: [{ emoji: "👍", by: "Mithil", mine: false }] }),
    ]);
    const chips = reactionChipButtons();
    expect(chips).toHaveLength(1);
    expect(chips[0].disabled).toBe(true);
    expect(chips[0].title).toBe(
      "Not saved yet — a reply or a reaction needs a message this company has stored.",
    );
  });
});
