import { expect, test, type Page } from "@playwright/test";

/**
 * Proof for issue #264: an agent can be opened, read, and edited **from the
 * Console**, against the live host.
 *
 * The issue's complaint is a dead end, so the evidence has to be the walk that
 * used to end nowhere: start on the Team tab, open a card, and read the things
 * that had no console surface at all — the instructions the agent was defined
 * with, its tier, the tools it may actually use, and the desks it sits on. The
 * tool grants had no *read endpoint* either (`GET …/team` sent `tools: null`
 * for every member), so this is also the first time that is checkable from
 * outside the process.
 *
 * Runs against the same live host as `wiring.spec.ts` (`companies/e2e_harness`),
 * whose manifest is what the assertions below are pinned to:
 *
 *   * `[tools] allow = ["composio", "mcp:*", "workspace", "workspace.*"]`
 *   * `ceo` is `tier = "orchestrator"`, asks for `mcp:*`, `composio`,
 *     `workspace.read`, and sits on no desk
 *   * `engineer` sits on the Engineering desk
 *
 * Default features are enough: the routes this exercises ship in the default
 * build, so nothing here is behind `capabilities.ts`.
 *
 * The spec removes the teammate it creates, so it can run repeatedly against a
 * host whose data directory persists between runs.
 */

/** The card for the teammate whose role matches `role`. */
function card(page: Page, role: string) {
  return page.getByTestId("team-card").filter({ hasText: role }).first();
}

/**
 * A fresh host greets the first visit with a welcome tour rendered over the
 * console, which swallows clicks on the view beneath it.
 *
 * Two halves, and the first is what matters. The tour is suppressed **before
 * the app boots** by seeding its own localStorage markers through an init
 * script, so it never renders and there is nothing to wait for. This is the
 * pattern `board-columns.spec.ts` uses, and it is here for a measured reason:
 * the earlier version of this helper blocked on `waitFor({ timeout: 15_000 })`
 * and swallowed the timeout, which costs the FULL fifteen seconds every time
 * the tour is absent — the common case. This spec navigates three times (the
 * `beforeEach`, the storage-cleared reload, and the cleanup), so it paid that
 * toll three times and blew the 60s test budget without a single assertion
 * failing.
 *
 * The click half stays as a belt-and-braces fallback for a host whose marker
 * key this list does not name, but it polls briefly rather than blocking.
 */
async function dismissOnboarding(page: Page) {
  const skip = page.getByRole("button", { name: "Skip for now" });
  for (let attempt = 0; attempt < 5; attempt += 1) {
    if (!(await skip.isVisible().catch(() => false))) return;
    await skip.click({ force: true }).catch(() => {});
    await page.waitForTimeout(300);
  }
  await expect(skip).toHaveCount(0);
}

async function goToTeam(page: Page) {
  await page.goto("/#/team");
  await dismissOnboarding(page);
  await expect(page.getByTestId("team-card").first()).toBeVisible({ timeout: 30_000 });
}

test.beforeEach(async ({ page }) => {
  // Registered before any navigation, so it also re-seeds after the
  // storage-clearing reload below — which wipes the tour markers along with
  // everything else and would otherwise bring the tour back mid-test.
  await page.addInitScript(() => {
    const seen = JSON.stringify({ skipped: true, seenAt: Date.now() });
    for (const key of ["oc-tour:single", "oc-tour:e2e-harness-co", "oc-tour:null"]) {
      window.localStorage.setItem(key, seen);
    }
  });
  await goToTeam(page);
});

