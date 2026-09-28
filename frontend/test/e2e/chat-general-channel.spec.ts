import { expect, test, type Page } from "@playwright/test";

import { openChannel } from "./chat-helpers";

/**
 * `#general` against a live host: the host lists it first in `GET .../desks`
 * with every non-retired teammate as a member, and keeps that membership in
 * step with the roster. The console renders it from that entry and offers no
 * membership control on it.
 */

const API = "/api/v1/company";

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const real = Storage.prototype.getItem;
    Storage.prototype.getItem = function getItem(key: string) {
      return key.startsWith("oc-tour:") ? '{"skipped":true}' : real.call(this, key);
    };
  });
});

const membersToggle = (page: Page) => page.getByRole("button", { name: /agents$/i });
const pane = (page: Page) => page.getByRole("complementary").last();

async function openPane(page: Page) {
  if ((await membersToggle(page).getAttribute("aria-pressed")) !== "true") {
    await membersToggle(page).click();
  }
  await expect(page.getByRole("heading", { name: "Team" })).toBeVisible();
}

const inChannel = (page: Page) => pane(page).locator("ul").first();

test("the host lists #general first, immutable, with the whole roster", async ({ request }) => {
  const desks = (await (await request.get(`${API}/desks`)).json()) as Array<{
    id: string;
    kind?: string;
    mutable?: boolean;
    members: string[];
  }>;
  const team = (await (await request.get(`${API}/team`)).json()) as Array<{ id: string }>;

  expect(desks[0]).toMatchObject({ id: "general", kind: "general", mutable: false });
  expect(new Set(desks[0].members)).toEqual(new Set(team.map((m) => m.id)));
});

test("#general offers no membership control", async ({ page }) => {
  await openChannel(page, "general");
  await openPane(page);

  await expect(pane(page).getByRole("heading", { name: "In this channel" })).toBeVisible();
  await expect(pane(page).getByRole("button", { name: /to this channel$/ })).toHaveCount(0);
  await expect(pane(page).getByRole("button", { name: "Manage on the org chart" })).toHaveCount(0);
});

test("an agent added or removed elsewhere appears in and leaves #general live", async ({
  page,
  request,
}) => {
  await openChannel(page, "general");
  await openPane(page);
  await expect(inChannel(page).locator("li").first()).toBeVisible();
  const urlBefore = page.url();

  // Out of band, the way the Team page or the orchestrator would: the Room only
  // learns of it from the `teammate_added` / `desk_members_changed` frames.
  const name = `General joiner ${Date.now()}`;
  const created = await request.post(`${API}/team`, { data: { name, role: "Tester" } });
  expect(created.ok(), await created.text()).toBeTruthy();
  const id = ((await created.json()) as { id: string }).id;

  try {
    await expect(inChannel(page)).toContainText(name, { timeout: 15_000 });

    expect((await request.delete(`${API}/team/${encodeURIComponent(id)}`)).status()).toBe(204);
    await expect(inChannel(page)).not.toContainText(name, { timeout: 15_000 });
    // No navigation happened: the pane is the same one, on the same address.
    expect(page.url()).toBe(urlBefore);
    await expect(page.getByRole("heading", { name: "Team" })).toBeVisible();
  } finally {
    await request.delete(`${API}/team/${encodeURIComponent(id)}`);
  }
});

test("a membership write to #general is refused", async ({ request }) => {
  const team = (await (await request.get(`${API}/team`)).json()) as Array<{ id: string }>;
  const response = await request.delete(
    `${API}/desks/general/members/${encodeURIComponent(team[0].id)}`,
  );
  expect(response.status()).toBe(409);
});
