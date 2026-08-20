import { expect, test } from "@playwright/test";

/**
 * However the sidebar is hidden, there is always a way back to it.
 *
 * Two shapes, because the sidebar has two. On mobile it is a sheet that closes
 * entirely, taking its own controls with it, so the way back is a button
 * docked in its own chrome bar below the page. On desktop it collapses to a
 * 3rem icon rail that keeps its header, so the way back is the same control
 * that put it there.
 *
 * The desktop half also pins WHERE that control lives (issue #1177). It used to
 * be a full-width row directly above Overview — the nav row shape exactly, for
 * something that is not a destination — and the fix is a header button. The
 * assertion that it is inside `sidebar-header` and absent from `sidebar-content`
 * is what stops it drifting back into the list, and it is deliberately paired
 * with the reachability claims rather than filed on its own: a control that is
 * in the right place but unreachable, or reachable but nameless, is the same
 * bug in a different coat.
 *
 * The mobile half used to be `position: fixed`, floating over whatever content
 * happened to scroll into the same bottom-left corner and winning every
 * hit-test there (issue #1265). It is now a normal-flow bar that reserves its
 * own row instead of overlaying one, which is what the overlap test below
 * pins down.
 */

/** The tour can cover the fixed trigger while it is showing. */
async function dismissTour(page: import("@playwright/test").Page) {
  const skip = page.getByRole("button", { name: "Skip for now" });
  try {
    await skip.waitFor({ state: "visible", timeout: 10_000 });
  } catch {
    // The signed-in browser profile may already have completed the tour.
    return;
  }
  await skip.click();
  // The welcome dialog's backdrop is `fixed inset-0`, so it covers the WHOLE
  // viewport — not just the card it frames. Base UI runs a close animation
  // before unmounting it (`data-closed` + `data-ending-style`, `duration-100`),
  // and a click resolving does not wait for that: the backdrop is still in the
  // DOM, still hit-testable, for up to ~100ms after "Skip for now" is clicked.
  // A later `elementFromPoint` call anywhere on screen — including at a target
  // scrolled to the bottom of an unrelated page — can land on that fading
  // backdrop instead of the real content under it. Wait for the overlay itself
  // to detach, not just for the click to resolve.
  await expect(page.locator('[data-slot="dialog-overlay"]')).toHaveCount(0);
}

/** `--sidebar-width-icon`, in px. The whole width the collapsed control has. */
const RAIL_WIDTH = 48;

