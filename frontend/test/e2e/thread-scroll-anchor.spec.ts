import { expect, test, type Locator, type Page } from "@playwright/test";

import { LIVE_BRAIN } from "./capabilities";
import { bubbles, openChannel } from "./chat-helpers";

/**
 * End-to-end proof that both transcripts stay anchored to their newest row.
 *
 * The thread panel had no scroll machinery at all: it opened at the very
 * beginning, every thread and every visit. The channel timeline had four
 * anchoring rules, each added by a separate issue, and the fix lifts all four
 * into a hook both panes use.
 *
 * # Why this is an e2e spec
 *
 * Only the predicate — how close to the bottom still counts as the bottom — is
 * pure, and it is pinned in `test/unit/room-bottom-anchor.test.ts`. Everything
 * else here is a layout effect, two `ResizeObserver`s and a scroll handler
 * racing a transcript that grows underneath them. None of that exists without a
 * document, and the vitest runner is `environment: "node"` and collects
 * `test/unit/**` only — a component test could not even be selected.
 *
 * The channel walk at the end is not redundant with the thread one. The rules
 * were moved out of the channel timeline to get here, and a move that changes
 * behaviour is exactly what a second reader of the same hook cannot notice.
 *
 * # Why every test parks the pane by hand before asserting
 *
 * The growth rule animates, and a burst of sends that lands mid-animation
 * leaves the pane wherever the interrupted travel stopped — measured on
 * upstream `main` before this change, in the channel, and unchanged by it. So
 * building a transcript is setup, not an assertion: each test scripts the
 * scroller to the bottom once the rows are in place, and the assertions are
 * about what happens *from* there. Growth-following is asserted where it is
 * actually reliable — one row arriving into a settled pane.
 */

const ENGINEERING = "engineering";
const CONTENT = "content";

/**
 * A short viewport, so a handful of rows already overflows a pane. Anchoring is
 * only observable on a transcript taller than its box, and waiting for one to
 * accumulate naturally is minutes of sending.
 */
test.use({ viewport: { width: 1280, height: 520 } });

test.beforeEach(async ({ page }) => {
  // The first-run tour renders a modal over the console and swallows every
  // click beneath it — the pattern every chat spec here uses.
  await page.addInitScript(() => {
    const real = Storage.prototype.getItem;
    Storage.prototype.getItem = function getItem(key: string) {
      return key.startsWith("oc-tour:") ? '{"skipped":true}' : real.call(this, key);
    };
  });
});

/**
 * The offline echo brain answers every message with `You said: <text>`, which
 * is how a thread reaches a useful length in one test rather than in minutes.
 * The live-brain lane's mock answers something else deliberately, so the waits
 * below can never be satisfied there — the same guard four other chat specs
 * carry, for the same reason.
 */
const NEEDS_ECHO_BRAIN =
  "builds its transcript from the offline echo brain's `You said: <text>` replies.";

/** The slack the anchoring rules themselves use. */
const SLACK = 32;

function panel(page: Page): Locator {
  return page.locator("aside").filter({ has: page.getByRole("heading", { name: "Thread" }) });
}

function threadTranscript(page: Page): Locator {
  return page.getByTestId("thread-transcript");
}

function channelTranscript(page: Page): Locator {
  return page.getByTestId("channel-transcript");
}

/** How far from the bottom a scroller is parked, in CSS pixels. */
function fromBottom(scroller: Locator): Promise<number> {
  return scroller.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight);
}

/** How much of the transcript does not fit in its box. */
function overflow(scroller: Locator): Promise<number> {
  return scroller.evaluate((el) => el.scrollHeight - el.clientHeight);
}

/** Long enough for an in-flight smooth scroll to have finished travelling. */
const SETTLE_MS = 400;

