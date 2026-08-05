import { expect, test, type APIRequestContext, type Locator, type Page } from "@playwright/test";

/**
 * End-to-end proof for issue #367 — the Chat tab receives what the company
 * says, not only what this browser tab typed.
 *
 * `#361` made Chat the nav-listed surface while every live writer stayed
 * pointed at the parked Conversation, so a channel showed the console's own
 * turns and nothing else: an inbound reply appeared only after a reload, a
 * running turn showed a generic dot instead of its tool rows, and the rail's
 * unread badges were fed a hard-coded empty map.
 *
 * Three of the four tests drive a **live host** (`companies/e2e_harness`) and a
 * real SSE stream, because the defect was in the addressing between a host
 * *thread* id and a console *channel* id — a mocked stream would have agreed
 * with whatever the console believed. The last one mocks `…/events` on purpose:
 * the offline echo brain calls no tools, so the only way to put real
 * `tool_call` / `tool_result` frames on the wire without a model is to write
 * them, and what is under test there is the rendering, not the plumbing.
 *
 * Like the rest of `test/e2e` this needs a running host and is not a CI gate —
 * the Playwright config declares no `webServer`.
 */

/**
 * The single-company alias the host answers on. Used only for the out-of-band
 * POSTs below — the console itself addresses the company by name
 * (`/api/v1/companies/<id>/…`), which is why the route patterns further down
 * match by suffix instead.
 */
const SCOPE = "/api/v1/company";

/** The harness manifest's two desks. Their ids are their channel ids. */
const ENGINEERING = { id: "engineering", channel: "engineering-desk" };
const CONTENT = { id: "content", channel: "content-desk" };

/**
 * The first-run product tour renders a modal over the console and swallows
 * every click beneath it. Answer "already skipped" for whatever company id the
 * host resolves to rather than hard-coding the harness's.
 */
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const real = Storage.prototype.getItem;
    Storage.prototype.getItem = function getItem(key: string) {
      return key.startsWith("oc-tour:") ? '{"skipped":true}' : real.call(this, key);
    };
  });
});

/** Opens one channel by id and waits for its transcript to be on screen. */
async function openChannel(page: Page, channelId: string) {
  await page.goto(`/#/chat/${channelId}`);
  await expect(page.getByPlaceholder(/^Message /)).toBeVisible({ timeout: 30_000 });
}

/**
 * A channel's row in the rail, located by the name the rail renders.
 *
 * Not an exact name match: an unread badge is inside the same button, so the
 * row's accessible name grows a count the moment the thing under test happens —
 * and an exact matcher would stop resolving exactly when it matters.
 */
function railRow(page: Page, channelName: string): Locator {
  return page.getByRole("complementary").first().getByRole("button", { name: channelName });
}

/** Every non-system bubble currently rendered in the open channel. */
function bubbles(page: Page): Locator {
  return page.locator("article[data-message-id]");
}

/**
 * The bubble count, once the channel's rehydration has stopped adding to it.
 *
 * A channel opens empty and fills from `chat/history` a moment later, so a
 * count taken on arrival is a count of nothing — and every "one more bubble
 * than before" assertion below would be measuring the hydration instead of the
 * reply. Waits for two equal readings rather than a fixed sleep.
 */
async function settledBubbleCount(page: Page): Promise<number> {
  let last = -1;
  await expect
    .poll(
      async () => {
        const current = await bubbles(page).count();
        const settled = current === last;
        last = current;
        return settled;
      },
      { intervals: [400, 400, 400, 400, 400, 400, 400, 400], timeout: 20_000 },
    )
    .toBe(true);
  return last;
}

/**
 * Says something to a desk from **outside the browser**, over the same session.
 *
 * This is the whole point of the first two tests: nothing the page did produced
 * this turn, so the only way its reply can reach the channel is the SSE stream.
 * It stands in for the inbound Telegram turn / background desk turn from the
 * issue, which cannot be triggered from a test.
 */
async function sayFromElsewhere(request: APIRequestContext, deskId: string, text: string) {
  const response = await request.post(`${SCOPE}/chat`, { data: { text, chat: deskId } });
  expect(
    response.ok(),
    `posting to ${deskId} failed: ${response.status()} ${await response.text()}`,
  ).toBeTruthy();
}

