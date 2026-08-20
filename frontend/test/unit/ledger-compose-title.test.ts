import { describe, expect, it } from "vitest";

import {
  composeDialogDescription,
  composeDialogTitle,
  type LedgerSummary,
} from "@/api/ledgers";

/**
 * Issue #1264: the compose dialog's title flipping to "Amend <id>" the moment
 * an operator types the first character of a brand-new row's id — before the
 * row is saved, before it exists, while the submit button still correctly
 * reads "Record".
 *
 * The bug was branching on `composing.id` being non-empty rather than on
 * whether the typed id names a row this ledger already has. Both branches are
 * pure, which is why they live in `api/ledgers.ts` rather than inside the view
 * — the decision is the whole bug and it should be assertable without a
 * render (the reasoning `filteredEmptyNotice` uses for issue #1217).
 */

const PROMISES = {
  slug: "customer-promises",
  title: "Customer promises",
  purpose: "",
  source: "events",
  derived: "derived/CUSTOMER_PROMISES.md",
  writtenBy: "",
  builtin: false,
  fields: [],
  statuses: [
    { name: "open" },
    { name: "kept", closed: true },
    { name: "broken", closed: true },
  ],
  sections: [],
  open: 9,
  closed: 5,
} as unknown as LedgerSummary;

const EXISTING_IDS = new Set(["promise-shipped-dark-mode", "promise-refund"]);

describe("the compose dialog's title", () => {
  it("reads 'New row on <ledger>' for a brand-new, untouched row", () => {
    expect(
      composeDialogTitle(PROMISES, { id: "", closing: false }, EXISTING_IDS),
    ).toBe("New row on Customer promises");
  });

  it("stays 'New row' while typing an id that names no existing row (#1264)", () => {
    // This is the exact bug: `composing.id` is non-empty the instant the
    // operator types, but the row still does not exist.
    expect(
      composeDialogTitle(
        PROMISES,
        { id: "promise-test-dark-mode", closing: false },
        EXISTING_IDS,
      ),
    ).toBe("New row on Customer promises");
  });

  it("switches to 'Amend <id>' once the typed id names a row that exists", () => {
    expect(
      composeDialogTitle(
        PROMISES,
        { id: "promise-shipped-dark-mode", closing: false },
        EXISTING_IDS,
      ),
    ).toBe("Amend promise-shipped-dark-mode");
  });

  it("trims the typed id before checking existence, like save() does", () => {
    expect(
      composeDialogTitle(
        PROMISES,
        { id: "  promise-refund  ", closing: false },
        EXISTING_IDS,
      ),
    ).toBe("Amend   promise-refund  ");
  });

  it("always reads 'Close <id>' while closing, regardless of existingIds", () => {
    expect(
      composeDialogTitle(
        PROMISES,
        { id: "promise-refund", closing: true },
        EXISTING_IDS,
      ),
    ).toBe("Close promise-refund");
    expect(
      composeDialogTitle(
        PROMISES,
        { id: "not-in-the-set", closing: true },
        new Set<string>(),
      ),
    ).toBe("Close not-in-the-set");
  });
});

describe("the compose dialog's description", () => {
  it("gives the new-row hint when the id names no existing row", () => {
    expect(composeDialogDescription({ id: "" }, EXISTING_IDS)).toBe(
      "Give it a short, readable id — it is how anybody names this row later.",
    );
    expect(
      composeDialogDescription(
        { id: "promise-test-dark-mode" },
        EXISTING_IDS,
      ),
    ).toBe(
      "Give it a short, readable id — it is how anybody names this row later.",
    );
  });

  it("gives the amend hint once the id names a row that exists", () => {
    expect(
      composeDialogDescription({ id: "promise-refund" }, EXISTING_IDS),
    ).toBe(
      "Only what you change is written; everything else on the row is left alone.",
    );
  });
});
