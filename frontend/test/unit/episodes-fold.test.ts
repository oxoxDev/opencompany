import { describe, expect, it } from "vitest";

import type { EpisodeFrame, TurnBracketFrame } from "@/hooks/use-events";
import type { ChatMessage } from "@/lib/chat";
import { EMPTY_EPISODE_FRAMES, reduceEpisodeFrame } from "@/lib/episode-frames";
import { anyOpen, foldEpisodes, roundOf, workingSeats } from "@/lib/episodes";

/**
 * `foldEpisodes` (`lib/episodes.ts`): the transcript rows are the durable
 * record, the frames are the present tense, and the two must agree — a seat
 * with a row is committed whatever its last bracket said, and a frame never
 * conjures a row.
 */

const ROWS: ChatMessage[] = [
  { id: "h1", from: "you", byPerson: true, at: 0, text: "Ship it?" },
  { id: "h2", from: "company", channel: "engineer", at: 10, text: "Staging first.", episode: { id: "ep-1", revision: 0, kind: "post" } },
  { id: "h3", from: "company", channel: "ceo", at: 11, text: "How long?", episode: { id: "ep-1", revision: 0, kind: "post" } },
  { id: "h4", from: "company", channel: "engineer", at: 20, text: "Two days.", episode: { id: "ep-1", revision: 1, kind: "broadcast" } },
  { id: "h5", from: "company", channel: "ceo", at: 21, text: "Own the checklist?", audience: ["engineer"], episode: { id: "ep-1", revision: 1, kind: "dm", to: ["engineer"] } },
  { id: "h6", from: "company", channel: "ceo", at: 30, text: "Decision: staging.", episode: { id: "ep-1", revision: 2, kind: "complete_episode" } },
];