test("a reply the console never asked for lands in the open channel, with no reload", async ({
  page,
  request,
}) => {
  await openChannel(page, ENGINEERING.id);
  const before = await settledBubbleCount(page);

  const marker = `inbound-${Date.now()}`;
  await sayFromElsewhere(request, ENGINEERING.id, marker);

  // The offline brain answers "You said: <text>", so the marker rides back in
  // the reply. Only the company's half arrives here — the operator line of a
  // turn this console did not send is not ours to draw.
  await expect(page.getByText(`You said: ${marker}`)).toBeVisible({ timeout: 60_000 });
  await expect(bubbles(page)).toHaveCount(before + 1);
});

test("a reply to a channel you are not on leaves an unread badge, and opening it clears the badge", async ({
  page,
  request,
}) => {
  // Sit on the Content desk; the reply below is addressed to Engineering.
  await openChannel(page, CONTENT.id);

  const marker = `unread-${Date.now()}`;
  await sayFromElsewhere(request, ENGINEERING.id, marker);

  const badge = railRow(page, ENGINEERING.channel).getByTestId("channel-unread");
  await expect(badge).toBeVisible({ timeout: 60_000 });

  // Opening the channel both shows the line and settles the badge — an unread
  // count that never clears is the same failure as one that never appears.
  await railRow(page, ENGINEERING.channel).click();
  await expect(page.getByText(`You said: ${marker}`)).toBeVisible({ timeout: 30_000 });
  await expect(badge).toHaveCount(0);
});

test("a turn sent from the composer renders exactly one company bubble", async ({ page }) => {
  // The regression guard for the duplicate-bubble race, and the reason the
  // send bracket had to ship in the same commit as the SSE injection: the host
  // journals an `AgentReply` for the console's own turn too and pushes it over
  // SSE *while the POST is still in flight*. Without the bracket the injected
  // echo and the awaited reply both render and one turn answers twice.
  await openChannel(page, ENGINEERING.id);
  const before = await settledBubbleCount(page);

  const marker = `composer-${Date.now()}`;
  await page.getByPlaceholder(/^Message /).fill(marker);
  await page.keyboard.press("Enter");

  // Your line plus one reply — never three rows.
  await expect(bubbles(page)).toHaveCount(before + 2, { timeout: 60_000 });
  await expect(page.getByText(`You said: ${marker}`)).toHaveCount(1);

  // A late echo would land after the POST resolved, so settle before believing
  // the count above.
  await page.waitForTimeout(3_000);
  await expect(bubbles(page)).toHaveCount(before + 2);
  await expect(page.getByText(`You said: ${marker}`)).toHaveCount(1);
});

test("a running turn shows its tool rows in the channel", async ({ page }) => {
  // The one test that writes its own stream. The frames below are the exact
  // shape `src/turn_stream.rs` puts on the wire and `use-events.ts` types; the
  // offline brain this suite runs against calls no tools, so there is no live
  // turn to watch without inventing one. What is being proved is that a frame
  // carrying a desk's thread id reaches *that channel's* timeline — which is
  // what Chat never did.
  const frames = [
    { type: "tool_call", seq: 1, chatId: ENGINEERING.id, toolCallId: "t1", label: "workspace_list" },
    {
      type: "tool_result",
      seq: 2,
      chatId: ENGINEERING.id,
      toolCallId: "t1",
      label: "workspace_list",
      status: "ok",
      elapsedMs: 120,
    },
    { type: "tool_call", seq: 3, chatId: ENGINEERING.id, toolCallId: "t2", label: "workspace_read" },
  ];
  await page.route("**/events", (route) =>
    route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream", "cache-control": "no-cache" },
      body: frames.map((f) => `data: ${JSON.stringify(f)}\n\n`).join(""),
    }),
  );

  await openChannel(page, ENGINEERING.id);

  // The rows themselves, not a typing dot — and the finished one keeps the
  // elapsed time the frame carried.
  await expect(page.getByText("workspace_list").first()).toBeVisible({ timeout: 30_000 });
  await expect(page.getByText("workspace_read").first()).toBeVisible();
  await expect(page.getByText("Replying…")).toHaveCount(0);

  // Addressed, not broadcast: the other desk's channel shows none of it.
  await openChannel(page, CONTENT.id);
  await expect(page.getByText("workspace_list")).toHaveCount(0);
});
