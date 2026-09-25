import { describe, expect, it } from "vitest";

import type { EpisodeFrame } from "@/hooks/use-events";
import type { ChatMessage } from "@/lib/chat";
import { EMPTY_EPISODE_FRAMES, reduceEpisodeFrame } from "@/lib/episode-frames";
import { foldEpisodes } from "@/lib/episodes";
import { buildTimeline, buildTimelineItems, type Channel } from "@/views/room/model";

/**
 * `buildTimelineItems` with episodes: an episode's rows collapse into ONE
 * `round` item at the position of its first row, that band tracks the newest
 * wave, a completed episode gets its marker after it, and everything outside
 * an episode is untouched.
 *
 * It used to mint a band per wave, which stacked nine bands for an episode
 * that ran nine and labelled them with raw revision numbers — so a desk that
 * ran nine rounds could show one headed "Round 17", because conversation
 * waves take revisions of their own. One band per episode is what the desk
 * actually did.
 */

const CHANNEL: Channel = { id: "engineering", name: "engineering", voice: "Engineering desk", kind: "channel", purpose: "" };

const ROWS: ChatMessage[] = [
  { id: "h1", from: "you", byPerson: true, at: 0, text: "Ship it?" },
  { id: "h2", from: "company", channel: "engineer", at: 10, text: "Staging first.", episode: { id: "ep-1", revision: 0, kind: "post" } },
  { id: "h3", from: "company", channel: "ceo", at: 11, text: "How long?", episode: { id: "ep-1", revision: 0, kind: "post" } },
  { id: "h4", from: "company", channel: "engineer", at: 20, text: "Two days.", episode: { id: "ep-1", revision: 1, kind: "broadcast" } },
  { id: "h5", from: "company", channel: "ceo", at: 30, text: "Decision: staging.", episode: { id: "ep-1", revision: 2, kind: "complete_episode" } },
  { id: "h6", from: "you", byPerson: true, at: 40, text: "thanks" },
];

const kinds = (rows: ChatMessage[]) =>
  buildTimelineItems(buildTimeline(rows, CHANNEL, []), [], {}, foldEpisodes(rows)).map((item) =>
    item.kind === "round"
      ? `round:${item.round.revision}[${item.items.map((r) => r.key).join(",")}]`
      : item.kind === "episode_complete"
        ? `complete:${item.episode.id}`
        : `${item.kind}:${item.key}`,
  );

describe("round grouping", () => {
  it("leaves a transcript with no episodes exactly as it was", () => {
    const plain = ROWS.filter((row) => !row.episode);
    const items = buildTimelineItems(buildTimeline(plain, CHANNEL, []), [], {}, foldEpisodes(plain));
    expect(items.map((item) => item.kind)).toEqual(["message", "message"]);
  });

  it("collapses the whole episode into one band, in transcript order, then the marker", () => {
    // One band holding every row, carrying the newest wave's state.
    expect(kinds(ROWS)).toEqual([
      "message:h1",
      "round:2[h2,h3,h4,h5]",
      "complete:ep-1",
      "message:h6",
    ]);
  });

  it("tracks a live round that has no rows yet", () => {
    const frames = (
      [
        { type: "episode_opened", seq: 1, atMillis: 5, chatId: "engineering", episodeId: "ep-1", openedBySeq: 1, participants: ["engineer", "ceo"], plan: { kind: "hive", primaryId: "engineer", invitedIds: ["ceo"] } },
        { type: "round_started", seq: 2, atMillis: 8, chatId: "engineering", episodeId: "ep-1", revision: 0, agentIds: ["engineer", "ceo"] },
        { type: "round_committed", seq: 5, atMillis: 12, chatId: "engineering", episodeId: "ep-1", revision: 0, utterances: [{ agentId: "engineer", sequence: 2, kind: "post", messageSeq: 2 }, { agentId: "ceo", sequence: 3, kind: "post", messageSeq: 3 }] },
        { type: "round_started", seq: 6, atMillis: 15, chatId: "engineering", episodeId: "ep-1", revision: 1, agentIds: ["engineer"] },
      ] as EpisodeFrame[]
    ).reduce(reduceEpisodeFrame, EMPTY_EPISODE_FRAMES);
    const rows = ROWS.slice(0, 3);
    const items = buildTimelineItems(buildTimeline(rows, CHANNEL, []), [], {}, foldEpisodes(rows, frames, "engineering"));
    // One band, tracking the wave that has opened but not yet spoken: it owns
    // no rows of its own, so nothing else would carry its state onto the band.
    expect(items.map((item) => item.kind)).toEqual(["message", "round"]);
    const live = items[1];
    expect(live.kind === "round" && live.round.status).toBe("open");
    expect(live.kind === "round" && live.round.revision).toBe(1);
    // The band keeps the rows the earlier wave committed.
    expect(live.kind === "round" && live.items.map((r) => r.key)).toEqual(["h2", "h3"]);
    // No marker: the episode is still open.
    expect(items.some((item) => item.kind === "episode_complete")).toBe(false);
  });

  it("keeps an approval raised mid-episode in the channel at its own time", () => {
    const approvals = [
      { id: "a1", kind: "shell.run", at_millis: 15, agent: "engineer", thread: "engineering" },
    ] as never[];
    const items = buildTimelineItems(buildTimeline(ROWS, CHANNEL, []), approvals, {}, foldEpisodes(ROWS));
    const kindsOut = items.map((item) => item.kind);
    expect(kindsOut).toEqual(["message", "round", "approval", "episode_complete", "message"]);
  });

  it("puts the completion marker after the last round even when the frames say when", () => {
    const frames = (
      [
        { type: "episode_completed", seq: 9, atMillis: 31, chatId: "engineering", episodeId: "ep-1", revision: 3, completedBy: "ceo", rounds: 3, reason: "complete_episode" },
      ] as EpisodeFrame[]
    ).reduce(reduceEpisodeFrame, EMPTY_EPISODE_FRAMES);
    const items = buildTimelineItems(buildTimeline(ROWS, CHANNEL, []), [], {}, foldEpisodes(ROWS, frames, "engineering"));
    const marker = items.find((item) => item.kind === "episode_complete");
    expect(marker?.at).toBe(31);
    expect(items.indexOf(marker!)).toBe(items.length - 2);
  });
});