test("a company agent opens from its card and shows what it is", async ({ page }) => {
  // The walk that used to end nowhere: the card's own name is the way in.
  await card(page, "Chief Executive").getByTestId("team-card-open").click();

  // A sub-page, not a modal: the agent is addressable, so it survives a
  // refresh and Back returns to the roster.
  await expect(page).toHaveURL(/#\/team\/ceo$/);

  await expect(page.getByTestId("agent-name")).toHaveText("Chief Executive");
  await expect(page.getByTestId("agent-id")).toHaveText("ceo");

  // The tier, resolved by the host rather than read off the manifest string.
  await expect(page.getByTestId("agent-tier")).toContainText("Orchestrator");
  await expect(page.getByTestId("agent-source")).toHaveText("Company blueprint");

  // The instructions it was defined with — the "AGENT.md for that agent" the
  // issue asks for, which the manifest always carried and the console never
  // showed after creation.
  await expect(page.getByTestId("agent-description")).toContainText("Sets direction");

  // The effective tool grants. Every one of these is an intersection of the
  // agent's own `tools` line with the company allow-list, and none of it was
  // readable anywhere before this issue.
  const tools = page.getByTestId("agent-tools");
  await expect(tools).toContainText("workspace.read");
  await expect(tools).toContainText("composio");
  await expect(tools).toContainText("mcp:*");

  // This agent sits on no desk, and says so rather than rendering nothing.
  await expect(page.getByTestId("agent-desks-empty")).toBeVisible();

  // A blueprint teammate is read-only here, and the screen says why instead of
  // offering an edit that would 409.
  await expect(page.getByTestId("agent-edit")).toHaveCount(0);
  await expect(page.getByTestId("agent-readonly-note")).toContainText("company.toml");

  // Back returns to the roster.
  await page.getByRole("button", { name: "Back to team" }).click();
  await expect(page.getByTestId("team-card").first()).toBeVisible();
});

test("desk membership is on the agent, and an agent is reachable by link", async ({ page }) => {
  // Deep link straight to an agent: the detail view resolves the id against the
  // host rather than falling back to the roster.
  await page.goto("/#/team/engineer");
  await dismissOnboarding(page);

  await expect(page.getByTestId("agent-name")).toHaveText("Engineer", { timeout: 30_000 });
  await expect(page.getByTestId("agent-tier")).toContainText("Worker");
  await expect(page.getByTestId("agent-desks")).toContainText("Engineering desk");
});

test("an agent defined in the console can be read back and edited", async ({ page }) => {
  const role = "Spec Runner";

  // The `try` opens BEFORE the teammate is created, not after. The POST lands
  // as soon as the dialog is submitted, so a failure in the assertion that
  // follows it would otherwise skip the cleanup and leave the teammate on the
  // host — which breaks the next run of a spec that is meant to be repeatable,
  // and leaves a second card for `card(page, role)` to match.
  try {
    // Define one through the dialog the issue calls create-only.
    await page.getByRole("button", { name: "Add teammate" }).first().click();
    const dialog = page.getByRole("dialog");
    await dialog.getByTestId("agent-field-name").fill("Detail Spec");
    await dialog.getByTestId("agent-field-role").fill(role);
    await dialog.getByTestId("agent-field-description").fill("Original instructions.");
    await dialog.getByRole("button", { name: "Add teammate" }).click();
    await expect(card(page, role)).toBeVisible({ timeout: 30_000 });

    // Open it. This is the half that was impossible: the roster was write-once
    // per member, so iterating on an agent meant deleting it and starting over.
    await card(page, role).getByTestId("team-card-open").click();
    await expect(page.getByTestId("agent-source")).toHaveText("Added here");
    await expect(page.getByTestId("agent-description")).toContainText("Original instructions.");

    // A console-defined agent holds the company's standard grant, so it reads
    // back with the whole allow-list rather than an empty tool list.
    await expect(page.getByTestId("agent-tools")).toContainText("composio");

    // Edit it.
    await page.getByTestId("agent-edit").click();
    await page.getByTestId("agent-field-description").fill("Rewritten instructions.");
    await page.getByTestId("agent-field-role").fill("Spec Runner II");
    await page.getByTestId("agent-save").click();
    await expect(page.getByTestId("agent-description")).toContainText("Rewritten instructions.", {
      timeout: 30_000,
    });

    // Host-backed, not local state: it survives a storage-cleared reload. This
    // is the assertion that separates "the console remembers" from "the company
    // was actually changed".
    //
    // `reload`, not `goto(page.url())`. Navigating to the URL the page is
    // already on is a same-document navigation — the app keeps every piece of
    // in-memory state, so the two assertions below would pass off the panel's
    // own state and prove nothing about the host. That is not hypothetical: it
    // is why this spec went green on the panel while the roster underneath it
    // was stale.
    await page.evaluate(() => {
      localStorage.clear();
      sessionStorage.clear();
    });
    await page.reload();
    await dismissOnboarding(page);
    await expect(page.getByTestId("agent-description")).toContainText("Rewritten instructions.", {
      timeout: 30_000,
    });
    await expect(page.getByTestId("agent-role")).toHaveText("Spec Runner II");

    // …and the roster the operator came from agrees, rather than only the panel
    // they edited in.
    await page.getByRole("button", { name: "Back to team" }).click();
    await expect(card(page, "Spec Runner II")).toBeVisible({ timeout: 30_000 });
  } finally {
    // Leave the company as we found it, whatever happened above.
    await goToTeam(page);
    const leftover = page.getByTestId("team-card").filter({ hasText: "Detail Spec" }).first();
    if (await leftover.count()) {
      await leftover.getByLabel("Teammate actions").click();
      await page.getByRole("menuitem", { name: "Remove" }).click();
      await expect(leftover).toHaveCount(0, { timeout: 30_000 });
    }
  }
});
