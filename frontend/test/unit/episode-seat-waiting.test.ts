// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ApprovalSummary } from "@/api/types";
import { handleEvent, type CompanyStreamEvent, type EpisodeFrame } from "@/hooks/use-events";
import type { ChatMessage } from "@/lib/chat";
import { EMPTY_EPISODE_FRAMES, reduceEpisodeFrame, type EpisodeFrames } from "@/lib/episode-frames";
import { foldEpisodes } from "@/lib/episodes";
import { EpisodeWaitingMarker } from "@/views/room/EpisodeWaitingMarker";
import { buildTimeline, buildTimelineItems, type Channel, type TimelineItem } from "@/views/room/model";

/**
 * A seat of a hive episode parked on an approval: the card sits inside the
 * episode's band, and a "Waiting for approval" marker names the teammate until
 * the seat resumes, the approval is decided, or the episode ends.
 */

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const CHANNEL: Channel = { id: "engineering", name: "engineering", voice: "Engineering desk", kind: "channel", purpose: "" };
const NAMES = { engineer: "Engineer", ceo: "CEO" };

const ROWS: ChatMessage[] = [
  { id: "h1", from: "you", byPerson: true, at: 0, text: "Ship it?" },
  { id: "h2", from: "company", channel: "engineer", at: 10, text: "Staging first.", episode: { id: "ep-1", revision: 0, kind: "post" } },
  { id: "h3", from: "company", channel: "ceo", at: 11, text: "Checking spend.", episode: { id: "ep-1", revision: 0, kind: "post" } },
];

const BASE = { chatId: "engineering", episodeId: "ep-1" };
const OPENED: EpisodeFrame[] = [
  { type: "episode_opened", seq: 1, atMillis: 5, ...BASE, openedBySeq: 1, participants: ["engineer", "ceo"], plan: { kind: "hive", primaryId: "engineer", invitedIds: ["ceo"] } },
  { type: "round_started", seq: 2, atMillis: 8, ...BASE, revision: 0, agentIds: ["engineer", "ceo"] },
];
const PARKED: EpisodeFrame = { type: "episode_seat_parked", seq: 3, atMillis: 12, ...BASE, seat: "ceo", approvalIds: ["ap-1"] };
const RESUMED: EpisodeFrame = { type: "episode_seat_resumed", seq: 4, atMillis: 20, ...BASE, seat: "ceo" };
const COMPLETED: EpisodeFrame = { type: "episode_completed", seq: 5, atMillis: 30, ...BASE, revision: 1, rounds: 1, reason: "complete_episode" };

function approval(over: Partial<ApprovalSummary> & Pick<ApprovalSummary, "id">): ApprovalSummary {
  return { kind: "payment.send", amount_usd: 12, at_millis: 12, agent: "ceo", thread: "engineering", ...over };
}

function fold(frames: EpisodeFrame[]): EpisodeFrames {
  return frames.reduce(reduceEpisodeFrame, EMPTY_EPISODE_FRAMES);
}

function items(frames: EpisodeFrame[], approvals: ApprovalSummary[] = [], decided = {}): TimelineItem[] {
  return buildTimelineItems(
    buildTimeline(ROWS, CHANNEL, []),
    approvals,
    decided,
    foldEpisodes(ROWS, fold(frames), "engineering"),
  );
}

function waiting(list: TimelineItem[]) {
  return list.filter((i): i is Extract<TimelineItem, { kind: "episode_waiting" }> => i.kind === "episode_waiting");
}

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

function renderMarker(item: Extract<TimelineItem, { kind: "episode_waiting" }>, agentNames?: Record<string, string>) {
  act(() => {
    root.render(createElement(EpisodeWaitingMarker, { episode: item.episode, seats: item.seats, agentNames }));
  });
  return host.querySelector('[data-testid="episode-waiting"]') as HTMLElement | null;
}

