// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { ApprovalSummary, GrantScope, Verdict } from "@/api/types";
import { ApprovalRow } from "@/views/chat/ApprovalRow";

/**
 * The consolidated card's decisions (issue #842).
 *
 * This suite is normally for pure functions — see `vitest.config.ts` — and the
 * exception is earned the same way `provider-detail-render` earns it: the thing
 * under test *is* what reaches the operator's hand. The issue's whole claim is
 * that one click can answer three gated calls **without** widening what any of
 * them buys, and three of those claims are only true at the click:
 *
 *  1. one Approve resolves every item, each on its own id — so each approved
 *     call still mints its own host-scoped grant (#739) rather than one grant
 *     spanning the batch, and three fetches still produce three independently
 *     revocable standing permissions;
 *  2. the card is **all-or-nothing** — it answers every item it is still asking
 *     about, because the turn stays blocked until each parked call has a
 *     verdict (#469), so a decision that left one open would hold the turn
 *     while looking as though it had resolved the card;
 *  3. an item decided elsewhere — the Approvals page, another tab — is not
 *     re-resolved, and the card stops listing it as pending.
 *
 * A pure test of the grouping cannot reach any of them: it can see the card is
 * built, not what pressing it sends.
 */

const T0 = new Date("2026-03-02T10:00:00Z").getTime();

function approval(id: string, url: string): ApprovalSummary {
  return {
    id,
    kind: "web_fetch",
    amount_usd: null,
    at_millis: T0,
    agent: "seo",
    thread: "desk-marketing",
    batch: "turn-1",
    broadly_grantable: true,
    payload: { url },
  };
}

const ESPN = approval("a1", "https://espn.com/nba");
const BBC = approval("a2", "https://bbc.com/sport");
const GUARDIAN = approval("a3", "https://theguardian.com/uk");

interface Decision {
  id: string;
  verdict: Verdict;
  scope: GrantScope;
}

let container: HTMLDivElement;
let root: Root;
let decisions: Decision[];

async function render(
  approvals: ApprovalSummary[],
  decided: Record<string, Verdict> = {},
  failed: Record<string, string> = {},
  deciding: ReadonlyMap<string, Verdict> = new Map(),
) {
  await act(async () => {
    root.render(
      createElement(ApprovalRow, {
        approvals,
        now: T0 + 60_000,
        askerNames: new Map([["seo", "SEO Specialist"]]),
        deciding,
        decided,
        failed,
        onDecide: (approval: ApprovalSummary, verdict: Verdict, scope: GrantScope) =>
          decisions.push({ id: approval.id, verdict, scope }),
      }),
    );
  });
}

/** Every item line on the card, in render order. */
function items(): HTMLElement[] {
  return [...container.querySelectorAll<HTMLElement>("[data-approval-item]")];
}

/** The one compact transcript row a fully settled turn leaves behind (#970). */
function receipts(): HTMLElement[] {
  return [...container.querySelectorAll<HTMLElement>("[data-approval-receipt]")];
}

function button(label: string): HTMLButtonElement {
  const match = [...container.querySelectorAll("button")].find((b) =>
    (b.textContent ?? "").includes(label),
  );
  if (!match) throw new Error(`no "${label}" button on the card: ${container.textContent}`);
  return match as HTMLButtonElement;
}

async function click(el: HTMLElement) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

/**
 * Toggle a checkbox or radio the way a person does — by clicking it.
 *
 * Deliberately **not** by assigning `.checked` first: React tracks an input's
 * last-rendered value to decide whether a click changed anything, so setting it
 * by hand makes the click look like a no-op and `onChange` never fires. The
 * click's own activation behaviour flips the box, which is both what a browser
 * does and what React is watching for.
 */
