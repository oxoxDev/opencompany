// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { SetupStatus } from "@/api/setup";
import { SETUP_HANDOFF_FRAGMENT } from "@/setup/state";
import { SetupWizard } from "@/views/setup/SetupWizard";

/**
 * The console button's destination on completions that do not hand over a link.
 *
 * Every completion — a host that asks nobody to sign in, an operator the
 * wizard signed in with the password they set, and a sign-in that could not be
 * arranged — finishes through the same `setup-open-console` button. When that
 * button's `onDone` hands off to a
 * fresh `AppShell` (the connection console's re-probe), it must write the same
 * fragment first, or the fresh shell lands on Overview with the tour free to
 * open over the roster setup just built — the exact miss the link path was
 * fixed to avoid. When it instead completes in place (the in-shell dialog,
 * whose running shell suppresses the welcome through `onCompleted`), the
 * button must NOT write the marker: no mount is waiting to consume it, so it
 * would be read as a fresh hand-off on the next reload.
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

/**
 * Routed by path: the wizard makes different calls through `post` (the roster
 * design and the apply) and signs in through `postSignIn`, so a blanket
 * override would silently change what the others see.
 */
function clientWith(s: SetupStatus, over: { login?: () => Promise<unknown> } = {}): OpenCompanyClient {
  return {
    scopeFor: (company: string | null) => `/api/v1/companies/${company}`,
    get: async () => s,
    postSignIn: async () =>
      over.login
        ? over.login()
        : { id: "u1", email: "ada@example.com", role: "admin", company: "acme" },
    post: async (path: string) => {
      if (path.endsWith("/setup/roster")) {
        return {
          agents: [{ name: "Ada", role: "Operations", description: "Runs the desk." }],
          template: "ecommerce",
          source: "fallback",
        };
      }
      return {
        complete: true,
        config_path: s.config_path,
        restart_required: [],
        seeded_company: "acme",
      };
    },
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;
let done: () => void;

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  done = () => {};
  window.location.hash = "";
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  delete (window as unknown as { __TAURI__?: unknown }).__TAURI__;
});

async function show(
  client: OpenCompanyClient,
  props: { expectsShellRemount?: boolean } = {},
) {
  await act(async () => {
    root.render(createElement(SetupWizard, { client, onDone: done, ...props }));
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

const next = async () =>
  act(async () => {
    const match = Array.from(container.querySelectorAll("button")).find((b) =>
      ["Next", "Looks good"].includes(b.textContent?.trim() ?? ""),
    );
    expect(match, "no advance button").toBeTruthy();
    match!.click();
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

/** Lets the design, apply and sign-in requests settle. */
const settle = async () =>
  act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });

/**
 * The desktop install: a host that asks nobody to sign in, so the wizard
 * preselects `none` and the address step is absent.
 */
async function finishNoSignIn() {
  await skipConnect();
  await next(); // -> business
  await fill("setup-field-industry", "E-commerce — homeware");
  await next(); // -> sign-in (none preselected)
  await next(); // -> review
  await settle();
  await click("setup-finish");
  await settle();
}

/** An email-sign-in host: the account step, with its password, then finish. */
async function finishWithSignIn() {
  await skipConnect();
  await next(); // -> business
  await fill("setup-field-industry", "E-commerce — homeware");
  await next(); // -> sign-in
  await next(); // -> account
  await fill("setup-field-email", "ada@example.com");
  await next(); // -> review
  await settle();
  await click("setup-finish");
  await settle();
}

describe("the console button after setup applies without a hand-off link", () => {
  it("carries the roster destination out of a no-sign-in setup", async () => {
    // The desktop runtime is what makes the wizard preselect `none`; it must
    // be in place before the wizard mounts (`isDesktopRuntime` is read once).
    (window as unknown as { __TAURI__: unknown }).__TAURI__ = { core: {} };
    let calls = 0;
    done = () => {
      calls += 1;
    };
    // The connection console's `onDone` re-probes and boots a fresh `AppShell`,
    // so this completion writes the marker for that shell to read.
    await show(clientWith(status()), { expectsShellRemount: true });
    await finishNoSignIn();

    // Nobody to invite, so there is nobody to sign in — only the console.
    expect(find("setup-handoff-signed-in")).toBeNull();
    expect(find("setup-open-console")).toBeTruthy();

    await click("setup-open-console");

    // The fresh `AppShell` this hands off to reads the fragment: it routes to
    // `#/company`, suppresses the tour welcome, and clears the marker.
    expect(window.location.hash).toBe(SETUP_HANDOFF_FRAGMENT);
    expect(calls).toBe(1);
  });

  it("carries the same destination out of a signed-in completion", async () => {
    await show(clientWith(status()), { expectsShellRemount: true });
    await finishWithSignIn();

    expect(find("setup-handoff-signed-in")).toBeTruthy();

    await click("setup-open-console");

    expect(window.location.hash).toBe(SETUP_HANDOFF_FRAGMENT);
  });

  it("carries the same destination out of the sign-in-failed escape", async () => {
    await show(
      clientWith(status(), {
        login: async () => {
          throw new Error("boom");
        },
      }),
      { expectsShellRemount: true },
    );
    await finishWithSignIn();

    expect(find("setup-handoff-password")).toBeTruthy();
    expect(find("setup-open-console")?.textContent).toContain("anyway");

    await click("setup-open-console");

    expect(window.location.hash).toBe(SETUP_HANDOFF_FRAGMENT);
  });

  it("leaves the URL alone when completion happens in place", async () => {
    // The in-shell dialog's `onDone` closes in place — the running shell
    // suppresses the welcome through `onCompleted`, so no mount follows to
    // consume a marker. Writing one here would be read as a fresh hand-off on
    // the next reload, so the button must leave the address untouched.
    (window as unknown as { __TAURI__: unknown }).__TAURI__ = { core: {} };
    let calls = 0;
    done = () => {
      calls += 1;
    };
    window.location.hash = "#/overview";
    await show(clientWith(status()));
    await finishNoSignIn();

    await click("setup-open-console");

    expect(window.location.hash).toBe("#/overview");
    expect(calls).toBe(1);
  });
});
