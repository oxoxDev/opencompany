import { expect, test } from "@playwright/test";

import { LIVE_BRAIN } from "./capabilities";
import { openChannel, workingRow } from "./chat-helpers";

/**
 * Where the live pair sits, and what it says, while a turn is still running.
 *
 * Three claims no unit test can make, because all three are about **rendered
 * order and rendered state** rather than about a pure function:
 *
 * 1. the line names the newest running call rather than a settled one;
 * 2. a call parked on a sign-off reads as parked *while it waits*, not once
 *    the reply lands.
 *
 * Not the agent hand-over: a running step outranks the name, so these
 * scenarios never put one on screen. That lives in `live-pair-render.test.ts`,
 * which controls `agentNames` and asserts the name directly.
 *
 * The ordering claim — that the row sits beneath every line journaled while
 * the turn ran — lives in `chat-concurrent-episodes.spec.ts` instead, and has
 * to. A turn with no `messageSeq` keys its rows by thread, and for such a turn
 * an `agent_reply` is the end signal: the bucket retires, correctly, and the
 * row goes with it. Only a query-keyed turn journals lines *while it continues*
 * — which is every hive episode, and why the claim belongs with them.
 *
 * Like `chat-live-events.spec.ts`'s synthetic fixture — whose pattern this
 * borrows wholesale — these write their own SSE stream. The offline brain this
 * suite runs against calls no tools and never delegates, so there is no live
 * multi-agent turn to watch without inventing one, and what is under test is
 * the rendering rather than the plumbing. The frames below are the exact shape
 * `turn_stream.rs` puts on the wire and `use-events.ts` types.
 *
 * Like the rest of `test/e2e` this needs a running host and is not a CI gate —
 * the Playwright config declares no `webServer`.
 */

/**
 * The desk these run against. Its id is its channel id.
 *
 * Defaults to the harness manifest's engineering desk, like every sibling
 * spec. Overridable because these mock the whole event stream — the only thing
 * they need from the host is a desk that exists, so pinning one company's
 * manifest would make them unrunnable against any other host for no reason the
 * tests themselves care about.
 */
const DESK_ID = process.env.PW_CHAT_DESK ?? "engineering";
const ENGINEERING = { id: DESK_ID, channel: `${DESK_ID}-desk` };

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const real = Storage.prototype.getItem;
    Storage.prototype.getItem = function getItem(key: string) {
      return key.startsWith("oc-tour:") ? '{"skipped":true}' : real.call(this, key);
    };
  });
});

/**
 * Opens the channel with `frames` already queued on the intercepted stream.
 *
 * The release dance is load-bearing and is why this is a helper rather than
 * three copies: Playwright counts a routed stream as pending navigation work,
 * so awaiting the navigation before releasing deadlocks — while fulfilling
 * immediately can deliver frames before the channel map has mounted. The
 * visible composer is the readiness boundary that threads between the two.
 */
async function openWithFrames(page: import("@playwright/test").Page, frames: unknown[]) {
  let releaseFrames: (() => void) | undefined;
  const framesReleased = new Promise<void>((resolve) => {
    releaseFrames = resolve;
  });
  let streamRequested: (() => void) | undefined;
  const streamIsWaiting = new Promise<void>((resolve) => {
    streamRequested = resolve;
  });
  // The API's SSE route only. A bare `**/events**` also matches the dev
  // server's own unbundled modules — `src/hooks/use-events.ts` among them —
  // and holding those until the frames release means the console never boots,
  // which presents as a page stuck on "Waking this company…".
  await page.route(
    (url) => url.pathname.startsWith("/api/") && url.pathname.endsWith("/events"),
    async (route) => {
    streamRequested?.();
    await framesReleased;
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream", "cache-control": "no-cache" },
      body: frames.map((f) => `data: ${JSON.stringify(f)}\n\n`).join(""),
      });
    },
  );

  const channelOpened = openChannel(page, ENGINEERING.id);
  await streamIsWaiting;
  await expect(page.getByPlaceholder(/^Message /)).toBeVisible({ timeout: 30_000 });
  releaseFrames?.();
  await channelOpened;
}

test("the line names the newest running call, not a settled one", async ({ page }) => {
  test.skip(LIVE_BRAIN, "the default Console E2E lane covers the synthetic SSE rendering fixture");
  // One turn, two agents, and the second's call still open.
  //
  // This asserts the STEP, not the agent name — deliberately, because a
  // running step outranks the name by design (`WorkingIndicator`), so in this
  // scenario no name is on screen to assert. The agent hand-over is covered
  // in `live-pair-render.test.ts`, which can hold `agentNames` steady and so
  // can see the name the running label would otherwise hide. Confirmed by
  // mutation: reverting the live-agent preference leaves this test green.
  const atMillis = Date.now();
  await openWithFrames(page, [
    {
      type: "tool_call",
      seq: 1,
      atMillis,
      chatId: ENGINEERING.id,
      agentId: "a-ada",
      toolCallId: "t1",
      label: "workspace_list",
    },
    {
      type: "tool_result",
      seq: 2,
      atMillis: atMillis + 1,
      chatId: ENGINEERING.id,
      agentId: "a-ada",
      toolCallId: "t1",
      status: "ok",
      elapsedMs: 40,
    },
    // The floor passes. Same chat, same query, different seat.
    {
      type: "tool_call",
      seq: 3,
      atMillis: atMillis + 2,
      chatId: ENGINEERING.id,
      agentId: "a-grace",
      toolCallId: "t2",
      label: "workspace_search",
    },
  ]);

  // The newest running step names the line, and it belongs to the second agent.
  await expect(workingRow(page)).toContainText("workspace_search", { timeout: 30_000 });
  await expect(workingRow(page)).not.toContainText("workspace_list");
});

test("a call parked on a sign-off reads as parked while it waits", async ({ page }) => {
  test.skip(LIVE_BRAIN, "the default Console E2E lane covers the synthetic SSE rendering fixture");
  // The state the fold used to flatten into `ok`. A gated call rendered as one
  // that had succeeded for the whole time it sat waiting, then flipped to
  // "awaiting approval · didn't run" when the reply landed — so the one state
  // the operator could act on was the one the live timeline could not show.
  const atMillis = Date.now();
  await openWithFrames(page, [
    {
      type: "tool_call",
      seq: 1,
      atMillis,
      chatId: ENGINEERING.id,
      agentId: "a-ada",
      toolCallId: "t1",
      label: "composio_execute",
    },
    {
      type: "tool_result",
      seq: 2,
      atMillis: atMillis + 1,
      chatId: ENGINEERING.id,
      agentId: "a-ada",
      toolCallId: "t1",
      label: "composio_execute",
      status: "awaiting_approval",
    },
  ]);

  // No running step is left, so the line falls back to its generic wording
  // rather than naming a settled call — but the turn is still open, so the row
  // is still there. What must NOT happen is the row reading as finished work.
  await expect(workingRow(page)).toBeVisible({ timeout: 30_000 });
  await expect(page.getByText("didn't run")).toHaveCount(0);
});
