import { describe, expect, it } from "vitest";

import type { NotificationDto } from "@/api/types";
import {
  isBadgingKind,
  mentionCountsByChannel,
  mentionsToClear,
} from "@/lib/mention-badge";

/**
 * A parked blocker badges the DM it lands in (#1862), driven by the
 * `blocker_parked` notification's `context`.
 */

const parked = (
  over: Partial<NotificationDto> & Pick<NotificationDto, "id">,
): NotificationDto => ({
  kind: "blocker_parked",
  subjectKind: "approval",
  subjectId: "appr-1",
  title: "eng is blocked on t-1: could not connect",
  createdAt: 1,
  context: "dm:eng",
  ...over,
});

describe("isBadgingKind", () => {
  it("badges mentions and parked blockers, nothing else", () => {
    expect(isBadgingKind("mention")).toBe(true);
    expect(isBadgingKind("blocker_parked")).toBe(true);
    expect(isBadgingKind("approval_expired")).toBe(false);
  });
});

describe("mentionCountsByChannel with blockers", () => {
  it("counts an unread parked blocker on its DM channel", () => {
    const counts = mentionCountsByChannel(
      [parked({ id: "a" }), parked({ id: "b", context: "dm:ceo" })],
    );
    expect(counts).toEqual({ "dm:eng": 1, "dm:ceo": 1 });
  });

  it("ignores a blocker already read", () => {
    const counts = mentionCountsByChannel(
      [parked({ id: "a", readAt: 9 })],
    );
    expect(counts["dm:eng"]).toBeUndefined();
  });
});

describe("mentionsToClear with blockers", () => {
  it("clears a parked blocker when its DM is opened, with no message-loaded gate", () => {
    const cleared = mentionsToClear(
      [parked({ id: "a" })],
      "dm:eng",
      new Map(),
      null,
      // A message set that does not contain the (approval) subject — a mention
      // would be withheld by this gate; a blocker must clear regardless.
      new Set<string>(),
    );
    expect(cleared).toEqual(["a"]);
  });

  it("does not clear a blocker for a different DM", () => {
    const cleared = mentionsToClear(
      [parked({ id: "a", context: "dm:ceo" })],
      "dm:eng",
    );
    expect(cleared).toEqual([]);
  });
});
