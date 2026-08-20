// `roleSubtitle` — the one rule behind the muted line under a teammate's name
// (issue #1208).
//
// The Company cards and the org chart's seats both drew the name and then the
// role. A roster row carries both, and `fromDto` resolves `name` as
// `dto.name?.trim() || dto.role`, so a manifest-declared agent — which is every
// agent in every shipped company, none of which sets a `name` — made the two
// one string and every row said it twice. This is the guard that keeps the
// second slot honest, tested as a function rather than only through rendering.

import { describe, expect, it } from "vitest";

import { fromDto, roleSubtitle } from "@/lib/team";
import type { AgentDetailDto, TeamMemberDto } from "@/api/types";

describe("roleSubtitle", () => {
  it("keeps a role that says something the name does not", () => {
    expect(roleSubtitle("Maya", "Backend Engineer")).toBe("Backend Engineer");
  });

  it("draws nothing when the role is the name over again", () => {
    expect(roleSubtitle("Backend Engineer", "Backend Engineer")).toBeNull();
  });

  it("is not fooled by case or padding — the same repeat in another costume", () => {
    expect(roleSubtitle("Backend Engineer", "backend engineer")).toBeNull();
    expect(roleSubtitle("  Backend Engineer  ", "Backend Engineer")).toBeNull();
    expect(roleSubtitle("Backend Engineer", "  Backend Engineer ")).toBeNull();
  });

  it("draws nothing for an empty or blank role, rather than an empty line", () => {
    expect(roleSubtitle("Maya", "")).toBeNull();
    expect(roleSubtitle("Maya", "   ")).toBeNull();
  });

  it("returns the trimmed role, so the rendered line carries no stray padding", () => {
    expect(roleSubtitle("Maya", "  Backend Engineer  ")).toBe("Backend Engineer");
  });

  /**
   * The case the issue was reported from, end to end: what the host actually
   * sends for a manifest agent, through the same mapper the views use.
   */
  it("silences the repeat for a host roster row that carries no display name", () => {
    const dto = {
      id: "backend_engineer",
      role: "Backend Engineer",
      description: "Build and operate the backend and services.",
    } as TeamMemberDto;
    const member = fromDto(dto);

    expect(member.name).toBe("Backend Engineer");
    expect(roleSubtitle(member.name, member.role)).toBeNull();
  });

  /**
   * The agent detail page derives its title the same way, from a different DTO
   * (`AgentDetailDto`, not the roster row), so the repeat reached that page too.
   */
  it("silences the repeat on the agent detail header, which derives its title separately", () => {
    const agent = { id: "backend_engineer", role: "Backend Engineer" } as AgentDetailDto;
    const display = agent.name?.trim() || agent.role;

    expect(display).toBe("Backend Engineer");
    expect(roleSubtitle(display, agent.role)).toBeNull();
  });

  /** A teammate the operator named keeps both lines, which is the whole point. */
  it("keeps both lines for a teammate the host does name", () => {
    const dto = {
      id: "growth_analyst",
      name: "Ada",
      role: "Growth Analyst",
    } as TeamMemberDto;
    const member = fromDto(dto);

    expect(roleSubtitle(member.name, member.role)).toBe("Growth Analyst");
  });
});
