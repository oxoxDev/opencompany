import {
  expect,
  test,
  type APIRequestContext,
  type Page,
} from "@playwright/test";

import {
  backToWorkflowIndex,
  expectWorkflowIndex,
  openWorkflow,
  workflowCard,
  workflowDetailName,
} from "./workflows";

/**
 * Issue #384: workflow create / update / delete events reached the console's
 * SSE switch, matched no arm, and were dropped.
 *
 * The host has journalled and projected all three since #112/#259 — `GET
 * {scope}/events` carries `workflow_created`, `workflow_updated` and
 * `workflow_deleted` — so nothing was missing on the wire. The console simply
 * had no case for them, which is the failure shape this switch has produced
 * twice before (#464 for the board, #371 for the run canvas): the frames
 * arrive, fall through to `default:`, and nothing logs.
 *
 * What the operator saw: with the Workflows tab open, a workflow authored by
 * the orchestrator's `create_workflow` tool, by a second console session, or by
 * a machine credential did not appear; a rename did not land; and a delete left
 * its entry sitting in the picker, so the next click ran or edited a workflow
 * the host no longer had.
 *
 * **Every test here writes from outside the browser** and asserts the open tab
 * followed, with no reload and no company switch. That is the whole property —
 * a spec that clicked the console's own Create button would pass against the
 * broken build, because the local handler splices the row in by hand.
 *
 * Runs against the live host `playwright.config.ts` brings up, in the
 * `Console E2E` CI lane (issue #428).
 */

const COMPANY_SCOPE = "/api/v1/company";

/**
 * Dismisses the first-run tour if it is up. Its overlay swallows pointer
 * events. Tolerates its absence — a company that has seen it never shows it
 * again, so this is a no-op on every run after the first.
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
function graphBody(id: string, name: string) {
  return {
    id,
    name,
    description: "Created by the #384 e2e spec.",
    nodes: [
      { id: "start", kind: "trigger", name: "Start", schedule: "0 9 * * *" },
      { id: "done", kind: "output", name: "Report" },
    ],
    edges: [{ from: "start", to: "done" }],
  };
}

/**
 * Authors a workflow over HTTP — the stand-in for the orchestrator's
 * `create_workflow` tool and for a second console session, neither of which a
 * browser test can drive. The host journals `WorkflowCreated` on this path, so
 * the page under test can only learn about it through the SSE stream.
 */
/** Creates a workflow over HTTP and returns its version token. */
async function createWorkflow(
  request: APIRequestContext,
  id: string,
  name: string,
): Promise<string> {
  const res = await request.post(`${COMPANY_SCOPE}/workflows`, { data: graphBody(id, name) });
  expect(res.ok(), `create ${id}: ${res.status()} ${await res.text()}`).toBeTruthy();
  const body = await res.json();
  return body.version as string;
}

/**
 * Renames a saved workflow out-of-band. Journals `WorkflowUpdated`.
 *
 * The graph goes in flat — `UpdateWorkflowBody` flattens it and carries only
 * `expectedVersion` alongside. `expectedVersion` is required (issue #1013), so
 * this needs the token the caller read the graph at — an out-of-band actor is
 * not exempt from the same conditional-write contract the console follows.
 */
async function renameWorkflow(
  request: APIRequestContext,
  id: string,
  name: string,
  expectedVersion: string,
) {
  const res = await request.put(`${COMPANY_SCOPE}/workflows/${id}`, {
    data: { ...graphBody(id, name), expectedVersion },
  });
  expect(res.ok(), `rename ${id}: ${res.status()} ${await res.text()}`).toBeTruthy();
}

/** Reads back a workflow's current version, or `null` if it is gone already. */
async function currentVersion(request: APIRequestContext, id: string): Promise<string | null> {
  const res = await request.get(`${COMPANY_SCOPE}/workflows/${id}`);
  if (!res.ok()) return null;
  const body = await res.json();
  return (body.version as string | null) ?? null;
}

