import {
  expect,
  test,
  type APIRequestContext,
  type Locator,
  type Page,
} from "@playwright/test";

import { expectWorkflowIndex, openWorkflow } from "./workflows";

/**
 * Issue #824: the workflows toolbar laid out wider than its container and
 * overflowed into an `overflow-hidden` ancestor, so its last control —
 * **New workflow** — was clipped off the right edge. Nothing in that chain
 * scrolls, so the button was not merely off-screen: it could not be clicked.
 *
 * Issue #1135 split the bar in two and moved `New workflow` out of the detail
 * view to the index, so the control the defect was reported against is no
 * longer on the crowded row at all. That does not retire the spec: the property
 * it pins is "every control the operator can see is reachable", which now has
 * to hold on **two** surfaces. So the controls are listed per surface, the
 * detail row is measured inside a workflow, and the index row on the index —
 * and the clickability test moved with the button it is about.
 *
 * This is a **layout** defect, so it is only observable in a browser. jsdom
 * computes no geometry, and a unit test asserting the class string would pass
 * against any width — including a row that still overflows. The measurement has
 * to be a real one.
 *
 * The row grows every time a control is added. `Pause` (#814) took the overhang
 * from 22px to 113px, which is what made it visible, but the row was already
 * overflowing before it. So the spec asserts the **property** rather than
 * today's button count: every control in the toolbar is inside the viewport and
 * clickable. A tenth control that reintroduces the overflow fails here.
 *
 * Runs at two widths. 1280 is the common laptop width the defect was reported
 * at; 1024 is the narrow end, where a fix that merely bought a few pixels would
 * still fail.
 */

const COMPANY_SCOPE = "/api/v1/company";
const WORKFLOW_ID = "e2e-824-toolbar";
const WORKFLOW_NAME = "Toolbar reachability";

/** The tour's overlay swallows pointer events; tolerate its absence. */
async function dismissTour(page: Page) {
  const skip = page.getByRole("button", { name: "Skip for now" });
  try {
    await skip.waitFor({ state: "visible", timeout: 10_000 });
  } catch {
    return;
  }
  await skip.click();
  await expect(skip).toBeHidden();
}

async function createWorkflow(request: APIRequestContext) {
  const res = await request.post(`${COMPANY_SCOPE}/workflows`, {
    data: {
      id: WORKFLOW_ID,
      name: WORKFLOW_NAME,
      description: "Created by the #824 e2e spec.",
      nodes: [
        // The `schedule` is load-bearing, not decoration: it is what makes
        // `isScheduled` true and mounts the Pause control asserted below.
        { id: "start", kind: "trigger", name: "Start", schedule: "0 9 * * *" },
        { id: "done", kind: "output", name: "Report" },
      ],
      edges: [{ from: "start", to: "done" }],
    },
  });
  expect(res.ok(), `create: ${res.status()} ${await res.text()}`).toBeTruthy();
}

/**
 * Best-effort teardown so a failed spec does not poison the next run.
 * `expectedVersion` is required (issue #1013), so this reads the workflow's
 * current token first.
 */
async function removeWorkflow(request: APIRequestContext) {
  const version = await request
    .get(`${COMPANY_SCOPE}/workflows/${WORKFLOW_ID}`)
    .then(async (res) => (res.ok() ? ((await res.json()).version as string | null) : null))
    .catch(() => null);
  const query = version ? `?expectedVersion=${encodeURIComponent(version)}` : "";
  await request
    .delete(`${COMPANY_SCOPE}/workflows/${WORKFLOW_ID}${query}`)
    .catch(() => undefined);
}

/**
 * Opens the workflow, so the per-workflow half of the toolbar mounts and the
 * row is at its **widest** — the state the defect appears in.
 *
 * Issue #1110 turned this from belt-and-braces into the load-bearing step it
 * always claimed to be. The note that used to sit here — "the view does
 * auto-select, so this is belt-and-braces today; were that ever to change, the
 * assertions below would start failing on a toolbar that was simply never
 * populated, which reads as a layout regression and is not one" — described
 * exactly what has now changed. `#/workflows` is the index, the wide row exists
 * only inside a workflow, and this is how the spec gets there.
 */
async function selectWorkflow(page: Page, name: string) {
  await openWorkflow(page, name);
}

/**
 * The **detail** toolbar's controls, each with the locator that finds it —
 * every control issue #1135 kept on a workflow's own two rows, in the order it
 * put them: row 1's way back, then row 2's run intent, secondary group, and the
 * utility pair ending in Delete.
 *
 * `Pause` is not addressed by name like the rest: its accessible name comes
 * from an `aria-label` that flips to `Resume schedule` once the schedule is
 * off, so a name match would silently stop matching the day the fixture starts
 * out paused. The test id is stable across both.
 *
 * It also only mounts for a **scheduled** workflow (`isScheduled` — a trigger
 * node carrying a `schedule`), which is why the fixture's trigger has one.
 * That matters more than it looks: Pause is the control that made this defect
 * visible, taking the overhang from 22px to 113px, so a version of this spec
 * that skipped it would be measuring the row that fits.
 */
