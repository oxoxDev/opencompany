import { describe, expect, it, vi } from "vitest";

import { drainReReadQueue, type PendingReRead } from "@/lib/re-read-queue";

/**
 * A parked entry, keyed the way `app-shell` keys them (state key + settled
 * turn) so two threads of one desk park separately.
 */
const park = (desk: string, over: Partial<PendingReRead> = {}) =>
  new Map<string, PendingReRead>([
    [`${over.stateKey ?? desk}\u0000${over.turnId ?? ""}`, { desk, stateKey: desk, ...over }],
  ]);

describe("drainReReadQueue (issue #1701)", () => {
  it("leaves a parked thread parked while its channel is unknown", () => {
    const pending = park("thread-a");
    const reRead = vi.fn();
    drainReReadQueue(pending, {}, reRead);
    expect(reRead).toHaveBeenCalledTimes(0);
    expect(pending.size).toBe(1);
  });

  it("replays a parked thread exactly once when its channel appears", () => {
    const pending = park("thread-a");
    const reRead = vi.fn();
    drainReReadQueue(pending, { "thread-a": "general" }, reRead);
    expect(reRead).toHaveBeenCalledTimes(1);
    expect(reRead).toHaveBeenCalledWith("thread-a", undefined, "thread-a");
    expect(pending.size).toBe(0);
  });

  it("does not re-read on a second drain against the same populated map", () => {
    const pending = park("thread-a");
    const reRead = vi.fn();
    const map = { "thread-a": "general" };
    drainReReadQueue(pending, map, reRead);
    drainReReadQueue(pending, map, reRead);
    expect(reRead).toHaveBeenCalledTimes(1);
  });

  it("does not replay a stale thread cleared on company switch", () => {
    // Old company parked `thread-a`; the switch clears the queue before the new
    // company's channel map lands — even one that reuses the `general` id.
    const pending = park("thread-a");
    pending.clear();
    const reRead = vi.fn();
    drainReReadQueue(pending, { "thread-a": "general" }, reRead);
    expect(reRead).toHaveBeenCalledTimes(0);
  });

  it("never parks a thread that settles after the map is populated", () => {
    // The fast path in the callback folds directly and never enqueues; the
    // queue stays empty, so a drain is a no-op.
    const pending = new Map<string, PendingReRead>();
    const reRead = vi.fn();
    drainReReadQueue(pending, { "thread-a": "general" }, reRead);
    expect(reRead).toHaveBeenCalledTimes(0);
    expect(pending.size).toBe(0);
  });

  it("replays a parked #general thread once its channel is mapped", () => {
    const pending = park("general");
    const reRead = vi.fn();
    drainReReadQueue(pending, { general: "general" }, reRead);
    expect(reRead).toHaveBeenCalledWith("general", undefined, "general");
    expect(pending.size).toBe(0);
  });

  /**
   * Codex review on #2044. A replay carries the state key its cleanup is filed
   * under, not just the desk — the two differ for a threaded turn, and a
   * desk-only replay clears whatever sits under the desk, which on the cold
   * load this queue exists for can be a live unthreaded send's own state.
   */
  it("replays a threaded park with both identities and its settled turn", () => {
    const pending = park("engineering", { stateKey: "engineering#41", turnId: "t-1" });
    const reRead = vi.fn();
    drainReReadQueue(pending, { engineering: "engineering-desk" }, reRead);
    // The desk is what the host is asked about; the state key is what the
    // cleanup is addressed by; the turn id is excluded from its guard.
    expect(reRead).toHaveBeenCalledWith("engineering", "t-1", "engineering#41");
    expect(pending.size).toBe(0);
  });

  it("parks two threads of one desk separately rather than collapsing them", () => {
    // Keyed on the pair. Keyed on the desk alone — which is what a Set of ids
    // amounted to — the second park would overwrite the first and one settled
    // thread would never be replayed.
    const pending = new Map<string, PendingReRead>([
      ["engineering#41\u0000t-1", { desk: "engineering", stateKey: "engineering#41", turnId: "t-1" }],
      ["engineering#77\u0000t-2", { desk: "engineering", stateKey: "engineering#77", turnId: "t-2" }],
    ]);
    const reRead = vi.fn();
    drainReReadQueue(pending, { engineering: "engineering-desk" }, reRead);
    expect(reRead).toHaveBeenCalledTimes(2);
    expect(reRead).toHaveBeenCalledWith("engineering", "t-1", "engineering#41");
    expect(reRead).toHaveBeenCalledWith("engineering", "t-2", "engineering#77");
  });
});