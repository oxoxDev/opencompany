import { expect, test } from "@playwright/test";

/**
 * Issue #1408 — every way of finishing with the tour must be remembered.
 *
 * The bug was pure wiring: `TourController` registered its terminal handler as
 * `options.after`, which is react-joyride's **per-step** hook, so a run that
 * reached "Done" — or that the operator abandoned with "Skip tour" — recorded
 * nothing at all. `localStorage` had no `oc-tour:` key, and the welcome card
 * came back on the next full page load, forever. The only exit that ever
 * persisted was "Skip for now", because that one never goes through joyride.
 *
 * This is the half of the regression cover that a unit test cannot reach.
 * `test/unit/tour-terminal.test.ts` pins the decision — which event and status
 * mean "the run is over" — but it would have passed on the broken build,
 * because on the broken build the decision function was simply never called.
 * Only a browser can say whether the handler is attached to a prop joyride
 * actually invokes.
 *
 * So all three exits are walked, in a browser, and each is asserted twice: the
 * key is written, **and** the welcome stays away across a reload. The second
 * assertion is the one an operator would notice, and it is not implied by the
 * first — a key written at the wrong scope (a global `oc-tour`, say, instead of
 * the per-connection-per-company one) writes something and still re-offers.
 *
 * NOT a test of #1306, which is a separate, correctly-filed bug: the connection
 * profile churns on the first loads and can orphan a key written before it
 * settles. `settleConnectionScope` below waits that out on purpose, so these
 * specs report on the terminal handler and nothing else.
 */

type Page = import("@playwright/test").Page;

/**
 * How many stops `src/tour/steps.ts` declares.
 *
 * Hard-coded for the same reason `tour-popover-in-viewport.spec.ts` hard-codes
 * it — the e2e project has no `@/*` alias — and it rots just as loudly: the
 * per-step wait below reads the card's own "Step N of M" chip.
 */
const TOUR_STOPS = 7;

/** The welcome card's heading; its presence is the whole symptom of #1408. */
const WELCOME = "Welcome to your company";

/**
 * Load the console and let the browser-local connection scope stop moving.
 *
 * The tour key is scoped per (connection, company) — `oc-tour:<connection>::<company>`
 * — and issue #1306 has the connection profile still being re-minted for the
 * first load or two of a fresh profile. A key written before that settles is
 * orphaned by the next load, which looks exactly like the bug under test here
 * and is not it. Reloading until `oc.connections.v1` holds still separates the
 * two, so a failure below is unambiguously the terminal handler.
 */
async function settleConnectionScope(page: Page): Promise<void> {
  const profiles = () =>
    page.evaluate(() => window.localStorage.getItem("oc.connections.v1") ?? "");

  await page.goto("/#/overview");
  await expect(page.getByText(WELCOME), "first run should offer the tour").toBeVisible();

  // Two reloads is what the audit needed; poll rather than fix the count so a
  // slower host does not turn this into a flake.
  let previous = await profiles();
  for (let attempt = 0; attempt < 5; attempt += 1) {
    await page.reload();
    await expect(page.getByText(WELCOME)).toBeVisible();
    const current = await profiles();
    if (current === previous) return;
    previous = current;
  }
  throw new Error(
    "the connection profile list never stopped changing across reloads; see issue #1306",
  );
}

/** The tour's per-(connection, company) key and its parsed value, from the running app. */
async function readTourRecord(page: Page): Promise<{ key: string; value: Record<string, unknown> }> {
  const found = await page.evaluate(() => {
    const key = Object.keys(window.localStorage).find((k) => k.startsWith("oc-tour:"));
    return key ? { key, raw: window.localStorage.getItem(key) ?? "" } : null;
  });
  expect(found, "the console should have written a tour key").not.toBeNull();
  return { key: found!.key, value: JSON.parse(found!.raw) as Record<string, unknown> };
}

/**
 * Assert the record is scoped, not global.
 *
 * `oc-tour` on its own would suppress the tour for every company on every host
 * an operator has ever opened — which is a *worse* bug than the one being
 * fixed, and one that would sail past a "the welcome is gone" assertion.
 */
function expectScopedKey(key: string): void {
  expect(key, "the tour key must carry a scope, not be a bare global").not.toBe("oc-tour");
  expect(key).toMatch(/^oc-tour:.+/);
}

