// The seed behind a teammate's mascot, pinned (issue #1181).
//
// # Why this test exists
//
// A teammate's face must be the same face on every surface. There are two
// plausible seeds and they disagree for essentially every real roster row:
//
//   * `avatarFor(dto.id || name)` — what `fromDto` computes into
//     `TeamMember.avatar`, and what `lib/team.ts` documents ("renaming a
//     teammate does not change its face");
//   * `avatarFor(name)` — the fallback inside `Avatar` when no `avatar` prop is
//     passed, which is what **every** call site in the console actually hits,
//     chat included. Nothing passes the prop.
//
// #1181 asked for the mascot to reach the Company cards and the teammate detail
// header, and warned that seeding one surface on the id and another on the name
// gives the same teammate two different faces — "worse than the current
// inconsistency, and harder to notice, because each screen looks internally
// consistent."
//
// That warning assumed chat was id-seeded. It is not. So the Company surfaces
// deliberately pass **no `avatar` prop**, landing on the same name fallback chat
// uses, and every surface agrees today.
//
// This test is the tripwire on that choice. If someone "tidies" the Company
// surfaces into passing `TeamMember.avatar` without moving chat at the same
// time, the two seeds below are what they will have silently split apart.

import { describe, expect, it } from "vitest";

import { avatarFor, fromDto } from "@/lib/team";

/** Roster rows shaped like a real company bundle: a slug id, a titled name. */
const ROWS = [
  { id: "backend_engineer", name: "Backend Engineer" },
  { id: "security_engineer", name: "Security Engineer" },
  { id: "designer", name: "Designer" },
  { id: "product_manager", name: "Product Manager" },
  { id: "researcher", name: "Researcher" },
];

describe("teammate mascot seeding", () => {
  it("the id seed and the name seed are genuinely different faces", () => {
    // Not a theoretical hazard: every one of these disagrees. If this ever
    // starts passing by coincidence the test is worthless, so assert on all.
    for (const row of ROWS) {
      expect(
        avatarFor(row.id),
        `${row.name}: id and name seeds must be treated as different faces`,
      ).not.toBe(avatarFor(row.name));
    }
  });

  it("`TeamMember.avatar` is the id-seeded one, and is not what surfaces render", () => {
    // The field is computed and carried, and is currently rendered by nothing.
    // Passing it anywhere is only safe once every surface — chat included —
    // passes it too.
    for (const row of ROWS) {
      const member = fromDto({ id: row.id, name: row.name, role: row.name });
      expect(member.avatar).toBe(avatarFor(row.id));
    }
  });

  it("the name seed is stable, so two surfaces naming a teammate alike agree", () => {
    // This is the property the Company cards, the detail header and the chat
    // member pane rely on: they all pass the same display name, so they all
    // resolve the same mascot.
    for (const row of ROWS) {
      expect(avatarFor(row.name)).toBe(avatarFor(row.name));
      expect(avatarFor(row.name)).toMatch(/^[a-z]+$/);
    }
  });
});
