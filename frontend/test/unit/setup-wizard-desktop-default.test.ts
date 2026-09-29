// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { SetupStatus } from "@/api/setup";
import { SetupWizard } from "@/views/setup/SetupWizard";

/**
 * What the wizard answers on the operator's behalf when it is running inside
 * the desktop app.
 *
 * The packaged desktop is a `none`-mode host: one machine, one person, no
 * accounts, and no mailbox to send a link to. An instance an operator created
 * there still opens the wizard — that is the whole point of `RunSetupWizard` —
 * and the wizard would otherwise ask a question whose answer is already settled
 * by where it is running, and then ask for an address to go with the wrong
 * answer. Seeding the choice removes the address step entirely.
 *
 * It is a *preselection*, not a lock. Someone who wants to share their instance
 * with a colleague can still pick `email` on this very screen; the host reads
 * the mode back out of `config.toml` at the next launch, so the choice holds.
 *
 * Two conditions, and the second is not redundant: the console is the desktop
 * runtime, **and** the host it is talking to offers `none` at all. A desktop
 * console can be pointed at a remote host through the switcher, and a routable
 * host withholds `none` precisely because it would serve an unauthenticated
 * admin console — preselecting it there would send an operator into a mode the
 * apply is going to refuse.
 */

function status(over: Partial<SetupStatus> = {}): SetupStatus {
  return {
    complete: false,
    config_path: "/data/config.toml",
    fields: [],
    templates: [],
    auth_modes: ["email", "wallet", "none"],
    build: {
      acp_in_build: false,
      acp_transport_mounted: false,
      mcp_in_build: false,
      harness_in_build: false,
      oauth_in_build: false,
    },
    companies: [],
    inference: { ready: false, provider: null, base_url: null },
    mail: { wired: false, echoes_code: true },
    ...over,
  };
}

function clientWith(s: SetupStatus): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/company",
    get: async () => s,
    post: async () => ({
      complete: true,
      config_path: s.config_path,
      restart_required: [],
      seeded_company: null,
    }),
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  delete (window as unknown as { __TAURI__?: unknown }).__TAURI__;
});

/**
 * Makes `isDesktopRuntime()` true.
 *
 * Presence of `__TAURI__` on `window` is the whole of that predicate, which is
 * what lets this be a fixture rather than a mock of the module under test.
 */
function asDesktop() {
  (window as unknown as { __TAURI__: unknown }).__TAURI__ = { core: {} };
}

async function show(client: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(SetupWizard, { client, onDone: () => {} }));
  });
}

const find = (testId: string) => container.querySelector(`[data-testid="${testId}"]`);

async function click(testId: string) {
  const el = find(testId) as HTMLElement | null;
  expect(el, `no element ${testId}`).toBeTruthy();
  await act(async () => {
    el!.click();
  });
}

/**
 * Gets past step 0 onto step 1, and is a no-op once already there.
 *
 * The flow opens on the setup-way choice, and step 1 sits behind
 * "Set it up yourself".
 */
async function chooseSelfManaged() {
  if (!find("setup-way-self-managed")) return;
  await click("setup-way-self-managed");
  await next();
}

/**
 * Gets past step 1 without connecting anything.
 *
 * The self-managed branch's step 1 is the real add-provider sequence now, and
 * both of its connections are optional — so leaving it unanswered is the whole
 * of skipping it, and Next is not gated. This presses the "set this up later"
 * affordance rather than choosing a "No model" the step no longer offers.
 */
async function skipConnect() {
  await chooseSelfManaged();
  await click("setup-provider-later");
}

function labelled(...wanted: string[]): HTMLButtonElement {
  const match = Array.from(container.querySelectorAll("button")).find((b) =>
    wanted.includes(b.textContent?.trim() ?? ""),
  );
  expect(match, `no button labeled ${wanted.join("/")}`).toBeTruthy();
  return match as HTMLButtonElement;
}

/** The wizard's own progress line, e.g. `Review · step 4 of 4`. */
function stepLabel(): string {
  const match = container.textContent?.match(/(\w[\w -]*) · step \d+ of \d+/);
  return match ? match[0] : "";
}

const next = async () =>
  act(async () => {
    labelled("Next", "Looks good").click();
  });

async function fill(testId: string, value: string) {
  const field = find(testId) as HTMLInputElement | HTMLTextAreaElement | null;
  expect(field, `no field ${testId}`).toBeTruthy();
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(
      field instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype,
      "value",
    )!.set!;
    setter.call(field!, value);
    field!.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

/** model (skipped) -> business (answered) -> sign-in. */
async function goToSignIn() {
  await skipConnect();
  await next();
  await fill("setup-field-industry", "E-commerce — homeware");
  await next();
}

const slots = () =>
  Array.from(container.querySelectorAll("[data-testid^='step-']")).map((el) =>
    el.getAttribute("data-testid"),
  );

describe("the sign-in a desktop install starts from", () => {
  it("arrives at the sign-in step with no sign-in already chosen", async () => {
    asDesktop();
    await show(clientWith(status()));
    await goToSignIn();

    expect(find("auth-mode-none")?.getAttribute("aria-pressed")).toBe("true");
    expect(find("auth-mode-email")?.getAttribute("aria-pressed")).toBe("false");
  });

  it("never shows a desktop operator the address field", async () => {
    asDesktop();
    await show(clientWith(status()));
    await goToSignIn();

    // The consequence of the seeded answer, and the reason it is seeded: the
    // address step is gone from the bar before the operator has pressed
    // anything, rather than appearing and then being taken away.
    expect(slots()).toEqual([
      "step-setup-way",
      "step-self-managed-connect",
      "step-business",
      "step-signin",
      "step-review",
    ]);

    await next();
    // Review, asserted first: both checks below are absences, and an absence on
    // a screen the press never left says nothing at all.
    expect(stepLabel(), "the press should have reached Review").toMatch(/^Review · step/);
    expect(find("setup-field-email")).toBeNull();
    expect(find("setup-advanced")).toBeNull();
  });

  it("still lets a desktop operator turn a sign-in on", async () => {
    // A preselection, not a lock. Somebody who means to invite a colleague to
    // the instance on their laptop chooses email here, and the address step
    // comes back with it.
    asDesktop();
    await show(clientWith(status()));
    await goToSignIn();
    await click("auth-mode-email");
    await next();

    expect(find("setup-field-email")).toBeTruthy();
  });

  it("leaves a browser console to ask the question", async () => {
    // No `__TAURI__`: `opencompany serve` on a laptop offers `none` too, and
    // answering for that operator would turn a host they may well be about to
    // expose into one with no sign-in, silently.
    await show(clientWith(status()));
    await goToSignIn();

    expect(find("auth-mode-none")?.getAttribute("aria-pressed")).toBe("false");
    await next();
    expect(find("setup-field-email")).toBeTruthy();
  });

  it("leaves a desktop console pointed at a routable host alone", async () => {
    // The switcher can aim this console at a remote host, and a routable host
    // withholds `none` from `auth_modes` because it would be serving an
    // unauthenticated admin console. Seeding it there would walk the operator
    // into a choice the apply refuses with a 409.
    asDesktop();
    await show(clientWith(status({ auth_modes: ["email", "wallet"] })));
    await goToSignIn();

    expect(find("auth-mode-none")).toBeNull();
    await next();
    expect(find("setup-field-email")).toBeTruthy();
  });
});
