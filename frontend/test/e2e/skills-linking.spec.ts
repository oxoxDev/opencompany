import { expect, test, type APIRequestContext } from "@playwright/test";

import {
  hostSkills,
  installedRow,
  skillPageUrl,
  suppressTour,
} from "./skills";

/**
 * How a skill's page is reached from outside the list.
 *
 * `skills-page.spec.ts` drives the page once it is open; this file covers the
 * two ways in that do not start from a card. Both are addresses rather than
 * behaviour, which is exactly why they need a browser: a unit test can assert
 * what a link's `href` says and still leave the address unanswered by any
 * route, and until the page became addressable there was nothing to answer it
 * with.
 *
 * The canonical route is `#/connections/skills`. `#/settings/skills`, which the
 * rest of the suite navigates to, only still resolves through a rewrite — so a
 * link written against the old spelling would depend on that rewrite outliving
 * the move, and this file navigates the canonical one deliberately.
 *
 * Default features are enough; every route here ships in the default build.
 */

/** A skill the harness company bundles, so it is always there to link at. */
const SLUG = "meeting-brief";
const NAME = "Meeting Brief";

/** The first teammate the host reports as holding `SLUG`. */
async function holderOf(request: APIRequestContext, slug: string) {
  const skill = (await hostSkills(request)).find((row) => row.id === slug);
  expect(skill, `the harness company should bundle ${slug}`).toBeTruthy();
  const holder = (skill?.agents ?? []).find((agent) => agent.holds);
  expect(
    holder,
    `${slug} should reach at least one teammate for this spec to follow`,
  ).toBeTruthy();
  return holder!.id;
}

test.beforeEach(async ({ page }) => {
  await suppressTour(page);
});

test("a skill's address opens its page cold, and Back returns to the list", async ({
  page,
}) => {
  // Cold: no card was clicked, so the page has to resolve the slug out of the
  // address on its first render rather than from state a click left behind.
  await page.goto(skillPageUrl(SLUG));

  await expect(page.getByTestId("skill-page")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("skill-detail-name")).toHaveText(NAME);

  // The list is not also on screen underneath it.
  await expect(page.getByTestId("installed-row")).toHaveCount(0);

  await page.getByTestId("skill-page-back").click();

  await expect(installedRow(page, NAME)).toBeVisible();
  await expect(page.getByTestId("skill-page")).toHaveCount(0);
  // And the address drops the skill again, so a reload lands on the list.
  expect(new URL(page.url()).hash).not.toContain("skill=");
});

test("a slug the company does not have is not mistaken for a skill", async ({
  page,
}) => {
  // The address is the one thing a stale link can get wrong, and the list is
  // the honest answer to it — an empty page frame would read as a skill that
  // exists and has nothing in it.
  await page.goto(skillPageUrl("no-such-skill-at-all"));

  await expect(page.getByTestId("skills-read-only-note")).toBeVisible({
    timeout: 30_000,
  });
  await expect(page.getByTestId("skill-page")).toHaveCount(0);
  await expect(page.getByTestId("installed-row").first()).toBeVisible();
});

test("a teammate's Skills tab links each skill it reads at that skill's page", async ({
  page,
  request,
}) => {
  const agent = await holderOf(request, SLUG);
  // `#/company/agent/<id>` is the canonical teammate address: `#/team/<id>`
  // rewrites onto it and drops the query with it, so a `?tab=` written against
  // the short form lands on Overview (`agent-session.spec.ts` says the same).
  await page.goto(`/#/company/agent/${agent}?tab=skills`);

  const link = page.getByTestId(`agent-skill-open-${SLUG}`);
  await expect(link).toBeVisible({ timeout: 30_000 });
  await link.click();

  // The round trip: the teammate page's link lands on the skill's own page,
  // opened at that skill, and the page agrees this teammate reads it.
  await expect(page.getByTestId("skill-page")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("skill-detail-name")).toHaveText(NAME);

  // An admin's roster is behind "Selected agents" — the whole company reads a
  // bundled skill, so the page opens on "All agents" with nothing listed.
  await page.getByTestId("skill-detail-mode-selected").click();
  await expect(page.getByTestId(`skill-agent-reach-${agent}`)).toHaveText(
    "Reached",
  );
});
