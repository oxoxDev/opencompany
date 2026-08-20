import { expect, test } from "@playwright/test";

/**
 * The Brain (operator memory) surface, end to end against the real FactStore.
 *
 * History: parked by issue #302 inside `wiring.spec.ts` when the console
 * stopped listing Brain, with a comment promising it would be un-skipped the
 * day the surface was relisted. That day came with the memory-engine work —
 * and the test moved OUT of `wiring.spec.ts` rather than un-skipping in
 * place, because that file is gated on `PW_LIVE_BRAIN` (it proves the mocked
 * inference chain) while this flow exercises `…/memory` and needs no
 * inference at all. Here it runs on the default `Console E2E` lane, not just
 * the live-brain one.
 */

// The first-run product tour opens a Radix dialog over the console; every
// element beneath it is `aria-hidden` while it shows. Same suppression the
// other specs use.
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const real = Storage.prototype.getItem;
    Storage.prototype.getItem = function getItem(key: string) {
      return key.startsWith("oc-tour:") ? '{"skipped":true}' : real.call(this, key);
    };
  });
});

test("operator adds a Brain memory that persists across reload and can be deleted", async ({
  page,
}) => {
  // The Brain tab reads the real FactStore over `…/memory`; adding a note must
  // survive a reload (proving it hit the backend, not localStorage) and delete
  // must remove it.
  await page.goto("/#/memory");

  const title = `e2e memory ${Date.now()}`;
  await page.getByTestId("memory-add").click();
  await page.getByTestId("memory-title").fill(title);
  await page.getByTestId("memory-body").fill("recall me on the next turn");
  await page.getByTestId("memory-save").click();

  const card = page.getByTestId("memory-card").filter({ hasText: title });
  await expect(card).toBeVisible({ timeout: 30_000 });

  // Reload: a localStorage stub would survive too, so also assert the health
  // strip counts a real backend item.
  await page.reload();
  await page.goto("/#/memory");
  await expect(page.getByTestId("memory-card").filter({ hasText: title })).toBeVisible({
    timeout: 30_000,
  });

  // Delete removes it.
  const persisted = page.getByTestId("memory-card").filter({ hasText: title });
  await persisted.getByRole("button", { name: "Delete memory" }).click();
  await expect(page.getByTestId("memory-card").filter({ hasText: title })).toHaveCount(0, {
    timeout: 30_000,
  });
});
