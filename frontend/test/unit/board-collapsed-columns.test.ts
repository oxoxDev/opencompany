// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { TaskColumn } from "@/lib/board-columns";
import { LedgerBoard } from "@/views/LedgerBoard";

/**
 * Issue #1101 — a board whose work is all in later columns reads as empty.
 *
 * The report: To-do, Planning and In progress are the three columns that fit an
 * ordinary window, and on a company that has actually shipped something they
 * are the three that are empty. The operator gets three confident zeros and no
 * hint that 101 cards are two columns off the right edge.
 *
 * The fix collapses an empty column to a rail so the populated ones fit, and
 * the whole risk of it is the board's main gesture: **empty columns are exactly
 * the columns you drag into.** To-do is where returned work lands, In progress
 * is where a card is handed to its assignee. So the claims that matter here are
 * not "it looks narrower" — they are that a rail is still a drop target, that it
 * opens under a drag, and that a column the operator pinned open stays open.
 *
 * This suite is normally for pure functions (see `vitest.config.ts`), and it
 * earns the exception the same way `task-blocked-card.test.ts` does: every
 * claim above only exists at the rendered board. A helper returning
 * `collapsed: true` would prove nothing about whether the element under the
 * pointer still takes a drop.
 */

const COLUMNS: TaskColumn[] = [
  { id: "todo", label: "To-do", closed: false },
  { id: "planning", label: "Planning", closed: false },
  { id: "in_progress", label: "In progress", closed: false },
  { id: "paused", label: "Paused", closed: false },
  { id: "in_review", label: "In review", closed: false },
  { id: "done", label: "Done", closed: true },
];

interface Row {
  id: string;
  status: string;
}

/** The board as the issue found it: everything parked in the later columns. */
function laterColumnsOnly(): Row[] {
  return [
    ...Array.from({ length: 47 }, (_, n) => ({ id: `p${n}`, status: "paused" })),
    ...Array.from({ length: 54 }, (_, n) => ({ id: `r${n}`, status: "in_review" })),
  ];
}

let container: HTMLDivElement;
let root: Root;
let moves: Array<{ id: string; status: string }>;

async function render(rows: Row[], extra: { columnHeader?: boolean } = {}) {
  moves = [];
  await act(async () => {
    root.render(
      createElement(LedgerBoard<Row>, {
        columns: COLUMNS,
        rows,
        statusOf: (row) => row.status,
        renderCard: (row) => createElement("span", null, row.id),
        onMove: (row, status) => {
          moves.push({ id: row.id, status });
        },
        onMiss: () => {},
        columnHeader: extra.columnHeader
          ? (column) => (column.id === "todo" ? createElement("button", null, "+") : null)
          : undefined,
      }),
    );
  });
}

function column(id: string): HTMLElement {
  const found = container.querySelector<HTMLElement>(`[data-column="${id}"]`);
  if (!found) throw new Error(`no ${id} column in:\n${container.innerHTML}`);
  return found;
}

const isCollapsed = (id: string) => column(id).dataset.collapsed === "true";

/** Dispatches a bare DOM event React will wrap. jsdom has no `DragEvent`. */
async function fire(target: Element, type: string) {
  await act(async () => {
    target.dispatchEvent(new Event(type, { bubbles: true, cancelable: true }));
  });
}

/** Picks a card up, so the board's `dragId` fallback holds it like a real drag. */
async function pickUp(id: string) {
  const card = Array.from(container.querySelectorAll<HTMLElement>("[draggable=true]")).find(
    (held) => held.textContent === id,
  );
  if (!card) throw new Error(`no card ${id} to pick up`);
  await fire(card, "dragstart");
}

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

describe("a board whose work has moved to the later columns", () => {
  it("collapses the empty columns and leaves the populated ones alone", async () => {
    await render(laterColumnsOnly());

    expect(isCollapsed("todo")).toBe(true);
    expect(isCollapsed("planning")).toBe(true);
    expect(isCollapsed("in_progress")).toBe(true);
    expect(isCollapsed("done")).toBe(true);
    expect(isCollapsed("paused")).toBe(false);
    expect(isCollapsed("in_review")).toBe(false);
  });

  it("gives a rail an accessible name carrying the column and its count", async () => {
    await render(laterColumnsOnly());

    // The label is painted sideways and the count sits under it, which a screen
    // reader cannot see and would otherwise read as "To-do0". The name is the
    // one place both facts are actually available.
    const rail = column("todo").querySelector("button");
    expect(rail).not.toBeNull();
    expect(rail?.getAttribute("aria-label")).toBe("Expand To-do, 0 cards");
  });

  it("collapses nothing when the board is empty everywhere", async () => {
    // Six rails and no board is a worse answer to "show me the work" than the
    // three honest zeros this issue is about.
    await render([]);

    for (const held of COLUMNS) expect(isCollapsed(held.id)).toBe(false);
  });

  it("keeps a column open when its header slot holds a control", async () => {
    // A rail has nowhere to put the intake `+`, so collapsing To-do would hide
    // a control rather than some whitespace.
    await render(laterColumnsOnly(), { columnHeader: true });

    expect(isCollapsed("todo")).toBe(false);
    expect(isCollapsed("planning")).toBe(true);
  });
});

describe("dragging a card into a collapsed column", () => {
  it("opens the rail under the drag and takes the drop", async () => {
    await render(laterColumnsOnly());
    await pickUp("p0");

    await fire(column("in_progress"), "dragover");
    expect(isCollapsed("in_progress")).toBe(false);
    // And it reads as a landing spot rather than the same "nothing here" it
    // shows at rest.
    expect(column("in_progress").textContent).toContain("Drop it here");

    await fire(column("in_progress"), "drop");
    expect(moves).toEqual([{ id: "p0", status: "in_progress" }]);
  });

  it("folds the rail back once the drag moves on, without pinning it", async () => {
    await render(laterColumnsOnly());
    await pickUp("p0");

    await fire(column("todo"), "dragover");
    expect(isCollapsed("todo")).toBe(false);

    await fire(column("todo"), "dragleave");
    expect(isCollapsed("todo")).toBe(true);
  });

  it("leaves the other empty columns collapsed while one is hovered", async () => {
    await render(laterColumnsOnly());
    await pickUp("p0");

    await fire(column("in_progress"), "dragover");
    expect(isCollapsed("todo")).toBe(true);
    expect(isCollapsed("planning")).toBe(true);
  });
});

describe("pinning a collapsed column open", () => {
  it("opens on a click and stays open", async () => {
    await render(laterColumnsOnly());

    const rail = column("todo").querySelector("button");
    await act(async () => rail?.click());
    expect(isCollapsed("todo")).toBe(false);

    // Nothing but another click may fold it: a column that re-collapsed itself
    // while somebody was reading it would be this bug's mirror image. A drag
    // over its neighbour is the cheapest re-render to prove that with.
    await pickUp("p0");
    await fire(column("planning"), "dragover");
    await fire(column("planning"), "dragleave");
    expect(isCollapsed("todo")).toBe(false);
  });

  it("offers a control that folds it back up", async () => {
    await render(laterColumnsOnly());

    await act(async () => column("todo").querySelector("button")?.click());
    const fold = column("todo").querySelector<HTMLButtonElement>(
      'button[aria-label="Collapse To-do"]',
    );
    expect(fold).not.toBeNull();

    await act(async () => fold?.click());
    expect(isCollapsed("todo")).toBe(true);
  });

  it("offers no fold control on a column that holds work", async () => {
    await render(laterColumnsOnly());

    expect(column("paused").querySelector('button[aria-label="Collapse Paused"]')).toBeNull();
  });
});
