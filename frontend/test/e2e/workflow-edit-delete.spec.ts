import { expect, test, type Page, type APIRequestContext } from "@playwright/test";

/**
 * Issue #259: a saved workflow used to be write-once. There was no `PUT` and no
 * `DELETE`, so a typo'd cron or a node pointed at the wrong teammate was
 * permanent — the only recovery was to author a second workflow and leave the
 * broken one in the picker, still firing on its schedule.
 *
 * These specs cover the half that is only observable in a browser:
 *
 * * the Delete affordance is **disabled with an explanation** for a workflow
 *   defined by a file in the company source tree (the host answers 409, and the
 *   console must say so before the click, not after);
 * * deleting is confirm-gated, and the confirm copy states both what goes (the
 *   workflow, its schedule) and what stays (past runs);
 * * a **version conflict surfaces distinctly and recoverably**. This is the one
 *   that matters most: the console must never silently overwrite an edit it
 *   never saw. The spec forces the race for real — it edits the graph
 *   out-of-band over HTTP while the console holds a stale copy — rather than
 *   trusting that the token is wired through.
 *
 * Runs against the live host the harness brings up (see `playwright.config.ts`).
 * Not run by CI, which has no host.
 */

const COMPANY_SCOPE = "/api/v1/company";

/**
 * Dismisses the first-run tour if it is up. Its overlay swallows pointer
 * events, so without this the specs fail on an unrelated modal. Tolerates its
 * absence — a company that has seen it never shows it again.
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

/** A minimal valid graph body: one trigger, one output, one edge. */
function graphBody(id: string, name: string, schedule: string, description: string) {
  return {
    id,
    name,
    description,
    nodes: [
      { id: "start", kind: "trigger", name: "Start", schedule },
      { id: "done", kind: "output", name: "Report" },
    ],
    edges: [{ from: "start", to: "done" }],
  };
}

/** Creates a workflow over HTTP and returns its version token. */
async function createWorkflow(
  request: APIRequestContext,
  id: string,
  name: string,
): Promise<string> {
  const res = await request.post(`${COMPANY_SCOPE}/workflows`, {
    data: graphBody(id, name, "0 9 * * *", "Created by the #259 e2e spec."),
  });
  expect(res.ok(), `create ${id}: ${res.status()} ${await res.text()}`).toBeTruthy();
  const body = await res.json();
  expect(body.editable, "a console-created workflow must be editable").toBe(true);
  expect(body.version, "a console-created workflow must carry a version token").toBeTruthy();
  return body.version as string;
}

/** Best-effort teardown so a failed spec does not poison the next run. */
async function removeWorkflow(request: APIRequestContext, id: string) {
  await request.delete(`${COMPANY_SCOPE}/workflows/${id}`).catch(() => undefined);
}

/**
 * Selects the workflow named `name` in the picker and waits for the selection
 * to settle.
 *
 * Picks by name (the listbox options render `name`) but *asserts* on `id`,
 * because the closed trigger renders the workflow **id**: the picker binds
 * `<SelectItem value={w.id}>` and Base UI's `SelectValue` renders the raw value
 * rather than the item's children. That is pre-existing on `upstream/main` and
 * untouched by #259 — worth its own issue, but keying this helper on the id is
 * what makes these specs describe the console as it actually is.
 */
async function selectWorkflow(page: Page, id: string, name: string) {
  await page.getByRole("combobox").first().click();
  await page.getByRole("option", { name, exact: true }).click();
  await expect(page.getByRole("combobox").first()).toContainText(id);
}

const DELETE = "workflow-delete";

test("a source-defined workflow cannot be deleted, and the console says why", async ({
  page,
}) => {
  await page.goto("/#/workflows");
  await dismissTour(page);

  await selectWorkflow(page, "committed", "Committed flow");

  const button = page.getByTestId(DELETE);
  await expect(button).toBeVisible();
  await expect(button, "a seed-backed workflow must not be deletable").toBeDisabled();

  // The explanation lives on the wrapper (a disabled button swallows hover), and
  // it must name the actual remedy, not just refuse.
  const explanation = page.locator(`span:has([data-testid="${DELETE}"])`);
  await expect(explanation).toHaveAttribute("title", /company source tree/);
  await expect(explanation).toHaveAttribute("title", /workflows\/committed\.toml/);
});