async function toggle(input: HTMLInputElement) {
  await act(async () => {
    input.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  decisions = [];
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("the consolidated approval card", () => {
  it("asks once for a turn's three gated calls, naming each of them", async () => {
    await render([ESPN, BBC, GUARDIAN]);

    const text = container.textContent ?? "";
    expect(text).toContain("SEO Specialist");
    expect(text).toContain("3 sign-offs");
    for (const url of [
      "https://espn.com/nba",
      "https://bbc.com/sport",
      "https://theguardian.com/uk",
    ]) {
      expect(text).toContain(url);
    }
    // One decision to make, not three — and no per-item control beside it.
    // Granularity is the Approvals page's job; offering it here too would be a
    // second copy of the same state to keep in step.
    expect(container.querySelectorAll("button")).toHaveLength(2);
    expect(items()).toHaveLength(3);
    expect(container.querySelectorAll('input[type="checkbox"]')).toHaveLength(0);
  });

  it("resolves every item on its own id, so each mints its own grant", async () => {
    await render([ESPN, BBC, GUARDIAN]);
    await click(button("Approve"));

    // Three resolves, not one batch resolve. The host has no batch to decide —
    // the park is the unit of truth, and a grant is minted per approval from
    // that approval's own arguments.
    expect(decisions).toEqual([
      { id: "a1", verdict: "approve", scope: { kind: "once" } },
      { id: "a2", verdict: "approve", scope: { kind: "once" } },
      { id: "a3", verdict: "approve", scope: { kind: "once" } },
    ]);
  });

  it("carries the chosen scope to every item, so each gets its own standing grant", async () => {
    await render([ESPN, BBC]);
    // The broader option. One choice on the card, one standing permission per
    // item — each scoped to that item's own host when the host mints it (#739),
    // which is why approving three fetches leaves three independently revocable
    // rows under Standing permissions rather than one that spans them.
    const forAPeriod = [...container.querySelectorAll<HTMLInputElement>('input[type="radio"]')][1];
    await toggle(forAPeriod);
    await click(button("Approve"));

    expect(decisions).toEqual([
      { id: "a1", verdict: "approve", scope: { kind: "tool", expiresInMillis: 60 * 60 * 1000 } },
      { id: "a2", verdict: "approve", scope: { kind: "tool", expiresInMillis: 60 * 60 * 1000 } },
    ]);
  });

  it("declines the whole batch with one Decline, granting nothing", async () => {
    await render([ESPN, BBC]);
    // Even with the broader scope selected: a decline has nothing to grant, so
    // it must not carry a duration the operator picked for a yes.
    const forAPeriod = [...container.querySelectorAll<HTMLInputElement>('input[type="radio"]')][1];
    await toggle(forAPeriod);
    await click(button("Decline"));

    expect(decisions).toEqual([
      { id: "a1", verdict: "deny", scope: { kind: "once" } },
      { id: "a2", verdict: "deny", scope: { kind: "once" } },
    ]);
  });

  it("stops listing an item decided on the Approvals page, and says how many are left", async () => {
    // The drift case: both surfaces open, one row approved over there. The card
    // must not go on claiming three things are pending.
    await render([ESPN, BBC, GUARDIAN], { a1: "approve" });

    const text = container.textContent ?? "";
    expect(text).toContain("Approved");
    expect(text).toContain("1 of 3 decided");
    // Still listed — the operator has to see their own decision land — but
    // shown as settled rather than as something still being asked about.
    expect(items()).toHaveLength(3);
    expect(items()[0].textContent).toContain("Approved");

    await click(button("Approve"));
    // And an approve here covers only what is still open. Re-resolving a1 would
    // be a second decision on an approval the host has already dropped.
    expect(decisions.map((d) => d.id)).toEqual(["a2", "a3"]);
  });

  it("names the item whose decision did not land, and does not call it pending", async () => {
    // The failure consolidation makes worse. One click, three resolves, and the
    // third fails: two effects are authorised and one is not. An item that
    // simply dropped back to its pending look would read as "still working",
    // and the operator's honest conclusion would be that they got all three.
    await render([ESPN, BBC, GUARDIAN], { a1: "approve", a2: "approve" }, { a3: "host is away" });

    const text = container.textContent ?? "";
    expect(text).toContain("Not recorded");
    expect(text).toContain("host is away");
    // Which one, on the row itself — a toast says a decision failed without
    // saying which, and is gone by the time the operator looks back.
    const failedRow = container.querySelector('[data-approval-failed="true"]');
    expect(failedRow?.getAttribute("data-approval-item")).toBe("a3");
    expect(failedRow?.textContent).toContain("https://theguardian.com/uk");
  });

  it("counts the failures honestly rather than claiming nothing was recorded", async () => {
    await render([ESPN, BBC, GUARDIAN], { a1: "approve", a2: "approve" }, { a3: "host is away" });

    // Two of the three DID take. "Nothing was recorded" would be a fresh lie in
    // place of the silence this replaces.
    const text = container.textContent ?? "";
    expect(text).toContain("1 of 3 weren't recorded");
    expect(text).not.toContain("None of the 3");
  });

  it("shows the settled verdict, not a stale failure, once the item resolves elsewhere", async () => {
    // Failed here, then resolved on the Approvals page or in another tab: the
    // item carries both a failure and a verdict. A failure describes one
    // *attempt*; the verdict describes the approval, and the host has already
    // acted on it. Saying "not recorded" over that would be the card
    // contradicting the queue — the drift this whole change exists to remove.
    await render([ESPN, BBC], { a2: "approve" }, { a2: "host is away" });

    const settled = container.querySelector('[data-approval-item="a2"]');
    expect(settled?.textContent).toContain("Approved");
    expect(settled?.textContent).not.toContain("Not recorded");
    expect(container.querySelector('[data-approval-failed="true"]')).toBeNull();
    // And the card counts only what is still open: a2 is decided, so it is not
    // one of the failures still waiting on anybody.
    expect(container.textContent ?? "").not.toContain("weren't recorded");
  });

  it("leaves the buttons live after a failure, because a retry is the way out", async () => {
    await render([ESPN, BBC, GUARDIAN], { a1: "approve", a2: "approve" }, { a3: "host is away" });

    expect(button("Approve").disabled).toBe(false);
    await click(button("Approve"));
    // Only the item that never landed is retried — the two that did are settled
    // and re-resolving them would be a second decision on approvals the host
    // has already dropped.
    expect(decisions.map((d) => d.id)).toEqual(["a3"]);
  });

  it("leaves one expandable release receipt for a fully approved turn", async () => {
    await render([ESPN, BBC, GUARDIAN], { a1: "approve", a2: "approve", a3: "approve" });

    // Three decisions from one parked turn do not become three permanent
    // transcript rows. The receipt is about the one release, not the clicks.
    expect(receipts()).toHaveLength(1);
    expect(receipts()[0].textContent).toContain(
      "Approved 3 actions — the teammate is picking it up now",
    );
    expect(container.querySelectorAll("button")).toHaveLength(0);

    // The individual verdicts remain inspectable, but do not flood the channel
    // until the operator asks for them.
    const disclosure = receipts()[0].querySelector("details");
    expect(disclosure?.open).toBe(false);
    expect(disclosure?.textContent).toContain("Show individual decisions");
    expect(items()).toHaveLength(3);
    expect(items().map((item) => item.textContent)).toEqual(
      expect.arrayContaining([
        expect.stringContaining("https://espn.com/nba"),
        expect.stringContaining("https://bbc.com/sport"),
        expect.stringContaining("https://theguardian.com/uk"),
      ]),
    );
  });

  it("summarizes mixed verdicts honestly once the turn releases", async () => {
    await render([ESPN, BBC], { a1: "approve", a2: "deny" });

    expect(receipts()).toHaveLength(1);
    expect(receipts()[0].textContent).toContain(
      "Approved 1 action and declined 1 action — the teammate is picking it up now",
    );
    // Nothing left to decide, so nothing left to press.
    expect(container.querySelectorAll("button")).toHaveLength(0);
  });

  it("says plainly when every action in a settled turn was declined", async () => {
    await render([ESPN, BBC], { a1: "deny", a2: "deny" });

    expect(receipts()).toHaveLength(1);
    expect(receipts()[0].textContent).toContain(
      "Declined 2 actions — the teammate will not take them",
    );
    expect(receipts()[0].querySelector("details")?.open).toBe(false);
  });

  it("keeps a settled single approval natural", async () => {
    await render([ESPN], { a1: "approve" });

    // Unlike the multi-item receipts above, a single-item card cannot know the
    // turn's stillAwaiting count is zero — #561's neutral "recorded" wording,
    // not a "picking it up now" claim this card has no basis for.
    expect(receipts()).toHaveLength(1);
    expect(receipts()[0].textContent).toBe("Approved — recorded");
    expect(receipts()[0].querySelector("details")).toBeNull();
  });

  it("renders a single-call turn exactly as it did before batching", async () => {
    await render([ESPN]);

    // No item list, no counts — the consolidation earns its furniture only when
    // there is something to consolidate.
    expect(items()).toHaveLength(0);
    expect(container.textContent ?? "").not.toContain("sign-offs");
    await click(button("Approve"));
    expect(decisions).toEqual([{ id: "a1", verdict: "approve", scope: { kind: "once" } }]);
  });
});
