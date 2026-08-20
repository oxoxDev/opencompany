import { describe, expect, it } from "vitest";

import { avatarFor } from "@/lib/team";
import type { TeamMember } from "@/lib/team";
import { buildChannels, dmFace, type Channel } from "@/views/chat/model";

/**
 * The seed a DM's avatar is drawn from (issue #1170).
 *
 * The failure this guards is silent and only visible side by side: the rail row
 * and the chat header both draw the teammate on the other end of a DM, and
 * `Avatar` hashes its mascot out of the `name` it is handed. Seed the two from
 * different fields — the name in one place, the id or the role in the other —
 * and one person wears two faces on one screen, which is worse than the generic
 * glyph the header used to draw. Nothing throws, nothing fails to render, and
 * no type objects.
 *
 * So this pins the seed itself rather than a rendered tile: both call sites go
 * through `dmFace`, and `dmFace` promises the teammate's own name.
 */

function member(over: Partial<TeamMember> & Pick<TeamMember, "id" | "name">): TeamMember {
  return {
    role: "Engineer",
    description: "",
    tone: "sky",
    avatar: "green",
    inboxEnabled: false,
    effectiveTools: [],
    desks: [],
    ...over,
  };
}

function dmFor(m: TeamMember): Channel {
  const dms = buildChannels([m], []).find((s) => s.id === "dms");
  expect(dms?.channels).toHaveLength(1);
  return dms!.channels[0];
}

describe("dmFace", () => {
  it("hands back the teammate's own name and tone, so every caller seeds alike", () => {
    const ada = member({ id: "agent_ada", name: "Ada", tone: "violet" });
    expect(dmFace(dmFor(ada))).toEqual({ name: "Ada", tone: "violet" });
  });

  it("resolves to one mascot for one teammate", () => {
    // What the rail row and the header each end up drawing. They call the same
    // function, so the only way these can differ is a change to `dmFace` — the
    // point of routing both through it.
    const face = dmFace(dmFor(member({ id: "agent_backend", name: "Backend Engineer" })));
    expect(face).not.toBeNull();
    expect(avatarFor(face!.name)).toBe(avatarFor("Backend Engineer"));
  });

  it("does not seed on the id, which would give the rail and the roster row different faces", () => {
    // A guard against the plausible-looking "fix" of seeding on the stable id:
    // it is stable, but it is not what `Avatar` hashes when a caller passes only
    // a name, and these two ids hash to different mascots than the name does.
    const face = dmFace(dmFor(member({ id: "agent_ada", name: "Ada" })));
    expect(face!.name).not.toBe("agent_ada");
  });

  it("has no face for a channel — a desk line has no one person behind it", () => {
    const desks = buildChannels([], [{ id: "d1", channel: "front-desk", name: "Front desk", blurb: "" }]);
    const channel = desks.find((s) => s.id === "channels")!.channels[0];
    expect(channel.kind).toBe("channel");
    expect(dmFace(channel)).toBeNull();
  });

  it("has no face for a DM with no roster entry behind it", () => {
    // Both call sites fall back to a glyph here rather than inventing a mascot
    // for a teammate the roster cannot name.
    const orphan: Channel = { id: "dm:gone", name: "Gone", kind: "dm", purpose: "" };
    expect(dmFace(orphan)).toBeNull();
  });

  it("gives a private channel no face either — the lock still speaks for it", () => {
    const locked: Channel = { id: "c1", name: "ops", kind: "channel", purpose: "", private: true };
    expect(dmFace(locked)).toBeNull();
  });
});
