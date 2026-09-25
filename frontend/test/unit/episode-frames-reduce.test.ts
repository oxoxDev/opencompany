import { describe, expect, it } from "vitest";

import type { ChatMessage } from "@/lib/chat";
import type { EpisodeFrame, TurnBracketFrame } from "@/hooks/use-events";
import { withLiveExchanges, type Episode } from "@/lib/episodes";
import {
  allRounds,
  EMPTY_EPISODE_FRAMES,
  EPISODE_FRAME_CAP,
  episodesOf,
  reduceEpisodeFrame,
  type EpisodeFrames,
} from "@/lib/episode-frames";

/**
 * The bounded fold over the episode frames (`lib/episode-frames.ts`).
 *
 * Every rule here is one the room band depends on and a browser could only
 * report as "the lane looked wrong": a seat's bracket arriving before its
 * round's frame, a commit overriding a lost settle, a bracket with no episode
 * behind it, and the cap.
 */

const opened = (episodeId: string, seq = 1, chatId = "engineering"): EpisodeFrame => ({
  type: "episode_opened",
  seq,
  atMillis: seq * 10,
  chatId,
  episodeId,
  openedBySeq: seq - 1,
  participants: ["engineer", "ceo"],
  plan: { kind: "hive", primaryId: "engineer", invitedIds: ["ceo"] },
});
const started = (episodeId: string, revision: number, seq: number, agentIds = ["engineer", "ceo"]): EpisodeFrame => ({
  type: "round_started",
  seq,
  atMillis: seq * 10,
  chatId: "engineering",
  episodeId,
  revision,
  agentIds,
});
const turn = (
  type: "turn_started" | "turn_settled",
  agentId: string,
  seq: number,
  extra: Partial<TurnBracketFrame> = {},
): TurnBracketFrame => ({
  type,
  seq,
  atMillis: seq * 10,
  chatId: "engineering",
  agentId,
  episodeId: "ep-1",
  roundRevision: 0,
  ...extra,
});

function fold(frames: (EpisodeFrame | TurnBracketFrame)[], from: EpisodeFrames = EMPTY_EPISODE_FRAMES) {
  return frames.reduce(reduceEpisodeFrame, from);
}

