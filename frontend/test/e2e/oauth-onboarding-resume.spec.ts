import { expect, test } from "@playwright/test";

/**
 * Issue #300 — a legacy OAuth connection query from inside the onboarding tour.
 *
 * The redirect back from the provider is a **full-page navigation**, so neither
 * half of this can be proven by a unit test: the tour's step state lives in
 * react-joyride's memory and dies with the document, and the failure arms of the
 * host callback used to render a JSON body *as the page*. #838's callback now
 * has a terminal explanatory page; these cover the console's compatibility
 * handling for an older callback URL.
 *
 * These specs drive a running host (see `playwright.config.ts` — the harness
 * brings it up, there is no `webServer`). CI does not run Playwright.
 */

type Page = import("@playwright/test").Page;

/**
 * Dismiss the first-run welcome dialog if it is up.
 *
 * Not cosmetic: it is a Radix dialog, so while it is open every other element
 * is `aria-hidden` and therefore invisible to `getByRole`. Any role-based
 * assertion about the console underneath has to come after this.
 */
async function dismissWelcome(page: Page): Promise<void> {
  const skip = page.getByRole("button", { name: "Skip for now" });
  // Waits rather than sampling. The welcome is held until first-run setup has
  // read the roster and decided it has nothing to do
  // (`docs/spec/runtime/company-setup.md`), so it now appears one request later
  // than it used to — an instantaneous `isVisible()` loses that race and leaves
  // the dialog blocking every role-based assertion below. Absence is still
  // tolerated: a company that has already seen the tour never offers it.
  await skip.waitFor({ state: "visible", timeout: 10_000 }).catch(() => undefined);
  if (await skip.isVisible().catch(() => false)) await skip.click();
  await expect(skip).toBeHidden();
}

/** The tour's per-company localStorage key, discovered from the running app. */
async function tourKey(page: Page): Promise<string> {
  const key = await page.evaluate(() =>
    Object.keys(window.localStorage).find((k) => k.startsWith("oc-tour:")),
  );
  expect(key, "the console should have written a per-company tour key").toBeTruthy();
  return key!;
}

test("a legacy cancelled-handshake query lands in the console, not on a dead page", async ({ page }) => {
  // This is the query the former callback used after an operator cancelled at
  // the provider consent screen. Before the original fix it answered with
  // `{"error":"provider returned: access_denied"}` as the document body.
  await page.goto("/connections?connect_error=denied&provider=slack");

  // Assert the message first — the toast auto-dismisses, so anything that
  // blocks for a timeout before this would race it away.
  await expect(page.getByText(/cancelled/i)).toBeVisible();

  // The console renders — the operator is not stranded on raw JSON.
  await dismissWelcome(page);
  await expect(page.getByRole("heading", { name: "Connections", level: 1 })).toBeVisible();

  // The param is stripped, so a refresh doesn't re-fire the toast.
  await expect
    .poll(() => new URL(page.url()).searchParams.get("connect_error"))
    .toBeNull();

  // The grid is rendered and usable — the failure was a bounce-back, not a
  // terminal state.
  //
  // Issue #582 collapsed the page's two provider lists into one, so this asserts
  // the surviving grid's heading rather than a category section heading from the
  // catalogue grid that used to sit below it. The categories did not go away —
  // they are filter chips on the one grid now.
  await expect(page.getByRole("heading", { name: "Providers" })).toBeVisible();

  // Issue #599: this used to assert `getByRole("button", { name: "Connect" })`
  // was enabled. That button was only there because of the bug — this harness
  // company declares no `[[connection]]`, the binary carries no `composio`
  // feature and `host.sh` passes no `OPENCOMPANY_OAUTH_*`, so no route could
  // complete and clicking it 400'd with "provider is not enabled on this host".
  // A tile with no route now says so instead, which is what makes the retry
  // offer honest rather than merely present.
  await expect(page.getByTestId("provider-slack")).toContainText(/not available here/i);
});

test("an unknown failure code still produces a usable message", async ({ page }) => {
  // An older console against a newer host must not fall silent.
  await page.goto("/connections?connect_error=something_new_2099");
  await expect(page.getByText(/couldn't connect/i)).toBeVisible();
  await dismissWelcome(page);
  await expect(page.getByRole("heading", { name: "Connections", level: 1 })).toBeVisible();
});

test("the tour resumes on the Connections stop after a redirect", async ({ page }) => {
  await page.goto("/#/overview");

  // First run offers the tour. Skipping writes the per-company key, which is
  // how we learn the key name without hard-coding the company id.
  const skip = page.getByRole("button", { name: "Skip for now" });
  await expect(skip).toBeVisible();
  await skip.click();
  const key = await tourKey(page);

  // Seed exactly what `armTourResume` writes just before ConnectionsView hands
  // the browser to the provider: mid-tour, on the Connections stop, no
  // completed/skipped flag (the tour never finished).
  //
  // `"settings"`, NOT `"connections"`. The marker stores the stop's `view`
  // (`TourController`'s `before` hook publishes `stop.view` through
  // `setActiveTourStop`), and Connections became a *page of the Settings
  // section* — the stop is `{ view: "settings", sub: "connections" }`. This
  // spec seeded the pre-move value, which no `TOUR` stop matches, so the
  // controller's `findIndex` returned -1 and the resume was correctly skipped.
  // The spec had gone stale against a deliberate product change; it was not
  // catching a resume bug. See the unit suite's `tour-resume.test.ts`, which
  // pins the stop-view ⇄ marker coupling directly so this cannot rot again
  // without a fast test saying so.
  await page.evaluate(
    ([k]) =>
      window.localStorage.setItem(
        k,
        JSON.stringify({ pendingResume: { view: "settings", at: Date.now() } }),
      ),
    [key],
  );

  // The return trip from the provider.
  await page.goto("/connections?connected=slack");

  // Resumed on the stop the operator left...
  await expect(page.getByText("Connect your tools")).toBeVisible();
  // ...and NOT restarted from step 1, which is the bug.
  await expect(page.getByText("Welcome to your company")).toHaveCount(0);

  // The marker is consumed, so it can't fire again on a later visit.
  const after = await page.evaluate(([k]) => window.localStorage.getItem(k), [key]);
  expect(JSON.parse(after ?? "{}").pendingResume).toBeUndefined();
});

test("a stale resume marker does not hijack a later visit", async ({ page }) => {
  await page.goto("/#/overview");
  const skip = page.getByRole("button", { name: "Skip for now" });
  await expect(skip).toBeVisible();
  await skip.click();
  const key = await tourKey(page);

  // Older than the 15-minute TTL.
  //
  // `"settings"` for the same reason as above, and here it is what gives the
  // test any force at all: seeded with the stale `"connections"` this passed
  // whether or not the TTL was honoured, because no stop matches that view and
  // the tour would not have resumed either way. It asserted nothing. With a
  // view that *would* resume, the age is the only thing standing between this
  // marker and a hijacked visit — which is the property the test names.
  await page.evaluate(
    ([k]) =>
      window.localStorage.setItem(
        k,
        JSON.stringify({
          skipped: true,
          pendingResume: { view: "settings", at: Date.now() - 60 * 60 * 1000 },
        }),
      ),
    [key],
  );

  await page.goto("/#/overview");
  // No tour: the marker aged out and the tour is already marked skipped.
  await expect(page.getByText("Connect your tools")).toHaveCount(0);
  await expect(page.getByText("Welcome to your company")).toHaveCount(0);
});
