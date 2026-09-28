import { expect, test, type Page } from "@playwright/test";

import { LIVE_BRAIN } from "./capabilities";
import { openChannel, workingRow } from "./chat-helpers";

/**
 * Two questions in one channel, each deliberating in its own room.
 *
 * This is the shape the whole per-query keying exists for, and the one no unit
 * test can reach: it is about which rows reach which surface, across two turns
 * that share a channel, a desk and — for part of their lives — a responder.
 *
 * A hive episode makes the collision concrete. Inside one episode `messageSeq`
 * is **constant** (it is the trigger the room was convened on) while `agentId`
 * **varies** per seat, so a room of three produces a long run of frames that
 * agree on the query and disagree on the agent. Two rooms running at once in
 * one channel therefore differ *only* by `messageSeq` — and before that field
 * the console had one row-list per thread, so the second question did not merely
 * merge with the first, it cleared it.
 *
 * What these prove:
 *
 * 1. the two episodes' rows never merge — the foot pair shows one room's work,
 *    never a pile of both;
 * 2. the pair belongs to the **latest** query, so the newer room owns the anchor
 *    while the older one keeps its own rows off-screen rather than losing them;
 * 3. the line follows the floor *within* an episode as seats take turns.
 *
 * Like `chat-live-events.spec.ts`'s synthetic fixture — whose interception
 * pattern this borrows — these write their own SSE stream and their own
 * history. The offline brain this suite runs against calls no tools and
 * convenes no rooms, so there is no live episode to watch without inventing
 * one, and what is under test is the console's routing rather than the host's.
 * The frames are the exact shape `turn_stream.rs` puts on the wire.
 *
 * Like the rest of `test/e2e` this needs a running host and is not a CI gate —
 * the Playwright config declares no `webServer`.
 */

/**
 * The desk these run against. Its id is its channel id.
 *
 * Defaults to the harness manifest's engineering desk, like every sibling
 * spec. Overridable because these mock `chat/history` and the whole event
 * stream — the only thing they need from the host is a desk that exists, so
 * pinning one company's manifest would make them unrunnable against any other
 * host for no reason the tests themselves care about.
 */
const DESK_ID = process.env.PW_CHAT_DESK ?? "engineering";
const ENGINEERING = { id: DESK_ID, channel: `${DESK_ID}-desk` };

/**
 * The two operator messages the rooms were convened on, by journal sequence.
 *
 * The console namespaces a host seq into `h<seq>` (`hostMessageId`), and keys
 * `liveStepsByMessage` on that — so these numbers are what tie a frame's
 * `messageSeq` to a bubble in the transcript below.
 */
const FIRST_QUERY = 101;
const SECOND_QUERY = 202;

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const real = Storage.prototype.getItem;
    Storage.prototype.getItem = function getItem(key: string) {
      return key.startsWith("oc-tour:") ? '{"skipped":true}' : real.call(this, key);
    };
  });
});

/**
 * One operator question, as `chat/history` returns it.
 *
 * The id is the **bare host sequence**, not the console's `h`-prefixed form:
 * `fromHistory` namespaces it on the way in (`hostMessageId(entry.id)`), so a
 * fixture that pre-namespaces produces `hh101` and its frames — keyed off
 * `messageSeq` through the same function — never find their message.
 */
function question(seq: number, text: string, atMillis: number) {
  return {
    id: String(seq),
    channel: ENGINEERING.id,
    author: "operator",
    text,
    atMillis,
    mine: true,
  };
}

/**
 * A seat taking the floor: a call that starts and does not finish.
 *
 * No `atMillis`. Verified against a live host: a turn frame carries
 * `type`/`seq`/`agentId`/`chatId`/`toolCallId`/`label`/`status`/`messageSeq`
 * and nothing else — the wall-clock fields belong to the durable projections,
 * not to the transient bus. A fixture that invents one is a fixture that can
 * drift from the wire without anything saying so.
 */
function seatWorking(seq: number, messageSeq: number, agentId: string, label: string) {
  return {
    type: "tool_call",
    seq,
    chatId: ENGINEERING.id,
    agentId,
    messageSeq,
    toolCallId: `t${seq}`,
    label,
  };
}

/** That seat handing back: the same call, settled. See {@link seatWorking}. */
function seatDone(seq: number, messageSeq: number, agentId: string, callSeq: number) {
  return {
    type: "tool_result",
    seq,
    chatId: ENGINEERING.id,
    agentId,
    messageSeq,
    toolCallId: `t${callSeq}`,
    status: "ok",
    elapsedMs: 40,
  };
}

/**
 * Opens the channel with `history` seeded and `frames` queued on the stream.
 *
 * The release dance is load-bearing: Playwright counts a routed stream as
 * pending navigation work, so awaiting the navigation before releasing
 * deadlocks — while fulfilling immediately can deliver frames before the
 * channel map has mounted. The visible composer threads between the two.
 */
