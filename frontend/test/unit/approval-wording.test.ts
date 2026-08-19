import { describe, expect, it } from "vitest";

import { approvedByRuntimeLine, approvedLine } from "@/lib/approval-wording";

/**
 * Issue #561: the confirmation an operator reads after approving.
 *
 * Approving does not resume a suspended call — the host re-dispatches the teammate,
 * and since #469 it does that once per turn, when the LAST decision that turn
 * parked lands. The console used to say "the teammate is completing the action"
 * for every click, including the three out of four that release nothing. What
 * these pin is that the sentence follows the host's count.
 */
describe("the line an approve leaves behind", () => {
  it("says the teammate is picking it up only when this decision released the turn", () => {
    expect(approvedLine(0)).toBe("Approved — the teammate is picking it up now");
    expect(approvedLine(0, "send an email")).toBe(
      "Approved — the teammate is picking it up now: send an email",
    );
  });

  it("names what is still owed when the turn is still blocked", () => {
    expect(approvedLine(1)).toBe(
      "Approved — waiting on 1 more sign-off before the teammate continues",
    );
    expect(approvedLine(3)).toBe(
      "Approved — waiting on 3 more sign-offs before the teammate continues",
    );
  });

  it("claims nothing about what happens next when the host does not say", () => {
    // A host predating the field. Silence is honest; the optimistic guess is
    // the thing this issue is about.
    expect(approvedLine(undefined)).toBe("Approved — recorded");
    expect(approvedLine(undefined, "run a shell command")).toBe(
      "Approved — recorded: run a shell command",
    );
  });

  it("never says 'the teammate' for work the runtime performs itself", () => {
    // Issue #395: a paused workflow gate or a cold-recipient report has no
    // teammate to re-dispatch, and naming one is the same small lie.
    for (const line of [
      approvedByRuntimeLine(0),
      approvedByRuntimeLine(2),
      approvedByRuntimeLine(undefined),
    ]) {
      expect(line).not.toContain("teammate");
    }
    expect(approvedByRuntimeLine(0)).toBe("Approved — carrying it out now");
    expect(approvedByRuntimeLine(2)).toBe(
      "Approved — waiting on 2 more sign-offs before it runs",
    );
  });

  it("never claims completion — approving is not doing", () => {
    for (const still of [0, 1, 5, undefined] as const) {
      for (const line of [approvedLine(still), approvedByRuntimeLine(still)]) {
        expect(line).not.toMatch(/completing|completed|done/i);
      }
    }
  });
});
