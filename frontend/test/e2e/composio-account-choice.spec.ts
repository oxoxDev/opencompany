import { randomUUID } from "node:crypto";

import { expect, test, type Page } from "@playwright/test";

import {
  COMPOSIO,
  COMPOSIO_FIXTURE_URL,
  COMPOSIO_REASON,
  LIVE_BRAIN,
  LIVE_BRAIN_REASON,
} from "./capabilities";

/**
 * Issue #820 — a company says which connected account its agents act as, and
 * the agents act as it.
 *
 * A company can hold two Composio accounts for one toolkit: `ops@` and
 * `billing@` Gmail. Before this, nothing in the product chose between them.
 * `composio_execute` built its body as `{tool, arguments}` and sent no
 * connection id, so the account was resolved by Composio for the entity,
 * entirely outside this codebase — "send from the billing account, not ops"
 * was not sayable, and "which Gmail did the agent send from" was unanswerable
 * even after the fact.
 *
 * # Why this spec has two halves, and why both are needed
 *
 * The console half and the harness half fail independently, and each is
 * uninteresting alone:
 *
 *  * A stored preference nothing reads is the exact shape of #396 — a console
 *    control that writes a value no code path consults. So it is not enough to
 *    assert the button stored something.
 *  * A connection id sent on every execute regardless of what the operator
 *    chose would pass a wire assertion while ignoring the page. So it is not
 *    enough to assert the body carried an id.
 *
 * The claim worth pinning joins them: the id on the wire is *the one the
 * operator clicked*, and no id is sent at all until they click. The second test
 * asserts the negative on purpose — it is what protects every existing
 * single-account company from having its account resolution changed by this
 * feature.
 *
 * # What is a fixture here
 *
 * `composio-backend.mjs` stands in for the platform's
 * `/agent-integrations/composio/*` routes, because holding two live Gmail
 * connections per CI run is not a thing a test can arrange — and because the
 * assertion is about the request body the host sends, which only the receiver
 * can report. The host, its secret store, the roster rebuild, the tool belt and
 * the agent turn are all real. Only the model's *choice* of tool is scripted
 * (`__MOCK_TOOL_CALL__`), for the reason `mcp-agent.spec.ts` gives: a spec
 * cannot assert on a model that is free to decline.
 */

test.skip(!COMPOSIO || !COMPOSIO_FIXTURE_URL, COMPOSIO_REASON);

/** The Composio bearer the console pastes. The fixture checks no auth. */
const TOKEN = "e2e-composio-token";

/**
 * Open Connections with the first-run tour out of the way, and with this
 * company holding a Composio credential.
 *
 * The token is set through the API rather than by typing into the credential
 * card: that card is `ComposioSection`'s own subject, and driving it here would
 * make this spec fail for that surface's reasons. Everything this spec is
 * actually about is clicked.
 */
async function openConnections(page: Page): Promise<void> {
  const set = await page.request.put("/api/v1/company/composio/token", {
    data: { token: TOKEN },
  });
  expect(set.ok(), `setting the composio token failed: ${set.status()}`).toBeTruthy();

  await page.goto("/#/settings/connections");
  const skip = page.getByRole("button", { name: "Skip for now" });
  await skip
    .waitFor({ state: "visible", timeout: 10_000 })
    .then(() => skip.click())
    .catch(() => {
      /* already dismissed in this context */
    });
  await expect(skip).toBeHidden({ timeout: 10_000 });
  await expect(page.getByRole("heading", { name: "Providers" })).toBeVisible({ timeout: 30_000 });
}

/** Every execute body the fixture has received, oldest first. */
async function executes(page: Page): Promise<{ tool: string; connectionId?: string }[]> {
  const seen = await page.request.get(`${COMPOSIO_FIXTURE_URL}/__executes`);
  expect(seen.ok(), "the composio fixture did not answer /__executes").toBeTruthy();
  return seen.json();
}

/**
 * Forget what the fixture saw, so one test cannot read another's calls — and
 * restore its connection list, so one cannot remove another's accounts either.
 */
async function resetFixture(page: Page): Promise<void> {
  await page.request.post(`${COMPOSIO_FIXTURE_URL}/__reset`);
}

/** Return the company to "nothing chosen", whatever a test left behind. */
async function clearChoice(page: Page): Promise<void> {
  for (const id of ["ca_ops", "ca_billing"]) {
    await page.request.delete(`/api/v1/company/composio/connections/${id}/default`);
  }
}

