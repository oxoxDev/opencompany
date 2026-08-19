import { expect, test, type Page } from "@playwright/test";

import { openWorkflow } from "./workflows";

import { LIVE_BRAIN, LIVE_BRAIN_REASON } from "./capabilities";

/**
 * Issue #528: the Workflows console runs SYNCHRONOUSLY, so the settled run body
 * reaches `RunResultPanel` and the operator can actually read what a run
 * produced.
 *
 * Before this, the Run button always asked the host to `detach`: on a modern
 * host `isDetached` was true, `setResult` never fired, and the run-output drawer
 * — the ONLY surface that renders a run's per-node agent text — never mounted.
 * The full `output` is carried by that 200 body alone (the journal, the SSE
 * frames, and the runs list are all structural), so a detached run left the
 * operator with a green toast and nothing to read.
 *
 * This spec needs a real run, and a run needs a brain: the committed workflow's
 * agent node produces its text through inference. With the feature off the run
 * journals nothing and the drawer would have nothing to show, so it gates on
 * `LIVE_BRAIN` — the `Console E2E (live brain)` lane runs it (issue #467).
 */

/**
 * Dismisses the first-run tour if it is up; tolerates its absence. Its overlay
 * swallows pointer events, so without this the spec fails on an unrelated modal.
 */
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

/**
 * Opens a workflow by name, from the index (issue #1110).
 *
 * `#/workflows` is the list now — the picker this used to drive exists only
 * once a workflow is open, and Run has no subject until then.
 */
async function selectWorkflow(page: Page, name: string) {
  await openWorkflow(page, name);
}

test("running a workflow shows its per-node output in the result drawer", async ({
  page,
}) => {
  test.skip(!LIVE_BRAIN, LIVE_BRAIN_REASON);

  await page.goto("/#/workflows");
  await dismissTour(page);

  // The source-defined fixture — its agent node ("Draft the note") is what the
  // drawer must render text for. Its ids and names are pinned by the harness.
  await selectWorkflow(page, "Committed flow");

  await page.getByRole("button", { name: "Run", exact: true }).click();

  // The whole fix: the synchronous body reaches `setResult`, so the drawer
  // mounts. A detached run would leave this hidden forever.
  const drawer = page.getByTestId("workflow-run-result");
  await expect(drawer).toBeVisible({ timeout: 60_000 });

  // And it renders the run's per-node result, not just a heading — the node card
  // for the agent step carries its graph name, which only the parsed
  // (non-fallback) render path produces. Scope to the output-cards region
  // (issue #542 added a "Steps" timeline that also names each node, so a
  // drawer-wide match is ambiguous); this still proves the per-node OUTPUT card
  // mounted, which is the point of the spec.
  const nodeResults = drawer.getByTestId("workflow-run-node-results");
  await expect(nodeResults.getByText("Draft the note")).toBeVisible();
});
