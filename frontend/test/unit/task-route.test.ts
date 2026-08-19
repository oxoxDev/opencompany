// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useHashView } from "@/hooks/use-hash-view";
import { taskIdFromSegment } from "@/lib/task-route";

/**
 * The Tasks page is gone and its two addresses are not (issue #1140).
 *
 * `#/tasks/<id>` is the card detail, linked from chat, from an approval card,
 * from a workflow run's rows and from every card on the board — and Ledgers
 * deliberately reproduces none of what it shows. `#/tasks` is the page itself,
 * which an operator has in their history, their bookmarks and their fingers.
 * One has to keep resolving and the other has to land on the board, and both
 * failures are silent: a link that quietly shows Overview looks like a link
 * that worked.
 */

describe("taskIdFromSegment", () => {
  it("reads the card id out of the address's second segment", () => {
    expect(taskIdFromSegment("t-1")).toBe("t-1");
    expect(taskIdFromSegment("a%20b")).toBe("a b");
  });

  it("names no card when the address carries none", () => {
    expect(taskIdFromSegment(null)).toBeNull();
    expect(taskIdFromSegment("")).toBeNull();
    expect(taskIdFromSegment("   ")).toBeNull();
  });

  it("reads malformed encoding as no card rather than throwing", () => {
    // `decodeURIComponent("%")` throws `URIError`, and the address bar is
    // operator input: a typo must not end the render.
    expect(taskIdFromSegment("%")).toBeNull();
    expect(taskIdFromSegment("%zz")).toBeNull();
  });
});

/**
 * The shell's own rewrite, verbatim (`app-shell.tsx`). Duplicated rather than
 * exported because exporting it would make the shell's route table part of its
 * public surface for one test's benefit; what matters here is the rule, and the
 * rule is two lines.
 */
const REWRITE = (
  head: string,
  sub: string | null,
): [string, string | null] | null =>
  head === "tasks" && taskIdFromSegment(sub) === null ? ["ledgers", "tasks"] : null;

const VIEWS = ["overview", "ledgers", "tasks"] as const;

describe("the retired #/tasks address", () => {
  let container: HTMLDivElement;
  let root: Root;
  let seen: [string, string | null];

  function Probe() {
    const [view, sub] = useHashView<string>(
      VIEWS as unknown as readonly string[],
      "overview",
      REWRITE,
    );
    seen = [view, sub];
    return null;
  }

  async function visit(hash: string) {
    window.history.replaceState(null, "", hash);
    await act(async () => {
      root.render(createElement(Probe));
    });
  }

  beforeEach(() => {
    (
      globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
    ).IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
  });

  it("lands #/tasks on the board, which is the tasks ledger", async () => {
    await visit("#/tasks");
    expect(seen).toEqual(["ledgers", "tasks"]);
    expect(window.location.hash).toBe("#/ledgers/tasks");
  });

  it("replaces rather than pushes, so Back is not a trap", async () => {
    // A retired address that pushed its replacement would sit one Back away,
    // bounce the operator forward again on arrival, and never let go.
    await visit("#/tasks");
    const before = window.history.length;
    await visit("#/tasks");
    expect(window.history.length).toBe(before);
  });

  it("leaves #/tasks/<id> alone — the card detail still resolves", async () => {
    await visit("#/tasks/t-1");
    expect(seen).toEqual(["tasks", "t-1"]);
    expect(window.location.hash).toBe("#/tasks/t-1");
  });

  it("sends an address naming no readable card to the board", async () => {
    await visit("#/tasks/%");
    expect(seen).toEqual(["ledgers", "tasks"]);
    expect(window.location.hash).toBe("#/ledgers/tasks");
  });

  it("does not touch any other address", async () => {
    await visit("#/ledgers/goals");
    expect(seen).toEqual(["ledgers", "goals"]);
    expect(window.location.hash).toBe("#/ledgers/goals");
  });
});