describe("foldEpisodes", () => {
  it("folds nothing from a transcript with no episode rows", () => {
    expect(foldEpisodes([ROWS[0], { id: "h9", from: "company", channel: "ceo", at: 5, text: "hi" }])).toEqual([]);
  });

  it("rebuilds rounds, seats and the completion from rows alone", () => {
    const [episode] = foldEpisodes(ROWS, undefined, "engineering");
    expect(episode.id).toBe("ep-1");
    expect(episode.chatId).toBe("engineering");
    expect(episode.status).toBe("completed");
    expect(episode.completedBy).toBe("ceo");
    expect(episode.completedAt).toBe(30);
    expect(episode.reason).toBe("complete_episode");
    expect(episode.participants).toEqual(["engineer", "ceo"]);
    expect(episode.rounds.map((r) => r.revision)).toEqual([0, 1, 2]);
    expect(episode.roundCount).toBe(3);
    expect(episode.live).toBe(false);
    const [first, second] = episode.rounds;
    expect(first.status).toBe("committed");
    expect(first.messageIds).toEqual(["h2", "h3"]);
    expect(first.startedAt).toBe(10);
    expect(first.committedAt).toBe(11);
    expect(first.seats.map((s) => [s.agentId, s.status, s.utterance?.kind])).toEqual([
      ["engineer", "committed", "post"],
      ["ceo", "committed", "post"],
    ]);
    expect(second.seats.find((s) => s.agentId === "ceo")?.utterance).toEqual({ kind: "dm", to: ["engineer"] });
    expect(episode.messageIds).toEqual(["h2", "h3", "h4", "h5", "h6"]);
  });

  it("layers the live frames: the plan, the root, and a seat still working", () => {
    const frames = (
      [
        {
          type: "episode_opened",
          seq: 1,
          atMillis: 5,
          chatId: "engineering",
          episodeId: "ep-1",
          openedBySeq: 1,
          participants: ["engineer", "ceo"],
          plan: { kind: "hive", primaryId: "engineer", invitedIds: ["ceo"] },
        },
        { type: "round_started", seq: 2, atMillis: 8, chatId: "engineering", episodeId: "ep-1", revision: 0, agentIds: ["ceo", "engineer"] },
        { type: "turn_started", seq: 3, atMillis: 9, chatId: "engineering", agentId: "ceo", episodeId: "ep-1", roundRevision: 0 },
        { type: "turn_started", seq: 4, atMillis: 9, chatId: "engineering", agentId: "engineer", episodeId: "ep-1", roundRevision: 0 },
        { type: "turn_settled", seq: 5, atMillis: 10, chatId: "engineering", agentId: "engineer", episodeId: "ep-1", roundRevision: 0, outcome: "committed" },
      ] as (EpisodeFrame | TurnBracketFrame)[]
    ).reduce(reduceEpisodeFrame, EMPTY_EPISODE_FRAMES);
    const rows = ROWS.slice(0, 2); // only the engineer's row has landed
    const [episode] = foldEpisodes(rows, frames, "engineering");
    expect(episode.live).toBe(true);
    expect(episode.status).toBe("open");
    expect(episode.rootMessageId).toBe("h1");
    expect(episode.plan?.kind).toBe("hive");
    expect(episode.openedAt).toBe(5);
    const [round] = episode.rounds;
    // The host's lane order wins over transcript order.
    expect(round.seats.map((s) => s.agentId)).toEqual(["ceo", "engineer"]);
    expect(round.status).toBe("open");
    expect(round.startedAt).toBe(8);
    expect(round.seats[0].status).toBe("working");
    expect(round.seats[1].status).toBe("committed");
    expect(round.seats[1].messageId).toBe("h2");
    expect(anyOpen([episode])).toBe(true);
    expect(workingSeats([episode])).toEqual(["ceo"]);
  });

  it("never lets a bracket demote a seat whose row is already in the transcript", () => {
    const frames = (
      [
        { type: "round_started", seq: 2, atMillis: 8, chatId: "engineering", episodeId: "ep-1", revision: 0, agentIds: ["engineer", "ceo"] },
        { type: "turn_settled", seq: 5, atMillis: 12, chatId: "engineering", agentId: "engineer", episodeId: "ep-1", roundRevision: 0, outcome: "failed" },
      ] as (EpisodeFrame | TurnBracketFrame)[]
    ).reduce(reduceEpisodeFrame, EMPTY_EPISODE_FRAMES);
    const [episode] = foldEpisodes(ROWS.slice(0, 3), frames, "engineering");
    expect(episode.rounds[0].seats.find((s) => s.agentId === "engineer")?.status).toBe("committed");
  });

  it("shows a round the frames opened before any row landed", () => {
    const frames = (
      [
        { type: "episode_opened", seq: 1, atMillis: 5, chatId: "engineering", episodeId: "ep-9", openedBySeq: 1, participants: ["engineer"], plan: { kind: "one", primaryId: "engineer" } },
        { type: "round_started", seq: 2, atMillis: 8, chatId: "engineering", episodeId: "ep-9", revision: 0, agentIds: ["engineer"] },
      ] as EpisodeFrame[]
    ).reduce(reduceEpisodeFrame, EMPTY_EPISODE_FRAMES);
    const [episode] = foldEpisodes([ROWS[0]], frames, "engineering");
    expect(episode.rounds).toHaveLength(1);
    expect(episode.rounds[0].messageIds).toEqual([]);
    expect(episode.rounds[0].seats[0].status).toBe("waiting");
  });

  it("keeps another desk's frames off this desk", () => {
    const frames = (
      [
        { type: "episode_opened", seq: 1, atMillis: 5, chatId: "content", episodeId: "ep-c", openedBySeq: 1, participants: ["writer"], plan: { kind: "one", primaryId: "writer" } },
      ] as EpisodeFrame[]
    ).reduce(reduceEpisodeFrame, EMPTY_EPISODE_FRAMES);
    expect(foldEpisodes([ROWS[0]], frames, "engineering")).toEqual([]);
    expect(foldEpisodes([ROWS[0]], frames, "content")).toHaveLength(1);
  });

  it("orders episodes by when they opened and finds a row's round", () => {
    const later: ChatMessage[] = [
      { id: "h20", from: "company", channel: "ceo", at: 100, text: "again", episode: { id: "ep-2", revision: 0, kind: "post" } },
    ];
    const episodes = foldEpisodes([...later, ...ROWS], undefined, "engineering");
    expect(episodes.map((e) => e.id)).toEqual(["ep-1", "ep-2"]);
    expect(roundOf(episodes, "h5")?.round.revision).toBe(1);
    expect(roundOf(episodes, "h1")).toBeUndefined();
  });
});