/**
 * Put the company back as this file found it: no chosen accounts, and no
 * Composio token.
 *
 * Not housekeeping for its own sake. The suite runs serially against one host
 * with one data root, and this file sorts before `connections-*.spec.ts` — so a
 * token left set would hand those specs a credentialled company with a live
 * provider grid, and they would be asserting about a page this file configured.
 * The `PW_COMPOSIO` lane is new; the specs it runs alongside are not.
 */
test.afterAll(async ({ playwright }, testInfo) => {
  // `testInfo.project.use`, NOT the environment. `playwright.config.ts` DERIVES
  // both of these — `baseURL` defaults when `PW_BASE_URL` is unset, and
  // `storageState` defaults to `target/e2e/storage-state.json` whenever the run
  // manages its own host — so reading the variables gets the unset case wrong
  // in exactly the configuration CI uses. That is not hypothetical: this hook
  // built an ANONYMOUS context on the first CI run, its writes were refused
  // 401, the token stayed set, and `oauth-onboarding-resume.spec.ts` failed two
  // files later on a Slack tile this file had connected.
  const request = await playwright.request.newContext({
    baseURL: testInfo.project.use.baseURL,
    storageState: testInfo.project.use.storageState as string | undefined,
  });
  try {
    for (const id of ["ca_ops", "ca_billing"]) {
      await request.delete(`/api/v1/company/composio/connections/${id}/default`);
    }
    const cleared = await request.put("/api/v1/company/composio/token", {
      data: { token: "" },
    });
    // Asserted, not fired and forgotten. A silently-refused cleanup is what
    // broke the run above, and it broke it somewhere else — two files later, in
    // a spec with no idea this one exists. A failure here names the right file.
    expect(
      cleared.ok(),
      `clearing the composio token failed: ${cleared.status()} ${await cleared.text()}`,
    ).toBeTruthy();
  } finally {
    await request.dispose();
  }
});

test("an operator names the account, and the page says so", async ({ page }) => {
  await clearChoice(page);
  await openConnections(page);

  // The section exists at all only because this company holds two Gmail
  // accounts. A single-account company never sees it — a "default" control
  // beside the only account there is invites a decision that changes nothing.
  const gmail = page.getByTestId("accounts-gmail");
  await expect(gmail).toBeVisible({ timeout: 30_000 });
  await expect(gmail).toContainText("ops@acme.test");
  await expect(gmail).toContainText("billing@acme.test");

  // Nothing chosen yet, and the page says which of the two situations this is
  // rather than pointing at a row it cannot back up (#819's argument).
  await expect(gmail).toContainText("Composio picks");

  await gmail
    .getByTestId("account-ca_billing")
    .getByRole("button", { name: "Act as this" })
    .click();

  // The claim is the host's, re-read: the mark is drawn from `GET
  // …/composio/connections`, so a passing assertion here means the choice was
  // stored and reported, not merely painted locally.
  await expect(gmail.getByTestId("account-ca_billing")).toContainText("teammates act as this");
  await expect(gmail).not.toContainText("Composio picks");

  await page.reload();
  await openConnections(page);
  await expect(
    page.getByTestId("accounts-gmail").getByTestId("account-ca_billing"),
  ).toContainText("teammates act as this", { timeout: 30_000 });

  // Choosing for Gmail says nothing about any other provider.
  const rows = await page.request.get("/api/v1/company/composio/connections");
  const body = (await rows.json()) as {
    toolkit: string;
    defaultConnectionId?: string;
  }[];
  expect(body.find((r) => r.toolkit === "gmail")?.defaultConnectionId).toBe("ca_billing");
  // Asserted through the row, not through `find(...)?.` — an absent slack row
  // would satisfy `toBeUndefined()` just as an unchosen one does, and then the
  // isolation claim would hold vacuously.
  const slack = body.find((r) => r.toolkit === "slack");
  expect(slack, "the fixture serves a slack toolkit; without it this proves nothing").toBeDefined();
  expect(slack?.defaultConnectionId).toBeUndefined();
});

