import { describe, expect, it } from "vitest";

import type { NotificationDto } from "@/api/types";
import {
  mentionCountsByChannel,
  mentionsToClear,
  renderedChannelIdForContext,
  threadsToReReadForMentions,
} from "@/lib/mention-badge";

/**
 * The mention badge is the durable half of the feature: the SSE feed only
 * reaches an open browser, so a mention that landed overnight is visible here
 * and nowhere else. Getting the counting wrong therefore does not degrade the
 * feature, it removes it — a badge that clears too eagerly loses the summons
 * entirely, with nothing left to notice it by.
 */

const note = (over: Partial<NotificationDto> & Pick<NotificationDto, "id">): NotificationDto => ({
  kind: "mention",
  subjectKind: "message",
  subjectId: "42",
  title: "someone mentioned you",
  createdAt: 1,
  context: "engineering",
  ...over,
});

describe("mentionCountsByChannel", () => {
  it("counts unread mentions per channel", () => {
    expect(
      mentionCountsByChannel([
        note({ id: "a" }),
        note({ id: "b" }),
        note({ id: "c", context: "design" }),
      ]),
    ).toEqual({ engineering: 2, design: 1 });
  });

  it("ignores a mention that has been read", () => {
    expect(
      mentionCountsByChannel([note({ id: "a", readAt: 5 }), note({ id: "b" })]),
    ).toEqual({ engineering: 1 });
  });

  /**
   * `kind`, not `subjectKind`. A later notification about a message that is not
   * a mention — a reply, a reaction — must not silently start badging as one.
   */
  it("counts only rows whose kind is a mention", () => {
    expect(
      mentionCountsByChannel([
        note({ id: "a", kind: "reply" }),
        note({ id: "b" }),
      ]),
    ).toEqual({ engineering: 1 });
  });

  it("drops a row with no channel rather than placing it arbitrarily", () => {
    expect(mentionCountsByChannel([note({ id: "a", context: undefined })])).toEqual({});
  });

  it("badges #general under its own id", () => {
    expect(mentionCountsByChannel([note({ id: "a", context: "general" })])).toEqual({
      general: 1,
    });
  });

  it("no longer folds legacy General spellings onto #general", () => {
    expect(
      mentionCountsByChannel([
        note({ id: "a", context: "main" }),
        note({ id: "b", context: "General" }),
      ]),
    ).toEqual({ main: 1, General: 1 });
  });

  it("is empty for an empty feed", () => {
    expect(mentionCountsByChannel([])).toEqual({});
  });

  /**
   * A host answering `GET {scope}/notifications` with something other than the
   * documented shape must not take the console down.
   *
   * This is not hypothetical: a mocked host that returns a bare `[]` for
   * unmatched routes made `feed.notifications` `undefined`, and iterating it
   * threw during render — blanking the entire app and failing every unrelated
   * spec in the file. The badge is the least important thing on the screen and
   * has to fail like it.
   */
  it("survives a caller handing it something that is not a list", () => {
    for (const bad of [undefined, null, "nope", 7, {}]) {
      expect(
        mentionCountsByChannel(bad as unknown as NotificationDto[]),
      ).toEqual({});
    }
  });
});

describe("mentionsToClear", () => {
  const feed = [
    note({ id: "eng-1" }),
    note({ id: "eng-2" }),
    note({ id: "eng-read", readAt: 9 }),
    note({ id: "design-1", context: "design" }),
  ];

  /**
   * The case a bare "mark all read" gets wrong: opening one channel must not
   * clear a summons waiting in another.
   */
  it("clears only the opened channel's unread mentions", () => {
    expect(mentionsToClear(feed, "engineering")).toEqual(["eng-1", "eng-2"]);
    expect(mentionsToClear(feed, "design")).toEqual(["design-1"]);
  });

  it("clears #general's mentions when #general is opened", () => {
    expect(mentionsToClear([note({ id: "a", context: "general" })], "general")).toEqual(["a"]);
    expect(mentionsToClear([note({ id: "a", context: "general" })], "engineering")).toEqual([]);
  });

  it("does not clear a legacy `main` mention when #general is opened", () => {
    expect(mentionsToClear([note({ id: "a", context: "main" })], "general")).toEqual([]);
  });
  it("returns nothing for a channel with no mentions", () => {
    expect(mentionsToClear(feed, "random")).toEqual([]);
  });

  /**
   * A mention inside a thread reply must not clear on channel-open alone: the
   * main timeline folds replies into their parent (`buildTimeline`), so a
   * collapsed thread hides the text even while the channel is on screen —
   * clearing it would lose the summons without the person ever seeing it. The
   * notification names the message by its host sequence, which the loaded
   * transcript's reply map keys by the console's `h<seq>` id.
   */
  describe("with a mention inside a thread reply", () => {
    const replies = new Map([["h42", "h7"]]);
    const feed = [note({ id: "threaded", context: "engineering", subjectId: "42" })];

    it("keeps it unread while the channel is open but its thread is collapsed", () => {
      expect(mentionsToClear(feed, "engineering", replies, null)).toEqual([]);
    });

    it("clears it the moment the thread panel makes the reply visible", () => {
      expect(
        mentionsToClear(feed, "engineering", replies, "h7"),
      ).toEqual(["threaded"]);
    });

    it("clears a different thread's mention only when that thread is the open one", () => {
      // A reply under another parent is still hidden: opening a sibling thread
      // must not clear it either.
      expect(
        mentionsToClear(feed, "engineering", replies, "h99"),
      ).toEqual([]);
    });
  });

  it("still clears a top-level mention on channel open", () => {
    // A message with no parent id is on screen the moment the channel is: the
    // reply gate must not hold it hostage.
    expect(
      mentionsToClear(feed, "engineering", new Map(), null),
    ).toEqual(["eng-1", "eng-2"]);
  });

  /**
   * An empty list is a real instruction to the host ("mark nothing"), distinct
   * from omitting ids ("mark everything") — so the caller must not send it as
   * though it meant the latter.
   */
  it("returns an empty list rather than undefined when there is nothing to clear", () => {
    expect(mentionsToClear([], "engineering")).toEqual([]);
  });

  /**
   * A mention whose subject message is outside the loaded history window must
   * not silently clear — the person was never shown the summoning text, and
   * clearing it would lose the summons for good (Codex P1).
   */
  describe("with loadedMessageIds restricting what is visible", () => {
    const loaded = new Set(["h1", "h2", "h7"]);

    it("clears a top-level mention whose subject IS in the loaded transcript", () => {
      expect(
        mentionsToClear(
          [note({ id: "visible", subjectId: "1" })],
          "engineering",
          new Map(),
          null,
          loaded,
        ),
      ).toEqual(["visible"]);
    });

    it("keeps a top-level mention whose subject is NOT in the loaded transcript", () => {
      expect(
        mentionsToClear(
          [note({ id: "ghost", subjectId: "99" })],
          "engineering",
          new Map(),
          null,
          loaded,
        ),
      ).toEqual([]);
    });

    it("still clears a thread reply mention when its parent thread is open and the reply is loaded", () => {
      const replies = new Map([["h42", "h7"]]);
      const loadedWithReply = new Set(["h1", "h2", "h7", "h42"]);
      expect(
        mentionsToClear(
          [note({ id: "r", context: "engineering", subjectId: "42" })],
          "engineering",
          replies,
          "h7",
          loadedWithReply,
        ),
      ).toEqual(["r"]);
    });
  });

});

