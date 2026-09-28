import { expect, test, type Page } from "@playwright/test";

/**
 * Proof for issue #1653: a teammate's face is a way into who they are.
 *
 * The complaint is that an avatar was inert everywhere it appeared. You could
 * be three hundred lines into a channel, wonder what the agent answering you is
 * actually allowed to do, and have no way to find out that did not mean leaving
 * the conversation for `#/company/agent/<id>` and navigating back.
 *
 * So the evidence is the click that used to do nothing: open a DM, click the
 * teammate's face in the header, and read their persona, tier, desks and
 * resolved tool grants **over the transcript**. Then take the panel's own
 * offer — Edit agent — and land on their page with the form already open,
 * because a summary that cannot hand off to the real controls is a dead end of
 * its own.
 *
 * Runs against the same live host as `agent-detail.spec.ts`
 * (`companies/e2e_harness`), whose manifest declares `engineer` on the
 * Engineering desk. Default features are enough.
 */

/** Same tour suppression as `agent-detail.spec.ts` — seeded before boot. */
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const seen = JSON.stringify({ skipped: true, seenAt: Date.now() });
    for (const key of ["oc-tour:single", "oc-tour:e2e-harness-co", "oc-tour:null"]) {
      window.localStorage.setItem(key, seen);
    }
  });
});

/** The DM with one teammate, open and rendered. */
async function openDm(page: Page, agentId: string) {
  await page.goto(`/#/chat/dm:${agentId}`);
  await expect(page.getByPlaceholder(/^Message /)).toBeVisible({ timeout: 30_000 });
}

test("an agent's face opens who they are, without leaving the channel", async ({ page }) => {
  await openDm(page, "engineer");

  await page.getByRole("button", { name: /Open .*'s profile/ }).first().click();

  const panel = page.getByTestId("agent-profile-panel");
  await expect(panel).toBeVisible();
  await expect(panel.getByTestId("agent-profile-id")).toHaveText("engineer");
  // The three facts the transcript could not answer: what the agent is, where
  // it sits, and what it may actually use.
  await expect(panel.getByTestId("agent-profile-tier")).toBeVisible();
  await expect(panel.getByTestId("agent-profile-desk-engineering")).toBeVisible();
  await expect(panel.getByTestId("agent-profile-tools")).toBeVisible();

  // Over the channel, not instead of it: the address never changed, so closing
  // the panel returns to the same transcript rather than to whatever the
  // browser's Back button would have found.
  await expect(page).toHaveURL(/#\/chat\/dm:engineer$/);
});

test("the panel hands off to the agent's own page, with the form open", async ({ page }) => {
  await openDm(page, "engineer");
  await page.getByRole("button", { name: /Open .*'s profile/ }).first().click();
  await expect(page.getByTestId("agent-profile-panel")).toBeVisible();

  await page.getByTestId("agent-profile-edit").click();

  // The flag is what makes this a hand-off rather than a second dead end: the
  // page opens *editing*, so the operator is not asked to find the Edit button
  // again on arrival.
  await expect(page).toHaveURL(/#\/company\/agent\/engineer\?edit\b/);
  await expect(page.getByTestId("agent-save")).toBeVisible({ timeout: 30_000 });

  // And the panel got out of the way of the page it sent them to.
  await expect(page.getByTestId("agent-profile-panel")).toHaveCount(0);
});

test("Back closes the editor and leaves the agent's page standing", async ({ page }) => {
  // Edit lives on the Instructions tab, not the Overview tab a plain
  // (non-`?edit`) arrival opens on — named in the address up front so the
  // click below is the one history entry this test means to undo.
  await page.goto("/#/company/agent/engineer?tab=instructions");
  await page.getByTestId("agent-edit").click();
  await expect(page).toHaveURL(/#\/company\/agent\/engineer\?tab=instructions&edit$/);
  await expect(page.getByTestId("agent-save")).toBeVisible({ timeout: 30_000 });

  await page.goBack();

  await expect(page).toHaveURL(/#\/company\/agent\/engineer\?tab=instructions$/);
  await expect(page.getByTestId("agent-save")).toHaveCount(0);
  await expect(page.getByTestId("agent-edit")).toBeVisible();
});
