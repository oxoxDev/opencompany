import { expect, test } from "@playwright/test";

/**
 * However the sidebar is hidden, there is always a way back to it.
 *
 * Two shapes, because the sidebar has two. On mobile it is a sheet that closes
 * entirely, taking its own controls with it, so the way back is a button fixed
 * to the viewport. On desktop it collapses to a 3rem icon rail that keeps its
 * header, so the way back is the same control that put it there.
 *
 * The desktop half also pins WHERE that control lives (issue #1177). It used to
 * be a full-width row directly above Overview — the nav row shape exactly, for
 * something that is not a destination — and the fix is a header button. The
 * assertion that it is inside `sidebar-header` and absent from `sidebar-content`
 * is what stops it drifting back into the list, and it is deliberately paired
 * with the reachability claims rather than filed on its own: a control that is
 * in the right place but unreachable, or reachable but nameless, is the same
 * bug in a different coat.
 */

/** The tour can cover the fixed trigger while it is showing. */
async function dismissTour(page: import("@playwright/test").Page) {
  const skip = page.getByRole("button", { name: "Skip for now" });
  try {
    await skip.waitFor({ state: "visible", timeout: 10_000 });
    await skip.click();
  } catch {
    // The signed-in browser profile may already have completed the tour.
  }
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