/**
 * Best-effort teardown so a failed spec does not poison the next run.
 * `expectedVersion` is required (issue #1013), so this reads the workflow's
 * current token first — the caller's own copy may be stale by teardown time.
 */
async function removeWorkflow(request: APIRequestContext, id: string) {
  const version = await currentVersion(request, id).catch(() => null);
  const query = version ? `?expectedVersion=${encodeURIComponent(version)}` : "";
  await request.delete(`${COMPANY_SCOPE}/workflows/${id}${query}`).catch(() => undefined);
}

/* Issue #1135: `picker`, the workflow selector's trigger, lived here. The
 * detail toolbar no longer carries one — the workflow you are in is the
 * heading, and switching between them is what the index is for — so the rename
 * test below reads the open workflow's heading and then its index card, which
 * is every surface the console names a workflow on. */

/**
 * Opens the Workflows tab and waits for the index to settle.
 *
 * Issue #1110: the tab lands on the index, so "settled" is the list being up
 * rather than the picker being enabled — the picker is a control of an open
 * workflow now, and there is none yet.
 */
async function openWorkflows(page: Page) {
  await page.goto("/#/workflows");
  await dismissTour(page);
  await expectWorkflowIndex(page);
}

/* Issue #1110: `pickerOptionCount`, its `UNREADABLE` sentinel and the `settled`
 * helper it was built on lived here. All three existed to read the picker's
 * popup without letting a mid-re-render transient read as "the popup held
 * nothing" — a subtlety of counting options in a dropdown that has to be opened
 * to be read. The index renders every workflow as an ordinary element, so the
 * counting is now `toHaveCount`, which retries on its own, and none of that
 * machinery has a caller. */

/** Opens a workflow from the index and waits for its detail view to settle. */
async function selectWorkflow(page: Page, name: string) {
  await openWorkflow(page, name);
}

test("a workflow authored elsewhere reaches the list, with no reload", async ({
  page,
  request,
}) => {
  const stamp = Date.now();
  const id = `e2e-live-create-${stamp}`;
  const name = `Live create probe ${stamp}`;

  try {
    await openWorkflows(page);

    // Issue #1110: read off the INDEX rather than the picker popup. The two are
    // rendered from the same `workflows` array, so this pins the same property
    // — that the console re-read the list at all — against the surface the
    // operator is actually looking at. The picker exists only inside a workflow
    // now, and opening one to check whether a list contains something is the
    // step this issue was about removing.
    //
    // The baseline the fix has to move. Without it this stays at zero for the
    // life of the tab: the `workflow_created` frame reaches the console's
    // switch, matches no arm, and is discarded.
    await expect(
      workflowCard(page, name),
      "the probe must not exist before it is created",
    ).toHaveCount(0);

    await createWorkflow(request, id, name);

    await expect(workflowCard(page, name)).toHaveCount(1, { timeout: 20_000 });
  } finally {
    await removeWorkflow(request, id);
  }
});

test("a workflow renamed elsewhere renames on screen, with no reload", async ({
  page,
  request,
}) => {
  const stamp = Date.now();
  const id = `e2e-live-rename-${stamp}`;
  const before = `Live rename probe ${stamp}`;
  const after = `Live renamed probe ${stamp}`;
  const createdVersion = await createWorkflow(request, id, before);

  try {
    await openWorkflows(page);
    await selectWorkflow(page, before);

    // The graph on screen is renamed under the operator — the second-session
    // edit #259 made possible, and which nothing told this tab about.
    await renameWorkflow(request, id, after, createdVersion);

    // Read off the OPEN workflow's own heading first: the rename has to land on
    // the workflow the operator is looking at, not merely somewhere in a list.
    await expect(
      workflowDetailName(page),
      "a rename elsewhere must reach the open workflow live",
    ).toHaveText(after, { timeout: 20_000 });

    // …and then off the index, which is the other surface the same `workflows`
    // array feeds. Issue #1135 retired the toolbar picker this used to check;
    // the index card is where a name that failed to update would now be stale,
    // and it is checked for the OLD name too, so a list that grew a second row
    // rather than renaming its one row fails here.
    await backToWorkflowIndex(page);
    await expect(workflowCard(page, after), "…and the index card").toHaveCount(1, {
      timeout: 20_000,
    });
    await expect(workflowCard(page, before)).toHaveCount(0);
  } finally {
    await removeWorkflow(request, id);
  }
});

