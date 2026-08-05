import { expect, test, type Locator, type Page } from "@playwright/test";

/**
 * End-to-end proof for issue #368 — the line recording an approval decision is
 * written somewhere the operator can actually see it.
 *
 * It used to be appended to the literal `"main"`, which is the id of the first
 * *fallback* desk and of no channel on a company that defines its own. The
 * harness company defines `engineering` and `content` and nothing called
 * `main`, so both halves of the decision landed in a transcript bucket no
 * channel renders — the failure copy included, which is the half that matters:
 * a sign-off that did not go through looked exactly like one that did.
 *
 * The approvals themselves are mocked, and deliberately so. Parking a real one
 * needs a model that decides to make a costly call, and this is a test about
 * where the console *files the answer*, not about what the company asks for.
 * The decision POST is mocked for the same reason on the failure path: a host
 * that refuses the write is the case that has to stay visible, and it cannot be
 * provoked on demand.
 *
 * Needs a running host and is not a CI gate, like the rest of `test/e2e`.
 */

const ENGINEERING = "engineering";

/** One parked approval, in the shape `GET …/approvals` answers. */
const PARKED = [
  {
    id: "appr-e2e-1",
    kind: "payment.send",
    amount_usd: 42.5,
    at_millis: Date.now(),
    task: { link: "unlinked" },
  },
];

test.beforeEach(async ({ page }) => {
  // Skip the first-run tour, whose modal would swallow the clicks below.
  await page.addInitScript(() => {
    const real = Storage.prototype.getItem;
    Storage.prototype.getItem = function getItem(key: string) {
      return key.startsWith("oc-tour:") ? '{"skipped":true}' : real.call(this, key);
    };
  });
  // The poll that feeds the approvals inbox. Answered for the whole test, so
  // the card is there whenever the operator arrives.
  //
  // Matched by suffix rather than by full path: the console addresses a *named*
  // company (`/api/v1/companies/<id>/…`) while the host also answers a
  // single-company alias, and a pattern pinned to one of them stops
  // intercepting — silently — the moment the deployment shape changes.
  await page.route("**/approvals", (route) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(PARKED) }),
  );
});

/**
 * The console body, excluding the toast layer.
 *
 * `ApprovalsView` raises a toast carrying the very same sentence it hands the
 * shell, and a toast is not the fix — the whole complaint in #368 is that a
 * decision was *only* ever a toast. Scoping here is what makes these
 * assertions about the transcript rather than about the notification.
 */
function console_(page: Page): Locator {
  return page.getByRole("main").first();
}

/**
 * Moves between views the way an operator does — through the sidebar. A
 * fragment-only `page.goto` is a same-document navigation the console may or
 * may not have re-rendered by the time the next locator is queried; the nav
 * button is both deterministic and the path the issue actually describes.
 */
async function navigate(page: Page, label: string, expectView: RegExp) {
  await page.locator(`[data-tour="nav-${label.toLowerCase()}"]`).getByRole("button").click();
  await expect(page).toHaveURL(expectView);
}

/**
 * Walks the operator through the console the way the issue describes it: open a
 * channel first (so there is a "last channel" to address), then go and decide.
 */
async function decideFrom(page: Page, channelId: string, action: "Approve" | "Decline") {
  await page.goto(`/#/chat/${channelId}`);
  await expect(page.getByPlaceholder(/^Message /)).toBeVisible({ timeout: 30_000 });

  await navigate(page, "Approvals", /#\/approvals/);
  const button = page.getByRole("button", { name: action });
  await expect(button).toBeVisible({ timeout: 30_000 });
  await button.click();
}

test("the line recording a decision is visible in a real channel", async ({ page }) => {
  await page.route("**/approvals/*", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ responses: [] }),
    }),
  );

  await decideFrom(page, ENGINEERING, "Approve");

  // Back in the channel the operator was last in — the console keeps that
  // across the trip to Approvals, which is the whole of the fix.
  await navigate(page, "Chat", /#\/chat/);
  await expect(console_(page).getByText(/Approved — the agent is completing the action/)).toBeVisible({
    timeout: 30_000,
  });
});

test("a decision the host refuses says so in the same channel", async ({ page }) => {
  await page.route("**/approvals/*", (route) =>
    route.fulfill({
      status: 500,
      contentType: "application/json",
      body: JSON.stringify({ error: "the store is unavailable", code: "store_failed" }),
    }),
  );

  await decideFrom(page, ENGINEERING, "Approve");

  await navigate(page, "Chat", /#\/chat/);
  // The half that used to be indistinguishable from success.
  await expect(console_(page).getByText(/Couldn't record your decision/)).toBeVisible({
    timeout: 30_000,
  });
});

test("with no channel ever opened, the decision still reaches the first desk", async ({ page }) => {
  // The fallback arm: an operator who goes straight to Approvals has no "last
  // channel". The line has to land on the company's first desk — the same one
  // Chat opens on — rather than in a bucket nothing renders.
  await page.route("**/approvals/*", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ responses: [] }),
    }),
  );

  await page.goto("/#/approvals");
  const decline = page.getByRole("button", { name: "Decline" });
  await expect(decline).toBeVisible({ timeout: 30_000 });
  await decline.click();

  await navigate(page, "Chat", /#\/chat/);
  await expect(console_(page).getByText(/^Declined:/)).toBeVisible({ timeout: 30_000 });
});
