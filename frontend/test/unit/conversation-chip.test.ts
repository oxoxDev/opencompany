// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { ConversationChip } from "@/components/episode/ConversationChip";
import type { ConversationRecord } from "@/lib/episodes";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

function render(conversation: ConversationRecord, agentNames?: Record<string, string>) {
  act(() => {
    root.render(createElement(ConversationChip, { conversation, agentNames }));
  });
  return host.querySelector('[data-testid="conversation-chip"]') as HTMLElement;
}

const record = (over: Partial<ConversationRecord> = {}): ConversationRecord =>
  ({
    root: 7,
    asker: "engineer-4f1c",
    askee: "ceo-9a2b",
    conversationId: "pair-1",
    openedAtMillis: 1,
    ...over,
  }) as ConversationRecord;

describe("ConversationChip", () => {
  it("names both seats by display name", () => {
    const chip = render(record(), { "engineer-4f1c": "Engineer", "ceo-9a2b": "CEO" });
    expect(chip.title).toBe("Engineer asked CEO");
  });

  it("calls an unnamed seat a teammate, never its roster id", () => {
    const chip = render(record(), { "engineer-4f1c": "Engineer" });
    expect(chip.title).toBe("Engineer asked a teammate");
    expect(chip.outerHTML.replace(/data-conversation-root="[^"]*"/, "")).not.toContain("ceo-9a2b");
  });
});
