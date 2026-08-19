import { expect, test, type Locator, type Page } from "@playwright/test";

import { COMPOSIO, COMPOSIO_FIXTURE_URL, COMPOSIO_REASON } from "./capabilities";

/**
 * Issue #855 — the Composio arm of the connection detail panel, driven in a
 * browser against a host.
 *
 * #819 shipped the panel and said plainly what it had not done: nothing drove it
 * against a live host, because that needs a real connected account and there was
 * no path to one locally. Its coverage is `test/unit/provider-detail-render.test.ts`
 * — jsdom, rendering the real component through a real grid row — plus
 * `provider-grid.test.ts` for the route rule and `connection-detail.test.ts` for
 * the date. Good tests, and they pin every decision #404 argued about. What they
 * cannot span is the seam between the component and the host, which is where both
 * bugs #819 found actually lived: `disconnectConnection` answered `200` and
 * revoked nothing, and the post-authorize poll announced a sign-in two seconds
 * before it happened. The MCP arm of the same panel got its e2e in #821
 * (`mcp.spec.ts`); this is the Composio one.
 *
 * ## Why this is not the wire stub the issue proposed
 *
 * The issue proposed overlaying a synthetic answer for `GET …/composio/connections`
 * on the default-feature host, the way `connections-native-not-offered.spec.ts`
 * overlays a native row. That was right when it was written and is no longer the
 * best available, for two reasons:
 *
 *  1. **It would not open the panel.** A tile is openable only when its route is
 *     `composio` (`ProviderTile`), and `connectRoute` answers that only when
 *     `reach.inBuild` — which the default binary reports `false`, because it
 *     compiles none of the Composio plane. Stubbing the connection list alone
 *     leaves every tile `unavailable` and unopenable; making it open would mean
 *     stubbing `GET …/composio` as well, at which point the host under test is
 *     synthetic on both sides and the spec is a slower jsdom test.
 *  2. **A path to a connected account now exists.** #820 added
 *     `test/e2e/composio-backend.mjs` and the `PW_COMPOSIO` lane: a host built
 *     `--features composio` pointed at a fixture that serves **two Gmail accounts
 *     for one toolkit** — exactly the shape the issue was trying to synthesize.
 *     On that lane the status, the connection list, the grid, the panel and the
 *     revoke are all real code paths; only the provider at the far end is a
 *     fixture.
 *
 * So this file drives that lane. Two claims still need a wire overlay, and each is
 * argued where it is used: an account Composio did not date, and a host that does
 * not serve usage. Neither can be produced by a fixture shared with
 * `composio-account-choice.spec.ts` without changing what that spec sees.
 *
 * ## What this still cannot prove
 *
 * That `DELETE …/composio/connections/{id}` releases the account **at Composio**.
 * The fixture forgets it, the host reports it gone, and the panel stops offering
 * it — which is the console's half, end to end. Whether the real provider revokes
 * the grant needs a live tenant and belongs in whatever staging check covers real
 * provider I/O.
 *
 * ## What this file borrows, and returns
 *
 * The suite runs serially against one host and one data root. This file sets the
 * company's Composio token and deletes an account from the fixture. Both are put
 * back in `afterAll`, for the reason `composio-account-choice.spec.ts` gives at
 * length: a token left set hands `connections-*.spec.ts` — which sort after this
 * file — a credentialled company with a live provider grid, and they would then
 * be asserting about a page this file configured.
 */

test.skip(!COMPOSIO || !COMPOSIO_FIXTURE_URL, COMPOSIO_REASON);

/** The Composio bearer the company holds. The fixture checks no auth. */
const TOKEN = "e2e-composio-token";

/** The fixture's two Gmail accounts, as it seeds them. */
const OPS = "ca_ops";
const BILLING = "ca_billing";

/** The member the #403 half signs in as. */
const MEMBER_EMAIL = "member-855@example.test";

