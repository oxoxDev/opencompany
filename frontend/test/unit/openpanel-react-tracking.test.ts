// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, createElement, StrictMode } from "react";
import { createRoot, type Root } from "react-dom/client";

import { currentScreen, installOpenPanelTracking, OpenPanelTracking } from "@/lib/openpanel";

const track = vi.fn();
let dispose: (() => void) | undefined;
let root: Root | undefined;
let container: HTMLDivElement | undefined;

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
    true;
  window.op = track;
  window.location.hash = "#/settings/people?token=never-track-this";
  track.mockReset();
});

afterEach(() => {
  dispose?.();
  dispose = undefined;
  if (root) act(() => root?.unmount());
  container?.remove();
  root = undefined;
  container = undefined;
  delete window.op;
});

describe("OpenPanel React tracking", () => {
  it("records the initial screen and hash navigation without query or dynamic path data", () => {
    dispose = installOpenPanelTracking();
    expect(track).toHaveBeenCalledWith("track", "screen_viewed", { screen: "settings" });

    window.location.hash = "#/workflows/a-user-owned-id?token=still-private";
    window.dispatchEvent(new HashChangeEvent("hashchange"));
    expect(track).toHaveBeenLastCalledWith("track", "screen_viewed", { screen: "workflows" });
  });

  it("records every native and role button activation without its label", () => {
    dispose = installOpenPanelTracking();
    const native = document.createElement("button");
    native.type = "submit";
    native.textContent = "Customer-provided sensitive label";
    const roleButton = document.createElement("div");
    roleButton.setAttribute("role", "button");
    document.body.append(native, roleButton);

    native.click();
    roleButton.click();

    expect(track).toHaveBeenCalledWith("track", "button_clicked", {
      screen: "settings",
      control: "button",
      button_type: "submit",
    });
    expect(track).toHaveBeenLastCalledWith("track", "button_clicked", {
      screen: "settings",
      control: "role-button",
      button_type: null,
    });
    native.remove();
    roleButton.remove();
  });

  it("uses an opaque fallback for an unrecognised route", () => {
    window.location.hash = "#/customer%20name/private";
    expect(currentScreen()).toBe("unknown");
  });

  it("does not publish an unknown route head", () => {
    window.location.hash = "#/customer-123/private";
    expect(currentScreen()).toBe("unknown");
  });

  it("maps the root route to home", () => {
    window.location.hash = "#/";
    expect(currentScreen()).toBe("home");
  });

  it("stops tracking after disposal", () => {
    dispose = installOpenPanelTracking();
    track.mockReset();

    dispose();
    dispose = undefined;
    window.location.hash = "#/workflows/after-disposal";
    window.dispatchEvent(new HashChangeEvent("hashchange"));
    document.body.click();

    expect(track).not.toHaveBeenCalled();
  });

  it("mounts and unmounts its tracking listeners with the React lifecycle", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => root?.render(createElement(OpenPanelTracking)));
    expect(track.mock.calls.filter(([command, event]) => command === "track" && event === "screen_viewed")).toHaveLength(1);
    expect(track).toHaveBeenCalledWith("track", "screen_viewed", { screen: "settings" });

    window.location.hash = "#/workflows/a-private-id";
    window.dispatchEvent(new HashChangeEvent("hashchange"));
    const button = document.createElement("button");
    document.body.append(button);
    button.click();
    expect(track).toHaveBeenCalledWith("track", "screen_viewed", { screen: "workflows" });
    expect(track).toHaveBeenCalledWith("track", "button_clicked", {
      screen: "workflows",
      control: "button",
      button_type: "submit",
    });

    track.mockReset();
    act(() => root?.unmount());
    root = undefined;
    window.location.hash = "#/overview";
    window.dispatchEvent(new HashChangeEvent("hashchange"));
    button.click();

    expect(track).not.toHaveBeenCalled();
    button.remove();
  });

  it("does not duplicate the initial screen view when StrictMode replays effects", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => root?.render(createElement(StrictMode, null, createElement(OpenPanelTracking))));

    expect(track).toHaveBeenCalledTimes(1);
    expect(track).toHaveBeenCalledWith("track", "screen_viewed", { screen: "settings" });
  });
});
