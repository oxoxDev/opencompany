import { expect, test, type Browser, type Page } from "@playwright/test";

/**
 * Regression proof for #1332 — the sign-in composition must sit in the middle
 * of the space below the header, not pinned under it.
 *
 * `main` already said `justify-center`, and had since the view was written. It
 * was inert: `justify-center` distributes free space along its own main axis,
 * and `main`'s parent was a plain block box, so `main` was exactly as tall as
 * its content and had no free space to distribute. Measured at 1440x900 before
 * the fix, `main` was 402px tall against the 835px the header left over — the
 * card ended around y=465 with the remaining half of the page empty below it.
 *
 * The assertions are geometric on purpose. The markup *read* as centred
 * throughout, which is why this survived review and would survive any check on
 * class names: only a real layout distinguishes a stated intent from an
 * achieved one. Anything that re-breaks the chain — the height moved back onto
 * a non-flex ancestor, `flex-1` dropped from `main` — fails here whatever the
 * classes claim.
 *
 * Every test signs itself out. The suite's shared storage state is an
 * authenticated admin, who never sees this screen.
 */

/**
 * Vertical extents of `main`'s content, in CSS pixels, plus what bounds them.
 *
 * Says nothing about readiness — `none` mode renders no "Sign in" heading, so
 * each caller waits for the thing its own mode puts on screen.
 */
async function measure(page: Page) {
  return page.evaluate(() => {
    const main = document.querySelector("main")!;
    const box = main.getBoundingClientRect();
    return {
      viewportHeight: window.innerHeight,
      scrollHeight: document.scrollingElement!.scrollHeight,
      headerBottom: document.querySelector("header")!.getBoundingClientRect().bottom,
      mainTop: box.top,
      mainHeight: box.height,
      // The composition, not the box: `main` carries `py-16`, so its own edges
      // move with the padding while the content is what the eye centres on.
      contentTop: main.firstElementChild!.getBoundingClientRect().top,
      contentBottom: main.lastElementChild!.getBoundingClientRect().bottom,
    };
  });
}

/**
 * A signed-out page at `size`, parked on the sign-in screen of a host that
 * mails links.
 *
 * The harness host binds loopback with no mail transport, and such a host
 * draws the password form alone. The heights walked below are the mailed-link
 * screen's, so the host's answer is stubbed to say it mails; `auth/request`
 * still answers the real host, which acknowledges a stranger like a member.
 */
async function signedOut(browser: Browser, size: { width: number; height: number }) {
  const context = await browser.newContext({ storageState: undefined, viewport: size });
  const page = await context.newPage();
  await page.route("**/auth/config", (route) =>
    route.fulfill({ json: { mode: "email", passwords: true, magicLink: true, claimable: false } }),
  );
  await page.goto("/");
  return { context, page };
}

for (const size of [
  { width: 1440, height: 900 },
  { width: 1100, height: 900 },
]) {
  test(`the sign-in composition is centred below the header at ${size.width}x${size.height}`, async ({
    browser,
  }) => {
    const { context, page } = await signedOut(browser, size);
    try {
      await expect(page.getByRole("heading", { name: /^Sign in/ })).toBeVisible();
      const box = await measure(page);

      // `main` owns every pixel the header left over — the precondition the
      // inert `justify-center` never had.
      expect(box.mainTop).toBeCloseTo(box.headerBottom, 0);
      expect(box.mainHeight).toBeCloseTo(box.viewportHeight - box.headerBottom, 0);

      // ...and it spends them evenly, so the content's midpoint lands on the
      // midpoint of the region below the header. Before the fix that was off by
      // hundreds of pixels; 2px is the odd-pixel rounding of a half split.
      const contentMid = (box.contentTop + box.contentBottom) / 2;
      const regionMid = (box.headerBottom + box.viewportHeight) / 2;
      expect(Math.abs(contentMid - regionMid)).toBeLessThanOrEqual(2);

      // Centring against that region rather than the whole viewport offsets the
      // composition downward by exactly half the header — tens of pixels, which
      // still reads as centred. Anything larger means it has drifted again.
      expect(Math.abs(contentMid - box.viewportHeight / 2)).toBeLessThanOrEqual(
        box.headerBottom / 2 + 1,
      );

      // Nothing spills: a viewport this tall has room to spare.
      expect(box.scrollHeight).toBeLessThanOrEqual(box.viewportHeight + 1);
    } finally {
      await context.close();
    }
  });
}