/**
 * Put the fixture back to its seed: two Gmail accounts and one Slack.
 *
 * `DELETE …/connections/{id}` mutates a list one process serves to every spec, so
 * the revoke test below would otherwise decide what the tests after it see — a
 * failure that surfaces in a test that did not cause it.
 */
async function resetFixture(page: Page): Promise<void> {
  const reset = await page.request.post(`${COMPOSIO_FIXTURE_URL}/__reset`);
  expect(reset.ok(), `the composio fixture did not answer /__reset: ${reset.status()}`).toBeTruthy();
}

/**
 * Open Connections with this company holding a Composio credential and the
 * first-run tour out of the way.
 *
 * The token is set through the API rather than by typing into the credential card:
 * that card is `ComposioSection`'s own subject, and driving it here would make
 * this spec fail for that surface's reasons. Everything this file is actually
 * about is clicked.
 *
 * The welcome dialog is a Radix dialog, so while it is open every other element is
 * `aria-hidden` and invisible to `getByRole`; it also mounts a beat after the
 * navigation resolves, so an immediate `isVisible()` check races it and wins.
 */
async function openConnections(page: Page): Promise<void> {
  const set = await page.request.put("/api/v1/company/composio/token", {
    data: { token: TOKEN },
  });
  expect(set.ok(), `setting the composio token failed: ${set.status()}`).toBeTruthy();

  await page.goto("/#/settings/connections");
  await dismissTour(page);
  await expect(page.getByRole("heading", { name: "Providers" })).toBeVisible({ timeout: 30_000 });
}

/** Close the first-run tour if this browser context has not already seen it. */
async function dismissTour(page: Page): Promise<void> {
  const skip = page.getByRole("button", { name: "Skip for now" });
  await skip
    .waitFor({ state: "visible", timeout: 10_000 })
    .then(() => skip.click())
    .catch(() => {
      /* already dismissed in this context — nothing to close */
    });
  await expect(skip).toBeHidden({ timeout: 10_000 });
}

/**
 * Open the Gmail tile into the detail panel.
 *
 * The tile IS the control (`ProviderTile`) — a connected Composio tile carries no
 * inline Disconnect at all, precisely because a toolkit can hold two accounts and
 * a control that names neither has nothing to revoke.
 */
async function openGmail(page: Page): Promise<Locator> {
  const tile = page.getByTestId("open-provider-gmail");
  await expect(
    tile,
    "the fixture serves a gmail toolkit; without a tile there is nothing to open",
  ).toBeVisible({ timeout: 30_000 });
  await tile.click();
  const panel = page.getByRole("dialog");
  await expect(panel).toBeVisible();
  return panel;
}

test.beforeEach(async ({ page }) => {
  await resetFixture(page);
});

/**
 * Return the company as this file found it: the seed connection list, and no
 * Composio token.
 *
 * `testInfo.project.use`, NOT the environment. `playwright.config.ts` DERIVES both
 * of these, so reading `PW_BASE_URL` / `PW_STORAGE_STATE` gets the unset case
 * wrong in exactly the configuration CI uses — which once built an ANONYMOUS
 * context in this hook, had its writes refused `401`, and failed a spec two files
 * later. See `composio-account-choice.spec.ts`.
 */
test.afterAll(async ({ playwright }, testInfo) => {
  const request = await playwright.request.newContext({
    baseURL: testInfo.project.use.baseURL,
    storageState: testInfo.project.use.storageState as string | undefined,
  });
  try {
    if (COMPOSIO_FIXTURE_URL) await request.post(`${COMPOSIO_FIXTURE_URL}/__reset`);
    const cleared = await request.put("/api/v1/company/composio/token", { data: { token: "" } });
    // Asserted, not fired and forgotten. A silently-refused cleanup is what broke
    // the run above, and it broke it somewhere else — in a spec with no idea this
    // one exists. A failure here names the right file.
    expect(
      cleared.ok(),
      `clearing the composio token failed: ${cleared.status()} ${await cleared.text()}`,
    ).toBeTruthy();
  } finally {
    await request.dispose();
  }
});