test.describe("sidebar toggle reachability", () => {
  test("the mobile sheet has an in-viewport way back", async ({ page }) => {
    await page.setViewportSize({ width: 700, height: 800 });
    await page.goto("/#/overview");
    await dismissTour(page);

    const trigger = page.getByRole("button", { name: "Toggle sidebar" });
    await expect(trigger).toBeInViewport();
    await trigger.click();
    await expect(page.getByText("Workflows", { exact: true })).toBeVisible();
  });

  test("the mobile trigger does not overlap scrollable page content", async ({ page }) => {
    // The issue's own repro viewport (iPhone 12-class).
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto("/#/settings/general");
    await dismissTour(page);

    // Settings' General page is a single `flex-1 overflow-y-auto` column
    // (`SettingsView.tsx`) ending in a "Something off?" card — scrolling it to
    // the bottom is what used to land that card's button under the fixed
    // corner.
    const flagButton = page.getByRole("button", { name: "Flag something" });
    await flagButton.scrollIntoViewIfNeeded();

    const trigger = page.getByRole("button", { name: "Toggle sidebar" });
    await expect(trigger).toBeInViewport();

    const triggerBox = await trigger.boundingBox();
    const flagBox = await flagButton.boundingBox();
    expect(triggerBox, "the trigger should have a box").not.toBeNull();
    expect(flagBox, "the flag button should have a box").not.toBeNull();

    // No shared pixels in either axis: the trigger's row is reserved chrome,
    // not an overlay, so scrolled-to-the-end content and the trigger cannot
    // occupy the same screen space.
    const overlapsX = triggerBox!.x < flagBox!.x + flagBox!.width && flagBox!.x < triggerBox!.x + triggerBox!.width;
    const overlapsY = triggerBox!.y < flagBox!.y + flagBox!.height && flagBox!.y < triggerBox!.y + triggerBox!.height;
    expect(overlapsX && overlapsY, "the trigger and the scrolled-to content must not overlap").toBe(
      false,
    );

    // And the corner it used to cover hit-tests as the content now, not the
    // trigger — the concrete symptom from the issue's repro. Assert the hit
    // POSITIVELY resolves to the flag button, not just that it misses the
    // trigger: a hit-test landing on neither would satisfy the weaker check.
    const flagCenterX = flagBox!.x + flagBox!.width / 2;
    const flagCenterY = flagBox!.y + flagBox!.height / 2;
    const hit = await page.evaluate(
      ([x, y]) => {
        const el = document.elementFromPoint(x, y);
        return el instanceof Element ? (el.closest("button")?.textContent?.trim() ?? null) : null;
      },
      [flagCenterX, flagCenterY],
    );
    expect(hit, "the flag button's own point hits the flag button").toBe("Flag something");

    // Still reachable and still functional in its own right.
    await trigger.click();
    await expect(page.getByText("Workflows", { exact: true })).toBeVisible();
  });

  test("the inline sidebar's collapse control is a named, keyboard-operable header button", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1024, height: 800 });
    await page.goto("/#/overview");
    await dismissTour(page);

    const sidebar = page.locator("[data-slot=sidebar]");
    const toggle = page.getByRole("button", { name: "Collapse sidebar", exact: true });

    // Named and on screen. The name is the assertion as much as the position
    // is: this control is icon-only, so an `aria-label` lost in a refactor
    // leaves a button a screen reader announces as "button".
    await expect(toggle).toBeInViewport();

    // Chrome, not a destination. It lives in the header with the host switcher
    // and is nowhere in the nav list.
    await expect(
      page.locator("[data-slot=sidebar-header]").getByTestId("sidebar-collapse"),
      "the collapse control belongs to the sidebar's header",
    ).toHaveCount(1);
    await expect(
      page.locator("[data-slot=sidebar-content]").getByTestId("sidebar-collapse"),
      "…and never among the nav rows, which is what issue #1177 was",
    ).toHaveCount(0);

    // Beside the switcher, not on top of it, and not a second half of it: the
    // header holds two separate controls and the boxes must not overlap.
    const switcherBox = await page.getByTestId("host-switcher").boundingBox();
    const toggleBox = await toggle.boundingBox();
    expect(switcherBox, "the host switcher should have a box").not.toBeNull();
    expect(toggleBox, "the collapse control should have a box").not.toBeNull();
    expect(
      toggleBox!.x,
      "the collapse button stands clear of the host switcher's trailing edge",
    ).toBeGreaterThanOrEqual(switcherBox!.x + switcherBox!.width);

    // Operable from the keyboard, not just under a pointer. An icon-only
    // button is exactly the kind that gets rebuilt as a `div` with an
    // `onClick` and silently stops being reachable.
    await toggle.focus();
    await expect(toggle).toBeFocused();
    await page.keyboard.press("Enter");

    // It survives the state it just produced. This is the case most likely to
    // be got wrong: the control is now inside a 3rem column.
    await expect(sidebar).toHaveAttribute("data-state", "collapsed");
    const expand = page.getByRole("button", { name: "Expand sidebar", exact: true });
    await expect(expand).toBeVisible();
    await expect(expand).toBeInViewport();

    // `data-state` flips on the click; the column takes `duration-200` to get
    // there. Poll rather than sample, or this measures a sidebar caught half
    // way and reports a button that fits as one that does not.
    await expect
      .poll(
        async () => (await page.locator("[data-slot=sidebar-container]").boundingBox())?.width,
        { message: "the collapsed column settles at the icon rail's width" },
      )
      .toBe(RAIL_WIDTH);

    const railBox = await expand.boundingBox();
    expect(railBox, "the collapsed control should have a box").not.toBeNull();
    expect(railBox!.x, "…inside the rail, not hanging off its left edge").toBeGreaterThanOrEqual(0);
    expect(
      railBox!.x + railBox!.width,
      "…and inside the rail, not overflowing its right edge",
    ).toBeLessThanOrEqual(RAIL_WIDTH);

    // And back, from the keyboard, to where it started.
    await expand.focus();
    await expect(expand).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(sidebar).toHaveAttribute("data-state", "expanded");
    await expect(page.getByRole("button", { name: "Collapse sidebar", exact: true })).toBeVisible();
  });
});
