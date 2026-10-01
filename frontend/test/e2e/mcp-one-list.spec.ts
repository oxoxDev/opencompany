import { expect, test } from "@playwright/test";

import { LIVE_BRAIN } from "./capabilities";

/**
 * The MCP module's front door is one searchable list, and every row is a
 * link to that server's own page.
 *
 * Runs on the default-feature host the rest of this directory drives. Every
 * assertion is about rendering and navigation, which that host can answer. Tool
 * inventory reports `not_wired` without the `openhuman` feature, so nothing
 * about tool permissions belongs here: a spec asserting that a forbidden row is
 * absent would pass against a page that renders no rows at all.
 */

type Page = import("@playwright/test").Page;

async function openMcp(page: Page) {
  await page.goto("/#/connections/mcp");
  const skip = page.getByRole("button", { name: "Skip for now" });
  await skip
    .waitFor({ state: "visible", timeout: 10_000 })
    .then(() => skip.click())
    .catch(() => {
      /* already seen in this context */
    });
  await expect(page.getByTestId("mcp-search")).toBeVisible({ timeout: 30_000 });
}

/** The row for `name` in the one list. */
function row(page: Page, name: string) {
  return page.getByTestId("mcp-server-row").filter({ hasText: name });
}

async function selection(page: Page) {
  return page.evaluate(() => window.getSelection()?.toString() ?? "");
}

test("the list carries the manifest server and where it came from", async ({
  page,
}) => {
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));

  await openMcp(page);

  const deepwiki = row(page, "deepwiki");
  await expect(deepwiki).toBeVisible();
  await expect(deepwiki.getByTestId("mcp-source-badge")).toHaveText("manifest");

  expect(pageErrors).toEqual([]);
});

test("search narrows this company's own servers", async ({ page }) => {
  await openMcp(page);
  await expect(row(page, "deepwiki")).toBeVisible();

  await page.getByTestId("mcp-search").fill("no-such-server-anywhere");
  await expect(row(page, "deepwiki")).toHaveCount(0);

  await page.getByTestId("mcp-search").fill("deep");
  await expect(row(page, "deepwiki")).toBeVisible();
});

test("a row carries no expander and no description column", async ({ page }) => {
  await openMcp(page);
  const deepwiki = row(page, "deepwiki");
  await expect(deepwiki).toBeVisible();

  await expect(page.getByTestId("mcp-row-expander")).toHaveCount(0);
  await expect(page.getByTestId("mcp-row-detail")).toHaveCount(0);
  await expect(
    page.getByRole("columnheader", { name: "Description" }),
  ).toHaveCount(0);

  await deepwiki.hover();
  await expect(page.getByTestId("mcp-server-page")).toHaveCount(0);
});

test("clicking anywhere on the row opens the server's page", async ({ page }) => {
  await openMcp(page);

  await row(page, "deepwiki").getByTestId("mcp-source-badge").click();
  const detail = page.getByTestId("mcp-server-page");
  await expect(detail).toBeVisible();
  await expect(detail.getByRole("heading", { name: "deepwiki" })).toBeVisible();
  await expect(page.getByTestId("mcp-server-row")).toHaveCount(0);
  await expect(page).toHaveURL(/server=deepwiki/);

  await detail.getByTestId("mcp-page-back").click();
  await expect(row(page, "deepwiki")).toBeVisible();
});

test("the name is the link, so no row carries a View button", async ({ page }) => {
  await openMcp(page);
  const deepwiki = row(page, "deepwiki");

  await expect(deepwiki.getByRole("button", { name: "View" })).toHaveCount(0);
  await deepwiki.getByTestId("mcp-server-open").click();
  await expect(page.getByTestId("mcp-server-page")).toBeVisible();
});

test("a control on the row does its own job and nothing else", async ({
  page,
}) => {
  await openMcp(page);
  const deepwiki = row(page, "deepwiki");

  await deepwiki.getByTestId("mcp-row-overflow").click();
  await expect(page.getByTestId("mcp-toggle")).toBeVisible();
  await expect(page.getByTestId("mcp-server-page")).toHaveCount(0);
});

test("a row keeps its secondary controls behind the overflow", async ({ page }) => {
  await openMcp(page);
  const deepwiki = row(page, "deepwiki");

  // Counted at page level: the menu is portaled out of the row, so a
  // row-scoped absence check would pass with the menu open.
  for (const hidden of ["mcp-toggle", "mcp-test", "mcp-tools", "mcp-permissions"]) {
    await expect(page.getByTestId(hidden)).toHaveCount(0);
  }

  await deepwiki.getByTestId("mcp-row-overflow").click();
  const menu = page.getByRole("menu");
  await expect(menu.getByTestId("mcp-toggle")).toBeVisible();
  await expect(menu.getByTestId("mcp-permissions")).toBeVisible();
  await expect(menu.getByTestId("mcp-remove")).toHaveCount(0);
});

test("the page states what this build can do with these servers", async ({
  page,
}) => {
  await openMcp(page);

  const notice = page.getByTestId("mcp-bridge-absent");
  if (LIVE_BRAIN) await expect(notice).toHaveCount(0);
  else await expect(notice).toBeVisible();
});

test("a double-click opens the page and highlights nothing", async ({ page }) => {
  await openMcp(page);

  await row(page, "deepwiki").getByTestId("mcp-source-badge").dblclick();

  await expect(page.getByTestId("mcp-server-page")).toBeVisible();
  expect(await selection(page)).toEqual("");
});

test("dragging across a row opens it and selects nothing", async ({ page }) => {
  await openMcp(page);
  const badge = row(page, "deepwiki").getByTestId("mcp-source-badge");

  await badge.hover();
  const box = await badge.boundingBox();
  if (box === null) throw new Error("the source badge has no box to drag across");
  await page.mouse.down();
  for (const dx of [-18, -6, 6, 18]) {
    await page.mouse.move(box.x + box.width / 2 + dx, box.y + box.height / 2);
  }
  await page.mouse.up();

  expect(await selection(page)).toEqual("");
  await expect(page.getByTestId("mcp-server-page")).toBeVisible();
});

test("the endpoint in the details pop-up can be selected and copied", async ({
  page,
}) => {
  await openMcp(page);
  await row(page, "deepwiki").getByTestId("mcp-server-open").click();
  await page.getByTestId("mcp-page-details-open").click();

  const endpoint = page
    .getByTestId("mcp-page-details")
    .getByText("https://mcp.deepwiki.com/mcp");
  await expect(endpoint).toBeVisible();

  await endpoint.dblclick();
  expect((await selection(page)).length).toBeGreaterThan(0);
});

test("a deep link to a server still opens its page", async ({ page }) => {
  await page.goto("/#/connections/mcp?server=deepwiki");
  await expect(page.getByTestId("mcp-server-page")).toBeVisible({
    timeout: 30_000,
  });
});

test("the list fits a phone without scrolling sideways", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 800 });
  await openMcp(page);
  await expect(row(page, "deepwiki")).toBeVisible();

  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);
  await expect(row(page, "deepwiki").getByTestId("mcp-row-overflow")).toBeVisible();
});

test("a server's name has room at desktop width", async ({ page }) => {
  await openMcp(page);
  const name = row(page, "deepwiki").getByTestId("mcp-server-open");
  await expect(name).toBeVisible();

  const clipped = await name.evaluate((el) => el.scrollWidth > el.clientWidth);
  expect(clipped, "the name column is squeezed until it truncates").toBe(false);
});