test.describe("the agent acts as the chosen account", () => {
  test.skip(!LIVE_BRAIN, LIVE_BRAIN_REASON);

  /**
   * Approve the parked `composio_execute`, which is the only way one ever
   * reaches a provider.
   *
   * Not incidental plumbing: `composio_execute` is an `Execute`-level tool and
   * the harness parks it — "sends to a counterparty, which cannot be taken
   * back". So the send an operator authorises is the send that carries their
   * choice of account, and this spec would be describing a path that does not
   * exist if it bypassed the gate.
   */
  async function approvePendingSend(page: Page): Promise<void> {
    await expect
      .poll(
        async () => {
          const pending = await page.request.get("/api/v1/company/approvals");
          if (!pending.ok()) return 0;
          return ((await pending.json()) as unknown[]).length;
        },
        { timeout: 30_000 },
      )
      .toBeGreaterThan(0);

    const pending = (await (await page.request.get("/api/v1/company/approvals")).json()) as {
      id: string;
      tool?: string;
    }[];
    for (const approval of pending) {
      const decided = await page.request.post(
        `/api/v1/company/approvals/${encodeURIComponent(approval.id)}`,
        { data: { verdict: "approve" } },
      );
      expect(
        decided.ok(),
        `approving ${approval.id} failed: ${decided.status()} ${await decided.text()}`,
      ).toBeTruthy();
    }
  }

  /**
   * Make the agent run one `composio_execute` and hand back what the fixture
   * received for it.
   *
   * The POST is awaited explicitly before anything else happens, for the reason
   * `mcp-agent.spec.ts` documents: a turn runs inside the request that started
   * it, and the host drops the work when the client goes away.
   */
  async function runOneExecute(page: Page): Promise<{ tool: string; connectionId?: string }[]> {
    await resetFixture(page);
    await page.goto("/#/conversation");
    const skip = page.getByRole("button", { name: "Skip for now" });
    await skip
      .waitFor({ state: "visible", timeout: 5_000 })
      .then(() => skip.click())
      .catch(() => {
        /* already dismissed */
      });
    await page
      .getByRole("complementary")
      .getByRole("button", { name: /Your company/ })
      .first()
      .click();

    // A SEND, deliberately. `composio_execute` is parked for approval on the
    // strength of the slug — a fetch runs straight through — and the send is
    // both the case #820 is written around ("send from billing@, not ops@")
    // and the one that exercises the gate: park → operator approves → the
    // agent re-issues → the pinned account reaches the wire. A fetch here
    // would pass while quietly covering none of that.
    const directive = `__MOCK_TOOL_CALL__ ${JSON.stringify({
      name: "composio_execute",
      arguments: {
        tool: "GMAIL_SEND_EMAIL",
        arguments: { to: `${randomUUID()}@acme.test`, subject: "e2e", body: "e2e" },
      },
    })}`;

    const posted = page.waitForResponse(
      (response) => response.url().endsWith("/chat") && response.request().method() === "POST",
      { timeout: 90_000 },
    );
    await page.getByPlaceholder(/^Message /).fill(directive);
    // `exact`. The composer's button is labelled exactly "Send"; the sidebar's
    // thread preview takes its accessible name from the last message, so a
    // loose match resolves to both as soon as any message mentions sending.
    await page.getByRole("button", { name: "Send", exact: true }).click();
    expect((await posted).ok(), "the chat POST did not succeed").toBeTruthy();

    await approvePendingSend(page);

    await expect
      .poll(async () => (await executes(page)).length, { timeout: 30_000 })
      .toBeGreaterThan(0);
    return executes(page);
  }

  test("no id is sent until somebody chooses, and then it is theirs", async ({ page }) => {
    await clearChoice(page);
    await openConnections(page);

    // The untouched path first. Every company that holds one account per
    // toolkit is on it, and this feature must not have moved them: the body is
    // the one this tool has always sent, and Composio resolves the account.
    const before = await runOneExecute(page);
    expect(before[0].tool).toBe("GMAIL_SEND_EMAIL");
    expect(
      before[0].connectionId,
      `an unchosen toolkit must send no connection id: ${JSON.stringify(before[0])}`,
    ).toBeUndefined();

    // Now choose, through the page, exactly as an operator would.
    await openConnections(page);
    await page
      .getByTestId("accounts-gmail")
      .getByTestId("account-ca_billing")
      .getByRole("button", { name: "Act as this" })
      .click();
    await expect(
      page.getByTestId("accounts-gmail").getByTestId("account-ca_billing"),
    ).toContainText("teammates act as this");

    // …and the next turn acts as it. This is the whole issue in one assertion:
    // the choice made on the page reaches the request the harness sends, which
    // the backend forwards to Composio as the connected account.
    const after = await runOneExecute(page);
    expect(after[0].connectionId, `the chosen account did not reach the wire`).toBe("ca_billing");
  });
});
