// @vitest-environment jsdom

// What the live pair actually puts in the DOM, rendered against mock data.
//
// The source-shape guards beside this file say the wiring exists; they cannot
// say what it produces. These mount `MessageTimeline` for real and assert the
// three claims that are only true or false once something is on screen:
//
//   1. the pair is the LAST thing in the transcript, beneath every message
//      journaled while the turn ran — a "happening now" row rendered above a
//      line that already happened is a claim about time, and a false one;
//   2. it names whoever is working NOW, taking the live frame's agent over the
//      one the host started the turn on, which is never revised;
//   3. it shows what the turn has done, and a call parked on a sign-off reads
//      as parked rather than as finished work.
//
// No host, no SSE, no Playwright: the props are the seam. `MessageTimeline`
// takes `liveStepsByMessage` / `liveAgentByTurn` / `turnAgentId` as plain
// values, which is exactly the shape `AppShell.onTurnEvent` folds frames into,
// so driving them directly exercises the same rendering the stream reaches.

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { TurnStep } from "@/api/types";
import { MessageTimeline } from "@/views/room/MessageTimeline";
import type { Channel, TimelineItem } from "@/views/room/model";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  // jsdom has no layout, and the timeline anchors itself on mount.
  Element.prototype.scrollTo = () => {};
  Element.prototype.scrollIntoView = () => {};
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

const CHANNEL: Channel = {
  id: "engineering",
  name: "engineering",
  voice: "Engineering",
  kind: "channel",
  purpose: "Ships the product.",
  tone: "engineering",
};

/** One rendered line in the transcript. */
function messageItem(id: string, text: string, at: number, mine: boolean): TimelineItem {
  return {
    kind: "message",
    key: id,
    at,
    entry: {
      message: { id, from: mine ? "you" : "company", text, at },
      sender: mine
        ? { key: "you", name: "You", kind: "you" }
        : { key: "agent:ada", name: "Ada", kind: "agent", tone: "engineering" },
      continuation: false,
      replies: [],
    },
  } as unknown as TimelineItem;
}

const RUNNING: TurnStep[] = [
  { kind: "tool_call", status: "ok", label: "design_review", elapsedMs: 40 },
  { kind: "tool_call", status: "running", label: "changelog_read" },
];

function mount(props: Record<string, unknown>) {
  act(() =>
    root.render(
      createElement(MessageTimeline, {
        channel: CHANNEL,
        openThreadId: null,
        typing: false,
        onOpenThread: () => {},
        onReact: () => {},
        onDismissCard: () => {},
        dismissingCardId: null,
        ...props,
      } as never),
    ),
  );
}

/** The live row, however it is currently worded. */
function liveRow(): HTMLElement | null {
  return container.querySelector('[data-testid="working-indicator"]');
}

describe("the live pair renders at the foot", () => {
  it("sits after a message journaled while the turn was running", () => {
    // A room posts a line per seat. Anchored to the asking message, the pulsing
    // row drifted further up the transcript with every one of them.
    const items = [
      messageItem("h101", "What did design ship this week?", 1_000, true),
      messageItem("h102", "Onboarding flow went out Tuesday.", 2_000, false),
    ];
    mount({ items, liveStepsByMessage: { h101: RUNNING } });

    const live = liveRow();
    expect(live).not.toBeNull();

    const articles = container.querySelectorAll("article[data-message-id]");
    const last = articles[articles.length - 1];
    expect(last.textContent).toContain("Onboarding flow went out Tuesday.");

    // DOCUMENT_POSITION_FOLLOWING === 4: the live row comes after the message.
    expect(last.compareDocumentPosition(live!) & 4).toBeTruthy();
  });

  it("puts the steps under the line, not above it", () => {
    mount({
      items: [messageItem("h101", "What did design ship this week?", 1_000, true)],
      liveStepsByMessage: { h101: RUNNING },
    });

    const live = liveRow()!;
    const steps = container.querySelector("ol");
    // The summary now carries the running call after the count, so match the
    // count rather than anchoring on the end of the line.
    const summary = [...container.querySelectorAll("button")].find((b) =>
      /\d+ steps?\b/.test(b.textContent ?? ""),
    );
    // Either the collapsed summary or the open list — one of them must exist,
    // or chat can say a turn is running and never what it has done.
    expect(steps ?? summary).toBeTruthy();
    if (summary) expect(live.compareDocumentPosition(summary) & 4).toBeTruthy();
  });
});

