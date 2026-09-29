import { useEffect, useRef } from "react";
import { VIEWS } from "./console-routes";

type OpenPanelCommand = ((
  command: "track",
  event: string,
  properties: Record<string, unknown>,
) => void) & { q?: unknown[] };

declare global {
  interface Window {
    op?: OpenPanelCommand;
  }
}

/** The stable, non-identifying screen name for the hash-routed console. */
export function currentScreen(location: Location = window.location): string {
  const [head = ""] = location.hash.replace(/^#\/?/, "").split("?", 1)[0].split("/", 1);
  if (!head) return "home";
  const normalizedHead = head.toLowerCase();
  return normalizedHead === "styleguide" || VIEWS.includes(normalizedHead as (typeof VIEWS)[number])
    ? normalizedHead
    : "unknown";
}

function track(event: string, properties: Record<string, unknown>): void {
  window.op?.("track", event, properties);
}

/**
 * Captures navigation and activation across the React console without exposing
 * control labels, routes' dynamic segments, or any operator-provided content.
 */
export function installOpenPanelTracking(
  doc: Document = document,
  initialScreenView = true,
): () => void {
  const screenView = () => track("screen_viewed", { screen: currentScreen() });
  const buttonClick = (event: MouseEvent) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const control = target.closest<HTMLElement>('button, [role="button"]');
    if (!control) return;

    track("button_clicked", {
      screen: currentScreen(),
      control: control.tagName === "BUTTON" ? "button" : "role-button",
      button_type: control instanceof HTMLButtonElement ? control.type : null,
    });
  };

  if (initialScreenView) screenView();
  window.addEventListener("hashchange", screenView);
  doc.addEventListener("click", buttonClick, true);
  return () => {
    window.removeEventListener("hashchange", screenView);
    doc.removeEventListener("click", buttonClick, true);
  };
}

/** React lifecycle owner for the console-wide OpenPanel listeners. */
export function OpenPanelTracking(): null {
  const hasRun = useRef(false);
  useEffect(() => {
    const dispose = installOpenPanelTracking(document, !hasRun.current);
    hasRun.current = true;
    return dispose;
  }, []);
  return null;
}
