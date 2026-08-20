import { expect, test, type APIRequestContext, type Page } from "@playwright/test";

import { openWorkflow } from "./workflows";

/**
 * The run input, after issue #1204 took it off the toolbar.
 *
 * The bar is tidier — `workflow-toolbar-reachable.spec.ts` measures that — and
 * this spec is the other half: that the capability it carried is still there
 * and still delivers. That capability is real. The host seeds the payload as
 * the trigger node's item, a first step bound to `=items` reads it, and the run
 * echoes it back (issue #154); a version of this change that tidied the row and
 * quietly stopped sending the payload would look like a success and would run
 * those workflows on nothing.
 *
 * Delivery is asserted on the **outgoing request body**, not on the rendering.
 * A screenshot of a dialog proves the box exists; only the POST proves what
 * reached the host. That also makes the spec independent of whether this host
 * has a brain: it does not care what the run produced, only what it was asked.
 *
 * The echo is then asserted against **either** run drawer. A run on a host with
 * no inference source may well fail, and that is not this spec's business — the
 * echo has to be right either way, and both drawers carry it (`RunResultPanel`
 * from `ranWith`, `RunFailurePanel` from the failure's own captured request).
 */

const COMPANY_SCOPE = "/api/v1/company";
const WORKFLOW_ID = "e2e-1204-run-input";
const WORKFLOW_NAME = "Run input plumbing";

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
      description: "Created by the #1204 e2e spec.",
      nodes: [
        { id: "start", kind: "trigger", name: "Start" },
        { id: "done", kind: "output", name: "Report" },
      ],
      edges: [{ from: "start", to: "done" }],
    },
  });
  expect(res.ok(), `create: ${res.status()} ${await res.text()}`).toBeTruthy();
}

/** Best-effort teardown. `expectedVersion` is required (issue #1013). */
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

/** The trigger input, wherever on the page it currently is. */
function requestField(page: Page) {
  return page.getByLabel("Request for this run");
}

test.describe("running a workflow on a specific input (#1204)", () => {
  test.beforeEach(async ({ request }) => {
    await removeWorkflow(request);
    await createWorkflow(request);
  });

  test.afterEach(async ({ request }) => {
    await removeWorkflow(request);
  });

  test("the field is off the toolbar and behind the second half of Run", async ({
    page,
  }) => {
    await page.goto("/#/workflows");
    await dismissTour(page);
    await openWorkflow(page, WORKFLOW_NAME);

    // The complaint, stated as an assertion: no free-text box on the bar.
    await expect(requestField(page)).toHaveCount(0);

    const trigger = page.getByTestId("workflow-run-with-input");
    await expect(trigger).toBeVisible();
    // Icon-only, so its name comes from `sr-only` text — which is the thing an
    // operator on a screen reader has instead of the placeholder that went away.
    await expect(trigger).toHaveAccessibleName(/run with input/i);

    await trigger.click();
    await expect(page.getByTestId("workflow-run-input-dialog")).toBeVisible();
    await expect(requestField(page)).toBeVisible();
  });

  test("what is typed reaches the host as the run's trigger input", async ({ page }) => {
    await page.goto("/#/workflows");
    await dismissTour(page);
    await openWorkflow(page, WORKFLOW_NAME);

    await page.getByTestId("workflow-run-with-input").click();
    await requestField(page).fill("the Q3 board deck");

    const posted = page.waitForRequest(
      (req) => req.url().includes(`/workflows/${WORKFLOW_ID}/run`) && req.method() === "POST",
    );
    await page.getByTestId("workflow-run-input-submit").click();
    const body = (await posted).postDataJSON();

    // The whole capability, in one line: the operator's words, on the wire, in
    // the field the host seeds the trigger node's item from.
    expect(body).toMatchObject({ input: { request: "the Q3 board deck" } });

    // And it comes back — from whichever drawer this run earned.
    const echo = page
      .getByTestId("workflow-run-result")
      .or(page.getByTestId("workflow-run-failure"));
    await expect(echo).toBeVisible({ timeout: 60_000 });
    await expect(echo).toContainText("Requested:");
    await expect(echo).toContainText("the Q3 board deck");
  });

  test("the toolbar's Run still runs with no input, draft or not", async ({ page }) => {
    // The defect this change could have introduced: a field that used to be in
    // plain sight becoming one nobody can see. Typing a draft and dismissing
    // the dialog must leave Run exactly as it was.
    await page.goto("/#/workflows");
    await dismissTour(page);
    await openWorkflow(page, WORKFLOW_NAME);

    await page.getByTestId("workflow-run-with-input").click();
    await requestField(page).fill("a draft nobody meant to send");
    await page.getByTestId("workflow-run-input-cancel").click();
    await expect(page.getByTestId("workflow-run-input-dialog")).toHaveCount(0);

    const posted = page.waitForRequest(
      (req) => req.url().includes(`/workflows/${WORKFLOW_ID}/run`) && req.method() === "POST",
    );
    await page.getByTestId("workflow-run").click();
    const body = (await posted).postDataJSON();

    // `{}` is the host's "run with a null input" — what a scheduled run gets.
    expect(body.input).toEqual({});
  });
});