describe("renderedChannelIdForContext", () => {
  it("places a context on the channel it names, #general included", () => {
    expect(renderedChannelIdForContext("engineering")).toBe("engineering");
    expect(renderedChannelIdForContext("general")).toBe("general");
  });

  it("does not fold a legacy spelling onto #general", () => {
    expect(renderedChannelIdForContext("main")).toBe("main");
  });

  it("returns undefined for a missing context", () => {
    expect(renderedChannelIdForContext(undefined)).toBeUndefined();
    expect(renderedChannelIdForContext(null)).toBeUndefined();
  });
});

describe("threadsToReReadForMentions", () => {
  // The console's thread → channel map: desks, `#general` among them, keep
  // their own id as channel; DMs are `dm:<member>`.
  const byThread: Record<string, string> = {
    general: "general",
    engineering: "engineering",
    ada: "dm:ada",
  };
  const loaded = {
    engineering: new Set(["h1", "h2"]),
    "dm:ada": new Set(["h7"]),
  };

  it("re-reads the thread of a mention whose subject is not loaded", () => {
    expect(
      threadsToReReadForMentions(
        [note({ id: "m", context: "engineering", subjectId: "42" })],
        loaded,
        byThread,
        new Set(),
      ),
    ).toEqual({ threadIds: ["engineering"], subjects: ["h42"] });
  });

  it("re-reads a DM thread for a dm: context", () => {
    expect(
      threadsToReReadForMentions(
        [note({ id: "m", context: "dm:ada", subjectId: "9" })],
        loaded,
        byThread,
        new Set(),
      ),
    ).toEqual({ threadIds: ["ada"], subjects: ["h9"] });
  });

  it("re-reads #general's own thread for a #general mention", () => {
    expect(
      threadsToReReadForMentions(
        [note({ id: "m", context: "general", subjectId: "5" })],
        loaded,
        byThread,
        new Set(),
      ),
    ).toEqual({ threadIds: ["general"], subjects: ["h5"] });
  });

  it("skips a legacy General-spelled context no channel renders", () => {
    expect(
      threadsToReReadForMentions(
        [note({ id: "m", context: "General", subjectId: "5" })],
        loaded,
        byThread,
        new Set(),
      ),
    ).toEqual({ threadIds: [], subjects: [] });
  });

  it("skips a mention whose subject is already loaded", () => {
    expect(
      threadsToReReadForMentions(
        [note({ id: "m", context: "engineering", subjectId: "1" })],
        loaded,
        byThread,
        new Set(),
      ),
    ).toEqual({ threadIds: [], subjects: [] });
  });

  it("skips mentions already seen this session", () => {
    expect(
      threadsToReReadForMentions(
        [note({ id: "m", context: "engineering", subjectId: "42" })],
        loaded,
        byThread,
        new Set(["h42"]),
      ),
    ).toEqual({ threadIds: [], subjects: [] });
  });

  it("skips read and non-mention rows", () => {
    expect(
      threadsToReReadForMentions(
        [
          note({ id: "read", context: "engineering", subjectId: "42", readAt: 3 }),
          { ...note({ id: "reaction", context: "engineering", subjectId: "42" }), kind: "reaction" },
        ],
        loaded,
        byThread,
        new Set(),
      ),
    ).toEqual({ threadIds: [], subjects: [] });
  });

  it("dedupes two missing mentions that share a thread", () => {
    expect(
      threadsToReReadForMentions(
        [
          note({ id: "m1", context: "engineering", subjectId: "42" }),
          note({ id: "m2", context: "engineering", subjectId: "43" }),
        ],
        loaded,
        byThread,
        new Set(),
      ),
    ).toEqual({ threadIds: ["engineering"], subjects: ["h42", "h43"] });
  });

  it("skips a context no channel renders", () => {
    expect(
      threadsToReReadForMentions(
        [note({ id: "m", context: "ghost-desk", subjectId: "42" })],
        loaded,
        byThread,
        new Set(),
      ),
    ).toEqual({ threadIds: [], subjects: [] });
  });
});
