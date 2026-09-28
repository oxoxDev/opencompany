import { describe, expect, it } from "vitest";

import type { EpisodeFrame, TurnBracketFrame } from "@/hooks/use-events";
import {
  coordinationObservations,
  coordinationSummary,
  EMPTY_TURN_LEDGER,
  planTargets,
  reduceTurnBracket,
  workingAgents,
} from "@/lib/coordination";
import { EMPTY_EPISODE_FRAMES, reduceEpisodeFrame } from "@/lib/episode-frames";

/**
 * `lib/coordination.ts`: the turn ledger's peak and same-agent overlap count,
 * and the comms edges the episode fold implies. These are the console's copy
 * of the numbers `scripts/measure-coordination.mjs` prints.
 */

const bracket = (
  type: "turn_started" | "turn_settled",
  agentId: string,
  seq: number,
  extra: Partial<TurnBracketFrame> = {},
): TurnBracketFrame => ({ type, seq, atMillis: seq * 10, chatId: "engineering", agentId, ...extra });

describe("reduceTurnBracket", () => {
  it("counts the peak of open turns and closes them by turn id", () => {
    const ledger = [
      bracket("turn_started", "engineer", 1, { turnId: "t1" }),
      bracket("turn_started", "writer", 2, { turnId: "t2" }),
      bracket("turn_settled", "engineer", 3, { turnId: "t1" }),
      bracket("turn_started", "ceo", 4, { turnId: "t3" }),
      bracket("turn_settled", "writer", 5, { turnId: "t2" }),
      bracket("turn_settled", "ceo", 6, { turnId: "t3" }),
    ].reduce(reduceTurnBracket, EMPTY_TURN_LEDGER);
    expect(ledger.peak).toBe(2);
    expect(ledger.open).toEqual([]);
    expect(ledger.closed.map((t) => `${t.agentId}:${t.startedAtMillis}-${t.settledAtMillis}`)).toEqual([
      "engineer:10-30",
      "writer:20-50",
      "ceo:40-60",
    ]);
    expect(ledger.sameAgentOverlaps).toBe(0);
  });

  it("falls back to the agent when a settle carries no turn id, and ignores a stray settle", () => {
    const ledger = [
      bracket("turn_started", "engineer", 1),
      bracket("turn_settled", "engineer", 2),
    ].reduce(reduceTurnBracket, EMPTY_TURN_LEDGER);
    expect(ledger.open).toEqual([]);
    expect(reduceTurnBracket(ledger, bracket("turn_settled", "ceo", 3))).toBe(ledger);
  });

  it("counts a second turn opening on an agent whose first is still open", () => {
    const ledger = [
      bracket("turn_started", "ceo", 1, { turnId: "a" }),
      bracket("turn_started", "ceo", 2, { turnId: "b" }),
    ].reduce(reduceTurnBracket, EMPTY_TURN_LEDGER);
    expect(ledger.sameAgentOverlaps).toBe(1);
    expect(workingAgents(ledger)).toEqual(["ceo"]);
  });
});

describe("coordinationObservations", () => {
  const frames = (
    [
      { type: "episode_opened", seq: 1, atMillis: 10, chatId: "engineering", episodeId: "ep-1", openedBySeq: 1, participants: ["engineer", "ceo"], plan: { kind: "hive", primaryId: "engineer", invitedIds: ["ceo"] } },
      { type: "broadcast_routed", seq: 2, atMillis: 20, chatId: "engineering", episodeId: "ep-1", revision: 1, agentId: "engineer", messageSeq: 5, plan: { kind: "hive", primaryId: "ceo", invitedIds: ["engineer", "ceo"] }, router: "jev" },
      { type: "dm_delivered", seq: 3, atMillis: 30, chatId: "engineering", episodeId: "ep-1", from: "ceo", to: ["engineer"], messageSeq: 6 },
      { type: "referral", seq: 4, atMillis: 40, chatId: "engineering", sequence: 7, toDesk: "content", target: "writer", asker: "engineer", direct: false, returning: false, episodeId: "ep-1", toEpisodeId: "ep-2" },
      { type: "referral", seq: 5, atMillis: 50, chatId: "engineering", sequence: 8, toDesk: "content", target: "writer", asker: "engineer", direct: false, returning: true, episodeId: "ep-1", toEpisodeId: "ep-2" },
      { type: "episode_completed", seq: 6, atMillis: 60, chatId: "engineering", episodeId: "ep-1", revision: 2, completedBy: "ceo", rounds: 2, reason: "complete_episode" },
      { type: "episode_opened", seq: 7, atMillis: 70, chatId: "content", episodeId: "ep-2", openedBySeq: 7, participants: ["writer"], plan: { kind: "one", primaryId: "writer" } },
      { type: "round_started", seq: 8, atMillis: 80, chatId: "content", episodeId: "ep-2", revision: 0, agentIds: ["writer"] },
    ] as EpisodeFrame[]
  ).reduce(reduceEpisodeFrame, EMPTY_EPISODE_FRAMES);

  it("draws a broadcast to every seat but its author, a dm to its recipients, and a referral to the desk", () => {
    const observations = coordinationObservations(frames);
    expect(observations).toEqual([
      { kind: "spoke", from: "engineer", to: "ceo", via: "broadcast", atMillis: 20 },
      { kind: "spoke", from: "ceo", to: "engineer", via: "dm", atMillis: 30 },
      { kind: "spoke", from: "engineer", to: "content", via: "referral", atMillis: 40 },
    ]);
  });

  it("adds a speaking observation per agent with an open turn", () => {
    const ledger = reduceTurnBracket(EMPTY_TURN_LEDGER, bracket("turn_started", "writer", 9));
    expect(coordinationObservations(frames, ledger).at(-1)).toEqual({ kind: "speaking", agentId: "writer" });
  });

  it("summarises the way the measurement script does", () => {
    const ledger = [
      bracket("turn_started", "engineer", 1, { turnId: "a" }),
      bracket("turn_started", "writer", 2, { turnId: "b" }),
      bracket("turn_settled", "engineer", 3, { turnId: "a" }),
      bracket("turn_settled", "writer", 4, { turnId: "b" }),
    ].reduce(reduceTurnBracket, EMPTY_TURN_LEDGER);
    expect(coordinationSummary(frames, ledger)).toEqual({
      peakConcurrentTurns: 2,
      sameAgentOverlaps: 0,
      episodesOpened: 2,
      episodesCompleted: 1,
      roundsPerEpisode: [2, 1],
      broadcasts: 1,
      dms: 1,
      referrals: 1,
      pairs: ["ceo→engineer", "engineer→ceo", "engineer→content"],
    });
  });

  it("lists a plan's seats once, primary first", () => {
    expect(planTargets({ kind: "hive", primaryId: "ceo", invitedIds: ["engineer", "ceo"] })).toEqual(["ceo", "engineer"]);
    expect(planTargets({ kind: "clarify" })).toEqual([]);
  });
});
