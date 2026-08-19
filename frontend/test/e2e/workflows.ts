import { expect, type Page } from "@playwright/test";

/**
 * How a spec reaches a workflow, since issue #1110.
 *
 * `#/workflows` is the index — every workflow, and none of them open. The
 * per-workflow toolbar (Run, Test run, Copilot, History, Resume, Edit, Delete)
 * exists only once one is opened, so a spec that needs any of it has to open
 * one first. Before #1110 the view auto-selected the first row, and eight specs
 * relied on that without saying so; this module is where they say it.
 *
 * Issue #1135 made that the only route. The toolbar picker a spec could once
 * use to hop straight from one workflow to the next is gone — switching is
 * {@link backToWorkflowIndex} followed by {@link openWorkflow}.
 *
 * Deliberately shared rather than copied into each spec: "how do I get to a
 * workflow" is one answer, and the next change to the flow should have one
 * place to land.
 */

/** The index panel itself — the tab's front door. */
export function workflowIndex(page: Page) {
  return page.getByTestId("workflow-index");
}

/** The index card for one workflow, matched on its name exactly. */
export function workflowCard(page: Page, name: string) {
  return page
    .getByTestId("workflow-card")
    .filter({ has: page.getByText(name, { exact: true }) });
}

/** The heading that names the workflow whose detail view is on screen. */
export function workflowDetailName(page: Page) {
  return page.getByTestId("workflow-detail-name");
}

/** Wait for the index to be up and to have finished loading its rows. */
export async function expectWorkflowIndex(page: Page, timeout = 30_000) {
  await expect(workflowIndex(page)).toBeVisible({ timeout });
}

/**
 * Open one workflow's detail view from the index, by name.
 *
 * Asserts the heading afterwards rather than the picker: the heading is the
 * detail view's own claim about which workflow it is showing, and it is present
 * before the graph has loaded, so it is the earliest honest signal that the
 * navigation landed.
 */
export async function openWorkflow(page: Page, name: string, timeout = 30_000) {
  await expectWorkflowIndex(page, timeout);
  await workflowCard(page, name).first().click();
  await expect(workflowDetailName(page)).toHaveText(name, { timeout });
}

/** Go to the Workflows tab and open one workflow, by name. */
export async function gotoWorkflow(page: Page, name: string, timeout = 30_000) {
  await page.goto("/#/workflows");
  await openWorkflow(page, name, timeout);
}

/** Go back to the index from a workflow's detail view. */
export async function backToWorkflowIndex(page: Page) {
  await page.getByTestId("workflow-back-to-index").click();
  await expectWorkflowIndex(page);
}

/**
 * Open whichever workflow the index lists first.
 *
 * For specs that need *a* workflow rather than a named one — the ones that used
 * to rely on the auto-select and so never learned the name of what they were
 * driving. Returns the opened workflow's id, read out of the URL the detail
 * view pushes, which is the only place a spec can learn it without a fixture.
 */
export async function openFirstWorkflow(page: Page, timeout = 30_000): Promise<string> {
  await expectWorkflowIndex(page, timeout);
  const first = page.getByTestId("workflow-card").first();
  await expect(first).toBeVisible({ timeout });
  await first.click();
  await expect(workflowDetailName(page)).toBeVisible({ timeout });
  await expect(page).toHaveURL(/#\/workflows\/[^/]+$/, { timeout });
  return decodeURIComponent(new URL(page.url()).hash.split("/")[2] ?? "");
}
