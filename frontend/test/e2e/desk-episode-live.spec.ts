import { expect, test, type APIRequestContext, type Page } from "@playwright/test";

import { HIVE, HIVE_REASON, LIVE_BRAIN, LIVE_BRAIN_REASON } from "./capabilities";
import { openChannel, say, silenceTour, SCOPE } from "./orchestration";

/**
 * **A desk answering as a room, watched from the console.**
 *
 * The company under test is `companies/hive_demo`: two desks of two seats
 * each, sharing the CEO. The brain is the scripted mock, whose hive arm ends a
 * seat's turn with `post` on its first turn in the episode, `broadcast` (or a
 * `dm`, when the message carries `__MOCK_DM__ <agent>`) on its second, and
 * `complete_episode` from its third — so the shape of the episode is known
 * before it runs, and what this spec asserts is that the console **shows** it:
 *
 * 1. the round band appears with **two lanes working at once** — the one
 *    claim a relay race cannot satisfy, and the reason the band exists;
 * 2. the round the dm ran in shows a **dm chip** addressed to the seat named;
 * 3. the **completion marker** lands after the last round;
 * 4. `chat/history` carries the closing row with `episode.kind ===
 *    "complete_episode"`, so a reload rebuilds the same transcript.
 *
 * # What is not asserted
 *
 * Round count and who spoke first. The driver runs seats concurrently and
 * commits them in arrival order, so the transcript order inside a round is
 * the model's timing, not a contract. The mock is slowed with
 * `__MOCK_SLOW_MS__` precisely so the concurrent window is wide enough to be
 * observed, not so its order can be pinned.
 */

/** How long a two-seat, three-round episode may take end to end. */
const EPISODE_TIMEOUT = 180_000;

/** Every round band in the open channel. */
const bands = (page: Page) => page.getByTestId("round-band");

/**
 * Waits for one band whose lanes are both working — the concurrent window.
 *
 * Polled rather than asserted once: the two `turn_started` frames arrive
 * milliseconds apart, and a single read between them would see one lane.
 */
async function expectTwoLanesWorking(page: Page) {
  await expect
    .poll(
      async () => {
        const open = page.locator('[data-testid="round-band"][data-round-status="open"]');
        const count = await open.count();
        for (let i = 0; i < count; i += 1) {
          const working = await open
            .nth(i)
            .locator('[data-testid="round-seat"][data-seat-status="working"]')
            .count();
          if (working >= 2) return working;
        }
        return 0;
      },
      { timeout: 60_000, message: "two seats working at once in one round" },
    )
    .toBeGreaterThanOrEqual(2);
}

/** The closing row of the desk's newest episode, as `chat/history` returns it. */
async function completionRow(request: APIRequestContext, desk: string) {
  const history = await request.get(`${SCOPE}/chat/history?desk=${encodeURIComponent(desk)}&limit=200`);
  expect(history.ok()).toBe(true);
  const rows = (await history.json()) as {
    id: string;
    channel: string;
    episode?: { id: string; revision: number; kind: string; to?: string[] };
  }[];
  return rows.filter((row) => row.episode?.kind === "complete_episode").at(-1);
}

test("a two-seat desk answers as a room: two lanes at once, a dm, and a completion", async ({
  page,
  request,
}) => {
  test.skip(!LIVE_BRAIN, LIVE_BRAIN_REASON);
  test.skip(!HIVE, HIVE_REASON);
  test.setTimeout(EPISODE_TIMEOUT + 60_000);

  await silenceTour(page);
  await openChannel(page, "engineering");

  // Slowed so the concurrent window is observable; the dm directive makes the
  // round-1 chip deterministic. The marker keeps the message unique across
  // runs on the same data root.
  const stamp = Date.now();
  await say(page, `Plan the staging rollout __MOCK_SLOW_MS__ 2000 __MOCK_DM__ ceo hive-${stamp}`);

  // 1. Two lanes working in one round.
  await expect(bands(page).first()).toBeVisible({ timeout: 60_000 });
  await expectTwoLanesWorking(page);

  // 3. The episode completes, and its marker lands.
  const marker = page.getByTestId("episode-complete");
  await expect(marker.last()).toBeVisible({ timeout: EPISODE_TIMEOUT });
  const episodeId = await marker.last().getAttribute("data-episode-id");
  expect(episodeId).toBeTruthy();

  // Every band of that episode has committed; none is still running.
  const ofEpisode = page.locator(`[data-testid="round-band"][data-episode-id="${episodeId}"]`);
  await expect(ofEpisode.first()).toBeVisible();
  await expect(
    page.locator(`[data-testid="round-band"][data-episode-id="${episodeId}"][data-round-status="open"]`),
  ).toHaveCount(0);

  // 2. The dm chip, addressed to the seat the directive named. The engineer is
  // the seat that dms the CEO; the CEO's own round-1 act is a broadcast, since
  // a seat does not dm itself — either way exactly one dm chip lands. The CEO
  // agent carries no `name` in companies/hive_demo/agents/ceo.toml, so the
  // roster falls back to its `role`, "Chief Executive".
  const dmChip = ofEpisode.locator('[data-testid="utterance-chip"][data-kind="dm"]');
  await expect(dmChip.first()).toBeVisible();
  await expect(dmChip.first().getByTestId("utterance-audience")).toHaveText("Chief Executive");
  // The lead and the recipient are separate DOM nodes; a full-text check
  // guards against the two rendering with no space between them.
  await expect(dmChip.first()).toContainText("Sent to Chief Executive");

  // The chip's tooltip titles the round it belongs to, by revision, never by
  // episode id — `data-round-revision` is the same number the title derives.
  const dmRevision = await dmChip.first().getAttribute("data-round-revision");
  await expect(dmChip.first().locator("[title]").first()).toHaveAttribute(
    "title",
    `Round ${Number(dmRevision) + 1}`,
  );

  // And the closing chip on the row that ended it.
  await expect(
    ofEpisode.locator('[data-testid="utterance-chip"][data-kind="complete_episode"]').first(),
  ).toBeVisible();

  // 4. The durable record agrees with what was shown.
  await expect
    .poll(async () => (await completionRow(request, "engineering"))?.episode?.id ?? null, {
      timeout: 30_000,
      message: "chat/history carries the complete_episode row",
    })
    .toBe(episodeId);
});

test("a completed episode survives a reload as its marker, and draws no band", async ({ page }) => {
  test.skip(!LIVE_BRAIN, LIVE_BRAIN_REASON);
  test.skip(!HIVE, HIVE_REASON);

  await silenceTour(page);
  await openChannel(page, "engineering");
  // The previous test left at least one completed episode on this desk. A
  // fresh page has seen no frames, so everything it draws comes from
  // `chat/history`'s `episode` field — the reload path.
  await page.reload();
  await expect(page.getByPlaceholder(/^Message /)).toBeVisible({ timeout: 30_000 });
  // The marker is the reload path's evidence now: its round count is folded
  // from the same rebuilt episode the band used to be drawn from.
  const marker = page.getByTestId("episode-complete").first();
  await expect(marker).toBeVisible({ timeout: 30_000 });
  await expect(marker).toContainText(/\d+ round/);
  // And the band is a live instrument: a finished episode draws none, so
  // nothing on screen says "committed" about a desk that has stopped.
  await expect(bands(page)).toHaveCount(0);
});