describe("reduceEpisodeFrame", () => {
  it("returns the same object for a frame naming no episode", () => {
    const state = fold([opened("ep-1")]);
    const bracket = turn("turn_started", "engineer", 5, { episodeId: undefined, roundRevision: undefined });
    expect(reduceEpisodeFrame(state, bracket)).toBe(state);
    expect(reduceEpisodeFrame(EMPTY_EPISODE_FRAMES, bracket)).toBe(EMPTY_EPISODE_FRAMES);
  });

  it("opens an episode with its plan and seats, then a round with waiting lanes", () => {
    const state = fold([opened("ep-1"), started("ep-1", 0, 2)]);
    const episode = state.byId["ep-1"];
    expect(episode.plan).toEqual({ kind: "hive", primaryId: "engineer", invitedIds: ["ceo"] });
    expect(episode.participants).toEqual(["engineer", "ceo"]);
    expect(episode.status).toBe("open");
    const round = episode.rounds[0];
    expect(round.status).toBe("open");
    expect(round.agentIds).toEqual(["engineer", "ceo"]);
    expect(round.seats.engineer.status).toBe("waiting");
    expect(round.seats.ceo.status).toBe("waiting");
  });

  it("marks a seat working on its bracket and settles it by outcome", () => {
    const state = fold([
      opened("ep-1"),
      started("ep-1", 0, 2),
      turn("turn_started", "engineer", 3),
      turn("turn_started", "ceo", 4),
      turn("turn_settled", "ceo", 5, { outcome: "timed_out" }),
    ]);
    const { seats } = state.byId["ep-1"].rounds[0];
    expect(seats.engineer.status).toBe("working");
    expect(seats.engineer.startedAtMillis).toBe(30);
    expect(seats.ceo.status).toBe("timed_out");
    expect(seats.ceo.settledAtMillis).toBe(50);
  });

  it("mints the round when a seat's bracket lands before the round frame", () => {
    // The host emits the seat's bracket and the round's frame from different
    // tasks; the working state must survive the round frame landing after.
    const state = fold([opened("ep-1"), turn("turn_started", "engineer", 3), started("ep-1", 0, 4)]);
    const round = state.byId["ep-1"].rounds[0];
    expect(round.agentIds).toEqual(["engineer", "ceo"]);
    expect(round.seats.engineer.status).toBe("working");
    expect(round.seats.ceo.status).toBe("waiting");
    expect(round.startedAtMillis).toBe(30);
  });

  it("commits every seat the round names, and closes the ones it does not", () => {
    const state = fold([
      opened("ep-1"),
      started("ep-1", 0, 2),
      turn("turn_started", "engineer", 3),
      turn("turn_started", "ceo", 4),
      {
        type: "round_committed",
        seq: 6,
        atMillis: 60,
        chatId: "engineering",
        episodeId: "ep-1",
        revision: 0,
        utterances: [{ agentId: "engineer", sequence: 7, kind: "post", messageSeq: 7 }],
      },
      // A settle that lands after the commit must not demote the seat.
      turn("turn_settled", "engineer", 8, { outcome: "failed" }),
    ]);
    const round = state.byId["ep-1"].rounds[0];
    expect(round.status).toBe("committed");
    expect(round.committedAtMillis).toBe(60);
    expect(round.seats.engineer.status).toBe("committed");
    expect(round.seats.engineer.utterance).toEqual({ kind: "post", sequence: 7, messageSeq: 7, to: undefined });
    expect(round.seats.ceo.status).toBe("no_utterance");
  });

  it("records broadcasts, dms, referrals and the completion", () => {
    const state = fold([
      opened("ep-1"),
      {
        type: "broadcast_routed",
        seq: 3,
        atMillis: 30,
        chatId: "engineering",
        episodeId: "ep-1",
        revision: 1,
        agentId: "engineer",
        messageSeq: 9,
        plan: { kind: "one", primaryId: "ceo" },
        router: "jev",
      },
      { type: "dm_delivered", seq: 4, atMillis: 40, chatId: "engineering", episodeId: "ep-1", from: "ceo", to: ["engineer"], messageSeq: 10 },
      {
        type: "referral",
        seq: 5,
        atMillis: 50,
        chatId: "engineering",
        sequence: 11,
        toDesk: "content",
        target: "writer",
        asker: "engineer",
        direct: false,
        returning: false,
        episodeId: "ep-1",
        toEpisodeId: "ep-2",
      },
      { type: "episode_completed", seq: 6, atMillis: 60, chatId: "engineering", episodeId: "ep-1", revision: 2, completedBy: "ceo", rounds: 3, reason: "complete_episode", summarySeq: 12 },
    ]);
    const episode = state.byId["ep-1"];
    expect(episode.broadcasts).toHaveLength(1);
    expect(episode.broadcasts[0].router).toBe("jev");
    expect(episode.dms).toEqual([{ from: "ceo", to: ["engineer"], messageSeq: 10, atMillis: 40 }]);
    expect(episode.referrals[0].toEpisodeId).toBe("ep-2");
    expect(episode.status).toBe("completed");
    expect(episode.completedBy).toBe("ceo");
    expect(episode.roundCount).toBe(3);
    expect(episode.summarySeq).toBe(12);
  });

  it("ignores a referral that names no episode", () => {
    const state = fold([opened("ep-1")]);
    const next = reduceEpisodeFrame(state, {
      type: "referral",
      seq: 5,
      atMillis: 50,
      chatId: "engineering",
      sequence: 11,
      toDesk: "content",
      target: "writer",
      asker: "engineer",
      direct: false,
      returning: false,
    });
    expect(next).toBe(state);
  });

  it("narrows to one desk and lists every round", () => {
    const state = fold([
      opened("ep-1", 1, "engineering"),
      started("ep-1", 0, 2),
      opened("ep-2", 3, "content"),
      started("ep-2", 0, 4, ["writer", "ceo"]),
    ]);
    expect(episodesOf(state, "engineering").map((e) => e.id)).toEqual(["ep-1"]);
    expect(episodesOf(state, "content").map((e) => e.id)).toEqual(["ep-2"]);
    expect(allRounds(state).map(({ episode, round }) => `${episode.id}:${round.revision}`)).toEqual([
      "ep-1:0",
      "ep-2:0",
    ]);
  });

  it("keeps the fold under the cap, evicting completed episodes first", () => {
    let state = EMPTY_EPISODE_FRAMES;
    for (let i = 0; i < EPISODE_FRAME_CAP; i += 1) state = reduceEpisodeFrame(state, opened(`ep-${i}`, i + 1));
    // Complete the second one; it should be the first to go.
    state = reduceEpisodeFrame(state, {
      type: "episode_completed",
      seq: 500,
      atMillis: 5000,
      chatId: "engineering",
      episodeId: "ep-1",
      revision: 1,
      rounds: 1,
      reason: "complete_episode",
    });
    state = reduceEpisodeFrame(state, opened("ep-new", 600));
    expect(state.order).toHaveLength(EPISODE_FRAME_CAP);
    expect(state.byId["ep-1"]).toBeUndefined();
    expect(state.byId["ep-0"]).toBeDefined();
    expect(state.order.at(-1)).toBe("ep-new");
    // With nothing completed, the oldest goes.
    state = reduceEpisodeFrame(state, opened("ep-newer", 700));
    expect(state.byId["ep-0"]).toBeUndefined();
  });
});

