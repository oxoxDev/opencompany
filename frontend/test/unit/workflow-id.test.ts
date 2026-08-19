import { describe, expect, it } from "vitest";

import { isSafeId, slugifyWorkflowId } from "@/lib/workflow-id";

/**
 * Deriving a workflow id from its name (issue #1053).
 *
 * The form used to reject "Weekly digest" for a missing id, then reject
 * "weekly digest" for an unsafe one — two rejections for something it could
 * derive. The trap in fixing that is writing the character rule down twice, once
 * to test an id and once to make one: a slugger that emits a character the
 * checker refuses produces an error message about the operator's own generated
 * id, which is worse than the bug.
 *
 * So the property that actually matters is the round trip — whatever the slugger
 * emits, the checker must accept.
 */
describe("slugifyWorkflowId", () => {
  it("derives the id the operator would have typed", () => {
    expect(slugifyWorkflowId("Weekly digest")).toBe("weekly-digest");
  });

  it("lower-cases, because an id is a path component", () => {
    expect(slugifyWorkflowId("WEEKLY Digest")).toBe("weekly-digest");
  });

  it("collapses runs of unusable characters instead of repeating separators", () => {
    expect(slugifyWorkflowId("  Weekly   digest!!  ")).toBe("weekly-digest");
  });

  it("keeps characters the id rule already allows", () => {
    expect(slugifyWorkflowId("campaign_pipeline-v2")).toBe("campaign_pipeline-v2");
  });

  /**
   * A name with nothing usable in it derives nothing. The caller must leave the
   * field alone rather than write `""` — an empty id fails `isSafeId`, so
   * writing one would replace a clear error with a mystery.
   */
  it("returns empty when the name has nothing usable in it", () => {
    expect(slugifyWorkflowId("???")).toBe("");
    expect(slugifyWorkflowId("   ")).toBe("");
  });

  /**
   * The round trip, over the cases most likely to break it. This is the test
   * that fails if the slugger and the checker are ever written from two
   * different character rules.
   */
  it("only ever emits ids the id rule accepts", () => {
    for (const name of [
      "Weekly digest",
      "  Weekly   digest!!  ",
      "WEEKLY Digest",
      "campaign_pipeline-v2",
      "Report: Q3 — 2026 (final)",
      "a/b/c",
      "emoji 🎉 name",
      // Characters a *second*, hand-written character rule would plausibly keep
      // — `.` above all, which reads as harmless and which `isSafeId` refuses.
      // Without these the round trip passes against a drifted slugger and this
      // test is decorative.
      "v1.2 release",
      "Q3.final",
      "name~with+extras",
      "path.to.thing",
    ]) {
      const derived = slugifyWorkflowId(name);
      if (derived) expect(isSafeId(derived), `derived \`${derived}\` from \`${name}\``).toBe(true);
    }
  });
});

describe("isSafeId", () => {
  it("accepts letters, digits, underscore and hyphen", () => {
    expect(isSafeId("weekly-digest_2")).toBe(true);
  });

  it("refuses an empty id and anything with a path separator or space", () => {
    expect(isSafeId("")).toBe(false);
    expect(isSafeId("weekly digest")).toBe(false);
    expect(isSafeId("a/b")).toBe(false);
  });
});