test("the panel names every account the company holds, and marks none of them", async ({
  page,
}) => {
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));

  await openConnections(page);
  const panel = await openGmail(page);

  // Which of the three connection systems this is, said rather than left to be
  // inferred from the fact that a panel opened at all.
  await expect(panel).toContainText("Composio");
  await expect(panel).toContainText("2 accounts connected");

  // Both accounts, each named by the label the provider published, and each with
  // its own Disconnect. One control for a two-account toolkit is the shape the
  // panel exists to replace: it would name neither.
  const ops = panel.getByTestId(`provider-account-${OPS}`);
  const billing = panel.getByTestId(`provider-account-${BILLING}`);
  await expect(ops).toContainText("ops@acme.test");
  await expect(billing).toContainText("billing@acme.test");
  await expect(ops.getByRole("button", { name: "Disconnect ops@acme.test" })).toBeVisible();
  await expect(billing.getByRole("button", { name: "Disconnect billing@acme.test" })).toBeVisible();

  // Composio's own status string, forwarded rather than re-spelled: "set up and
  // since expired" and "never finished setting up" both flatten to "not connected".
  await expect(ops).toContainText("ACTIVE");

  // #404 asked for no "Default" chip and #820 did not put one back. Which account
  // agents act as is chosen in ONE place — `AccountChoiceSection`, on the page
  // behind this panel — and a second surface reading it back is how two surfaces
  // come to disagree. So the panel marks nothing and points at the control.
  await expect(panel, "no account may be marked as the one agents use").not.toContainText(
    "Default",
  );
  await expect(panel).not.toContainText("teammates act as this");
  await expect(panel).toContainText("Which account teammates act as");
  await expect(panel).toContainText("Composio resolves it for the company");

  // And specifically NOT the claim it replaced. #819 wrote "`composio_execute`
  // sends no connection id, so Composio resolves it. Disconnect the one you do not
  // want an agent to use", which is the sentence #855 still quotes and which #820
  // made false. An operator must not be told to revoke an account in order to
  // control which one acts.
  await expect(panel).not.toContainText("sends no connection id");

  // The date Composio recorded, rendered as a date. Matched on the year only:
  // `connectedOn` formats through `toLocaleDateString`, so the exact spelling is
  // the browser locale's business and pinning it would make this assertion about
  // the runner rather than about the panel.
  await expect(ops).toContainText(/connected .*2026/);
  await expect(ops).not.toContainText("connection date not recorded");

  expect(pageErrors, `the page threw: ${pageErrors.join(" | ")}`).toEqual([]);
});

test("an account Composio did not date says so rather than showing a blank", async ({ page }) => {
  // The one account shape the fixture cannot serve. Its seed dates every
  // connection, and adding an undated one there would change what
  // `composio-account-choice.spec.ts` sees on the same list — so it is overlaid on
  // the host's own answer at the wire, the pattern
  // `connections-native-not-offered.spec.ts` established for exactly this: a row
  // the harness cannot produce, over a response that is otherwise the host's.
  //
  // The glob ends at `connections`, so the revoke route — which is
  // `…/composio/connections/{id}` — is deliberately not matched by it.
  await page.route("**/composio/connections", async (route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const response = await route.fetch();
    const rows: unknown = response.ok() ? await response.json() : [];
    const listed = Array.isArray(rows) ? (rows as { toolkit?: string; accounts?: unknown[] }[]) : [];
    await route.fulfill({
      json: listed.map((row) =>
        row.toolkit === "gmail"
          ? {
              ...row,
              accounts: [
                ...(row.accounts ?? []),
                // No `createdAt`, and no account label either. Composio publishes
                // neither for some providers, and each has a wrong answer that
                // looks right: a blank cell, which reads as "never", and a label
                // guessed from the toolkit, which makes two accounts
                // indistinguishable at the moment one has to be picked.
                { id: "ca_undated", status: "ACTIVE", connected: true },
              ],
            }
          : row,
      ),
    });
  });

  await openConnections(page);
  const panel = await openGmail(page);

  const undated = panel.getByTestId("provider-account-ca_undated");
  await expect(undated).toContainText("connection date not recorded");
  await expect(undated).toContainText("Account name not published");

  // The dated account is unaffected, which is what makes the line above a claim
  // about this account rather than about the panel's ability to read a date at all.
  await expect(panel.getByTestId(`provider-account-${OPS}`)).toContainText(/connected .*2026/);
});