test("deleting the workflow on screen elsewhere returns to the list, and takes it out of the list", async ({
  page,
  request,
}) => {
  const stamp = Date.now();
  const id = `e2e-live-delete-${stamp}`;
  const name = `Live delete probe ${stamp}`;
  const createdVersion = await createWorkflow(request, id, name);

  try {
    await openWorkflows(page);
    await selectWorkflow(page, name);

    // Deleted from another session while this tab has it selected and its graph
    // on the canvas. This is the worst symptom in the issue: the entry used to
    // stay put, and the next Run or Edit addressed a workflow the host had
    // already dropped. `expectedVersion` is required (issue #1013).
    const deleted = await request.delete(
      `${COMPANY_SCOPE}/workflows/${id}?expectedVersion=${encodeURIComponent(createdVersion)}`,
    );
    expect(deleted.ok(), `delete ${id}: ${deleted.status()}`).toBeTruthy();

    // Issue #1110: the view leaves the deleted workflow for the INDEX, not for
    // a neighbouring graph. Both halves matter — the canvas must stop showing a
    // graph the host no longer has, and what replaces it must not be a workflow
    // the operator never opened.
    await expectWorkflowIndex(page);
    await expect(workflowDetailName(page)).toHaveCount(0, { timeout: 20_000 });

    // …and it is GONE from the list, not merely closed or greyed out.
    await expect(
      workflowCard(page, name),
      "a deleted workflow must leave the list, not sit in it greyed out",
    ).toHaveCount(0, { timeout: 20_000 });
  } finally {
    await removeWorkflow(request, id);
  }
});

/**
 * The selection can now move without the operator moving it, which is what
 * makes this worth pinning: the hash mirror pushes a history entry for a
 * selection change, and that was unconditionally right while only a click could
 * cause one.
 *
 * Measured as `history.length` rather than by pressing Back, because Back's
 * destination depends on which workflow the view happened to select on mount —
 * it could legitimately be any entry in the harness company. The stack not
 * growing is the property itself, and it is true regardless.
 */
test("a delete elsewhere corrects the URL in place, without pushing history", async ({
  page,
  request,
}) => {
  const stamp = Date.now();
  const id = `e2e-live-history-${stamp}`;
  const name = `Live history probe ${stamp}`;
  const createdVersion = await createWorkflow(request, id, name);

  try {
    await openWorkflows(page);

    // Picking it IS a navigation the operator made, so this one is a genuine
    // history entry — and it is the entry the correction has to replace.
    await selectWorkflow(page, name);
    await expect.poll(() => page.url(), { timeout: 20_000 }).toContain(id);
    const entriesBefore = await page.evaluate(() => window.history.length);

    // `expectedVersion` is required (issue #1013).
    const deleted = await request.delete(
      `${COMPANY_SCOPE}/workflows/${id}?expectedVersion=${encodeURIComponent(createdVersion)}`,
    );
    expect(deleted.ok(), `delete ${id}: ${deleted.status()}`).toBeTruthy();

    // The selection moves off the deleted workflow and the hash follows it.
    await expect.poll(() => page.url(), { timeout: 20_000 }).not.toContain(id);

    // …by REPLACING that entry rather than stacking on it. A push would leave
    // a URL naming a workflow the host no longer has one Back press away — an
    // address the operator could copy out and share for a graph that does not
    // exist, reached by undoing something they never did.
    expect(
      await page.evaluate(() => window.history.length),
      "correcting a deleted selection must replace the history entry, not push one",
    ).toBe(entriesBefore);
  } finally {
    await removeWorkflow(request, id);
  }
});
