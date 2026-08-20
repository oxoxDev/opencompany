import { expect, test, type Page } from "@playwright/test";

/**
 * Proof for issue #343: an admin can set, change and clear a teammate's daily
 * cap **from the Console**, against the live host, with no redeploy.
 *
 * `team-budget.spec.ts` (issue #304) proves the cap is *displayed*. This proves
 * it is *editable* — which is the whole of #343, because before it the only way
 * to change a cap was for us to edit `company.toml` and ship a new image.
 *
 * Issue #1206 moved the editing controls off the roster card's `⋯` menu onto
 * the teammate's own detail page, beside Inbox (the same move #1190 made for
 * the Inbox switch) — the card kept the `⋯` menu, and it now holds only
 * Remove. So every interaction below opens the teammate first; the assertions
 * check both the detail page (where the edit landed) and the roster card
 * (which still displays the cap, unedited) agree on the same host state.
 *
 * Runs against the same live host as `wiring.spec.ts` (`companies/e2e_harness`),
 * whose `writer` carries a $5.00/day cap and whose `engineer` carries none. The
 * suite signs in through `global-setup.ts` as `harness-e2e@tinyhumans.ai`, which
 * the harness manifest lists under `[users] admins` — so the session driving
 * these specs really is an admin, and the controls below are visible.
 *
 * The spec restores every teammate it touches, so it can run repeatedly against
 * a host whose data directory persists between runs.
 */

/** The card for the teammate whose role matches `role`. */
function card(page: Page, role: string) {
  return page.getByTestId("team-card").filter({ hasText: role }).first();
}

/**
 * A fresh host greets the first visit with a welcome tour rendered over the
 * console, which swallows clicks on the view beneath it. Dismiss it if present.
 */
async function dismissOnboarding(page: Page) {
  const skip = page.getByRole("button", { name: "Skip for now" });
  await skip.waitFor({ state: "visible", timeout: 15_000 }).catch(() => {});
  if (await skip.isVisible()) {
    await skip.click();
    await expect(skip).toBeHidden();
  }
}

async function goToTeam(page: Page) {
  await page.goto("/#/company");
  await dismissOnboarding(page);
  await expect(page.getByTestId("team-card").first()).toBeVisible({ timeout: 30_000 });
}

/** Opens the teammate's detail page from its roster card. */
async function openDetail(page: Page, role: string) {
  await card(page, role).getByTestId("team-card-open").click();
  await expect(page.getByTestId("agent-budget")).toBeVisible({ timeout: 30_000 });
}

/** Sets a cap through the dialog, from the detail page. */
async function setCap(page: Page, amount: string) {
  await page.getByTestId("team-budget-edit").click();
  const input = page.getByTestId("team-budget-input");
  await expect(input).toBeVisible();
  await input.fill(amount);
  await page.getByTestId("team-budget-save").click();
}

test.beforeEach(async ({ page }) => {
  await goToTeam(page);
});

test("an admin can cap a teammate the company left uncapped, and reset it back", async ({
  page,
}) => {
  // The engineer starts uncapped: no budget line at all on the card.
  await expect(card(page, "Engineer").getByTestId("team-budget")).toHaveCount(0);

  await openDetail(page, "Engineer");
  await expect(page.getByTestId("agent-budget")).toContainText("spends freely");
  await setCap(page, "9");

  // The cap lands on the detail page, from the host — and attributed to a
  // person.
  await expect(page.getByTestId("agent-budget")).toHaveText(/\$9\.00\/day/, { timeout: 30_000 });
  await expect(page.getByTestId("agent-budget-attribution")).toContainText(/Set by/);

  // …and the card the operator came from agrees.
  await page.getByTestId("agent-breadcrumb-company").click();
  await expect(card(page, "Engineer").getByTestId("team-budget")).toHaveText(/\$9\.00\/day/, {
    timeout: 30_000,
  });

  // Host-backed, not local state: it survives a storage-cleared reload. This is
  // the assertion that separates "the console remembers" from "the company was
  // actually changed".
  await page.evaluate(() => {
    localStorage.clear();
    sessionStorage.clear();
  });
  await goToTeam(page);
  await expect(card(page, "Engineer").getByTestId("team-budget")).toHaveText(/\$9\.00\/day/, {
    timeout: 30_000,
  });

  // Reset hands the teammate back to the company's own definition — which for
  // the engineer means uncapped, so both the cap and the attribution go away.
  await openDetail(page, "Engineer");
  await page.getByTestId("team-budget-reset").click();
  await expect(page.getByTestId("agent-budget")).toContainText("spends freely", {
    timeout: 30_000,
  });
  await expect(page.getByTestId("agent-budget-attribution")).toHaveCount(0);
  await page.getByTestId("agent-breadcrumb-company").click();
  await expect(card(page, "Engineer").getByTestId("team-budget")).toHaveCount(0, {
    timeout: 30_000,
  });
});

test("an admin can change a company-set cap, remove it, and reset it back", async ({ page }) => {
  // The writer's $5.00 comes from the company definition, so no attribution.
  await expect(card(page, "Writer").getByTestId("team-budget")).toHaveText(/\$5\.00\/day/, {
    timeout: 30_000,
  });
  await expect(card(page, "Writer").getByTestId("team-budget-attribution")).toHaveCount(0);

  await openDetail(page, "Writer");
  await expect(page.getByTestId("agent-budget-attribution")).toHaveCount(0);

  // Raising it beats the company's number — the remedy this issue exists for.
  await setCap(page, "42");
  await expect(page.getByTestId("agent-budget")).toHaveText(/\$42\.00\/day/, { timeout: 30_000 });

  // Removing the cap is not the same as zeroing it: the line disappears
  // entirely rather than reading "$0.00/day", and the attribution stays so an
  // operator can see a human did this.
  await page.getByTestId("team-budget-remove").click();
  await expect(page.getByTestId("agent-budget")).toContainText("spends freely", {
    timeout: 30_000,
  });
  await expect(page.getByTestId("agent-budget-attribution")).toContainText(/Uncapped by/);
  await page.getByTestId("agent-breadcrumb-company").click();
  await expect(card(page, "Writer").getByTestId("team-budget")).toHaveCount(0, {
    timeout: 30_000,
  });

  // Reset restores the company's $5.00 — which no "set" could express, because
  // the value lives in the manifest rather than in the console.
  await openDetail(page, "Writer");
  await page.getByTestId("team-budget-reset").click();
  await expect(page.getByTestId("agent-budget")).toHaveText(/\$5\.00\/day/, { timeout: 30_000 });
  await expect(page.getByTestId("agent-budget-attribution")).toHaveCount(0);
  await page.getByTestId("agent-breadcrumb-company").click();
  await expect(card(page, "Writer").getByTestId("team-budget")).toHaveText(/\$5\.00\/day/, {
    timeout: 30_000,
  });
  await expect(card(page, "Writer").getByTestId("team-budget-attribution")).toHaveCount(0);
});
