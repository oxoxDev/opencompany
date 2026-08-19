import { expect, test, type Page } from "@playwright/test";

import { openWorkflow } from "./workflows";

/**
 * Issue #528 / #514: a run the host refuses because the company has no inference
 * provider surfaces as a PERSISTENT, actionable banner — not a vanishing
 * raw-string toast.
 *
 * #514 gives the host a `409 { code: "inference_required" }` for this exact
 * state (a company that never configured an inference source). The console keys
 * on that structured `code` — never the localisable prose — raises a banner that
 * says what is wrong in plain words, and links the operator straight to the
 * inference settings so the dead-end toast becomes a way forward.
 *
 * Needs no brain: the refusal is injected with `page.route`, so this runs in
 * BOTH the default `Console E2E` lane and the live-brain lane.
 */

/** Dismisses the first-run tour if it is up; tolerates its absence. */
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

/** The run POST, and only it — not `…/workflows/runs`, not `…/cron/preview`. */
const RUN_POST = /\/workflows\/[^/]+\/run(\?|$)/;

test("a run refused for a missing inference provider shows a persistent, linked banner", async ({
  page,
}) => {
  // Injecting the host's #514 refusal means this needs no inference backend, and
  // pins the console's reading to the structured code rather than the prose.
  await page.route(RUN_POST, async (route) => {
    if (route.request().method() !== "POST") return route.fallback();
    await route.fulfill({
      status: 409,
      contentType: "application/json",
      body: JSON.stringify({
        error:
          "workflow execution needs an inference source, and none is configured " +
          "for this company. Set a provider in Settings → Inference, then run again.",
        code: "inference_required",
      }),
    });
  });

  await page.goto("/#/workflows");
  await dismissTour(page);

  await selectWorkflow(page, "Committed flow");
  await page.getByRole("button", { name: "Run", exact: true }).click();

  // The banner is persistent (an Alert, not a toast) and explains the state in
  // plain words.
  const alert = page.getByTestId("workflow-run-inference-alert");
  await expect(alert).toBeVisible();
  await expect(alert).toContainText(/no inference provider configured/i);

  // …and it links the operator to where they fix it.
  const cta = page.getByTestId("workflow-run-inference-cta");
  await expect(cta).toBeVisible();
  await expect(cta).toHaveAttribute("href", "#/settings/connections");
});