/**
 * A private exchange between two seats.
 *
 * Its own rows are in the pair's channel, so a fold over this desk's
 * transcript never sees them — the reference frames are the only way the
 * room knows it happened, and the only thing the indicator can read.
 */
describe("conversations", () => {
  const askedFrame = (episodeId: string, root: number, seq: number): EpisodeFrame => ({
    type: "conversation_opened",
    seq,
    atMillis: seq * 10,
    chatId: "engineering",
    episodeId,
    conversationId: "dm:ceo+engineer",
    root,
    asker: "engineer",
    askee: "ceo",
  });
  const concludedFrame = (
    episodeId: string,
    root: number,
    seq: number,
    forced = false,
  ): EpisodeFrame => ({
    type: "conversation_concluded",
    seq,
    atMillis: seq * 10,
    chatId: "engineering",
    episodeId,
    conversationId: "dm:ceo+engineer",
    root,
    asker: "engineer",
    askee: "ceo",
    forced,
  });

  const fold = (frames: EpisodeFrame[]): EpisodeFrames =>
    frames.reduce((state, frame) => reduceEpisodeFrame(state, frame), EMPTY_EPISODE_FRAMES);

  it("is live until something ends it", () => {
    const state = fold([opened("ep-1"), askedFrame("ep-1", 14, 2)]);
    const [conversation] = Object.values(state.byId["ep-1"].conversations);

    expect(conversation.asker).toBe("engineer");
    expect(conversation.askee).toBe("ceo");
    expect(conversation.conversationId).toBe("dm:ceo+engineer");
    expect(conversation.endedAtMillis).toBeUndefined();
  });

  it("ends on the same root it opened on", () => {
    const state = fold([opened("ep-1"), askedFrame("ep-1", 14, 2), concludedFrame("ep-1", 14, 3)]);
    const conversations = Object.values(state.byId["ep-1"].conversations);

    expect(conversations).toHaveLength(1);
    expect(conversations[0].endedAtMillis).toBe(30);
    expect(conversations[0].forced).toBe(false);
  });

  /**
   * The case an indicator watching only for an answer would hang on: a
   * conversation that ran out of turns ends without one, and is still over.
   */
  it("ends without an answer when it was forced", () => {
    const state = fold([
      opened("ep-1"),
      askedFrame("ep-1", 14, 2),
      concludedFrame("ep-1", 14, 3, true),
    ]);
    const [conversation] = Object.values(state.byId["ep-1"].conversations);

    expect(conversation.endedAtMillis).toBe(30);
    expect(conversation.forced).toBe(true);
  });

  /** A fold that started mid-episode never saw the opening frame. */
  it("still ends usably when only the concluding frame was seen", () => {
    const state = fold([opened("ep-1"), concludedFrame("ep-1", 14, 3)]);
    const [conversation] = Object.values(state.byId["ep-1"].conversations);

    expect(conversation.asker).toBe("engineer");
    expect(conversation.askee).toBe("ceo");
    expect(conversation.endedAtMillis).toBe(30);
  });

  it("keeps two conversations apart by their root", () => {
    const state = fold([
      opened("ep-1"),
      askedFrame("ep-1", 14, 2),
      askedFrame("ep-1", 21, 3),
      concludedFrame("ep-1", 14, 4),
    ]);
    const conversations = Object.values(state.byId["ep-1"].conversations);

    expect(conversations).toHaveLength(2);
    expect(conversations.find((one) => one.root === 14)?.endedAtMillis).toBe(40);
    expect(conversations.find((one) => one.root === 21)?.endedAtMillis).toBeUndefined();
  });
});