const DETAIL_CONTROLS: Array<{ label: string; find: (page: Page) => Locator }> = [
  {
    // Issue #1110: what "Browse" became. It opened the index over the canvas;
    // the index is the tab's front door now, so the control is the way back to
    // it. Addressed by test id for the same reason Pause is — it sits in row 1
    // with the heading rather than in the action row, and its name is prose.
    label: "All workflows",
    find: (p) => p.getByTestId("workflow-back-to-index"),
  },
  {
    // Issue #1204 made Run a split control, so it is addressed by test id now.
    // The name match it used to use — `{ name: "Run", exact: false }.first()` —
    // would silently resolve to whichever half rendered first, and "whichever
    // half" is not what a reachability spec should be measuring.
    label: "Run",
    find: (p) => p.getByTestId("workflow-run"),
  },
  {
    // The other half: the affordance the run-input field moved behind. It is
    // the newest control on the row, which by this spec's own reasoning — "the
    // row grows every time a control is added" — is exactly the one most worth
    // measuring. Icon-only, with its name in `sr-only` text, so it is addressed
    // by test id like Pause and the back link.
    label: "Run with input",
    find: (p) => p.getByTestId("workflow-run-with-input"),
  },
  {
    label: "Test run",
    find: (p) => p.getByRole("button", { name: "Test run" }).first(),
  },
  {
    label: "Copilot",
    find: (p) => p.getByRole("button", { name: "Copilot" }).first(),
  },
  {
    label: "History",
    find: (p) => p.getByRole("button", { name: "History" }).first(),
  },
  { label: "Pause", find: (p) => p.getByTestId("workflow-toggle-enabled") },
  {
    label: "Edit",
    find: (p) => p.getByRole("button", { name: "Edit" }).first(),
  },
  {
    label: "Delete",
    find: (p) => p.getByRole("button", { name: "Delete" }).first(),
  },
];

/**
 * The index's own row. Short by comparison, and that is the point of #1135 —
 * but it is the row `New workflow` lives on now, so this is where the control
 * the original defect clipped has to be measured.
 */
const INDEX_CONTROLS: Array<{ label: string; find: (page: Page) => Locator }> = [
  { label: "Cards", find: (p) => p.getByTestId("workflow-index-cards") },
  { label: "List", find: (p) => p.getByTestId("workflow-index-list") },
  {
    label: "New workflow",
    find: (p) => p.getByRole("button", { name: "New workflow" }),
  },
];

test.describe("workflows toolbar reachability (#824)", () => {
  test.beforeEach(async ({ request }) => {
    await removeWorkflow(request);
    await createWorkflow(request);
  });

  test.afterEach(async ({ request }) => {
    await removeWorkflow(request);
  });

  for (const width of [1280, 1024]) {
    test(`every detail toolbar control is inside the viewport at ${width}px`, async ({
      page,
    }) => {
      await page.setViewportSize({ width, height: 800 });
      await page.goto("/#/workflows");
      await dismissTour(page);

      // Wait for the toolbar to mount before measuring anything.
      await expect(
        page.getByRole("button", { name: "New workflow" }),
      ).toBeVisible();
      await selectWorkflow(page, WORKFLOW_NAME);

      for (const { label, find } of DETAIL_CONTROLS) {
        const control = find(page);
        await expect(control, `${label} should be mounted`).toBeVisible();
        // The assertion that matters. `toBeVisible` is true for a button that
        // has been pushed past the right edge — it is painted, just not
        // anywhere reachable. `toBeInViewport` is what distinguishes the two.
        await expect(
          control,
          `${label} should be reachable at ${width}px`,
        ).toBeInViewport();
      }

      // Issue #1135: `New workflow` is an index action, and the detail toolbar
      // is where it used to be stranded on a row of its own. Its absence here
      // is the other half of the reachability property — a control nobody can
      // see cannot be clipped.
      await expect(
        page.getByRole("button", { name: "New workflow" }),
        "New workflow belongs to the index, not to one workflow",
      ).toHaveCount(0);
    });

    test(`every index toolbar control is inside the viewport at ${width}px`, async ({
      page,
    }) => {
      await page.setViewportSize({ width, height: 800 });
      await page.goto("/#/workflows");
      await dismissTour(page);
      await expectWorkflowIndex(page);

      for (const { label, find } of INDEX_CONTROLS) {
        const control = find(page);
        await expect(control, `${label} should be mounted`).toBeVisible();
        await expect(
          control,
          `${label} should be reachable at ${width}px`,
        ).toBeInViewport();
      }
    });
  }

  test("New workflow can actually be clicked, not merely rendered", async ({
    page,
  }) => {
    // The defect's real cost. Every control above could be in the viewport and
    // this could still fail if something overlapped it, so the last word is an
    // actual click with its actual consequence.
    //
    // Issue #1135: on the INDEX, which is where the button lives now. The
    // "click against the crowded row" reasoning went with it — the crowded row
    // no longer has this button on it, and the row it does have is the one the
    // test above measures.
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto("/#/workflows");
    await dismissTour(page);
    await expectWorkflowIndex(page);

    const newWorkflow = page.getByRole("button", { name: "New workflow" });
    // Measured before the click, and the reason is not belt-and-braces: a
    // Playwright click scrolls its target into view first, and it manages that
    // even inside the `overflow-hidden` ancestor a person cannot scroll. So
    // `click()` alone SUCCEEDS against the clipped layout — verified by
    // reverting the fix, where this test passed and the two above failed. The
    // click is what proves the button works; this line is what proves a person
    // could reach it.
    await expect(newWorkflow).toBeInViewport();

    await newWorkflow.click();
    await expect(page.getByRole("dialog")).toBeVisible();
    await expect(page.getByText("Describe the workflow")).toBeVisible();
  });
});
