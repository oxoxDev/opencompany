import { describe, expect, it } from "vitest";

import { GENERAL_CHANNEL_ID } from "@/lib/chat";
import { channelForThread, dmThreadId } from "@/views/room/model";
import type { TeamMember } from "@/lib/team";

/**
 * Where a live turn frame lands. The live-state maps (`liveStepsByThread`,
 * `receiptByThread`) are keyed by the frame's own thread id, which `RoomView`
 * reads back by the desk id or `dmThreadId`; the channel comes from the shell's
 * thread → channel map through `channelForThread`.
 */

const ADA: TeamMember = { id: "ada", name: "Ada" } as TeamMember;

/** The shell's thread → channel map once the desk list has landed. */
const LOADED: Record<string, string> = {
  [GENERAL_CHANNEL_ID]: GENERAL_CHANNEL_ID,
  engineering: "engineering",
  ada: "dm:ada",
};

describe("a live frame's thread", () => {
  it("is keyed in the host-thread namespace the readers use", () => {
    expect(dmThreadId(ADA)).toBe("ada");
    expect(channelForThread(LOADED, "ada")).toBe("dm:ada");
  });

  it("renders a #general frame in #general", () => {
    expect(channelForThread(LOADED, GENERAL_CHANNEL_ID)).toBe(GENERAL_CHANNEL_ID);
  });

  it("does not fold a legacy General spelling onto #general", () => {
    for (const spelling of ["", "main", "General", "GENERAL"]) {
      expect(channelForThread(LOADED, spelling)).toBeNull();
    }
  });

  it("renders nowhere before the desks load, rather than somewhere wrong", () => {
    expect(channelForThread({}, GENERAL_CHANNEL_ID)).toBeNull();
    expect(channelForThread({}, "ada")).toBeNull();
  });

  it("addresses a teammate whose id is `general` on its prefixed DM thread", () => {
    const namesake = { id: GENERAL_CHANNEL_ID, name: "Gen" } as TeamMember;
    expect(dmThreadId(namesake)).toBe("dm:general");
  });
});
