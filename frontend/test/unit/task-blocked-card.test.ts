// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { Task } from "@/api/tasks";
import type { ApprovalSummary } from "@/api/types";
import { taskApprovalBlock } from "@/lib/task-approvals";
import { TaskItem } from "@/views/TaskCard";

/**
 * The paused card says what it is waiting on, and does not offer Resume as the
 * way out of it (issue #883).
 *
 * This suite is normally for pure functions — see `vitest.config.ts` — and the
 * exception is earned exactly as `approval-batch-card.test.ts` earns it: the
 * claim under test only exists at the rendered card. The issue's reproduction
 * is a loop, and the loop is a click:
 *
 *   1. a turn parks five approvals;
 *   2. the operator decides one, nothing visibly happens — the turn continues
 *      only when the *last* of them lands (#469);
 *   3. so Resume is the natural next click, and it re-dispatches: the agent
 *      re-runs from the top, parks the same calls again, the queue grows.
 *
 * `taskApprovalBlock` is unit-tested next door and decides *whether* the card is
 * blocked. What it cannot reach is whether the button the operator's hand lands
 * on is actually stopped, which is the half that breaks the loop.
 */

const T0 = new Date("2026-03-02T10:00:00Z").getTime();
const NOW = T0 + 240_000;

function card(): Task {
  return {
    id: "task-1",
    title: "Triage the release blockers",
    column: "paused",
    priority: "high",
    assignee: "qa",
    updatedAt: T0,
  } as Task;
}

function parked(id: string, kind: string, at = T0): ApprovalSummary {
  return {
    id,
    kind,
    amount_usd: null,
    at_millis: at,
    agent: "qa",
    task: { link: "task", id: "task-1" },
    payload: { url: "https://example.com/a" },
  };
}

let container: HTMLDivElement;
let root: Root;
let resumes: number;

async function render(approvals: ApprovalSummary[]) {
  resumes = 0;
  await act(async () => {
    root.render(
      createElement(TaskItem, {
        task: card(),
        dragging: false,
        block: taskApprovalBlock(approvals, "task-1"),
        now: NOW,
        onOpen: () => {},
        onResume: () => {
          resumes += 1;
        },
        onReview: () => {},
      }),
    );
  });
}

function resumeButton(): HTMLButtonElement {
  const button = Array.from(container.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Resume"),
  );
  if (!button) throw new Error(`no Resume button in:\n${container.innerHTML}`);
  return button as HTMLButtonElement;
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

describe("a paused card with approvals outstanding", () => {
  it("names the one call it is blocked on, rather than the mechanism", async () => {
    await render([parked("a1", "web_fetch")]);
    // `approvalAction`'s words — the same function the Approvals page and the
    // chat card label their rows with, so all three say one thing about one
    // approval instead of three different things.
    expect(container.textContent).toContain("Fetch a web page");
    expect(container.textContent).toContain("Waiting for your approval");
  });

  it("counts them when there is more than one, instead of quoting a list", async () => {
    await render([
      parked("a1", "shell"),
      parked("a2", "shell", T0 + 1_000),
      parked("a3", "shell", T0 + 2_000),
      parked("a4", "publish_artifact", T0 + 3_000),
    ]);
    expect(container.textContent).toContain("Blocked on 4 approvals");
  });

  /**
   * The behaviour the issue is actually about. Step 3 of its reproduction is an
   * operator pressing Resume on a card that is waiting, and the card getting
   * worse for it.
   */
  it("disables Resume, so the re-dispatch loop cannot be started from here", async () => {
    await render([parked("a1", "web_fetch")]);
    const button = resumeButton();
    expect(button.disabled).toBe(true);
    await act(async () => {
      button.click();
    });
    expect(resumes).toBe(0);
  });

  /**
   * Disabled, not hidden. A card with no visible next action is the ambiguity
   * being fixed — the operator has to see that Resume is the wrong click now,
   * not wonder where it went.
   */
  it("still shows the Resume button, with the reason on it", async () => {
    await render([parked("a1", "web_fetch")]);
    expect(resumeButton().getAttribute("title")).toContain("decide its approvals first");
  });
});

describe("a paused card with nothing outstanding", () => {
  it("renders no blocked row and an enabled Resume", async () => {
    await render([]);
    expect(container.textContent).not.toContain("Waiting for your approval");
    const button = resumeButton();
    expect(button.disabled).toBe(false);
    await act(async () => {
      button.click();
    });
    expect(resumes).toBe(1);
  });

  it("is not blocked by another card's approvals", async () => {
    await render([
      { ...parked("b1", "web_fetch"), task: { link: "task", id: "task-2" } },
    ]);
    expect(resumeButton().disabled).toBe(false);
  });
});
