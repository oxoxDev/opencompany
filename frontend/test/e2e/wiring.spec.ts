import { expect, test } from "@playwright/test";

import { LIVE_BRAIN, LIVE_BRAIN_REASON } from "./capabilities";

// File-level: this spec exists only to prove the mocked-backend chain, so there
// is nothing in it that a default-feature host can answer.
test.skip(!LIVE_BRAIN, LIVE_BRAIN_REASON);

/**
 * End-to-end wiring proof for the operator console.
 *
 * This single spec exercises the whole chain the console depends on:
 *
 *   magic-link auth → session cookie → console → POST /api/v1/company/chat
 *     → (mocked) LLM backend → reply rendered as a company bubble
 *
 * It runs against a feature-gated host with a mocked inference backend behind
 * it — `mock-brain.mjs`, which `playwright.config.ts` starts and points the
 * host at when `PW_LIVE_BRAIN=1` (issue #467). The spec only asserts on that
 * backend's `__MOCK_LLM__` marker (never exact echo text): the agent harness
 * transforms the prompt before it reaches the backend, so only the marker is
 * stable.
 *
 * The admin address must match `companies/e2e_harness/company.toml`'s
 * `[users] admins`, which is what makes the login flow succeed.
 */

// The first-run product tour opens a Radix dialog over the console, and while
// it is up every element beneath it is `aria-hidden` — so `getByRole` finds
// nothing, including the composer's Send button. `getByPlaceholder` still
// matches (it is an attribute selector, and Playwright's visibility model is
// CSS-based), which is why this spec appeared to reach the composer and then
// timed out on the very next line. Nineteen of the other specs already do this;
// this one predates the tour and was never updated.
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const real = Storage.prototype.getItem;
    Storage.prototype.getItem = function getItem(key: string) {
      return key.startsWith("oc-tour:") ? '{"skipped":true}' : real.call(this, key);
    };
  });
});

test("operator console renders a mocked backend reply end to end", async ({
  page,
}) => {
  // Authentication is performed once by global-setup.ts and shared through
  // Playwright storage state so multiple specs do not trip the resend throttle.
  // Open the conversation view. The default "Your company" thread is
  //    pre-selected.
  await page.goto("/#/conversation");

  // Send a unique prompt through the operator chat input.
  const prompt = `e2e wiring ping ${Date.now()}`;
  await page.getByPlaceholder(/^Message /).fill(prompt);
  // `exact`, because the composer's button is labelled exactly "Send" while the
  // sidebar's thread previews take their accessible names from message text — so
  // a loose match resolves to two elements the moment any message in the
  // transcript mentions sending, and dies on a strict-mode violation in a spec
  // that has nothing to do with whatever wrote that message.
  await page.getByRole("button", { name: "Send", exact: true }).click();

  // The mocked backend reply must render as a company bubble, and no send
  //    error may appear.
  await expect(page.getByText("__MOCK_LLM__").first()).toBeVisible({
    timeout: 60_000,
  });
  await expect(page.getByText(/^Couldn't send/)).toHaveCount(0);
});
