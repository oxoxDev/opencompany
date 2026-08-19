import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import type { TeamMemberDto } from "@/api/types";
import {
  SETUP_STEPS,
  MAX_JOBS,
  adminEmailProblem,
  jobItems,
  buildOutLabel,
  draftIsSubmittable,
  emptySetupDraft,
  shouldOfferSetup,
  shouldPromptSetup,
  stepProblem,
  teamIsEmpty,
  type SetupDraft,
} from "@/lib/company-setup";

/**
 * First-run company setup's decisions (docs/spec/runtime/company-setup.md).
 *
 * The one that matters most is `shouldOfferSetup`. Setup *creates* a team, so
 * offering it twice would stack a second team on the first — the failure this
 * feature would be judged on. It is a pure function of the roster and the skip
 * flag precisely so it can be pinned here rather than left to an effect.
 */

const member = (id: string): TeamMemberDto =>
  ({ id, role: "Analyst", inboxEnabled: false }) as TeamMemberDto;

const draft = (over: Partial<SetupDraft> = {}): SetupDraft => ({
  ...emptySetupDraft(),
  ...over,
});

describe("the three questions", () => {
  it("asks exactly three, in order, and only the first is required", () => {
    expect(SETUP_STEPS.map((s) => s.key)).toEqual(["industry", "teamHint", "automate"]);
    expect(SETUP_STEPS.filter((s) => s.required).map((s) => s.key)).toEqual(["industry"]);
  });

  /**
   * Every question must change what gets built — the test applied to anything
   * anyone wants to add. A question with no field behind it is a screen that
   * costs 15 seconds and earns nothing.
   */
  it("has a field, a question and a hint for each", () => {
    for (const step of SETUP_STEPS) {
      expect(step.question.length).toBeGreaterThan(0);
      expect(step.hint.length).toBeGreaterThan(0);
      expect(step.placeholder.length).toBeGreaterThan(0);
      expect(emptySetupDraft()).toHaveProperty(step.key);
    }
  });
});

describe("stepProblem", () => {
  it("blocks an empty first question", () => {
    expect(stepProblem(SETUP_STEPS[0], draft())).toBeTruthy();
    expect(stepProblem(SETUP_STEPS[0], draft({ industry: "  " }))).toBeTruthy();
    expect(stepProblem(SETUP_STEPS[0], draft({ industry: "E-commerce" }))).toBeUndefined();
  });

  /**
   * The last two questions are genuinely skippable. Someone who tells us they
   * run a homeware shop and nothing else should still get a team — a required
   * field there turns a 40-second flow into a wall.
   */
  it("never blocks the optional questions", () => {
    for (const step of SETUP_STEPS.filter((s) => !s.required)) {
      expect(stepProblem(step, draft())).toBeUndefined();
    }
  });

  /**
   * No length minimum. Three words is an answer, and the host is handed a
   * reference team precisely so a terse answer still lands a real roster.
   */
  it("accepts a terse answer", () => {
    expect(draftIsSubmittable(draft({ industry: "a shop" }))).toBe(true);
  });

  it("is not submittable until the first question is answered", () => {
    expect(draftIsSubmittable(draft())).toBe(false);
    expect(draftIsSubmittable(draft({ automate: "loads of things" }))).toBe(false);
  });
});

describe("shouldOfferSetup", () => {
  it("offers on an empty roster", () => {
    expect(shouldOfferSetup({ roster: [], skipped: false })).toBe(true);
  });

  /**
   * The duplicate-team guard. Whatever else changes, a company that already has
   * people on it must never be offered setup unprompted.
   */
  it("never offers once anyone is on the team", () => {
    expect(shouldOfferSetup({ roster: [member("a")], skipped: false })).toBe(false);
    expect(shouldOfferSetup({ roster: [member("a")], skipped: true })).toBe(false);
  });

  it("respects a skip", () => {
    expect(shouldOfferSetup({ roster: [], skipped: true })).toBe(false);
  });

  /**
   * The accepted cost of defining first-run as emptiness: a company whose
   * manifest already names agents — every company under `companies/` — is never
   * offered setup, because it was never unstaffed. Pinned so the trade is a
   * decision rather than a surprise.
   */
  it("does not offer to a company that shipped with a roster", () => {
    const manifestRoster = ["ceo", "cto", "designer"].map(member);
    expect(shouldOfferSetup({ roster: manifestRoster, skipped: false })).toBe(false);
  });
});

