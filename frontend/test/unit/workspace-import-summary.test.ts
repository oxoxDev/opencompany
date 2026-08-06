import { describe, expect, it } from "vitest";

import { importSummary } from "@/views/WorkspaceView";

/**
 * The legacy-import receipt (issue #500).
 *
 * The retired scratchpad is a flat list of two kinds, and the toast used to
 * report its whole length as "notes" — so a mixed import over-reported, and a
 * folder-only import claimed notes it never created. Every branch below is a
 * shape a real scratchpad can have, which is the point: the string has to be
 * true for all of them, not just the files-only one the fixture happened to
 * cover.
 */

describe("importSummary", () => {
  it("names a single note in the singular", () => {
    expect(importSummary(1, 0)).toBe("1 note");
  });

  it("names several notes in the plural", () => {
    expect(importSummary(3, 0)).toBe("3 notes");
  });

  it("names a single folder in the singular", () => {
    expect(importSummary(0, 1)).toBe("1 folder");
  });

  it("names several folders in the plural", () => {
    expect(importSummary(0, 4)).toBe("4 folders");
  });

  it("never mentions notes for a folder-only scratchpad", () => {
    // The old expression said "Imported 1 note." here, having imported none.
    expect(importSummary(0, 1)).not.toContain("note");
  });

  it("never mentions folders for a files-only scratchpad", () => {
    expect(importSummary(2, 0)).not.toContain("folder");
  });

  it("names both categories for a mixed scratchpad", () => {
    // The issue's own example: the old expression called this "4 notes".
    expect(importSummary(3, 1)).toBe("3 notes and 1 folder");
    expect(importSummary(2, 3)).toBe("2 notes and 3 folders");
  });

  it("stays total for an empty import the banner can never offer", () => {
    expect(importSummary(0, 0)).toBe("nothing");
  });
});
