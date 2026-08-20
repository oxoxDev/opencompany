import { expect, test, type Page } from "@playwright/test";

import { LIVE_BRAIN, LIVE_BRAIN_REASON } from "./capabilities";

/**
 * End-to-end proof for the chat↔card edge (issue #246).
 *
 * Runs against a live host the harness brings up separately, with an inference
 * backend whose *choices* are scripted: a prompt carrying `SPAWNONE` makes the
 * orchestrator call `spawn_task` once. Everything else — the harness, the tool
 * plumbing, the cycle, the journal, the HTTP surface — is real.
 *
 * Three things are asserted that a curl cannot reach:
 *
 * 1. the "Add to board" action exists on a **desk** thread, where no responder
 *    carries the delegation tools and a card was previously unreachable;
 * 2. the chip a spawned card produces survives a **reload**, not merely the
 *    live POST response;
 * 3. the card links back to the conversation it came from.
 */

/**
 * A console opened against a fresh company home shows the welcome tour, which
 * renders as a modal over the whole console and swallows the first click.
 * Dismiss it when it is up. Whether it appears depends on console-local state,
 * so its absence is not a failure.
 */
async function dismissWelcome(page: Page) {
  const skip = page.getByRole("button", { name: /Skip for now/ });
  try {
    await skip.waitFor({ state: "visible", timeout: 5_000 });
  } catch {
    return;
  }
  await skip.click();
  await skip.waitFor({ state: "hidden", timeout: 10_000 });
}

/**
 * Opens the conversation view and selects a thread by its contact name.
 *
 * Scoped to the chat list: the sidebar's company switcher is also a button and
 * also carries the company name, and it precedes the list in the DOM — so an
 * unscoped `.first()` resolves to the switcher and never opens a thread.
 */
async function openThread(page: Page, name: RegExp) {
  await page.goto("/#/conversation");
  await dismissWelcome(page);
  await page.getByRole("complementary").getByRole("button", { name }).first().click();
}

test("any message on a desk thread can be added to the board", async ({ page }) => {
  await openThread(page, /Engineering desk/);

  const prompt = `ship the launch checklist ${Date.now()}`;
  await page.getByPlaceholder(/^Message /).fill(prompt);
  await page.getByRole("button", { name: "Send", exact: true }).click();

  // The operator's own bubble is the one being turned into a card.
  const bubble = page.getByText(prompt, { exact: true }).first();
  await expect(bubble).toBeVisible({ timeout: 60_000 });

  // The action is hover-revealed but always focusable; hover for realism.
  await bubble.hover();
  const row = page.locator("div.group\\/msg", { hasText: prompt }).first();
  await row.getByRole("button", { name: "Add to board" }).click();

  // The confirmation chip appears on that message and links to the card.
  const chip = row.getByRole("link", { name: /Added to the board/ });
  await expect(chip).toBeVisible({ timeout: 30_000 });
  const href = await chip.getAttribute("href");
  expect(href).toMatch(/^#\/tasks\/.+/);

  // The card is real, titled from the message, and — the spend gate — did NOT
  // land in `in_progress`.
  await page.goto(href!);
  await dismissWelcome(page);
  await expect(page.getByText(prompt).first()).toBeVisible({ timeout: 30_000 });
  await expect(page.getByText("In progress", { exact: true })).toHaveCount(0);

  // …and it knows which conversation opened it.
  await expect(page.getByRole("button", { name: /Opened from chat/ })).toBeVisible();
});

test("a card the orchestrator opens is chipped in chat, and survives a reload", async ({
  page,
}) => {
  // Only THIS test needs the scripted backend — the one above it drives the
  // console's own "Add to board" action and passes against a default host, so
  // the skip is per-test rather than per-file.
  test.skip(!LIVE_BRAIN, LIVE_BRAIN_REASON);

  await openThread(page, /Your company/);

  // `SPAWNONE` is the scripted backend's cue to call `spawn_task` once.
  const prompt = `please track this SPAWNONE ${Date.now()}`;
  await page.getByPlaceholder(/^Message /).fill(prompt);
  await page.getByRole("button", { name: "Send", exact: true }).click();

  // Live: the reply bubble says a card was opened.
  const chip = page.getByRole("link", { name: /Card opened/ }).last();
  await expect(chip).toBeVisible({ timeout: 60_000 });
  const href = await chip.getAttribute("href");
  expect(href).toMatch(/^#\/tasks\/.+/);

  // After a reload the transcript is rehydrated from `chat/history`, so a chip
  // that only existed on the live POST response would vanish here.
  await page.reload();
  await openThread(page, /Your company/);
  const rehydrated = page.getByRole("link", { name: /Card opened/ }).last();
  await expect(rehydrated).toBeVisible({ timeout: 30_000 });
  expect(await rehydrated.getAttribute("href")).toBe(href);
});

/**
 * **The dismissal, end to end, including the reload (issue #984).**
 *
 * The affordance had no coverage at all, and the half that had none was the
 * half that was broken: `clearTaskCard` only touches React state, so a
 * dismissal that looked right in the session came back on the next reload — the
 * console rehydrates from `chat/history`, and the host still had `task_id` on
 * the journaled row. The chip returned pointing at a card that no longer
 * existed, which reads as the delete having failed.
 *
 * So the reload is the assertion that matters here, and it is deliberately the
 * mirror image of the reload assertion in the test above: that one proves a
 * live card's chip *survives*, this one proves a dismissed card's chip *does
 * not come back*. Neither is safe without the other — a host that dropped every
 * `task_id` would pass this and fail that.
 *
 * Runs on the "Add to board" path rather than the scripted-backend one, so it
 * needs no `LIVE_BRAIN` and runs on every CI.
 */
test("a dismissed card's chip goes away and does not come back on reload", async ({ page }) => {
  await openThread(page, /Engineering desk/);

  const prompt = `dismiss this one ${Date.now()}`;
  await page.getByPlaceholder(/^Message /).fill(prompt);
  await page.getByRole("button", { name: "Send", exact: true }).click();

  const bubble = page.getByText(prompt, { exact: true }).first();
  await expect(bubble).toBeVisible({ timeout: 60_000 });
  await bubble.hover();
  const row = page.locator("div.group\\/msg", { hasText: prompt }).first();
  await row.getByRole("button", { name: "Add to board" }).click();

  const chip = row.getByRole("link", { name: /Added to the board/ });
  await expect(chip).toBeVisible({ timeout: 30_000 });
  const href = await chip.getAttribute("href");

  // The control is a confirm, not a bare delete — a card is not something to
  // lose to a stray click.
  await row.getByRole("button", { name: "Dismiss this card" }).click();
  await expect(page.getByText("Dismiss this card?")).toBeVisible();
  await page.getByRole("button", { name: "Dismiss card", exact: true }).click();

  // Gone from the transcript in-session…
  await expect(chip).toBeHidden({ timeout: 30_000 });

  // …and gone from the board, which is what makes it a dismissal rather than a
  // hidden chip over a card that is still filling the board.
  await page.goto(href!);
  await dismissWelcome(page);
  await expect(page.getByText(prompt).first()).toHaveCount(0, { timeout: 30_000 });

  // …and still gone after a reload. This is the regression: the transcript is
  // rehydrated from the host here, not from the React state the click cleared.
  await openThread(page, /Engineering desk/);
  await page.reload();
  await openThread(page, /Engineering desk/);
  await expect(page.getByText(prompt, { exact: true }).first()).toBeVisible({ timeout: 30_000 });
  await expect(page.getByRole("link", { name: /Added to the board/ })).toHaveCount(0);
});