async function openWithEpisodes(page: Page, history: unknown[], frames: unknown[]) {
  await page.route("**/chat/history?*", (route) => {
    const desk = new URL(route.request().url()).searchParams.get("desk");
    if (desk !== ENGINEERING.id) return route.continue();
    return route.fulfill({
      status: 200,
      headers: { "content-type": "application/json" },
      body: JSON.stringify(history),
    });
  });

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

test("two rooms in one channel keep their rows apart", async ({ page }) => {
  test.skip(LIVE_BRAIN, "the default Console E2E lane covers the synthetic SSE rendering fixture");

  const t0 = Date.now();
  await openWithEpisodes(
    page,
    [
      question(FIRST_QUERY, "What did design ship this week?", t0),
      question(SECOND_QUERY, "And what is blocking the launch?", t0 + 10),
    ],
    [
      // Room one deliberates. Two seats, one query, still open at the end —
      // a room blocked on a peer emits nothing further, which is exactly the
      // turn the old per-thread reset used to erase.
      seatWorking(1, FIRST_QUERY, "a-ada", "design_review"),
      seatDone(2, FIRST_QUERY, "a-ada", 1),
      seatWorking(3, FIRST_QUERY, "a-grace", "changelog_read"),

      // Room two convenes on the SECOND question while the first is still
      // going. Same channel, same desk — only `messageSeq` tells them apart.
      seatWorking(4, SECOND_QUERY, "a-lin", "issue_search"),
      seatDone(5, SECOND_QUERY, "a-lin", 4),
      seatWorking(6, SECOND_QUERY, "a-moss", "dependency_graph"),
    ],
  );

  const live = workingRow(page);
  await expect(live).toBeVisible({ timeout: 30_000 });

  // The anchor belongs to the latest query, so it names room two's open seat.
  await expect(live).toContainText("dependency_graph", { timeout: 30_000 });

  // And it is one room's work, not both piled together. Room one's labels must
  // not appear on it — the merge this keying exists to prevent.
  await expect(live).not.toContainText("changelog_read");
  await expect(live).not.toContainText("design_review");
});

test("the line follows the floor inside one room", async ({ page }) => {
  test.skip(LIVE_BRAIN, "the default Console E2E lane covers the synthetic SSE rendering fixture");

  // A single episode, three seats in sequence. `messageSeq` never moves;
  // `agentId` does. The row must track the seat holding the floor rather than
  // latching whoever opened the round.
  const t0 = Date.now();
  await openWithEpisodes(
    page,
    [question(FIRST_QUERY, "Should we ship on Friday?", t0)],
    [
      seatWorking(1, FIRST_QUERY, "a-ada", "risk_register"),
      seatDone(2, FIRST_QUERY, "a-ada", 1),
      seatWorking(3, FIRST_QUERY, "a-grace", "release_notes"),
      seatDone(4, FIRST_QUERY, "a-grace", 3),
      seatWorking(5, FIRST_QUERY, "a-moss", "oncall_roster"),
    ],
  );

  const live = workingRow(page);
  await expect(live).toContainText("oncall_roster", { timeout: 30_000 });
  // The two settled seats are not the current activity and must not be named.
  await expect(live).not.toContainText("risk_register");
  await expect(live).not.toContainText("release_notes");
});

test("a room's rows stay beneath the lines it journals as it deliberates", async ({ page }) => {
  test.skip(LIVE_BRAIN, "the default Console E2E lane covers the synthetic SSE rendering fixture");

  // The ordering claim, in the setting that makes it bite. A room posts a line
  // per seat, so anchoring the pair to the asking message left the pulsing row
  // drifting further up the transcript with every turn — claiming work had
  // finished before every line beneath it.
  const t0 = Date.now();
  await openWithEpisodes(
    page,
    [question(FIRST_QUERY, "What did design ship this week?", t0)],
    [
      seatWorking(1, FIRST_QUERY, "a-ada", "design_review"),
      {
        type: "agent_reply",
        seq: 2,
        atMillis: t0 + 2,
        chatId: ENGINEERING.id,
        agentId: "a-ada",
        text: "Onboarding flow went out Tuesday.",
      },
      seatWorking(3, FIRST_QUERY, "a-grace", "changelog_read"),
      {
        type: "agent_reply",
        seq: 4,
        atMillis: t0 + 4,
        chatId: ENGINEERING.id,
        agentId: "a-grace",
        text: "Settings redesign landed too.",
      },
    ],
  );

  const live = workingRow(page);
  await expect(live).toBeVisible({ timeout: 30_000 });

  const lastSeatLine = page
    .locator("article[data-message-id]")
    .filter({ hasText: "Settings redesign landed too." });
  await expect(lastSeatLine).toBeVisible({ timeout: 30_000 });

  // Document order is the assertion. `compareDocumentPosition` returns
  // DOCUMENT_POSITION_FOLLOWING (4) when the argument comes after the node it
  // is called on — so the live row must follow the newest seat line.
  const liveFollowsLastLine = await lastSeatLine.evaluate(
    (node, liveEl) => Boolean(node.compareDocumentPosition(liveEl as Node) & 4),
    await live.elementHandle(),
  );
  expect(liveFollowsLastLine).toBe(true);
});