describe("the live pair names who is working", () => {
  it("prefers the live frame's agent over the turn's opening responder", () => {
    // `turnAgentId` is set once when the host reports the turn. The floor moves
    // — a hand-off, a room's next seat — and only the frames know.
    mount({
      items: [messageItem("h101", "Should we ship on Friday?", 1_000, true)],
      liveStepsByMessage: { h101: [{ kind: "tool_call", status: "ok", label: "risk_register" }] },
      liveAgentByTurn: { h101: "a-grace" },
      turnAgentId: "a-ada",
      agentNames: { "a-ada": "Ada", "a-grace": "Grace" },
    });

    const live = liveRow()!;
    expect(live.textContent).toContain("Grace");
    expect(live.textContent).not.toContain("Ada");
  });

  it("falls back to the opening responder when no frame has named one", () => {
    // The reload leg: a re-armed row has the host's turn and no frames yet.
    mount({
      items: [messageItem("h101", "Should we ship on Friday?", 1_000, true)],
      liveStepsByMessage: { h101: [{ kind: "tool_call", status: "ok", label: "risk_register" }] },
      turnAgentId: "a-ada",
      agentNames: { "a-ada": "Ada" },
    });

    expect(liveRow()!.textContent).toContain("Ada");
  });

  it("leaves the running step to the steps row and keeps naming the agent", () => {
    mount({
      items: [messageItem("h101", "Should we ship on Friday?", 1_000, true)],
      liveStepsByMessage: { h101: RUNNING },
      liveAgentByTurn: { h101: "a-grace" },
      agentNames: { "a-grace": "Grace" },
    });

    // The line stays on who; the collapsed steps summary names the call.
    expect(liveRow()!.textContent).toContain("Grace is working…");
    expect(liveRow()!.textContent).not.toContain("changelog_read");
    expect(container.textContent).toContain("changelog_read");
  });
});

describe("a parked call reads as parked while it waits", () => {
  it("does not render a gated call as finished work", () => {
    // The fold used to flatten `awaiting_approval` into `ok`, so a call waiting
    // on a sign-off looked like one that had succeeded — for the whole time the
    // operator could have granted it.
    const parked: TurnStep[] = [
      { kind: "tool_call", status: "awaiting_approval", label: "composio_execute" },
    ];
    mount({
      items: [messageItem("h101", "Refund order 4830.", 1_000, true)],
      liveStepsByMessage: { h101: parked },
    });

    const text = container.textContent ?? "";
    // `StepTimeline` force-opens on a parked step, so its wording is on screen
    // rather than behind a collapsed count.
    expect(text).toContain("Awaiting approval");
    // And its duration is the honest one: a gated call never ran.
    expect(text).toContain("didn't run");
  });
});

describe("the live agent does not outlive its turn", () => {
  it("drops the thread's agent when its rows are retired", () => {
    // A thread key is reused by every turn a conversation ever runs. Clearing
    // only the rows leaves the previous turn's agent on the key, so the next
    // turn names whoever answered last until a frame happens to carry a new
    // id — and on a turn that never reports one, that is the whole turn
    // (CodeRabbit on #2423). The two are one fact and retire together.
    const agents: Record<string, string> = { thread: "a-ada" };
    const steps: Record<string, TurnStep[]> = { thread: [...RUNNING] };

    // What `clearLiveThread` does, as the shell does it.
    delete agents.thread;
    steps.thread = [];

    mount({
      items: [messageItem("h101", "Anything else?", 1_000, true)],
      liveSteps: steps.thread,
      liveAgentByTurn: agents,
      turnAgentId: undefined,
      agentNames: { "a-ada": "Ada" },
      typing: true,
    });

    // The next turn opens with no name of its own, so the row must say nothing
    // about who — never the previous turn's teammate.
    expect(container.textContent).not.toContain("Ada");
  });
});
