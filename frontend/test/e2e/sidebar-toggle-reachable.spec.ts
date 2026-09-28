import { expect, test } from "@playwright/test";

/**
 * However the sidebar is hidden, there is always a way back to it.
 *
 * Two shapes, because the sidebar has two. On mobile it is a sheet that closes
 * entirely, taking its own controls with it, so the way back is a button
 * docked in its own chrome bar below the page. On desktop it collapses to a
 * 3rem icon rail, and the way back is a control that never belonged to the
 * column in the first place.
 *
 * The desktop half also pins WHERE that control lives. It used to be a
 * full-width row directly above Overview — the nav row shape exactly, for
 * something that is not a destination (issue #1177) — then a button in the
 * sidebar's header, which put the control that *hides* a panel inside the panel
 * it hides. It now sits on the leading seam of the content card, outside the
 * sidebar entirely, and the assertions that it is absent from `sidebar` and
 * from `sidebar-content` and centred on that seam are what stop it drifting
 * back into either. They are deliberately paired with the reachability claims
 * rather than filed on their own: a control that is in the right place but
 * unreachable, or reachable but nameless, is the same bug in a different coat.
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
    await page.goto("/#/company");
    await dismissTour(page);

    const trigger = page.getByRole("button", { name: "Toggle sidebar" });
    await expect(trigger).toBeInViewport();
    await trigger.click();
    await expect(page.getByText("Automations", { exact: true })).toBeVisible();
  });

  test("the seam control is desktop-only, so the sheet has exactly one way back", async ({
    page,
  }) => {
    // 700px is below `md` (768), which is also exactly where `useIsMobile`
    // flips — the CSS gate and the hook agree by construction rather than by
    // coincidence.
    //
    // The seam button used to render here too, which was two controls for one
    // job on one viewport and the second one was wrong in both halves:
    // `SidebarCollapseButton` treats mobile as not-collapsed on purpose, so
    // with the sheet CLOSED it announced itself "Collapse sidebar" and drew the
    // close icon while pressing it OPENED the sheet.
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto("/#/company");
    await dismissTour(page);

    // `toBeHidden`, not `toHaveCount(0)`: the gate is CSS (`hidden md:block`),
    // matching the `md:hidden` on the mobile trigger it defers to, so the node
    // stays in the DOM with `display: none`. That is the whole claim, because
    // `display: none` also takes an element out of the accessibility tree and
    // out of the tab order — which the role query on the next line is what
    // actually proves.
    await expect(
      page.getByTestId("sidebar-collapse"),
      "the seam control is not shown below md",
    ).toBeHidden();
    await expect(
      page.getByRole("button", { name: "Collapse sidebar", exact: true }),
      "…so nothing here claims it collapses a column that is a sheet",
    ).toHaveCount(0);

    // One control, and it is the one that means what it says.
    const trigger = page.getByRole("button", { name: "Toggle sidebar" });
    await expect(trigger).toHaveCount(1);
    await expect(trigger).toBeInViewport();
    await trigger.click();
    await expect(page.getByRole("dialog", { name: "Sidebar" })).toBeVisible();

    // And it comes back at `md`, which is what stops this being satisfied by a
    // control that was deleted, or hidden at every width, rather than gated.
    await page.setViewportSize({ width: 1024, height: 800 });
    await expect(page.getByTestId("sidebar-collapse")).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Collapse sidebar", exact: true }),
    ).toHaveCount(1);
  });

  test("the mobile sheet closes after selecting a destination", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto("/#/company");
    await dismissTour(page);

    await page.getByRole("button", { name: "Toggle sidebar" }).click();
    const sheet = page.getByRole("dialog", { name: "Sidebar" });
    await expect(sheet).toBeVisible();
    await expect(sheet).toHaveAttribute("aria-modal", "true");

    // A channel, not a section's child row. Since #2130 the sheet holds the four
    // sections and the Room rail — a section's own pages are a content rail, so
    // "Work" is not in here to pick any more. The channel list is the thing this
    // sheet now has that nothing else does, and it is the harder case: it is
    // portalled in from `RoomView` rather than rendered by the sidebar, so a
    // dismiss that only fired for the sidebar's own rows would miss it (which is
    // what `room-rail.tsx`'s `dismiss` exists for). Picking one still closes the
    // sheet behind it, which is the pattern under test.
    await sheet.getByRole("button", { name: "engineering-desk", exact: true }).click();
    await expect(page).toHaveURL(/#\/chat\//);
    await expect(sheet).toBeHidden();
  });

  test("Escape closes the mobile sheet after focus moves inside it", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto("/#/company");
    await dismissTour(page);

    await page.getByRole("button", { name: "Toggle sidebar" }).click();
    const sheet = page.getByRole("dialog", { name: "Sidebar" });
    const destination = sheet.getByRole("button", { name: "Room", exact: true });
    await destination.focus();
    await expect(destination).toBeFocused();

    await page.keyboard.press("Escape");
    await expect(sheet).toBeHidden();
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
    await expect(page.getByText("Automations", { exact: true })).toBeVisible();
  });

  test("the inline sidebar's collapse control is a named, keyboard-operable control on the seam", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1024, height: 800 });
    await page.goto("/#/company");
    await dismissTour(page);

    const sidebar = page.locator("[data-slot=sidebar]");
    const toggle = page.getByRole("button", { name: "Collapse sidebar", exact: true });

    // Named and on screen. The name is the assertion as much as the position
    // is: this control is icon-only, so an `aria-label` lost in a refactor
    // leaves a button a screen reader announces as "button".
    await expect(toggle).toBeInViewport();

    // Not in the panel it hides — which is the whole of the move. It used to
    // live in the sidebar's own header, so collapsing the column took the
    // control with it and the rail had to keep a copy standing. It now renders
    // from `app-shell.tsx`'s `WindowTitleBar` (`sidebarToggle` slot), and there
    // is exactly one of it in either state.
    await expect(
      sidebar.getByTestId("sidebar-collapse"),
      "the collapse control is not inside the panel it collapses",
    ).toHaveCount(0);
    await expect(
      page.getByTestId("window-title-bar").getByTestId("sidebar-collapse"),
      "…it belongs to the title bar's chrome",
    ).toHaveCount(1);
    await expect(
      page.locator("[data-slot=sidebar-content]").getByTestId("sidebar-collapse"),
      "…and never among the nav rows, which is what issue #1177 was",
    ).toHaveCount(0);
    await expect(page.getByTestId("sidebar-collapse")).toHaveCount(1);

    // It also left the sidebar footer it used to be gathered onto with
    // Settings, Feedback and Discord — that footer is gone entirely now
    // (`app-shell.tsx`: "No footer."), and Settings/Discord are their own
    // title-bar glyphs (`title-bar-utilities.tsx`), reachable independently of
    // this control rather than bundled beside it in a shared container.

    // Beside the switcher at the title row's leading end (`window-title-bar.tsx`)
    // rather than tracking the content card's edge the way the old
    // `SidebarInset`-anchored control did — chrome now, not part of the seam
    // it operates on. What has to hold is that it is fixed there regardless of
    // sidebar state, not that it moves with a seam it no longer sits on.
    const position = async () => {
      const box = await page.getByTestId("sidebar-collapse").boundingBox();
      expect(box, "the collapse control should have a box").not.toBeNull();
      return box!;
    };
    const expandedBox = await position();
    const titleBar = await page.getByTestId("window-title-bar").boundingBox();
    expect(titleBar, "the title bar should have a box").not.toBeNull();
    // Within the row's own vertical bounds on both edges, not merely above
    // its bottom -- a control that started above the row's top would have
    // passed a bottom-only check just as happily.
    expect(
      expandedBox.y,
      "the control does not start above the title row",
    ).toBeGreaterThanOrEqual(titleBar!.y);
    expect(
      expandedBox.y + expandedBox.height,
      "the control sits inside the title row's chrome, not below it",
    ).toBeLessThanOrEqual(titleBar!.y + titleBar!.height);

    // Operable from the keyboard, not just under a pointer. An icon-only
    // button is exactly the kind that gets rebuilt as a `div` with an
    // `onClick` and silently stops being reachable.
    await toggle.focus();
    await expect(toggle).toBeFocused();
    await page.keyboard.press("Enter");

    // It survives the state it just produced — the case most likely to be got
    // wrong, and the one the old placement got wrong by construction.
    await expect(sidebar).toHaveAttribute("data-state", "collapsed");
    const expand = page.getByRole("button", { name: "Expand sidebar", exact: true });
    await expect(expand).toBeVisible();
    await expect(expand).toBeInViewport();

    // `data-state` flips on the click; the column takes `duration-200` to get
    // there. Poll rather than sample, or this measures a sidebar caught half
    // way and reports a control positioned against a seam still in motion.
    await expect
      .poll(
        async () => (await page.locator("[data-slot=sidebar-container]").boundingBox())?.width,
        { message: "the collapsed column settles at the icon rail's width" },
      )
      .toBe(RAIL_WIDTH);

    // Fixed chrome, not part of the column it operates on: the rail settling
    // at a different width must not move the control that toggles it.
    const collapsedBox = await position();
    expect(
      Math.abs(collapsedBox.x - expandedBox.x),
      "the control's position does not move when the column it toggles does",
    ).toBeLessThanOrEqual(1);
    expect(
      collapsedBox.x,
      "…and no part of it hangs off the left of the window",
    ).toBeGreaterThanOrEqual(0);

    // And back, from the keyboard, to where it started.
    await expand.focus();
    await expect(expand).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(sidebar).toHaveAttribute("data-state", "expanded");
    await expect(page.getByRole("button", { name: "Collapse sidebar", exact: true })).toBeVisible();
  });
});