describe("episode seat waiting on an approval", () => {
  it("routes the parked and resumed frames to onEpisodeEvent", () => {
    for (const frame of [PARKED, RESUMED] as CompanyStreamEvent[]) {
      const onEpisodeEvent = vi.fn();
      handleEvent(frame, { onEpisodeEvent });
      expect(onEpisodeEvent).toHaveBeenCalledWith(frame);
    }
  });

  it("places an approval a seat raised inside its episode's band", () => {
    const seat = approval({ id: "ap-1", episode: { id: "ep-1", seat: "ceo" } });
    const loose = approval({ id: "ap-2", at_millis: 11 });
    const list = items(OPENED, [seat, loose]);
    const band = list.find((i): i is Extract<TimelineItem, { kind: "round" }> => i.kind === "round");
    expect(band?.items.map((row) => row.key)).toEqual(["h2", "h3", "approval:solo:ap-1"]);
    // An approval no seat raised stays in the channel.
    expect(list.map((i) => i.key)).toContain("approval:solo:ap-2");
    expect(band?.items.map((row) => row.key)).not.toContain("approval:solo:ap-2");
  });

  it("shows the marker for a parked seat and clears it when the seat resumes", () => {
    const [parked] = waiting(items([...OPENED, PARKED]));
    expect(parked?.seats).toEqual([{ agentId: "ceo", approvalIds: ["ap-1"] }]);
    expect(renderMarker(parked, NAMES)?.textContent).toBe("Waiting for approval — CEO");
    expect(waiting(items([...OPENED, PARKED, RESUMED]))).toEqual([]);
  });

  it("clears when the episode completes or the approval is decided", () => {
    expect(waiting(items([...OPENED, PARKED, COMPLETED]))).toEqual([]);
    const ap = approval({ id: "ap-1", episode: { id: "ep-1", seat: "ceo" } });
    expect(waiting(items([...OPENED, PARKED], [ap], { "ap-1": { verdict: "approve", approval: ap } }))).toEqual([]);
  });

  it("shows the marker from a pending approval alone, and clears once it leaves the list", () => {
    const ap = approval({ id: "ap-1", episode: { id: "ep-1", seat: "ceo" } });
    expect(waiting(items(OPENED, [ap]))[0]?.seats).toEqual([{ agentId: "ceo", approvalIds: ["ap-1"] }]);
    expect(waiting(items(OPENED, []))).toEqual([]);
  });

  it("never shows a raw id when the teammate has no name", () => {
    const [parked] = waiting(items([...OPENED, PARKED]));
    const text = renderMarker(parked, {})?.textContent ?? "";
    expect(text).toBe("Waiting for approval — a teammate");
    for (const raw of ["ceo", "ep-1", "ap-1"]) expect(text).not.toContain(raw);
  });

  it("keeps one blocker card across two episodes, outside either band", () => {
    const rows = ROWS.slice(0, 1);
    const first = approval({ id: "ap-7", at_millis: 15, group_key: "gmail", episode: { id: "ep-2", seat: "engineer" } });
    const second = approval({ id: "ap-8", at_millis: 16, group_key: "gmail", episode: { id: "ep-3", seat: "ceo" } });
    const list = buildTimelineItems(
      buildTimeline(rows, CHANNEL, []),
      [first, second],
      {},
      foldEpisodes(rows, undefined, "engineering"),
    );
    const cards = list.filter((i) => i.kind === "approval");
    expect(cards.map((i) => i.key)).toEqual(["approval:group:gmail"]);
    const bands = list.filter((i): i is Extract<TimelineItem, { kind: "round" }> => i.kind === "round");
    expect(bands.flatMap((b) => b.items.map((i) => i.key))).not.toContain("approval:group:gmail");
    expect(waiting(list).map((w) => w.episode.id)).toEqual(["ep-2", "ep-3"]);
  });

  it("keeps the band and marker after a reload for a seat that parked before any reply", () => {
    const rows = ROWS.slice(0, 1);
    const pending = approval({ id: "ap-9", at_millis: 15, episode: { id: "ep-2", seat: "engineer" } });
    const list = buildTimelineItems(
      buildTimeline(rows, CHANNEL, []),
      [pending],
      {},
      foldEpisodes(rows, undefined, "engineering"),
    );
    const band = list.find((i): i is Extract<TimelineItem, { kind: "round" }> => i.kind === "round");
    expect(band?.episode.id).toBe("ep-2");
    expect(band?.episode.roundCount).toBe(1);
    expect(band?.items.map((i) => i.key)).toEqual(["approval:solo:ap-9"]);
    expect(list.some((i) => i.kind === "approval")).toBe(false);
    expect(waiting(list).map((w) => w.seats)).toEqual([[{ agentId: "engineer", approvalIds: ["ap-9"] }]]);

    const decided = buildTimelineItems(
      buildTimeline(rows, CHANNEL, []),
      [pending],
      { "ap-9": { verdict: "approve", approval: pending } },
      foldEpisodes(rows, undefined, "engineering"),
    );
    expect(decided.some((i) => i.kind === "round" || i.kind === "episode_waiting")).toBe(false);
  });
});
