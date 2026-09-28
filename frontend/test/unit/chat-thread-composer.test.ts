// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { TeamMember } from "@/lib/team";
import { ThreadPanel } from "@/views/room/ThreadPanel";
import type { Channel } from "@/views/room/model";

/** The thread panel's own composer sends a reply. */

const CHANNEL: Channel = {
  id: "engineering",
  name: "engineering",
  kind: "channel",
  purpose: "",
};

const MEMBERS: TeamMember[] = [];

let container: HTMLDivElement;
let root: Root;
let sent: ReturnType<typeof vi.fn>;

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  sent = vi.fn();
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

async function render() {
  await act(async () => {
    root.render(
      createElement(ThreadPanel, {
        channel: CHANNEL,
        members: MEMBERS,
        parent: { id: "p", from: "company", text: "nightly report", at: 0 },
        replies: [],
        sending: false,
        onSend: sent,
        onClose: vi.fn(),
      }),
    );
  });
}

function textarea() {
  return container.querySelector("textarea") as HTMLTextAreaElement;
}

function sendButton() {
  return container.querySelector('[aria-label="Send"]') as HTMLButtonElement;
}

async function type(text: string) {
  const el = textarea();
  const setValue = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
  await act(async () => {
    setValue?.call(el, text);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

describe("the thread composer", () => {
  it("sends a reply", async () => {
    await render();
    await type("on it");

    expect(textarea().placeholder).toBe("Reply…");
    expect(sendButton().disabled).toBe(false);

    await act(async () => sendButton().click());
    expect(sent).toHaveBeenCalledTimes(1);
    expect(sent).toHaveBeenLastCalledWith("on it", undefined, undefined, undefined);
  });
});
