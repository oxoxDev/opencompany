import { expect, test } from "@playwright/test";

import { LIVE_BRAIN } from "./capabilities";

/**
 * Yours and Discover: searching the company's own servers stays local, and the
 * public directory is a separate mode that opens on its top connectors.
 *
 * Default-feature host. The directory route needs the `mcp` feature, so the
 * unwired notice is what this lane can assert on honestly, and it is asserted
 * in both directions so the live-brain lane checks the other half.
 */

type Page = import("@playwright/test").Page;

async function openMcp(page: Page, query = "") {
  await page.goto(`/#/connections/mcp${query}`);
  const skip = page.getByRole("button", { name: "Skip for now" });
  await skip
    .waitFor({ state: "visible", timeout: 10_000 })
    .then(() => skip.click())
    .catch(() => {
      /* already seen in this context */
    });
}

async function openYours(page: Page) {
  await openMcp(page, "?view=yours");
  await expect(page.getByTestId("mcp-search")).toBeVisible({ timeout: 30_000 });
}

test("Yours and Discover are one switch, not two tabs", async ({ page }) => {
  await openYours(page);

  await expect(page.getByTestId("mcp-mode-yours")).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByTestId("mcp-mode-discover")).toHaveAttribute("aria-pressed", "false");
  await expect(page.getByRole("tab", { name: "Discover" })).toHaveCount(0);

  await page.getByTestId("mcp-mode-discover").click();
  await expect(page.getByTestId("mcp-discover-search")).toBeVisible();
  await expect(page.getByTestId("mcp-search")).toHaveCount(0);
  await expect(page).toHaveURL(/view=discover/);
});

test("searching Yours never reaches the directory", async ({ page }) => {
  const directoryCalls: string[] = [];
  await page.route("**/mcp/registry/**", (route) => {
    directoryCalls.push(route.request().url());
    return route.continue();
  });

  await openYours(page);
  await expect(page.getByTestId("mcp-server-row").first()).toBeVisible();
  await page.getByTestId("mcp-search").fill("zz-no-such-server");
  await expect(page.getByTestId("mcp-search-nothing")).toBeVisible();
  expect(directoryCalls).toEqual([]);

  // The counter is wired: Discover does call the directory, for its top
  // connectors, with nothing typed.
  await page.getByTestId("mcp-mode-discover").click();
  await expect
    .poll(() => directoryCalls.length, { timeout: 15_000 })
    .toBeGreaterThan(0);
});

test("a Yours search with no match carries the term into Discover", async ({
  page,
}) => {
  await openYours(page);
  await page.getByTestId("mcp-search").fill("zz-no-such-server");

  await page.getByTestId("mcp-search-directory").click();
  await expect(page.getByTestId("mcp-discover-search")).toHaveValue("zz-no-such-server");
  await expect(page.getByTestId("mcp-mode-discover")).toHaveAttribute("aria-pressed", "true");
});

test("a build without the feature reads as a missing feature, not an error", async ({
  page,
}) => {
  await openMcp(page, "?view=discover");
  await expect(page.getByTestId("mcp-discover-search")).toBeVisible({ timeout: 30_000 });

  const unwired = page.getByTestId("mcp-registry-unwired");
  if (LIVE_BRAIN) {
    await expect(unwired).toHaveCount(0, { timeout: 15_000 });
    await expect(page.getByTestId("mcp-discover-card").first()).toBeVisible({
      timeout: 30_000,
    });
    return;
  }
  await expect(unwired).toBeVisible({ timeout: 15_000 });
  await expect(page.getByTestId("mcp-registry-error")).toHaveCount(0);
});

test("Yours opens as a list, switches to cards, and remembers it", async ({
  page,
}) => {
  await openYours(page);
  await expect(page.getByTestId("mcp-layout-list")).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByTestId("mcp-server-row").first()).toBeVisible();
  await expect(page.getByTestId("mcp-server-card")).toHaveCount(0);

  await page.getByTestId("mcp-layout-cards").click();
  const card = page.getByTestId("mcp-server-card").filter({ hasText: "deepwiki" });
  await expect(card).toBeVisible();
  await expect(page.getByTestId("mcp-server-row")).toHaveCount(0);

  await page.reload();
  await expect(card).toBeVisible({ timeout: 30_000 });

  await card.click();
  await expect(page.getByTestId("mcp-server-page")).toBeVisible();

  await page.getByTestId("mcp-page-back").click();
  await page.getByTestId("mcp-layout-list").click();
  await expect(page.getByTestId("mcp-server-row").first()).toBeVisible();
});

test("a link to mcp.json opens it as a pop-up over the list", async ({ page }) => {
  await openMcp(page, "?tab=json");
  const dialog = page.getByTestId("mcp-json-dialog");
  await expect(dialog).toBeVisible({ timeout: 30_000 });
  await expect(dialog.getByTestId("mcp-json-text")).toBeVisible();

  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(page).not.toHaveURL(/tab=json/);
});