/**
 * A completed episode must not read as a live one.
 *
 * The wave a seat completes from never gets a `round_committed` of its own —
 * the episode ends under it — so its seats stayed `working` and its status
 * stayed `open` for good. The band then printed "running together" beside the
 * completion marker with its lanes spinning, which is the one claim a live
 * indicator must never make falsely.
 */
describe("a completed episode settles what was still open", () => {
  it("closes the open round and stops its seats working", () => {
    const frames = (
      [
        { type: "episode_opened", seq: 1, atMillis: 1, chatId: "engineering", episodeId: "ep-9", openedBySeq: 1, participants: ["engineer", "ceo"], plan: { kind: "hive", primaryId: "engineer", invitedIds: ["ceo"] } },
        { type: "round_started", seq: 2, atMillis: 2, chatId: "engineering", episodeId: "ep-9", revision: 0, agentIds: ["engineer", "ceo"] },
        { type: "episode_completed", seq: 3, atMillis: 9, chatId: "engineering", episodeId: "ep-9", revision: 0, completedBy: "ceo", rounds: 1, reason: "complete_episode" },
      ] as EpisodeFrame[]
    ).reduce(reduceEpisodeFrame, EMPTY_EPISODE_FRAMES);

    const episode = frames.byId["ep-9"];
    expect(episode.status).toBe("completed");
    const round = episode.rounds[0];
    expect(round.status).toBe("committed");
    expect(Object.values(round.seats).map((seat) => seat.status)).toEqual([
      "no_utterance",
      "no_utterance",
    ]);
  });
});

/**
 * An exchange's lines arrive as ordinary reply rows in the pair channel.
 *
 * The desk never shows those rows, so the fold is the only thing that sees
 * them, and it is what lets the indicator say what has been said rather than
 * only that two seats are talking. It used to say "1 message so far" for the
 * whole exchange and jump to the full transcript on reload, because the frame
 * carried no `episodeId` and the fold drops any frame that names no episode.
 */
