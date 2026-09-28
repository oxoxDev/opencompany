// @vitest-environment jsdom

import { beforeEach, describe, expect, it } from "vitest";

import { clearMagicLinkFromUrl } from "@/App";
import { resolveConfig } from "@/config";
import { addConnection, resetConnections, restoreConnections } from "@/connections/registry";
import { readProfiles } from "@/connections/profileStore";
import { scopedKey } from "@/connections/types";

/**
 * What a landing credential is allowed to take with it when it leaves the URL.
 *
 * The address bar is not cosmetic here. `resolveConfig` reads `?company=` back
 * on every load, and that value is what `restoreConnections` is told this page
 * load *is* — so deleting the param is the same act as changing which
 * browser-local namespace the next load lands in. Issue #1306: the magic-link
 * cleaner dropped `company`, and the welcome tour came back after being
 * skipped.
 *
 * These drive the real cleaners against a real `localStorage`, then do what a
 * reload does — throw away the in-memory registry, re-read the URL, re-run the
 * bootstrap — and assert the connection id survived it.
 */

/** The landing URL a magic link produces, per the issue's repro table. */
function land(search: string, hash = ""): void {
  window.history.replaceState({}, "", `/login${search}${hash}`);
}

/**
 * A reload, as `App`'s bootstrap `useMemo` performs it.
 *
 * Mirrors the two calls at the top of that memo, including the same-origin
 * argument added for #1167 — which is the half that turns a missing `?company=`
 * into a skipped profile and a freshly minted id.
 */
function bootstrap(): string {
  resetConnections();
  const config = resolveConfig();
  restoreConnections(
    undefined,
    config.baseUrl === "" ? { defaultCompany: config.company } : undefined,
  );
  return addConnection({ baseUrl: config.baseUrl, defaultCompany: config.company });
}

beforeEach(() => {
  resetConnections();
  window.localStorage.clear();
  window.history.replaceState({}, "", "/");
});

describe("clearing a magic link", () => {
  it("takes the code out of the address bar", () => {
    land("?company=agentic-software-company&code=s3cret");

    clearMagicLinkFromUrl();

    expect(window.location.search).not.toContain("code");
    expect(window.location.search).not.toContain("s3cret");
  });

  it("keeps company, so a reload lands in the same connection scope", () => {
    // THE regression. `company` is not a credential — it is the scope. Stripping
    // it made the reload present `defaultCompany === null`, so `isThisConsole`
    // skipped the profile the link had just written and `addConnection` minted a
    // second id for the one host.
    land("?company=agentic-software-company&code=s3cret");
    const first = bootstrap();
    clearMagicLinkFromUrl();

    const second = bootstrap();

    expect(second).toBe(first);
    expect(readProfiles()).toHaveLength(1);
  });

  it("does not orphan the tour state it recorded before the reload", () => {
    // The symptom as an operator meets it: skip the welcome tour, reload, and
    // the welcome tour is back — because the key moved underneath it.
    land("?company=agentic-software-company&code=s3cret");
    const first = bootstrap();
    clearMagicLinkFromUrl();
    const company = "agentic-software-company";
    window.localStorage.setItem(
      scopedKey("oc-tour", { connection: first, company }),
      JSON.stringify({ skipped: true }),
    );

    const second = bootstrap();

    expect(
      window.localStorage.getItem(scopedKey("oc-tour", { connection: second, company })),
    ).toBe(JSON.stringify({ skipped: true }));
  });

  it("leaves the router's hash alone, so a deep link survives the strip", () => {
    land("?company=agentic-software-company&code=s3cret", "#/tasks/abc");

    clearMagicLinkFromUrl();

    expect(window.location.hash).toBe("#/tasks/abc");
  });

  it("does nothing at all when there is no code to strip", () => {
    land("?company=agentic-software-company", "#/overview");

    clearMagicLinkFromUrl();

    expect(window.location.search).toBe("?company=agentic-software-company");
    expect(window.location.hash).toBe("#/overview");
  });
});