test("usage is counted per provider, and an unmetered host reports no figure", async ({ page }) => {
  await openConnections(page);
  const panel = await openGmail(page);

  // This host serves the usage route and no agent has called Gmail, so a real zero
  // is the right answer. What matters is the sentence beneath it: the figure is
  // per *connection*, and both accounts above land on this one total, so reading
  // it against either one would be a number that means something else.
  const usage = panel.getByTestId("connection-detail-usage");
  await expect(usage).toContainText("in the last 30 days");
  await expect(usage).toContainText("per provider rather than per account");

  // And the case that must not be confused with it. A host without the usage route
  // 404s, and "0 calls" there claims the calls were counted and there were none.
  // Routed by predicate rather than by glob: the request carries `?range=30d`, and
  // a glob written against a query string is a way to match nothing by accident.
  await page.route(
    (url) => url.pathname.endsWith("/usage"),
    (route) => route.fulfill({ status: 404, json: { error: "not_found" } }),
  );

  // `reload`, not another `goto`. The URL is already `#/settings/connections`, so
  // a `goto` to it is a same-document no-op that leaves this panel open — and
  // while a Radix dialog is open everything behind it is `aria-hidden`, so the
  // wait for the Providers heading would time out against a page that is fine.
  await page.reload();
  await expect(page.getByRole("heading", { name: "Providers" })).toBeVisible({ timeout: 30_000 });
  const unmetered = (await openGmail(page)).getByTestId("connection-detail-usage");
  await expect(unmetered).toContainText("does not report usage");
  await expect(unmetered).not.toContainText("0 calls");
});

test("a disconnect addresses one account, and leaves the other connected", async ({ page }) => {
  // The assertion the original bug cannot come back past. Before #696 this page's
  // Disconnect posted to `…/connections/{provider}/disconnect`, which blanks the
  // host's own `oauth/{provider}` secret: for a Composio-connected provider it
  // blanked a secret that never existed, answered `200`, said "Disconnected Gmail",
  // and left Gmail connected on the next refresh.
  await openConnections(page);
  const panel = await openGmail(page);

  const revoked = page.waitForRequest(
    (request) =>
      request.method() === "DELETE" &&
      new URL(request.url()).pathname.includes("/composio/connections/"),
    { timeout: 30_000 },
  );
  await panel
    .getByTestId(`provider-account-${BILLING}`)
    .getByRole("button", { name: "Disconnect billing@acme.test" })
    .click();

  // ONE account id, and the one that was clicked. The route that could not work
  // carries the toolkit — `gmail` — in this position, so the defect is a single
  // path segment and this is where it would show.
  const path = new URL((await revoked).url()).pathname;
  expect(path.endsWith(`/composio/connections/${BILLING}`), `the revoke went to ${path}`).toBeTruthy();

  // The panel re-derives from the refreshed grid rather than crossing the row out
  // locally, so what is asserted here is the host's answer: billing is gone, ops
  // is not, and the toolkit is still connected.
  await expect(panel.getByTestId(`provider-account-${BILLING}`)).toHaveCount(0, { timeout: 30_000 });
  await expect(panel.getByTestId(`provider-account-${OPS}`)).toContainText("ops@acme.test");
  await expect(panel).toContainText("1 account connected");

  // Read back off the wire as well. The rendering above would also be produced by
  // a console that dropped the row optimistically, and "reports success while
  // revoking nothing" is the exact failure being pinned.
  const rows = await page.request.get("/api/v1/company/composio/connections");
  expect(rows.ok(), `re-reading the connection list failed: ${rows.status()}`).toBeTruthy();
  const gmail = ((await rows.json()) as { toolkit: string; accounts?: { id: string }[] }[]).find(
    (row) => row.toolkit === "gmail",
  );
  expect(gmail, "gmail must still be a row — one account was revoked, not the toolkit").toBeDefined();
  const ids = (gmail?.accounts ?? []).map((account) => account.id);
  expect(ids).toContain(OPS);
  expect(ids).not.toContain(BILLING);
});