describe("shouldPromptSetup", () => {
  /**
   * The other half of "blocking but skippable": the in-place prompt ignores the
   * skip flag, so dismissing the dialog is not a dead end.
   */
  it("keeps prompting an unstaffed company even after a skip", () => {
    expect(shouldPromptSetup([])).toBe(true);
    expect(shouldOfferSetup({ roster: [], skipped: true })).toBe(false);
  });

  it("stops once the company is staffed", () => {
    expect(shouldPromptSetup([member("a")])).toBe(false);
  });
});

describe("teamIsEmpty", () => {
  it("is emptiness and nothing else", () => {
    expect(teamIsEmpty([])).toBe(true);
    expect(teamIsEmpty([member("a")])).toBe(false);
  });
});

describe("buildOutLabel", () => {
  it("counts up to the total", () => {
    expect(buildOutLabel(0, 5)).toBe("0 of 5");
    expect(buildOutLabel(3, 5)).toBe("3 of 5");
  });

  /**
   * Clamped, so a late reveal cannot render "6 of 5" — the build-out sets
   * `created` per landed write and a retry would otherwise overshoot.
   */
  it("never exceeds the total", () => {
    expect(buildOutLabel(9, 5)).toBe("5 of 5");
  });

  it("says nothing when there is nothing to count", () => {
    expect(buildOutLabel(0, 0)).toBe("");
  });
});

describe("the job checklist", () => {
  /**
   * The rule that replaced `inferSignals`.
   *
   * The old chips were a regex over a hand-copied duplicate of the host's
   * template keywords, claiming the product had *understood* the operator
   * before anything had read a word — the chips were never sent anywhere and
   * never touched the roster. This does a smaller thing honestly: it splits
   * their own words the way the host splits them, so the checklist the roster is
   * judged against is one they can see.
   *
   * Driven by the SAME fixture the Rust test reads. Two implementations of one
   * rule is precisely how the keyword list drifted, and this file is what stops
   * it happening twice.
   */
  const fixture = JSON.parse(
    readFileSync(
      fileURLToPath(new URL("../../../tests/fixtures/setup-jobs.json", import.meta.url)),
      "utf8",
    ),
  ) as {
    maxJobs: number;
    cases: { why: string; input: string; items: string[] }[];
  };

  it("agrees with the host about the cap", () => {
    expect(MAX_JOBS).toBe(fixture.maxJobs);
  });

  it("has cases to assert on at all", () => {
    expect(fixture.cases.length).toBeGreaterThan(0);
  });

  for (const testCase of fixture.cases) {
    it(testCase.why, () => {
      expect(jobItems(testCase.input)).toEqual(testCase.items);
    });
  }
});

describe("the admin address", () => {
  /**
   * The bug: `as` passed the email step, the roster was designed, and the apply
   * then failed with "`[users].admins` has an invalid entry" on the last screen
   * — a configuration error about a mistake made four steps earlier.
   *
   * Driven by the SAME fixture the host's test reads. The console cannot call
   * `is_usable_admin_email`, so a fixture is what keeps the re-implementation
   * honest — and stops anyone "improving" this into a strict regex that refuses
   * addresses the host accepts everywhere else.
   */
  const fixture = JSON.parse(
    readFileSync(
      fileURLToPath(new URL("../../../tests/fixtures/setup-admin-email.json", import.meta.url)),
      "utf8",
    ),
  ) as { cases: { why: string; input: string; usable: boolean }[] };

  it("has cases to assert on at all", () => {
    expect(fixture.cases.length).toBeGreaterThan(0);
  });

  for (const testCase of fixture.cases) {
    it(testCase.why, () => {
      // `required` is true, so a blank address is a problem here as well as a
      // structurally invalid one — the host's predicate answers only the second
      // question, and both readings must reject everything it rejects.
      const problem = adminEmailProblem(testCase.input, true);
      expect(problem === undefined).toBe(testCase.usable);
    });
  }

  /** A host with no sign-in needs no address — but a typo is still a typo. */
  it("lets a blank address pass where sign-in is not required, but not a typo", () => {
    expect(adminEmailProblem("", false)).toBeUndefined();
    expect(adminEmailProblem("   ", false)).toBeUndefined();
    expect(adminEmailProblem("as", false)).toContain("@");
  });
});

