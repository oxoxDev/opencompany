import { describe, expect, it } from "vitest";

import type { SkillAgentScope } from "@/api/skills";
import {
  droppedSlugs,
  pinsAnInheritedScope,
  scopeUnchanged,
  showingInherited,
  skillReachSummary,
  skillScopeState,
  toggleSkillInScope,
} from "@/lib/skill-scope";

/**
 * The scope arithmetic, extracted from `AgentDetailView` so a second picker
 * cannot drift from the first.
 *
 * Every function here returns a legal scope whatever it gets wrong, which is why
 * each case is asserted against the exact list rather than against "a narrowing
 * happened". The host accepts `[S]` as happily as `["a","b",S]`; the difference
 * between them is a teammate silently losing every skill nobody touched.
 */

const CEILING = ["a", "b", "c"];

describe("skillScopeState", () => {
  it("reads a null scope as inherited", () => {
    expect(skillScopeState(null, "a")).toBe("inherited");
    expect(skillScopeState(undefined, "a")).toBe("inherited");
  });

  it("reads a list naming the slug as included", () => {
    expect(skillScopeState(["a", "b"], "a")).toBe("included");
  });

  it("reads a list omitting the slug as excluded", () => {
    expect(skillScopeState(["b"], "a")).toBe("excluded");
  });

  it("reads a deliberately empty scope as excluded, never as inherited", () => {
    // The collapse the host's projection also refuses. Both hold nothing while
    // the skill is disabled; only the inherited one gets it back.
    expect(skillScopeState([], "a")).toBe("excluded");
    expect(skillScopeState([], "a")).not.toBe(skillScopeState(null, "a"));
  });

  it("matches exactly, never by prefix", () => {
    for (const near of ["ab", "a-2", "a*", "*", "A"]) {
      expect(skillScopeState([near], "a"), `${near} is not a`).toBe("excluded");
    }
  });
});

describe("toggleSkillInScope", () => {
  it("adds to the stored list rather than replacing it", () => {
    // THE data-losing bug. A base of `[slug]` is a legal narrowing the host
    // stores without complaint, the toast says saved, and the panel looks right
    // afterwards — while the teammate has lost `a` and `b`.
    expect(toggleSkillInScope(["a", "b"], CEILING, "c", true)).toEqual([
      "a",
      "b",
      "c",
    ]);
    expect(toggleSkillInScope(["a", "b"], CEILING, "c", true)).not.toEqual([
      "c",
    ]);
  });

  it("materialises an inherited scope to the ceiling minus the slug", () => {
    // Turning one off on an inherited scope has to write the rest out
    // explicitly, or a screen reading "all but this one" saves "only this one".
    expect(toggleSkillInScope(null, CEILING, "b", false)).toEqual(["a", "c"]);
  });

  it("adds to a deliberately empty scope rather than to the ceiling", () => {
    expect(toggleSkillInScope([], CEILING, "a", true)).toEqual(["a"]);
  });

  it("narrows an in-progress draft once a switch has been touched", () => {
    const first = toggleSkillInScope(null, CEILING, "b", false);
    const second = toggleSkillInScope(null, CEILING, "a", false, {
      slugs: first,
      touched: true,
    });
    expect(second, "the second move narrows the first move's draft").toEqual([
      "c",
    ]);
  });

  it("does not treat an emptied draft as an inherited one", () => {
    // Every switch off, then one back on. The draft is empty and the stored
    // scope is still `null`, so a base chosen by "is the draft empty" would hand
    // the teammate the whole ceiling on a move that asked for one slug.
    const emptied = CEILING.reduce(
      (draft, slug) =>
        toggleSkillInScope(null, CEILING, slug, false, {
          slugs: draft,
          touched: true,
        }),
      toggleSkillInScope(null, CEILING, CEILING[0], false),
    );
    expect(emptied).toEqual([]);
    expect(
      toggleSkillInScope(null, CEILING, "b", true, {
        slugs: emptied,
        touched: true,
      }),
    ).toEqual(["b"]);
  });

  it("is idempotent on a move that changes nothing", () => {
    expect(toggleSkillInScope(["a", "b"], CEILING, "a", true)).toEqual([
      "a",
      "b",
    ]);
    expect(toggleSkillInScope(["a", "b"], CEILING, "c", false)).toEqual([
      "a",
      "b",
    ]);
  });

  it("never introduces a duplicate", () => {
    expect(toggleSkillInScope(["a", "a"], CEILING, "a", true)).toEqual(["a"]);
  });
});

describe("showingInherited", () => {
  it("stops showing the inherited view once a switch is touched", () => {
    expect(showingInherited(null, false)).toBe(true);
    expect(showingInherited(null, true)).toBe(false);
  });

  it("is never true for a scope that stores a list", () => {
    expect(showingInherited([], false)).toBe(false);
    expect(showingInherited(["a"], false)).toBe(false);
  });
});

describe("scopeUnchanged", () => {
  it("calls the whole ceiling unchanged against an inherited scope", () => {
    expect(scopeUnchanged(null, CEILING, CEILING)).toBe(true);
  });

  it("calls a narrowed draft changed against an inherited scope", () => {
    expect(scopeUnchanged(null, ["a", "b"], CEILING)).toBe(false);
  });

  it("ignores order against a stored list", () => {
    expect(scopeUnchanged(["a", "b"], ["b", "a"], CEILING)).toBe(true);
  });

  it("calls an emptied draft changed against a stored list", () => {
    expect(scopeUnchanged(["a"], [], CEILING)).toBe(false);
  });

  it("calls an empty draft unchanged against a stored empty scope", () => {
    expect(scopeUnchanged([], [], CEILING)).toBe(true);
  });
});

describe("droppedSlugs", () => {
  it("names a stored slug the company does not have enabled", () => {
    expect(droppedSlugs(["a", "gone"], ["a"])).toEqual(["gone"]);
  });

  it("names nothing for an inherited scope", () => {
    expect(droppedSlugs(null, ["a"])).toEqual([]);
  });
});

describe("skillReachSummary", () => {
  const scope = (id: string, holds: boolean): SkillAgentScope => ({
    id,
    state: holds ? "inherited" : "excluded",
    holds,
  });

  it("separates a host that cannot say from a skill nobody holds", () => {
    expect(skillReachSummary(undefined), "absent is not zero").toBeNull();
    expect(skillReachSummary([scope("a", false)])).toEqual({
      held: 0,
      total: 1,
    });
  });

  it("counts the agents that hold it", () => {
    expect(
      skillReachSummary([
        scope("a", true),
        scope("b", false),
        scope("c", true),
      ]),
    ).toEqual({
      held: 2,
      total: 3,
    });
  });

  it("reports an empty roster as nought of nought", () => {
    expect(skillReachSummary([])).toEqual({ held: 0, total: 0 });
  });
});

describe("pinsAnInheritedScope", () => {
  it("warns only about an agent that has never been scoped", () => {
    expect(pinsAnInheritedScope("inherited")).toBe(true);
    expect(pinsAnInheritedScope("included")).toBe(false);
    expect(pinsAnInheritedScope("excluded")).toBe(false);
  });
});
