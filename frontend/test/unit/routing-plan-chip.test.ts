// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { describePlan, RoutingPlanChip } from "@/components/episode/RoutingPlanChip";
import type { RoutingPlanDto, RoutingRouter } from "@/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

const NAMES = { engineer: "Engineer", ceo: "CEO" };

function render(plan: RoutingPlanDto, router?: RoutingRouter) {
  act(() => {
    root.render(createElement(RoutingPlanChip, { plan, router, agentNames: NAMES }));
  });
  return host.querySelector('[data-testid="routing-plan-chip"]') as HTMLElement;
}

describe("RoutingPlanChip", () => {
  it("describes each plan kind in one line", () => {
    expect(describePlan({ kind: "one", primaryId: "engineer" }, NAMES)).toBe("→ Engineer");
    expect(describePlan({ kind: "hive", primaryId: "engineer", invitedIds: ["engineer", "ceo"] }, NAMES)).toBe("→ Engineer + CEO");
    expect(describePlan({ kind: "hive", primaryId: "engineer", invitedIds: [] }, NAMES)).toBe("→ Engineer");
    expect(describePlan({ kind: "clarify", question: "Which one?" })).toBe("clarify: Which one?");
    expect(describePlan({ kind: "clarify" })).toBe("clarify");
    expect(describePlan({ kind: "fallback", reason: "no router" })).toBe("fallback: no router");
    // The host names the seat it fell back to (`RoutingPlanDto::Fallback.primary_id`).
    expect(describePlan({ kind: "fallback", primaryId: "ceo", reason: "provider_unavailable" }, NAMES)).toBe(
      "→ CEO (fallback: provider_unavailable)",
    );
    // An unnamed agent falls back to its id — the truth, not a blank.
    expect(describePlan({ kind: "one", primaryId: "writer" }, NAMES)).toBe("→ writer");
  });

  it("stamps the kind and the router, and shows the router as a badge", () => {
    const chip = render({ kind: "one", primaryId: "engineer" }, "explicit");
    expect(chip.dataset.planKind).toBe("one");
    expect(chip.dataset.router).toBe("explicit");
    expect(chip.textContent).toContain("explicit");
    expect(chip.title).toBe("routed by explicit");
  });

  it("shows no router badge when the host did not say", () => {
    const chip = render({ kind: "fallback", reason: "router unavailable" });
    expect(chip.dataset.router).toBeUndefined();
    expect(chip.textContent).toBe("fallback: router unavailable");
  });
});