describe("a pair-channel row joins the exchange it belongs to", () => {
  const PAIR = "dm:ceo+engineer";
  const open = (root: number) => ({
    type: "conversation_opened", seq: root + 1, atMillis: root + 1, chatId: "engineering",
    episodeId: "ep-7", conversationId: PAIR, root, asker: "engineer", askee: "ceo",
  });
  const row = (seq: number, agentId: string, text: string, parentId?: string) => ({
    type: "agent_reply", seq, atMillis: seq, chatId: PAIR, episodeId: "ep-7", agentId, text, parentId,
  });

  it("claims the ask that landed before it, and takes its parent as the anchor", () => {
    const frames = ([
      row(10, "engineer", "what are the constraints?", "5"),
      open(10),
    ] as EpisodeFrame[]).reduce(reduceEpisodeFrame, EMPTY_EPISODE_FRAMES);
    const held = frames.byId["ep-7"].conversations[10];
    expect(held.anchorId).toBe(5);
    expect(held.lines.map((line) => [line.authorId, line.outbound])).toEqual([["engineer", true]]);
  });

  it("does not take the conclusion as a line: it restates the askee's last one", () => {
    const frames = ([
      row(10, "engineer", "what are the constraints?", "5"),
      open(10),
      row(11, "ceo", "none on file", "10"),
      { ...row(12, "ceo", "concluded our conversation: none on file", "10"), utteranceKind: "dm" },
    ] as EpisodeFrame[]).reduce(reduceEpisodeFrame, EMPTY_EPISODE_FRAMES);
    const held = frames.byId["ep-7"].conversations[10];
    expect(held.lines.map((line) => line.authorId)).toEqual(["engineer", "ceo"]);
  });

  it("appends each later row, and knows which way it went", () => {
    const frames = ([
      row(10, "engineer", "what are the constraints?", "5"),
      open(10),
      row(12, "ceo", "none on file", "10"),
      row(14, "engineer", "anything at all?", "10"),
    ] as EpisodeFrame[]).reduce(reduceEpisodeFrame, EMPTY_EPISODE_FRAMES);
    const held = frames.byId["ep-7"].conversations[10];
    expect(held.lines.map((line) => [line.authorId, line.outbound])).toEqual([
      ["engineer", true],
      ["ceo", false],
      ["engineer", true],
    ]);
  });

  it("leaves a desk row alone", () => {
    const frames = ([
      row(10, "engineer", "asking", "5"),
      open(10),
      { ...row(12, "ceo", "said on the desk", "10"), chatId: "engineering" },
    ] as EpisodeFrame[]).reduce(reduceEpisodeFrame, EMPTY_EPISODE_FRAMES);
    expect(frames.byId["ep-7"].conversations[10].lines).toHaveLength(1);
  });
});

/**
 * The live copy of an exchange stands in only until the host places one.
 *
 * They anchor differently by design -- the host hangs a concluded exchange on
 * the asker's report, and live there is no report yet, so it hangs on the row
 * that sent the seats aside. Deduplicating per row missed that and drew the
 * same exchange twice, on two different rows, the moment it concluded.
 */
describe("withLiveExchanges", () => {
  const conversation = {
    root: 10, asker: "engineer", askee: "ceo", conversationId: "dm:ceo+engineer",
    openedAtMillis: 1, anchorId: 5,
    lines: [{ authorId: "engineer", text: "asking", outbound: true }],
  };
  const episode = { conversations: [conversation] } as unknown as Episode;
  const row = (id: string, over: Record<string, unknown> = {}) =>
    ({ id, from: "company", at: 1, text: "…", ...over }) as unknown as ChatMessage;

  it("attaches a running exchange to the row that provoked it", () => {
    const out = withLiveExchanges([row("h5")], [episode]);
    expect(out[0].agentConversations?.map((one) => one.conversationId)).toEqual(["dm:ceo+engineer"]);
  });

  it("stands down once the host has placed the same exchange on another row", () => {
    const settled = row("h9", {
      agentConversations: [
        { root: 10, askerId: "engineer", askeeId: "ceo", conversationId: "dm:ceo+engineer", concluded: true, forced: false, lines: [] },
      ],
    });
    const out = withLiveExchanges([row("h5"), settled], [episode]);
    expect(out[0].agentConversations ?? []).toEqual([]);
    expect(out[1].agentConversations).toHaveLength(1);
  });
});
