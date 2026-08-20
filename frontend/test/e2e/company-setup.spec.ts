import { expect, test, type APIRequestContext, type Page } from "@playwright/test";

/**
 * First-run company setup, end to end
 * (`docs/spec/runtime/company-setup.md`).
 *
 * Three questions asked once, then a team created on the host. This spec covers
 * the half only a browser proves: that the dialog opens by itself on an
 * unstaffed company, that the build-out screen names each teammate as its write
 * lands, and that the roster the operator is left looking at came from the
 * **host** rather than from the console's fabricated starter team.
 *
 * A unit test pins the decisions (`test/unit/company-setup.test.ts`); this pins
 * that they are wired to a real host.
 *
 * # This lane needs its own company
 *
 * Setup opens only when the roster is empty, and every company under
 * `companies/` declares agents in its manifest — including
 * `companies/e2e_harness`, this suite's default. So a company that came with a
 * team can never reach the flow, and running this spec against the default host
 * would fail on a dialog that correctly never appears.
 *
 * Run it against the fixture that ships with nobody on it:
 *
 * ```sh
 * PW_HOST_COMPANY=companies/e2e_setup npx playwright test company-setup
 * ```
 *
 * The guard below skips rather than fails when that is missing, so an ordinary
 * `npx playwright test` stays green — and so a reader of the skip reason learns
 * what to set. **It is therefore not covered by any lane that does not set
 * `PW_HOST_COMPANY`**; a CI job wanting this must set it, the same trap
 * `CLAUDE.md` describes for Rust integration targets.
 */

const COMPANY_SCOPE = "/api/v1/company";

/** The roster the host actually holds. */
async function hostRoster(request: APIRequestContext): Promise<Array<{ role: string }>> {
  const res = await request.get(`${COMPANY_SCOPE}/team`);
  expect(res.ok()).toBeTruthy();
  return (await res.json()) as Array<{ role: string }>;
}

/**
 * Removes every operator-added teammate, so a re-run starts from a first run
 * again. A manifest teammate 409s and is left alone — the fixture company has
 * none, which is the point of it.
 */
async function unstaffCompany(request: APIRequestContext) {
  for (const member of await hostRoster(request)) {
    const id = (member as { id?: string }).id;
    if (id) await request.delete(`${COMPANY_SCOPE}/team/${id}`).catch(() => undefined);
  }
}

/** Answers one question and advances. */
async function answer(page: Page, field: string, text: string) {
  await expect(page.getByTestId(`setup-field-${field}`)).toBeVisible();
  await page.getByTestId(`setup-field-${field}`).fill(text);
  await page.getByTestId("setup-next").click();
}

test.beforeEach(async ({ request }) => {
  await unstaffCompany(request);
  // A company that still has people on it after unstaffing declares them in its
  // manifest, so setup can never open. Say so, rather than failing on a missing
  // dialog thirty lines later.
  const left = await hostRoster(request);
  test.skip(
    left.length > 0,
    `this company ships with ${left.length} manifest agents, so first-run setup ` +
      "cannot open — run with PW_HOST_COMPANY=companies/e2e_setup",
  );
});

test("first-run setup builds a real team from three answers", async ({ page, request }) => {
  await page.addInitScript(() => {
    // Clear any skip recorded by an earlier run in this browser profile, and the
    // tour's own seen flag, so neither suppresses what this spec is watching.
    for (const key of Object.keys(window.localStorage)) {
      if (key.startsWith("oc-setup") || key.startsWith("oc-tour")) {
        window.localStorage.removeItem(key);
      }
    }
  });

  await page.goto("/#/overview");

  // 1. It opens by itself — nobody clicked anything.
  const dialog = page.getByTestId("setup-dialog");
  await expect(dialog).toBeVisible({ timeout: 20_000 });
  await expect(page.getByTestId("setup-question")).toContainText("What kind of company");

  // The tour must be holding: a walkthrough of an unstaffed company is the
  // first impression this feature exists to replace.
  await expect(page.getByRole("button", { name: "Take the tour" })).toBeHidden();

  // 2. The first question is required; the other two are not.
  await page.getByTestId("setup-next").click();
  await expect(page.getByTestId("setup-problem")).toBeVisible();

  await answer(page, "industry", "E-commerce — I sell homeware online");
  await answer(page, "teamHint", "");
  await answer(page, "automate", "Meta ads, order dispatch, daily sales reports");

  // 3. The build-out names each teammate as its write lands.
  await expect(page.getByTestId("setup-buildout-title")).toBeVisible({ timeout: 60_000 });
  const created = page.getByTestId("setup-agent-created");
  await expect(created.first()).toBeVisible({ timeout: 30_000 });

  // 4. It finishes, and says so as a starting point rather than a fait accompli.
  await expect(page.getByTestId("setup-buildout-title")).toContainText("ready", {
    timeout: 60_000,
  });
  const names = await created.allInnerTexts();
  expect(names.length, `build-out listed ${names.length} agents`).toBeGreaterThanOrEqual(4);

  await page.getByTestId("setup-finish").click();
  await expect(dialog).toBeHidden();

  // 5. The host really holds them — not the console's fabricated starter team.
  const roster = await hostRoster(request);
  expect(roster.length).toBeGreaterThanOrEqual(4);

  // 6. And the Team page shows that roster, refreshed without a reload.
  await page.goto("/#/company");
  for (const member of roster.slice(0, 3)) {
    await expect(page.getByText(member.role, { exact: false }).first()).toBeVisible();
  }

  // 7. A reload does not re-offer setup: the roster is no longer empty, which is
  // the whole reason emptiness is the signal rather than a stored flag.
  await page.reload();
  await page.goto("/#/overview");
  await expect(dialog).toBeHidden();
});

test("skipping setup leaves a way back in", async ({ page, request }) => {
  await page.addInitScript(() => {
    for (const key of Object.keys(window.localStorage)) {
      if (key.startsWith("oc-setup") || key.startsWith("oc-tour")) {
        window.localStorage.removeItem(key);
      }
    }
  });

  await page.goto("/#/overview");
  await expect(page.getByTestId("setup-dialog")).toBeVisible({ timeout: 20_000 });

  await page.getByTestId("setup-skip").click();
  await expect(page.getByTestId("setup-dialog")).toBeHidden();

  // Skipping must not be a dead end: the Team page keeps offering it in place.
  await page.goto("/#/company");
  await expect(page.getByTestId("setup-prompt")).toBeVisible({ timeout: 20_000 });

  // And nothing was created by skipping.
  expect(await hostRoster(request)).toHaveLength(0);

  // The prompt reopens the same dialog.
  await page.getByTestId("setup-prompt-run").click();
  await expect(page.getByTestId("setup-dialog")).toBeVisible();
});
