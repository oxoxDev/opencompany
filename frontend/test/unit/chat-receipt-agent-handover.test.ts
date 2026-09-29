import { describe, expect, it } from "vitest";

import { receiptAgentAfter } from "@/views/room/ChatLiveReceipt";

/**
 * Who the live receipt names while the floor changes hands.
 *
 * One `messageSeq` is not one agent. A desk hand-off runs the delegate's turn
 * under the same query, and a hive episode passes the floor between seats for
 * the length of the deliberation — the episode's trigger seq is captured once
 * at its start while `agent_id` is an argument to each seat's turn. So a
 * receipt keyed to the query sees several agents over its lifetime, and has to
 * decide which one to show.
 *
 * It used to latch the first (`existing.agentId ?? frameAgentId`), which is
 * correct for a single-responder turn and wrong for every other shape: the row
 * read "Picked up by <whoever spoke first>" for the whole episode while
 * somebody else was visibly working. That is the one question the receipt
 * exists to answer, answered with a stale name.
 */
describe("the receipt follows the floor", () => {
  it("names the first agent to report", () => {
    expect(receiptAgentAfter(undefined, "a-riley")).toBe("a-riley");
  });

  it("moves to the seat that spoke most recently", () => {
    // The regression this file exists for: the old rule returned "a-riley".
    expect(receiptAgentAfter("a-riley", "a-dana")).toBe("a-dana");
  });

  it("follows a hand-off across a whole episode, not just the first change", () => {
    const seats = ["a-riley", "a-dana", "a-sam", "a-riley"];
    const named = seats.reduce<string | undefined>(
      (current, seat) => receiptAgentAfter(current, seat),
      undefined,
    );

    expect(named).toBe("a-riley");
  });

  it("keeps the current name when a frame carries no agent", () => {
    // Absence is not a hand-back. Clearing here would drop the line to "Sent"
    // mid-turn, reading as though the turn had been un-picked-up.
    expect(receiptAgentAfter("a-dana", undefined)).toBe("a-dana");
  });

  it("treats an empty agent id as absent rather than as a name", () => {
    expect(receiptAgentAfter("a-dana", "")).toBe("a-dana");
  });

  it("stays unnamed while nothing has reported an agent", () => {
    // The "Sent" state — `resolveReceiptAgentName` returns `undefined` here and
    // the line says "Sent" rather than inventing a teammate.
    expect(receiptAgentAfter(undefined, undefined)).toBeUndefined();
  });
});
