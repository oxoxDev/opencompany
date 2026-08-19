// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { TaskPlan } from "@/api/tasks";
import { TaskPlanBrief } from "@/views/TaskPlanBrief";

/**
 * Issue #1106: a card whose planner could not separate two teammates asks,
 * instead of picking one and recording nothing about the other.
 *
 * This suite is normally for pure functions, and this is the same earned
 * exception `task-blocked-card.test.ts` takes: the claim only exists at the
 * rendered brief. The resolver's rules (drop, dedup, cap) and the park itself
 * are pinned in Rust, next to the code that enforces them. What no Rust test can
 * reach is whether the operator is actually shown the runner-up *and its
 * reason*, and whether one click answers — which is the whole acceptance.
 */

const PLANNED_AT = new Date("2026-03-02T10:00:00Z").getTime();

function plan(overrides: Partial<TaskPlan> = {}): TaskPlan {
  return {
    description: "Fetch what is trending and summarise it.",
    steps: [],
    prerequisites: [],
    risks: [],
    verification: "the digest exists",
    scope: "the digest only",
    plannedAtMillis: PLANNED_AT,
    ...overrides,
  };
}

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

function render(props: Parameters<typeof TaskPlanBrief>[0]) {
  act(() => root.render(createElement(TaskPlanBrief, props)));
}

function assignButtons(): HTMLButtonElement[] {
  return [...host.querySelectorAll("button")].filter((b) =>
    (b.textContent ?? "").includes("Assign"),
  ) as HTMLButtonElement[];
}

describe("the plan brief's ownership question", () => {
  /**
   * The unambiguous card is untouched. A plan with one proposal — or none —
   * renders exactly what it rendered before this field existed, so the common
   * case gains no new furniture.
   */
  it("renders nothing at all when the pass was not ambiguous", () => {
    render({ plan: plan({ proposedAssignee: "devrel" }), onPick: () => {} });
    expect(host.querySelector('[data-testid="plan-owner-choice"]')).toBeNull();
    expect(assignButtons()).toHaveLength(0);
  });

  it("treats an absent candidate list the same as an empty one", () => {
    render({ plan: plan({ assigneeCandidates: [] }), onPick: () => {} });
    expect(host.querySelector('[data-testid="plan-owner-choice"]')).toBeNull();
  });

  /**
   * The defect, inverted: both teammates are named, and each carries the reason
   * it was named for. A bare pair of ids would hand back the judgement the
   * planner already made.
   */
  it("shows every candidate with its reason", () => {
    render({
      plan: plan({
        assigneeCandidates: [
          { id: "devrel", reason: "talks to developers all day" },
          { id: "social_manager", reason: "owns the company's social accounts" },
        ],
      }),
      onPick: () => {},
    });

    const block = host.querySelector('[data-testid="plan-owner-choice"]');
    expect(block).not.toBeNull();
    const text = block?.textContent ?? "";
    expect(text).toContain("devrel");
    expect(text).toContain("talks to developers all day");
    expect(text).toContain("social_manager");
    expect(text).toContain("owns the company's social accounts");
  });

  /** One click answers, and it answers with that row's candidate. */
  it("hands the picked candidate's id back on one click", async () => {
    const onPick = vi.fn();
    render({
      plan: plan({
        assigneeCandidates: [
          { id: "devrel", reason: "first" },
          { id: "social_manager", reason: "second" },
        ],
      }),
      onPick,
    });

    const buttons = assignButtons();
    expect(buttons).toHaveLength(2);
    // Async, so the in-flight flag the handler clears on settle is flushed
    // inside `act` rather than after the test has moved on.
    await act(async () => {
      buttons[1].click();
    });

    expect(onPick).toHaveBeenCalledTimes(1);
    expect(onPick).toHaveBeenCalledWith("social_manager");
  });

  /**
   * No default and no highlighted "best" row: every candidate is offered on the
   * same terms. A pre-selected first option would be the silent pick this issue
   * removes, wearing a suggestion's clothes.
   */
  it("offers every candidate on equal terms, with none pre-selected", () => {
    render({
      plan: plan({
        assigneeCandidates: [
          { id: "devrel", reason: "first" },
          { id: "social_manager", reason: "second" },
        ],
      }),
      onPick: () => {},
    });

    const buttons = assignButtons();
    expect(buttons).toHaveLength(2);
    for (const button of buttons) {
      expect(button.disabled).toBe(false);
    }
    expect(host.querySelector("[aria-selected='true']")).toBeNull();
  });

  /**
   * A surface that cannot write the card still shows what the pass declined to
   * decide. The runner-up being *recorded* is the acceptance criterion; being
   * clickable is the convenience on top of it.
   */
  it("still records the candidates when no pick handler is wired", () => {
    render({
      plan: plan({
        assigneeCandidates: [
          { id: "devrel", reason: "first" },
          { id: "social_manager", reason: "second" },
        ],
      }),
    });

    const text = host.querySelector('[data-testid="plan-owner-choice"]')?.textContent ?? "";
    expect(text).toContain("devrel");
    expect(text).toContain("social_manager");
    expect(assignButtons()).toHaveLength(0);
  });
});
