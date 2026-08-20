import { describe, expect, it } from "vitest";

import {
  EVERY_STATUS,
  filteredEmptyNotice,
  statusFilterLabel,
  type LedgerSummary,
} from "@/api/ledgers";

/**
 * Issue #1217: what a ledger says when it is showing nothing, and what its
 * status filter calls itself.
 *
 * Both were the same class of mistake — a control reporting the wire's word
 * instead of the operator's. Searching a 14-row ledger for a term that matched
 * nothing produced "Nothing recorded here yet." beside a nav still counting
 * "9 open · 5 closed"; the filter's collapsed trigger read `all` while its own
 * open list read "Every status", and `in_progress` beside board columns headed
 * "In progress".
 *
 * Both branches are pure, which is why they live in `api/ledgers.ts` rather
 * than inside the view — the decision is the whole bug and it should be
 * assertable without a render (the reasoning `workflow-saved-toast` uses).
 */

/** A declared ledger: plain status names, one of them closing. */
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

/** The native board, whose statuses carry a `label` the wire word differs from. */
const BOARD = {
  ...PROMISES,
  slug: "tasks",
  source: "native",
  statuses: [
    { name: "in_progress", label: "In progress" },
    { name: "done", label: "Done", closed: true },
  ],
} as unknown as LedgerSummary;

describe("what the status filter calls itself", () => {
  it("names the sentinel rather than printing it", () => {
    expect(statusFilterLabel(PROMISES, EVERY_STATUS)).toBe("Every status");
    // A `Select` cannot store an empty string, but a host or a future default
    // that sends one must not render as a blank trigger either.
    expect(statusFilterLabel(PROMISES, "")).toBe("Every status");
  });

  it("qualifies a closing status, so the trigger reads like the list", () => {
    expect(statusFilterLabel(PROMISES, "open")).toBe("open");
    expect(statusFilterLabel(PROMISES, "kept")).toBe("kept (closed)");
  });

  it("prefers the declared label, which is what the board sends it for", () => {
    expect(statusFilterLabel(BOARD, "in_progress")).toBe("In progress");
    expect(statusFilterLabel(BOARD, "done")).toBe("Done (closed)");
  });

  it("falls back to the value, never to “Every status”", () => {
    // A filter we cannot name is still a filter. Rendering an unknown value as
    // the no-filter label would claim the ledger is unfiltered when it is not.
    expect(statusFilterLabel(PROMISES, "retired")).toBe("retired");
    expect(statusFilterLabel(null, "open")).toBe("open");
  });
});

describe("why a ledger is showing nothing", () => {
  it("says nothing at all when nothing is filtering", () => {
    // The one case where "Nothing recorded here yet." is a true sentence.
    expect(filteredEmptyNotice(PROMISES, "", EVERY_STATUS)).toBeNull();
    expect(filteredEmptyNotice(PROMISES, "   ", EVERY_STATUS)).toBeNull();
  });

  it("names the search that is hiding the rows", () => {
    expect(filteredEmptyNotice(PROMISES, "zzzznotathing", EVERY_STATUS)).toBe(
      "No rows match “zzzznotathing”.",
    );
    // Trimmed, so the quoted term is what the operator reads in the box.
    expect(filteredEmptyNotice(PROMISES, "  sso  ", EVERY_STATUS)).toBe(
      "No rows match “sso”.",
    );
  });

  it("names the status that is hiding the rows, in the operator's words", () => {
    expect(filteredEmptyNotice(PROMISES, "", "kept")).toBe(
      "No rows with status kept (closed).",
    );
    expect(filteredEmptyNotice(BOARD, "", "in_progress")).toBe(
      "No rows with status In progress.",
    );
  });

  it("names both when both are on", () => {
    expect(filteredEmptyNotice(PROMISES, "sso", "kept")).toBe(
      "No rows match “sso” with status kept (closed).",
    );
  });
});
