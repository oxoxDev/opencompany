// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { SidebarCollapseButton } from "@/components/sidebar-controls";
import { SidebarProvider } from "@/components/ui/sidebar";

/**
 * Issue #1177 — the sidebar's collapse control, as a control rather than a row.
 *
 * This suite is normally for pure functions (see `vitest.config.ts`), and it
 * earns the exception the same way `board-collapsed-columns.test.ts` does: the
 * claims here only exist at the rendered element. The one thing that has to
 * hold is that an ICON-ONLY button still says what it is and what it did.
 * There is no visible label to fall back on and no `aria-expanded` (see the
 * component for why), so the accessible NAME is the entire state channel: it
 * reads "Collapse sidebar" while the column is showing and "Expand sidebar"
 * once it is a rail. An `aria-label` is exactly the kind of attribute a
 * styling pass drops without breaking a single render, and a name that stops
 * tracking the provider's state is a control that lies about what it does.
 *
 * The other half of the fix — that the button sits in the sidebar's header and
 * not among the nav rows, and that it stays inside the 3rem rail it produces —
 * is geometry and composition, and lives in
 * `test/e2e/sidebar-toggle-reachable.spec.ts`. jsdom loads no stylesheet, so
 * asserting either of those here would pass against a build where the rail was
 * 3rem of clipped button.
 */

let host: HTMLDivElement;
let root: Root | null = null;

/**
 * jsdom ships no `matchMedia`, and `useIsMobile` — which `SidebarProvider`
 * calls — reaches for it unguarded. Same stub `working-indicator.test.ts`
 * installs, always reporting "not matching", which is the desktop case: at
 * jsdom's default 1024px window the sidebar is the inline column, not a sheet.
 */
function stubMatchMedia() {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    writable: true,
    value: (query: string) => ({
      matches: false,
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
      onchange: null,
    }),
  });
}

beforeEach(() => {
  // Tells React this run is an act environment, the same way the other
  // rendering suites here do. Without it every `act` prints a warning and
  // effects flush on their own schedule.
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  stubMatchMedia();
  host = document.createElement("div");
  document.body.append(host);
});

afterEach(() => {
  if (root) act(() => root!.unmount());
  root = null;
  host.remove();
});

/**
 * Mounts the button inside a real provider, so `state` is the real state.
 *
 * A fresh root every time: `defaultOpen` is an initial value, so re-rendering
 * the same tree with the other value would quietly keep the first one and the
 * collapsed case would test nothing.
 */
function mount(defaultOpen: boolean) {
  if (root) act(() => root!.unmount());
  root = createRoot(host);
  act(() => {
    root!.render(
      createElement(SidebarProvider, { defaultOpen }, createElement(SidebarCollapseButton)),
    );
  });
}

/** The one button on screen. Fails loudly if the render ever grows a second. */
function control(): HTMLButtonElement {
  const buttons = host.querySelectorAll("button");
  expect(buttons).toHaveLength(1);
  return buttons[0] as HTMLButtonElement;
}

describe("the sidebar collapse button", () => {
  it("names itself, on a real button", () => {
    mount(true);

    const button = control();
    // A real `<button>`, not a `div` with an `onClick`: that is what makes it
    // tab-reachable and Enter/Space-operable without any help from us.
    expect(button.tagName).toBe("BUTTON");
    expect(button.getAttribute("aria-label")).toBe("Collapse sidebar");
  });

  it("flips its name from the provider's state when pressed", () => {
    mount(true);

    act(() => control().click());

    // The name follows `useSidebar().state`, not a local flag: the click went
    // through the provider and came back out as a different label.
    const button = control();
    expect(button.getAttribute("aria-label")).toBe("Expand sidebar");

    // …and back, so the control is a toggle rather than a one-way trip.
    act(() => control().click());
    expect(control().getAttribute("aria-label")).toBe("Collapse sidebar");
  });

  it("is never nameless, in either state it can be mounted in", () => {
    for (const defaultOpen of [true, false]) {
      mount(defaultOpen);
      const button = control();
      // The failure this guards is an icon-only button announced as "button".
      // Neither the glyph nor the tooltip contributes a name — the tooltip is
      // not even in the DOM until it opens — so the label is all there is.
      expect(button.getAttribute("aria-label")).toBeTruthy();
      expect(button.textContent?.trim()).toBe("");
    }
  });
});
