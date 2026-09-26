// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ChatHistoryMessageDto } from "@/api/types";
import { fromHistory } from "@/lib/chat";
import type { TeamMember } from "@/lib/team";
import { MessageRow } from "@/views/room/MessageRow";
import { ThreadPanel } from "@/views/room/ThreadPanel";
import type { Channel, TimelineEntry } from "@/views/room/model";

/**
 * A desk seat's deliverable reaches the operator on the episode row that
 * carries it: the artifact link opens the card's Artifacts tab at the version
 * the seat wrote, and the row links the card it was filed on. A turn that only
 * published hands over on a row with no text.
 */

const DESK: Channel = {
  id: "engineering",
  name: "engineering",
  voice: "Engineering",
  kind: "channel",
  purpose: "",
};

const MEMBERS: TeamMember[] = [
  {
    id: "engineer",
    name: "Engineer",
    role: "Engineer",
    description: "",
    tone: "violet",
    avatar: "badger",
    inboxEnabled: true,
    effectiveTools: [],
    desks: [],
  },
];

// The artifact's own page, not the card's Artifacts tab. `publish_artifact`
// mints a card only to satisfy the artifact store's `(task_id, source)`
// identity; sending a chat reader through it made them open a board item to
// read the deliverable the row was already offering.
const ARTIFACT_HREF = "#/artifacts/art-1?v=1";

function episodeRow(id: string, text: string, kind: "complete_episode" | "post"): ChatHistoryMessageDto {
  return {
    id,
    channel: "engineer",
    author: "engineer",
    text,
    atMillis: 1_700_000_000_000,
    mine: false,
    parentId: "5",
    taskId: "card-1",
    outputs: [
      {
        kind: "artifact",
        targetId: "art-1",
        title: "Pilot slide outline",
        taskId: "card-1",
        version: 1,
      },
    ],
    episode: { id: "ep-1", revision: 0, kind },
  };
}

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

function renderRow(dto: ChatHistoryMessageDto) {
  const [message] = fromHistory([dto]);
  const entry: TimelineEntry = {
    message,
    sender: { key: "engineer", name: "Engineer", kind: "agent" },
    continuation: false,
    replies: [],
    replySenders: [],
  };
  act(() =>
    root.render(
      createElement(MessageRow, {
        entry,
        threadOpen: false,
        onOpenThread: () => {},
        onReact: () => {},
        onDismissCard: () => {},
        dismissingCardId: null,
      }),
    ),
  );
}

describe("a desk seat's deliverable on its episode row", () => {
  it("rehydrates the artifact, the card and the speech act together", () => {
    const [row] = fromHistory([episodeRow("19", "The outline is published.", "complete_episode")]);
    expect(row.taskId).toBe("card-1");
    expect(row.outputs).toEqual([
      { kind: "artifact", targetId: "art-1", title: "Pilot slide outline", taskId: "card-1", version: 1 },
    ]);
    expect(row.episode?.kind).toBe("complete_episode");
    expect(row.parentId).toBe("h5");
  });

  it("links the artifact at the version the seat wrote", () => {
    renderRow(episodeRow("19", "The outline is published.", "complete_episode"));
    const link = container.querySelector("[data-chat-output-links] a");
    expect(link?.textContent).toContain("Pilot slide outline");
    expect(link?.getAttribute("href")).toBe(ARTIFACT_HREF);
    // And NO card chip beside it. The card exists — `publish_artifact` cannot
    // file without one — but offering it here gave the row two doors to the
    // same thing, the second leading to a board item whose only content is the
    // deliverable already linked above it.
    const card = [...container.querySelectorAll("a, button")].find((el) =>
      el.textContent?.includes("Card opened"),
    );
    expect(card).toBeUndefined();
  });

  it("hands over on a row with no text", () => {
    renderRow(episodeRow("20", "", "post"));
    const link = container.querySelector("[data-chat-output-links] a");
    expect(link?.getAttribute("href")).toBe(ARTIFACT_HREF);
  });

  it("links the artifact from inside the episode's thread", () => {
    const [reply] = fromHistory([episodeRow("19", "The outline is published.", "complete_episode")]);
    act(() =>
      root.render(
        createElement(ThreadPanel, {
          channel: DESK,
          members: MEMBERS,
          parent: { id: "h5", from: "you", text: "Draft the slide outline.", at: 1 },
          replies: [reply],
          sending: false,
          onSend: vi.fn(),
          onClose: vi.fn(),
        }),
      ),
    );
    const link = container.querySelector("[data-chat-output-links] a");
    expect(link?.getAttribute("href")).toBe(ARTIFACT_HREF);
  });
});