test("a member is told what is connected and offered nothing that changes it", async ({
  page,
  browser,
}) => {
  // Issue #403, at the one surface `connections-authority.spec.ts` does not reach.
  // That spec proves the *page* offers a member no credential field and no write
  // control, and stops at the grid. The panel is a second place the same controls
  // could have been drawn, and it is where a member is most likely to end up:
  // "which account is Gmail wired to, and since when" is exactly what a member
  // opens this page to learn, which is why the panel opens for them at all. What
  // it must not do is hand them a Disconnect the host answers `403` to.
  //
  // The member signs in through the same magic-link flow the product uses, in its
  // own context, using the suite's admin session only to issue the invite.
  const memberContext = await browser.newContext({ storageState: undefined });
  try {
    // The credential is the company's, so an admin sets it — a member cannot, and
    // without one there is no Composio route and so no tile to open.
    await openConnections(page);

    const invited = await page.request.post("/api/v1/company/users/invites", {
      data: { email: MEMBER_EMAIL, role: "member" },
    });
    // Idempotent: a re-run hits `409 already a member`, which is a success for our
    // purposes — the address can sign in either way.
    expect(
      invited.ok() || invited.status() === 409,
      `inviting ${MEMBER_EMAIL} failed: ${invited.status()} ${await invited.text()}`,
    ).toBeTruthy();

    const requested = await memberContext.request.post("/api/v1/company/auth/request", {
      data: { email: MEMBER_EMAIL },
    });
    const devCode = (await requested.json())?.dev_code as string | undefined;
    expect(
      devCode,
      "no dev_code came back — the host must bind loopback with no mail transport " +
        "configured for the member half of this spec to sign in",
    ).toBeTruthy();
    const verified = await memberContext.request.post("/api/v1/company/auth/verify", {
      data: { code: devCode },
    });
    expect(verified.ok(), `member sign-in failed: ${await verified.text()}`).toBeTruthy();
    expect((await verified.json()).role).toBe("member");

    const memberPage = await memberContext.newPage();
    await memberPage.goto("/#/settings/connections");
    await dismissTour(memberPage);
    await expect(memberPage.getByTestId("connections-read-only")).toBeVisible({ timeout: 30_000 });

    const panel = await openGmail(memberPage);

    // The read is intact — this is what the panel is for, and it is not an
    // admin-only question.
    await expect(panel.getByTestId(`provider-account-${OPS}`)).toContainText("ops@acme.test");
    await expect(panel).toContainText("2 accounts connected");

    // And nothing that writes. Courtesy rather than enforcement — the host answers
    // `403` whatever this renders — but a control that can only fail is a poor
    // thing to put in front of someone reading the page to find out what is wired.
    await expect(panel.getByRole("button", { name: /^Disconnect / })).toHaveCount(0);
    await expect(panel.getByTestId("provider-detail-connect-another")).toHaveCount(0);
    await expect(panel).toContainText("Only an admin can connect or disconnect an account here");
  } finally {
    await memberContext.close();
  }
});