test("deleting is confirm-gated, and the confirmation says what goes and what stays", async ({
  page,
  request,
}) => {
  const id = `e2e-confirm-${Date.now()}`;
  const name = `Confirm probe ${Date.now()}`;
  await createWorkflow(request, id, name);

  try {
    await page.goto("/#/workflows");
    await dismissTour(page);
    await selectWorkflow(page, id, name);

    const button = page.getByTestId(DELETE);
    await expect(button, "an overlay-backed workflow must be deletable").toBeEnabled();
    await button.click();

    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText(name);
    // The two consequences an operator needs before committing.
    await expect(dialog, "must say the schedule stops").toContainText(/stops it running on its schedule/);
    await expect(dialog, "must say history is kept").toContainText(/Past runs stay/);

    // Backing out leaves it alone — a confirm gate that deletes anyway is worse
    // than none.
    await dialog.getByRole("button", { name: "Keep it" }).click();
    await expect(dialog).toBeHidden();
    const still = await request.get(`${COMPANY_SCOPE}/workflows/${id}`);
    expect(still.ok(), "cancelling must not delete").toBeTruthy();
  } finally {
    await removeWorkflow(request, id);
  }
});

test("confirming the delete removes it from the picker and from the host", async ({
  page,
  request,
}) => {
  const id = `e2e-delete-${Date.now()}`;
  const name = `Delete probe ${Date.now()}`;
  await createWorkflow(request, id, name);

  try {
    await page.goto("/#/workflows");
    await dismissTour(page);
    await selectWorkflow(page, id, name);

    await page.getByTestId(DELETE).click();
    await page.getByTestId("workflow-delete-confirm").click();

    // **Regression pin.** `AlertDialogAction` is a plain `Button`, not an
    // `AlertDialogPrimitive.Close` — only `AlertDialogCancel` is — so
    // confirming does NOT dismiss the dialog unless the view closes it
    // explicitly. Without that, the modal stays up over a view whose workflow
    // has just been deleted, rendering `Delete “”?`, with its backdrop
    // swallowing every click on the console behind it. Caught here first.
    await expect(
      page.getByRole("alertdialog"),
      "confirming must dismiss the dialog, not leave its backdrop blocking the app",
    ).toBeHidden({ timeout: 15_000 });

    // Gone from the picker: the selection moves off it and it is no longer an
    // option.
    await expect(page.getByRole("combobox").first()).not.toContainText(id, {
      timeout: 15_000,
    });

    // And gone from the host — the console did not merely hide it.
    await expect
      .poll(async () => (await request.get(`${COMPANY_SCOPE}/workflows/${id}`)).status(), {
        timeout: 15_000,
      })
      .toBe(404);
  } finally {
    await removeWorkflow(request, id);
  }
});

test("a version conflict surfaces distinctly with a way out, and deletes nothing", async ({
  page,
  request,
}) => {
  const id = `e2e-conflict-${Date.now()}`;
  const name = `Conflict probe ${Date.now()}`;
  await createWorkflow(request, id, name);

  try {
    await page.goto("/#/workflows");
    await dismissTour(page);
    await selectWorkflow(page, id, name);

    // Force the race for real: someone else edits the graph while this console
    // holds the token it loaded a moment ago.
    const edited = await request.put(`${COMPANY_SCOPE}/workflows/${id}`, {
      data: graphBody(id, name, "0 11 * * *", "Edited out-of-band, after the console loaded it."),
    });
    expect(edited.ok(), `out-of-band edit: ${edited.status()}`).toBeTruthy();

    await page.getByTestId(DELETE).click();
    await page.getByTestId("workflow-delete-confirm").click();

    // The conflict gets its own persistent banner — NOT a toast that fades and
    // NOT the generic load error — because the operator has to act on it.
    const banner = page.getByTestId("workflow-conflict");
    await expect(banner, "a 409 must surface distinctly").toBeVisible({ timeout: 15_000 });
    await expect(banner).toContainText(/changed since you loaded it/);
    await expect(banner).toContainText(/[Rr]eload/);

    // Nothing was deleted — this is the whole point of the guard.
    const survived = await request.get(`${COMPANY_SCOPE}/workflows/${id}`);
    expect(survived.ok(), "a refused delete must remove nothing").toBeTruthy();
    expect((await survived.json()).nodes[0].schedule).toBe("0 11 * * *");

    // The way out works: Reload re-reads the graph (and a fresh token), and the
    // banner clears.
    await banner.getByTestId("workflow-conflict-reload").click();
    await expect(banner).toBeHidden({ timeout: 15_000 });

    // …and now the delete goes through, so the loop actually terminates.
    await page.getByTestId(DELETE).click();
    await page.getByTestId("workflow-delete-confirm").click();
    await expect
      .poll(async () => (await request.get(`${COMPANY_SCOPE}/workflows/${id}`)).status(), {
        timeout: 15_000,
      })
      .toBe(404);
  } finally {
    await removeWorkflow(request, id);
  }
});