/**
 * Centring only reads as deliberate if it survives the screen changing height,
 * and this one has modes of quite different heights. Each is re-measured rather
 * than assumed: what this pins is that no mode escapes the flex column.
 *
 * This walks the `email` host's own heights; the other two modes are below.
 */
test("centring holds as the card changes height", async ({ browser }) => {
  const { context, page } = await signedOut(browser, { width: 1440, height: 900 });
  try {
    const centred = async (mode: string) => {
      await expect(page.getByRole("heading", { name: /^Sign in/ })).toBeVisible();
      const box = await measure(page);
      const contentMid = (box.contentTop + box.contentBottom) / 2;
      const regionMid = (box.headerBottom + box.viewportHeight) / 2;
      expect(Math.abs(contentMid - regionMid), `${mode} is not centred`).toBeLessThanOrEqual(2);
    };

    await centred("magic link");

    await page.getByRole("button", { name: "Use a password instead" }).click();
    await expect(page.getByLabel("Password")).toBeVisible();
    await centred("password");

    await page.getByRole("button", { name: "Email me a link instead" }).click();
    await page.getByLabel("Email").fill("centring-1332@example.test");
    await page.getByRole("button", { name: "Email me a link" }).click();
    await expect(page.getByText("Check your email")).toBeVisible();
    await centred("check your email");
  } finally {
    await context.close();
  }
});

/**
 * The other two sign-in modes, which are the same `main` with a different card
 * in it — and which no single host renders, because `auth/config` is what picks
 * between them and a host answers one way for its whole life.
 *
 * So the host's answer is what gets stubbed, not the layout: the console then
 * builds the real card for that mode, and the geometry is measured exactly as
 * above. `wallet` here is its no-wallet-installed variant, headless Chromium
 * having no wallet to find — the shortest card this screen has, and therefore
 * the one with the most free space to get wrong.
 */
for (const mode of ["wallet", "none"] as const) {
  const marker = mode === "wallet" ? "No wallet found" : "There is no sign-in here";

  test(`the ${mode}-mode card is centred below the header`, async ({ browser }) => {
    const context = await browser.newContext({
      storageState: undefined,
      viewport: { width: 1440, height: 900 },
    });
    try {
      const page = await context.newPage();
      await page.route("**/auth/config", (route) =>
        route.fulfill({ json: { mode, passwords: false, magicLink: false, claimable: false } }),
      );
      await page.goto("/");
      await expect(page.getByText(marker)).toBeVisible();

      const box = await measure(page);
      const contentMid = (box.contentTop + box.contentBottom) / 2;
      const regionMid = (box.headerBottom + box.viewportHeight) / 2;

      expect(box.mainHeight).toBeCloseTo(box.viewportHeight - box.headerBottom, 0);
      expect(Math.abs(contentMid - regionMid)).toBeLessThanOrEqual(2);
    } finally {
      await context.close();
    }
  });
}

test("a viewport too short for the card scrolls rather than clipping it", async ({ browser }) => {
  // The failure mode centring invites: a flex item centred in a box smaller
  // than its content overflows *both* ends, and the top half becomes
  // unreachable — nothing scrolls above the origin. `min-height: auto` on the
  // item is what prevents it, and this is the test that says so.
  const { context, page } = await signedOut(browser, { width: 1440, height: 360 });
  try {
    // The tallest variant a harness can reach: the "Check your email" state.
    await page.getByLabel("Email").fill("centring-1332@example.test");
    await page.getByRole("button", { name: "Email me a link" }).click();
    await expect(page.getByText("Check your email")).toBeVisible();

    const box = await page.evaluate(() => {
      const main = document.querySelector("main")!;
      return {
        viewportHeight: window.innerHeight,
        scrollHeight: document.scrollingElement!.scrollHeight,
        headerBottom: document.querySelector("header")!.getBoundingClientRect().bottom,
        mainTop: main.getBoundingClientRect().top,
        contentTop: main.firstElementChild!.getBoundingClientRect().top,
      };
    });

    // Taller than the viewport, so the whole card is reachable by scrolling...
    expect(box.scrollHeight).toBeGreaterThan(box.viewportHeight);
    // ...and none of it sits above the header, where no scrolling could reach it.
    expect(box.mainTop).toBeGreaterThanOrEqual(box.headerBottom - 1);
    expect(box.contentTop).toBeGreaterThanOrEqual(box.headerBottom - 1);
  } finally {
    await context.close();
  }
});
