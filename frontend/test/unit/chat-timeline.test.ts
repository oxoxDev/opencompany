import { describe, expect, it } from "vitest";

import { buildTimeline, type Channel } from "@/views/chat/model";
import type { ChatMessage } from "@/lib/chat";

/**
 * Timeline folding by parent.
 *
 * A reply must be folded into its parent rather than laid out inline. When that
 * goes wrong the transcript still renders — thread replies simply appear in the
 * channel, out of order and stripped of the question they answer, which reads
 * as the company talking to itself. Nothing throws, so only an assertion
 * catches it.
 */

const CHANNEL: Channel = {
  id: "engineering",
  name: "engineering",
  voice: "Engineering",
  kind: "channel",
  purpose: "",
};

const MINUTE = 60_000;
/** A fixed instant, so day-boundary grouping cannot drift with the clock. */
const T0 = new Date("2026-03-02T10:00:00Z").getTime();

function message(over: Partial<ChatMessage> & Pick<ChatMessage, "id">): ChatMessage {
  return { from: "you", text: "…", at: T0, ...over };
}

describe("buildTimeline", () => {
  it("keeps a reply out of the channel and hangs it on its parent", () => {
    const entries = buildTimeline(
      [
        message({ id: "a", text: "can we ship?" }),
        message({ id: "b", text: "yes", parentId: "a" }),
      ],
      CHANNEL,
      [],
    );

    // One row, not two: the reply is folded, not laid out inline.
    expect(entries).toHaveLength(1);
    expect(entries[0].message.id).toBe("a");
    expect(entries[0].replies.map((r) => r.id)).toEqual(["b"]);
  });

  it("keeps replies in order under their parent", () => {
    const entries = buildTimeline(
      [
        message({ id: "a" }),
        message({ id: "b", parentId: "a", at: T0 + 1 }),
        message({ id: "c", parentId: "a", at: T0 + 2 }),
      ],
      CHANNEL,
      [],
    );
    expect(entries[0].replies.map((r) => r.id)).toEqual(["b", "c"]);
  });

  it("renders a grandchild nowhere: the fold is exactly one level deep", () => {
    // The property the Rust side depends on (issue #435). Only a parentless
    // message becomes an entry, and only an entry carries a `replies` list, so
    // a reply-to-a-reply is bucketed under a row that is never itself rendered
    // and disappears — silently, with nothing thrown and the transcript looking
    // complete.
    //
    // `cycle_conversation` in `src/runtime/cycle.rs` parents an approval
    // continuation to the thread *root* rather than to the message that raised
    // it, and this is the whole reason: parenting to the raiser would put the
    // continuation exactly here whenever the raiser is itself a thread reply.
    // The existing "parent is not in this channel" case below does NOT pin
    // this — there the parent is absent from the transcript entirely, so it
    // would keep passing if the console ever grew a second fold level and
    // quietly made #435's routing choice unnecessary.
    const entries = buildTimeline(
      [
        message({ id: "a", text: "can we ship?" }),
        message({ id: "b", text: "yes", parentId: "a", at: T0 + 1 }),
        message({ id: "c", text: "when?", parentId: "b", at: T0 + 2 }),
      ],
      CHANNEL,
      [],
    );

    // The root is the only row, and it holds only its direct reply.
    expect(entries).toHaveLength(1);
    expect(entries[0].message.id).toBe("a");
    expect(entries[0].replies.map((r) => r.id)).toEqual(["b"]);
    // And `c` is reachable from nowhere in the rendered output.
    const rendered = entries.flatMap((e) => [e.message.id, ...e.replies.map((r) => r.id)]);
    expect(rendered).not.toContain("c");
  });

  it("drops a reply whose parent is not in this channel rather than promoting it", () => {
    // A reply pointing at an id this transcript does not hold must not fall
    // back to the channel — that is precisely the "the console lost my thread"
    // symptom reconcileIds exists to prevent.
    const entries = buildTimeline([message({ id: "b", parentId: "missing" })], CHANNEL, []);
    expect(entries).toHaveLength(0);
  });

  it("groups consecutive lines from one sender into a run", () => {
    const entries = buildTimeline(
      [
        message({ id: "a", from: "you", at: T0 }),
        message({ id: "b", from: "you", at: T0 + MINUTE }),
      ],
      CHANNEL,
      [],
    );
    expect(entries[0].continuation).toBe(false);
    expect(entries[1].continuation).toBe(true);
  });

  it("breaks the run once the grouping window has passed", () => {
    const entries = buildTimeline(
      [
        message({ id: "a", from: "you", at: T0 }),
        message({ id: "b", from: "you", at: T0 + 6 * MINUTE }),
      ],
      CHANNEL,
      [],
    );
    expect(entries[1].continuation).toBe(false);
  });

  it("ends a run after a row that carries replies", () => {
    // Otherwise the thread's summary row sits between two lines that read as a
    // single utterance.
    const entries = buildTimeline(
      [
        message({ id: "a", from: "you", at: T0 }),
        message({ id: "r", from: "you", at: T0 + 1, parentId: "a" }),
        message({ id: "b", from: "you", at: T0 + MINUTE }),
      ],
      CHANNEL,
      [],
    );
    expect(entries[0].replies).toHaveLength(1);
    expect(entries[1].continuation).toBe(false);
  });

  it("starts a new day with a divider, and never continues a run across one", () => {
    const nextDay = T0 + 24 * 60 * MINUTE;
    const entries = buildTimeline(
      [
        message({ id: "a", from: "you", at: T0 }),
        message({ id: "b", from: "you", at: nextDay }),
      ],
      CHANNEL,
      [],
    );
    expect(entries[0].dayLabel).toBeDefined();
    expect(entries[1].dayLabel).toBeDefined();
    expect(entries[1].continuation).toBe(false);
  });
});