/** Reload the document and assert the welcome card does not come back. */
async function expectWelcomeStaysAway(page: Page, exit: string): Promise<void> {
  // Back to Overview first. A finished tour leaves the console on its last
  // stop's view, and the liveness check below needs one known anchor for all
  // three exits. Changing only the fragment is not a document load, so the
  // `reload()` after it is what makes this the *full page load* the bug was
  // reported against.
  await page.goto("/#/overview");
  await page.reload();
  // Positively wait for the console to have rendered before concluding the
  // welcome is absent, otherwise this would pass against a blank page.
  await expect(page.getByRole("heading", { name: "Overview", level: 1 })).toBeVisible();
  await expect(
    page.getByText(WELCOME),
    `after ${exit}, the welcome must not be re-offered on reload`,
  ).toHaveCount(0);
}

/** Advance the running tour from the stop it is on. `"Done"` on the last one. */
async function advance(page: Page, step: number): Promise<void> {
  const card = page.getByRole("alertdialog");
  await expect(
    card.getByText(`Step ${step} of ${TOUR_STOPS}`),
    `step ${step} should render (and the tour should still have ${TOUR_STOPS} stops)`,
  ).toBeVisible();
  // floating-ui positions the card a frame or two after it mounts; clicking on
  // the first frame races the move.
  await page.waitForTimeout(250);
  // Matched on the rendered label: joyride puts its own locale string on
  // `primaryProps`' `aria-label`, which would win over the text on an
  // accessible-name match.
  await card.locator("button", { hasText: step === TOUR_STOPS ? "Done" : "Next" }).click();
}

test("completing the tour is remembered across a reload", async ({ page }) => {
  await settleConnectionScope(page);
  await page.getByRole("button", { name: "Take the tour" }).click();

  for (let step = 1; step <= TOUR_STOPS; step += 1) {
    await advance(page, step);
  }

  // The run ended rather than dead-ending on a stop.
  await expect(page.getByRole("alertdialog")).toHaveCount(0);

  const { key, value } = await readTourRecord(page);
  expectScopedKey(key);
  // Completing records `completed`, not `skipped`: both silence the welcome,
  // but "watched the whole tour" and "bailed out at step 3" stay different
  // facts. See `src/tour/events.ts`.
  expect(value.completed, "Done should record a completed run").toBe(true);
  expect(value.skipped).toBeUndefined();

  await expectWelcomeStaysAway(page, "completing the tour");
});

test("the tour's composer stops open #general, which is writable", async ({ page }) => {
  await settleConnectionScope(page);
  await page.getByRole("button", { name: "Take the tour" }).click();

  await advance(page, 1);
  const card = page.getByRole("alertdialog");
  await expect(card.getByText("Talk to your company")).toBeVisible();
  await expect.poll(() => new URL(page.url()).hash).toMatch(/^#\/chat\/general(?:\?|$)/);
  await expect(page.locator('[data-tour="chat-composer"]')).toBeVisible();
  await expect(page.getByPlaceholder("Message #general")).toBeVisible();
});

test("skipping from inside a running tour is remembered across a reload", async ({ page }) => {
  await settleConnectionScope(page);
  await page.getByRole("button", { name: "Take the tour" }).click();

  // Part-way in, which is the reported case: the operator accepted the
  // invitation and then bailed.
  await advance(page, 1);
  await advance(page, 2);

  const card = page.getByRole("alertdialog");
  await expect(card.getByText(`Step 3 of ${TOUR_STOPS}`)).toBeVisible();
  await page.waitForTimeout(250);
  // `locator("button", { hasText })` again, not an accessible-name match:
  // joyride labels `skipProps` with its own locale string ("Skip"), which wins
  // over the text `TourTooltip` renders.
  await card.locator("button", { hasText: "Skip tour" }).click();

  await expect(card).toHaveCount(0);

  const { key, value } = await readTourRecord(page);
  expectScopedKey(key);
  expect(value.skipped, "Skip tour should record a skipped run").toBe(true);
  expect(value.completed).toBeUndefined();

  await expectWelcomeStaysAway(page, "skipping mid-run");
});

test("refusing the tour before it starts is remembered across a reload", async ({ page }) => {
  // The one exit that already worked. It is here so the three are pinned
  // together: this path never touches joyride, so a future refactor that moved
  // all three onto one mechanism would need to keep it working too, and this
  // is what would say so.
  await settleConnectionScope(page);
  await page.getByRole("button", { name: "Skip for now" }).click();

  const { key, value } = await readTourRecord(page);
  expectScopedKey(key);
  expect(value.skipped, "Skip for now should record a skipped run").toBe(true);

  await expectWelcomeStaysAway(page, "refusing before the tour starts");
});
