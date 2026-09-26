import { describe, expect, it, vi } from "vitest";

import { handleEvent, type CompanyStreamEvent } from "@/hooks/use-events";

/**
 * Which subscriber each episode frame reaches (`hooks/use-events.ts`).
 *
 * This file has been bitten repeatedly by a frame the host was already sending
 * falling through to `default:` and vanishing with nothing to debug (#464,
 * #371, #384), so the routing of every new frame is pinned here directly
 * rather than only through a mounted hook. The episode frames are the newest
 * such set, and the turn bracket has been on the wire since #983 with no arm
 * at all.
 */

function subscribers() {
  return {
    onEpisodeEvent: vi.fn(),
    onTurnBracket: vi.fn(),
    onReferral: vi.fn(),
    onTurnEvent: vi.fn(),
    onAgentReply: vi.fn(),
    onDeskRoutingConfigured: vi.fn(),
    onTaskEvent: vi.fn(),
  };
}

const BASE = { seq: 1, atMillis: 1, chatId: "engineering", episodeId: "ep-1" };

describe("episode frames", () => {
  it.each([
    { type: "episode_opened", ...BASE, openedBySeq: 1, participants: ["engineer"], plan: { kind: "one", primaryId: "engineer" } },
    { type: "round_started", ...BASE, revision: 0, agentIds: ["engineer"] },
    { type: "round_committed", ...BASE, revision: 0, utterances: [] },
    { type: "broadcast_routed", ...BASE, revision: 0, agentId: "engineer", messageSeq: 2, plan: { kind: "one", primaryId: "ceo" }, router: "fallback" },
    { type: "dm_delivered", ...BASE, from: "engineer", to: ["ceo"], messageSeq: 3 },
    // The a2a pair. These are the only frames that tell this desk an exchange
    // happened at all — the conversation's rows live in the pair channel and
    // never reach the desk's stream — so a missing arm costs the whole
    // indicator with nothing on screen to hint at it.
    { type: "conversation_opened", ...BASE, conversationId: "dm:ceo+engineer", root: 33, asker: "engineer", askee: "ceo" },
    { type: "conversation_concluded", ...BASE, conversationId: "dm:ceo+engineer", root: 33, asker: "engineer", askee: "ceo", forced: false },
    { type: "episode_completed", ...BASE, revision: 1, rounds: 1, reason: "complete_episode" },
  ] as CompanyStreamEvent[])("routes $type to onEpisodeEvent and nowhere else", (event) => {
    const subs = subscribers();
    handleEvent(event, subs);
    expect(subs.onEpisodeEvent).toHaveBeenCalledWith(event);
    expect(subs.onTurnBracket).not.toHaveBeenCalled();
    expect(subs.onTurnEvent).not.toHaveBeenCalled();
    expect(subs.onTaskEvent).not.toHaveBeenCalled();
  });

  it("routes the turn bracket to onTurnBracket, not to the tool-row subscriber", () => {
    const subs = subscribers();
    const started: CompanyStreamEvent = { type: "turn_started", seq: 1, atMillis: 1, chatId: "engineering", agentId: "engineer", episodeId: "ep-1", roundRevision: 0 };
    const settled: CompanyStreamEvent = { type: "turn_settled", seq: 2, atMillis: 2, chatId: "engineering", agentId: "engineer", outcome: "committed" };
    handleEvent(started, subs);
    handleEvent(settled, subs);
    expect(subs.onTurnBracket).toHaveBeenNthCalledWith(1, started);
    expect(subs.onTurnBracket).toHaveBeenNthCalledWith(2, settled);
    expect(subs.onTurnEvent).not.toHaveBeenCalled();
    expect(subs.onEpisodeEvent).not.toHaveBeenCalled();
  });

  it("hands a referral to both the thread re-reader and the episode fold", () => {
    const subs = subscribers();
    const event: CompanyStreamEvent = {
      type: "referral",
      seq: 1,
      atMillis: 1,
      chatId: "engineering",
      sequence: 4,
      toDesk: "content",
      target: "writer",
      asker: "engineer",
      direct: false,
      returning: false,
      episodeId: "ep-1",
      toEpisodeId: "ep-2",
    };
    handleEvent(event, subs);
    expect(subs.onReferral).toHaveBeenCalledWith(event);
    expect(subs.onEpisodeEvent).toHaveBeenCalledWith(event);
  });

  it("routes desk_routing_configured to its own subscriber", () => {
    const subs = subscribers();
    const event: CompanyStreamEvent = { type: "desk_routing_configured", seq: 1, atMillis: 1, deskId: "engineering", reset: false };
    handleEvent(event, subs);
    expect(subs.onDeskRoutingConfigured).toHaveBeenCalledWith(event);
    expect(subs.onTaskEvent).not.toHaveBeenCalled();
  });

  it("carries a reply's episode and audience through to the transcript", () => {
    const subs = subscribers();
    handleEvent(
      {
        type: "agent_reply",
        seq: 9,
        atMillis: 1,
        chatId: "engineering",
        agentId: "ceo",
        text: "Own the checklist?",
        audience: ["engineer"],
        episode: { id: "ep-1", revision: 1, kind: "dm", to: ["engineer"] },
      },
      subs,
    );
    expect(subs.onAgentReply).toHaveBeenCalledWith(
      expect.objectContaining({
        seq: 9,
        audience: ["engineer"],
        episode: { id: "ep-1", revision: 1, kind: "dm", to: ["engineer"] },
      }),
    );
  });

  it("still passes a reply from a host that predates episodes", () => {
    const subs = subscribers();
    handleEvent({ type: "agent_reply", seq: 9, atMillis: 1, chatId: "engineering", agentId: "ceo", text: "hi" }, subs);
    const [payload] = subs.onAgentReply.mock.calls[0];
    expect(payload.episode).toBeUndefined();
    expect(payload.audience).toBeUndefined();
  });
});
