import { describe, expect, it } from "vitest";

import type { SkillAgentScope } from "@/api/skills";
import {
  SCOPE_CLEARS_TO_INHERITED_WARNING,
  SCOPE_PINS_INHERITED_WARNING,
  agentsToUnpin,
  droppedSlugs,
  pinsAnInheritedScope,
  scopeUnchanged,
  showingInherited,
  skillScopeState,
  slugsGainedByInheriting,
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

describe("agentsToUnpin", () => {
  const scope = (id: string, state: "inherited" | "included" | "excluded") => ({
    id,
    state,
    holds: state !== "excluded",
  });

  it("names every teammate that is not already inheriting", () => {
    expect(
      agentsToUnpin([
        scope("a", "inherited"),
        scope("b", "included"),
        scope("c", "excluded"),
      ]).map((agent) => agent.id),
    ).toEqual(["b", "c"]);
  });

  it("names nobody when the whole roster already inherits", () => {
    expect(agentsToUnpin([scope("a", "inherited")])).toEqual([]);
  });

  it("treats an absent roster as nothing to write", () => {
    expect(agentsToUnpin(undefined)).toEqual([]);
    expect(agentsToUnpin(null)).toEqual([]);
  });
});

describe("slugsGainedByInheriting", () => {
  it("names what a pinned list was leaving out", () => {
    expect(slugsGainedByInheriting(["a"], ["a", "b", "c"])).toEqual(["b", "c"]);
  });

  it("names nothing for a teammate that already inherits", () => {
    // Already inheriting: it holds everything, so returning to inherit is a
    // no-op rather than a widening.
    expect(slugsGainedByInheriting(null, ["a", "b"])).toEqual([]);
    expect(slugsGainedByInheriting(undefined, ["a", "b"])).toEqual([]);
  });

  it("names the whole enabled set for a list that holds nothing", () => {
    expect(slugsGainedByInheriting([], ["a", "b"])).toEqual(["a", "b"]);
  });

  it("names nothing when the pinned list already holds everything", () => {
    expect(slugsGainedByInheriting(["a", "b"], ["a", "b"])).toEqual([]);
  });
});

describe("SCOPE_CLEARS_TO_INHERITED_WARNING", () => {
  it("says the widening reaches past the skill being edited", () => {
    expect(SCOPE_CLEARS_TO_INHERITED_WARNING).toMatch(/not only this one/i);
    expect(SCOPE_CLEARS_TO_INHERITED_WARNING).toMatch(/every skill/i);
  });

  it("is not the same claim as its mirror", () => {
    expect(SCOPE_CLEARS_TO_INHERITED_WARNING).not.toBe(
      SCOPE_PINS_INHERITED_WARNING,
    );
  });
});

describe("pinsAnInheritedScope", () => {
  it("warns only about an agent that has never been scoped", () => {
    expect(pinsAnInheritedScope("inherited")).toBe(true);
    expect(pinsAnInheritedScope("included")).toBe(false);
    expect(pinsAnInheritedScope("excluded")).toBe(false);
  });
});
