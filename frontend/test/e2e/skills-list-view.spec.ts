import { expect, test, type Locator } from "@playwright/test";

import {
  amendSkills,
  hostSkills,
  installedCard,
  openSkills,
  suppressTour,
} from "./skills";

/**
 * The reach cell when a skill reaches more teammates than the cell can draw.
 *
 * Every other Skills spec asserts against what the host serves, and the drift
 * states that a live host could not reach now have one of their own in
 * `skills-drift-live.spec.ts`. This one keeps an interception, because what it
 * needs is not a state the host computes: it is a scope wider than the column,
 * and the harness company has six teammates. Widening the answer is cheaper
 * and steadier than creating a dozen agents to make a layout branch happen,
 * and the branch is client-side arithmetic over a width — `amendSkills` says
 * what a spec built this way can and cannot claim.
 *
 * Default features are enough; every route here ships in the default build.
 */

/** A skill the harness company bundles, used as the row that has drifted. */
const NAME = "Meeting Brief";
const SLUG = "meeting-brief";

test.beforeEach(async ({ page }) => {
  await suppressTour(page);
});

// ---------------------------------------------------------------------------
// The reach cell when the faces stop fitting
// ---------------------------------------------------------------------------

/** Every holder the reach cell drew, counted, and the overflow's own number. */
async function reachCounts(cell: Locator) {
  return cell.evaluate((node) => {
    const text = (selector: string) =>
      node.querySelector(selector)?.textContent ?? "";
    return {
      held: Number(text('[data-testid="skill-reach-count"]').split(" of ")[0]),
      faces: node.querySelectorAll("img").length,
      hidden: Number(
        text('[data-testid="skill-reach-overflow"]').replace("+", ""),
      ),
    };
  });
}

test("a scope too wide for the cell counts the faces it hid and opens the page", async ({
  page,
  request,
}) => {
  // Enough holders that no column width fits them, so this does not depend on
  // how a card happens to squeeze at a given viewport.
  const EXTRA = 14;
  const served = await hostSkills(request);
  const bundled = served.find((skill) => skill.id === SLUG);
  const holders = (bundled?.agents ?? []).filter((agent) => agent.holds).length;
  expect(holders, `${SLUG} should reach somebody to widen`).toBeGreaterThan(0);

  const extra = Array.from({ length: EXTRA }, (_, i) => ({
    id: `overflow-probe-${i}`,
    state: "included",
    holds: true,
  }));

  await amendSkills(page, (skills) =>
    skills.map((skill) =>
      skill.id === SLUG
        ? { ...skill, agents: [...(skill.agents ?? []), ...extra] }
        : skill,
    ),
  );
  await openSkills(page);

  const card = installedCard(page, NAME);
  await expect(card).toBeVisible({ timeout: 30_000 });
  const reach = card.getByTestId("skill-reach");
  await expect(card.getByTestId("skill-reach-overflow")).toBeVisible();

  // Read in one pass and polled to settle: the cell measures itself with a
  // `ResizeObserver`, so separate reads can straddle a re-measure and disagree
  // with each other rather than with the component. The invariant is that the
  // faces drawn plus the ones counted are every holder — a `+N` that does not
  // add up is the bug this control can actually have.
  await expect
    .poll(async () => {
      const { held, faces, hidden } = await reachCounts(reach);
      return { held, accounted: faces + hidden, drewSome: faces > 0 };
    })
    .toEqual({
      held: holders + EXTRA,
      accounted: holders + EXTRA,
      drewSome: true,
    });

  // Some were hidden, or there was nothing for this control to say.
  const { faces, hidden } = await reachCounts(reach);
  expect(hidden, `${faces} faces drawn of ${holders + EXTRA}`).toBeGreaterThan(
    0,
  );

  // It is a way into the page, where every teammate is named rather than
  // counted.
  await card.getByTestId("skill-reach-overflow").click();
  await expect(page.getByTestId("skill-page")).toBeVisible();
  await expect(page.getByTestId("skill-detail-name")).toHaveText(NAME);
});