function pause(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Parked at the bottom, and still there a moment later.
 *
 * Two readings rather than one, because the growth rule animates towards a
 * pixel offset captured when it fired: a single reading can catch that travel
 * passing through the bottom on its way to somewhere the rows that arrived
 * since have moved. What the assertion means is "settled at the newest row",
 * and one sample cannot say that.
 */
async function expectAnchored(scroller: Locator, why: string) {
  await expect
    .poll(
      async () => {
        const first = await fromBottom(scroller);
        await pause(SETTLE_MS);
        return Math.max(first, await fromBottom(scroller));
      },
      { timeout: 20_000, message: why },
    )
    .toBeLessThanOrEqual(SLACK);
}

/**
 * The assertions above mean nothing against a transcript that fits. Every test
 * that asks about a scroll position asks this first.
 */
async function expectOverflowing(scroller: Locator, why: string) {
  await expect
    .poll(() => overflow(scroller), { timeout: 15_000, message: why })
    .toBeGreaterThan(2 * SLACK);
}

/**
 * Setup, not an assertion: leaves the pane parked at the bottom and following.
 *
 * Repeated until it stays there, because the growth rule's own animation may
 * still be travelling to an offset the rows that arrived since have made
 * meaningless — which is the pixel-offset trap that rule's comment describes,
 * and which would otherwise drift the pane back up under the test.
 */
async function parkAtBottom(scroller: Locator) {
  await expect
    .poll(
      async () => {
        await scroller.evaluate((el) => {
          el.scrollTop = el.scrollHeight;
        });
        await pause(SETTLE_MS);
        const first = await fromBottom(scroller);
        await pause(SETTLE_MS);
        return Math.max(first, await fromBottom(scroller));
      },
      { timeout: 20_000 },
    )
    .toBeLessThanOrEqual(SLACK);
}

/** Parks a scroller at the very top and waits for the handler to see it. */
async function scrollToTop(scroller: Locator) {
  await scroller.evaluate((el) => {
    el.scrollTop = 0;
  });
  await expect.poll(() => scroller.evaluate((el) => el.scrollTop), { timeout: 15_000 }).toBe(0);
}

/** Sends one line into the open channel and waits for the echoed answer. */
async function sayInChannel(page: Page, text: string) {
  await page.getByPlaceholder(/^Message /).fill(text);
  await page.getByPlaceholder(/^Message /).press("Enter");
  await expect(bubbles(page).filter({ hasText: `You said: ${text}` }).first()).toBeVisible({
    timeout: 30_000,
  });
}

/** The operator's own bubble carrying `marker`, never the reply quoting it. */
function ownBubble(page: Page, marker: string): Locator {
  return bubbles(page).filter({ hasText: marker }).filter({ hasNotText: "You said:" }).first();
}

/** Opens the thread hanging off the operator's own `marker` bubble. */
async function openThreadOn(page: Page, marker: string) {
  const row = ownBubble(page, marker);
  await expect(row).toBeVisible({ timeout: 30_000 });
  await row.hover();
  const reply = row.getByRole("button", { name: "Reply in thread" });
  await expect(reply).toBeEnabled({ timeout: 30_000 });
  await reply.click();
  await expect(panel(page)).toBeVisible();
}

/** Sends one reply into the open thread and waits for the echoed answer. */
async function replyInThread(page: Page, text: string) {
  const thread = panel(page);
  await thread.getByPlaceholder("Reply…").fill(text);
  await thread.getByPlaceholder("Reply…").press("Enter");
  await expect(thread.getByText(`You said: ${text}`, { exact: true })).toBeVisible({
    timeout: 30_000,
  });
}

/**
 * A thread off a fresh channel message, long enough to overflow the panel, left
 * parked at its newest reply.
 *
 * Returns the parent's marker, which is also how its bubble is found again
 * after a reload or after switching to another thread.
 */
async function seedThread(page: Page, label: string, replies: number): Promise<string> {
  const marker = `${label}-${Date.now()}`;
  await sayInChannel(page, marker);
  await openThreadOn(page, marker);
  for (let i = 0; i < replies; i += 1) {
    await replyInThread(page, `${marker} reply ${i}`);
  }
  await expectOverflowing(threadTranscript(page), "the seeded thread must overflow its panel");
  await parkAtBottom(threadTranscript(page));
  return marker;
}

test.describe("thread panel", () => {
  test.skip(LIVE_BRAIN, NEEDS_ECHO_BRAIN);

  test("opens at the newest reply, not at the beginning", async ({ page }) => {
    await openChannel(page, ENGINEERING);
    const marker = await seedThread(page, "anchor-open", 4);

    // The reported journey: read to the end, leave, come back.
    await panel(page).getByRole("button", { name: "Close thread" }).click();
    await expect(panel(page)).toBeHidden();
    await openThreadOn(page, marker);

    await expectOverflowing(threadTranscript(page), "still a transcript taller than its box");
    await expectAnchored(threadTranscript(page), "a reopened thread opens at its newest reply");
  });

  test("re-anchors when the history it was opened over finally lands", async ({ page }) => {
    // The thread pane's version of the channel's own worst case. A panel
    // that anchors once, against a one-screen box, before the rows it was
    // meant to anchor to exist — and never runs again — is this bug's own
    // historical failure mode shipped as its fix, and keying the arrival rule
    // on the parent id alone is exactly how it comes back.
    //
    // Reaching that window needs the console to hold a message the host has not
    // confirmed yet: the channel's history is held open on a latch, a line is
    // sent into the unhydrated channel, and its thread is opened on the local
    // row. Everything below then happens while the console still calls the
    // transcript pending — which is why the growth rule stays out of it, and
    // why the arrival rule has to run again when the latch opens.
    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    let asked = false;
    await page.route(
      (url) => url.pathname.endsWith("/chat/history") && url.searchParams.get("desk") === ENGINEERING,
      async (route) => {
        // Fetched now, delivered later: the host's real answer, held rather
        // than replaced, so nothing here depends on a hand-written payload.
        const answer = await route.fetch();
        const body = await answer.text();
        asked = true;
        await held;
        await route.fulfill({ status: answer.status(), contentType: "application/json", body });
      },
    );

    await openChannel(page, ENGINEERING);
    await expect.poll(() => asked, { timeout: 30_000 }).toBe(true);

    const marker = `anchor-pending-${Date.now()}`;
    await sayInChannel(page, marker);
    await openThreadOn(page, marker);
    for (let i = 0; i < 4; i += 1) {
      await replyInThread(page, `${marker} reply ${i}`);
    }

    const transcript = threadTranscript(page);
    await expectOverflowing(transcript, "the thread must overflow while its channel is pending");
    // Left at the top deliberately: whatever the panel did on the way here, the
    // question is whether the history landing re-anchors it.
    await scrollToTop(transcript);

    release();

    await expectAnchored(transcript, "the arrival rule runs again when the history lands");
  });

  test("switching to a thread of the same length does not inherit its offset", async ({ page }) => {
    await openChannel(page, ENGINEERING);
    // Equal reply counts on purpose: a rule keyed on the count would not fire
    // for this switch at all, and neighbouring threads of one to three replies
    // are the common case.
    const first = await seedThread(page, "anchor-switch-a", 3);
    await panel(page).getByRole("button", { name: "Close thread" }).click();
    const second = await seedThread(page, "anchor-switch-b", 3);

    await openThreadOn(page, first);
    await expectOverflowing(threadTranscript(page), "the first thread must overflow");
    await scrollToTop(threadTranscript(page));

    // Swaps the parent on the same mounted panel — no unmount to reset it.
    await openThreadOn(page, second);
    await expect(panel(page).getByText(`You said: ${second} reply 2`, { exact: true })).toBeVisible();
    await expectOverflowing(threadTranscript(page), "the second thread must overflow");
    await expectAnchored(threadTranscript(page), "the second thread opens at its own newest reply");
  });

  test("a reply arriving while the reader is scrolled up does not move them", async ({ page }) => {
    await openChannel(page, ENGINEERING);
    const marker = await seedThread(page, "anchor-hold", 4);

    const transcript = threadTranscript(page);
    await scrollToTop(transcript);
    const before = await transcript.evaluate((el) => el.scrollTop);

    await replyInThread(page, `${marker} while reading`);

    // Not merely "did not jump to the bottom": the position must be the one
    // they left, which is what the following gate is for.
    await expect
      .poll(() => transcript.evaluate((el) => el.scrollTop), { timeout: 5_000 })
      .toBe(before);
    expect(await fromBottom(transcript), "still parked away from the bottom").toBeGreaterThan(SLACK);
  });

  test("offers a jump to the end, which lands and then resumes following", async ({ page }) => {
    await openChannel(page, ENGINEERING);
    const marker = await seedThread(page, "anchor-jump", 4);

    const transcript = threadTranscript(page);
    const jump = panel(page).getByTestId("jump-to-latest");
    await expect(jump, "nothing to offer while parked at the bottom").toBeHidden();

    await scrollToTop(transcript);
    await expect(jump, "offered once the reader has scrolled away").toBeVisible();

    await jump.click();
    await expectAnchored(transcript, "the jump lands at the newest reply");
    await expect(jump, "and the offer withdraws once it has been taken").toBeHidden();

    // The click resumes following, so the next reply arriving is followed
    // rather than leaving the reader behind again.
    await replyInThread(page, `${marker} after the jump`);
    await expectAnchored(transcript, "growth is followed again after the jump");
  });

  test("a multi-line draft does not slide the transcript behind the composer", async ({ page }) => {
    await openChannel(page, ENGINEERING);
    await seedThread(page, "anchor-draft", 4);

    const transcript = threadTranscript(page);
    // The precondition rule 3 is about: a reader parked at the newest reply,
    // who then starts typing. Stated rather than inherited, so the assertion
    // below is about the composer and not about how the seeding settled.
    await parkAtBottom(transcript);

    // The composer grows with the draft and takes its height out of the
    // scroller's box. `scrollTop` is untouched by that, so without the rule
    // watching the box the transcript slides up behind it.
    const box = await transcript.evaluate((el) => el.clientHeight);
    await panel(page).getByPlaceholder("Reply…").fill("one\ntwo\nthree\nfour\nfive");

    // The shrink is asserted, not assumed: a draft that did not grow the
    // composer would leave nothing for the rule to react to, and every
    // assertion after it would hold for the wrong reason.
    await expect
      .poll(() => transcript.evaluate((el) => el.clientHeight), { timeout: 15_000 })
      .toBeLessThan(box);
    await expectAnchored(transcript, "the transcript follows its own box shrinking");
  });
});

test.describe("channel timeline", () => {
  test.skip(LIVE_BRAIN, NEEDS_ECHO_BRAIN);

  /**
   * The same walk against the pane the rules came from. This is the regression
   * net for the extraction: the thread panel's tests all pass against a hook
   * that quietly changed what it does to a channel.
   */
  test("anchors, holds, offers a jump, and survives a growing composer", async ({ page }) => {
    await openChannel(page, ENGINEERING);

    const transcript = channelTranscript(page);
    const marker = `channel-anchor-${Date.now()}`;
    for (let i = 0; i < 4; i += 1) {
      await sayInChannel(page, `${marker} ${i}`);
    }
    await expectOverflowing(transcript, "the channel must overflow for its position to mean anything");
    await parkAtBottom(transcript);

    const jump = page.getByTestId("jump-to-latest");
    await expect(jump, "nothing to offer while parked at the bottom").toBeHidden();

    await scrollToTop(transcript);
    await expect(jump, "offered once the reader has scrolled away").toBeVisible();
    const parked = await transcript.evaluate((el) => el.scrollTop);

    await sayInChannel(page, `${marker} while reading`);
    await expect
      .poll(() => transcript.evaluate((el) => el.scrollTop), { timeout: 5_000 })
      .toBe(parked);

    await jump.click();
    await expectAnchored(transcript, "the jump lands at the newest message");
    await expect(jump).toBeHidden();

    await sayInChannel(page, `${marker} after the jump`);
    await expectAnchored(transcript, "growth is followed again after the jump");

    await parkAtBottom(transcript);
    const box = await transcript.evaluate((el) => el.clientHeight);
    const draft = page.getByPlaceholder(/^Message /);
    await draft.fill("one\ntwo\nthree\nfour\nfive");
    await expect
      .poll(() => transcript.evaluate((el) => el.clientHeight), { timeout: 15_000 })
      .toBeLessThan(box);
    await expectAnchored(transcript, "the transcript follows its own box shrinking");
    await draft.fill("");

    // Arriving at a channel anchors before paint — the rule the extraction was
    // most at risk of dropping.
    await openChannel(page, CONTENT);
    await openChannel(page, ENGINEERING);
    await expectOverflowing(transcript, "the channel is still taller than its box");
    await expectAnchored(transcript, "reopening a channel lands at its newest message");
  });
});
