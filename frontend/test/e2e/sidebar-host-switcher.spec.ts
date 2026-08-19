import { expect, test } from "@playwright/test";

/**
 * The host switcher, in the sidebar header, on a console holding two hosts.
 *
 * This spec used to guard the opposite arrangement: an icon rail down the left
 * edge, and a `position: fixed` sidebar that had to be offset by 56px or it
 * slid underneath and clipped every nav label — "Company" read as "mpany".
 * Issue #1142 removed the rail and moved the choice into the sidebar's own
 * header, so the offset is gone and the assertion inverts: the sidebar now
 * starts at the left edge of the window, and nothing stands in front of it.
 *
 * What carries over is why the spec exists at all. The broken state needed two
 * connections, and nothing else in the suite creates them — a design-system
 * migration and two contrast audits all passed straight over it, because every
 * one of them ran against the default single-host console. Two hosts is still
 * the only way to see the switcher be a control rather than a nameplate.
 */

// The first-run tour opens a dialog over the console and `aria-hidden`s
// everything beneath it. Same shim the rest of the suite uses.
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const real = Storage.prototype.getItem;
    Storage.prototype.getItem = function getItem(key: string) {
      return key.startsWith("oc-tour:") ? '{"skipped":true}' : real.call(this, key);
    };
  });
});

/** Must match `companies/e2e_harness/company.toml`'s `[users] admins`. */
const ADMIN_EMAIL = "harness-e2e@tinyhumans.ai";

test("the sidebar owns the left edge, and its header switches hosts", async ({
  page,
  baseURL,
}) => {
  const base = (baseURL ?? "http://127.0.0.1:8080").replace(/\/$/, "");

  // Signs in through the page's OWN context, so the session cookie lands in
  // the jar the page uses. Deliberately not relying on the shared
  // `storageState`: this spec seeds `oc.connections.v1` before boot, and a
  // spec that rewrites the connection list should not also depend on a
  // session minted for a different connection list.
  const api = page.context().request;
  const requested = await api.post(`${base}/api/v1/company/auth/request`, {
    data: { email: ADMIN_EMAIL },
  });
  const { dev_code: devCode } = (await requested.json()) as { dev_code?: string };
  expect(devCode, "the host should echo a dev_code when no mail is configured").toBeTruthy();
  const verified = await api.post(`${base}/api/v1/company/auth/verify`, {
    data: { email: ADMIN_EMAIL, code: devCode },
  });
  expect(verified.ok(), "sign-in should mint a session").toBe(true);
  // Two hosts, so the switcher opens a menu. The second address is never
  // contacted — the menu draws it as unreachable, which is all this spec needs.
  await page.addInitScript((primary: string) => {
    localStorage.setItem(
      "oc.connections.v1",
      JSON.stringify([
        {
          id: "switcher-spec-primary",
          baseUrl: primary,
          label: "Primary",
          defaultCompany: null,
          credential: { kind: "cookie" },
        },
        {
          id: "switcher-spec-second",
          baseUrl: "http://127.0.0.1:9",
          label: "Second host",
          defaultCompany: null,
          credential: { kind: "cookie" },
        },
      ]),
    );
  }, base);

  await page.goto("/#/company");

  // Nothing stands to the left of the sidebar any more.
  await expect(page.getByTestId("connection-rail")).toHaveCount(0);

  const sidebar = page.locator("[data-slot=sidebar-container]");
  await expect(sidebar).toBeVisible();
  const sidebarBox = await sidebar.boundingBox();
  expect(sidebarBox, "sidebar should have a box").not.toBeNull();
  expect(sidebarBox!.x, "the sidebar starts at the left edge of the window").toBe(0);

  // And the labels are actually on screen rather than clipped — the symptom a
  // reader would report, asserted separately from the cause so a future change
  // that moves the sidebar for some other reason still fails here rather than
  // passing on a technicality.
  // A nav row is a button, not a link — the shell routes on the hash rather
  // than navigating. `exact` keeps this off the page's own "Company" heading.
  const label = page.getByRole("button", { name: "Company", exact: true });
  const labelBox = await label.boundingBox();
  expect(labelBox, "the Company nav row should have a box").not.toBeNull();
  expect(labelBox!.x).toBeGreaterThanOrEqual(0);

  // The switcher took the rail's place, inside the sidebar rather than beside
  // it, and it holds both hosts.
  const switcher = page.getByTestId("host-switcher");
  await expect(switcher).toBeVisible();
  const switcherBox = await switcher.boundingBox();
  expect(switcherBox, "the switcher should have a box").not.toBeNull();
  expect(
    switcherBox!.x,
    "the switcher is inside the sidebar, not a column of its own to the left of it",
  ).toBeGreaterThanOrEqual(sidebarBox!.x);

  // Reachable from the keyboard, which the rail's unlabelled icon buttons
  // effectively were not — and holding both seeded hosts, plus the way to add
  // another that the rail's "+" used to be.
  await switcher.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("host-row-switcher-spec-primary")).toBeVisible();
  await expect(page.getByTestId("host-row-switcher-spec-second")).toBeVisible();
  await expect(page.getByTestId("host-switcher-add")).toBeVisible();
});
