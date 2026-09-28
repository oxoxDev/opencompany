// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { SetupStatus } from "@/api/setup";
import { SetupWizard } from "@/views/setup/SetupWizard";

/**
 * What the wizard says about mail, when it cannot send any — and how it gets
 * the operator in regardless.
 *
 * `email` sign-in and a working mailbox are two different questions — a
 * password signs people in with no transport at all — so the flow must
 * neither hide the mode nor promise a link it cannot deliver. The host says
 * whether a link is genuinely sent (`mail.wired`).
 *
 * The hand-off at the end no longer depends on mail at all: the "You" step
 * collects a password, the apply creates the account with it, and the wizard
 * signs the operator in with the same password. It used to be a magic-link
 * request whose outcome was inferred from an echoed `dev_code`, so a routable
 * host with no SMTP finished setup by telling its operator to check an inbox
 * that would stay empty forever. That is the bug these tests hold shut.
 */

function status(over: Partial<SetupStatus> = {}): SetupStatus {
  return {
    complete: false,
    config_path: "/data/config.toml",
    fields: [],
    templates: [],
    auth_modes: ["email"],
    build: {
      acp_in_build: false,
      acp_transport_mounted: false,
      mcp_in_build: false,
      harness_in_build: false,
      oauth_in_build: false,
    },
    companies: [],
    inference: { ready: false, provider: null, base_url: null },
    mail: { wired: false, echoes_code: false },
    ...over,
  };
}

/**
 * Routed by path: the wizard makes several calls through `post`, and the
 * sign-in goes through `postSignIn`. Records what the apply and the sign-in
 * were sent.
 */
function clientWith(
  s: SetupStatus,
  over: { login?: (body: unknown) => Promise<unknown> } = {},
): OpenCompanyClient & { applied: unknown[]; logins: unknown[] } {
  const applied: unknown[] = [];
  const logins: unknown[] = [];
  return {
    applied,
    logins,
    scopeFor: (company: string | null) => `/api/v1/companies/${company}`,
    get: async () => s,
    post: async (path: string, body: unknown) => {
      if (path.endsWith("/setup/roster")) {
        return {
          agents: [{ name: "Ada", role: "Operations", description: "Runs the desk." }],
          template: "ecommerce",
          source: "fallback",
        };
      }
      applied.push(body);
      return {
        complete: true,
        config_path: s.config_path,
        restart_required: [],
        seeded_company: "acme",
      };
    },
    postSignIn: async (path: string, body: unknown) => {
      expect(path).toBe("/api/v1/companies/acme/auth/login");
      logins.push(body);
      if (over.login) return over.login(body);
      return { id: "u1", email: "ada@example.com", role: "admin", company: "acme" };
    },
  } as unknown as OpenCompanyClient & { applied: unknown[]; logins: unknown[] };
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
});

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

/** Walks the whole flow and presses the finish button. */
async function finish() {
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

describe("the sign-in step, on a host that cannot send mail", () => {
  it("says people sign in with a password, whether the host echoes a code or not", async () => {
    for (const echoes_code of [true, false]) {
      await show(clientWith(status({ mail: { wired: false, echoes_code } })));
      await skipConnect();
      await next();
      await fill("setup-field-industry", "Homeware");
      await next(); // -> sign-in

      const note = find("setup-mail-note");
      expect(note?.textContent).toMatch(/password/i);
      expect(note?.textContent).not.toMatch(/browser|handed/i);
      // Email sign-in is not broken here — a password works without a
      // transport — so the mode must stay offered rather than be hidden from
      // an operator who may wire SMTP ten minutes later. The card is the
      // control, and the note sits beside it.
      expect(find("auth-mode-email")).toBeTruthy();
      expect((find("auth-mode-email") as HTMLButtonElement).disabled).toBe(false);
      await act(async () => root.unmount());
      root = createRoot(container);
    }
  });

  it("says nothing when the host has a mail transport", async () => {
    await show(clientWith(status({ mail: { wired: true, echoes_code: false } })));
    await skipConnect();
    await next();
    await fill("setup-field-industry", "Homeware");
    await next(); // -> sign-in

    expect(find("setup-mail-note")).toBeNull();
  });
});

describe("the hand-off after setup applies", () => {
  it("asks for a password on the account step and sends it with the apply", async () => {
    const client = clientWith(status());
    await show(client);
    await skipConnect();
    await next(); // -> business
    await fill("setup-field-industry", "E-commerce — homeware");
    await next(); // -> sign-in
    await next(); // -> account

    // Generated up front, in the clear: the person has to *see* the password
    // they are about to be signed in with, or the next visit is a lockout.
    const generated = (find("new-password") as HTMLInputElement).value;
    expect(generated.length).toBeGreaterThanOrEqual(12);

    await fill("setup-field-email", "ada@example.com");
    await fill("new-password", "correct horse battery staple");
    await next(); // -> review
    await settle();
    await click("setup-finish");
    await settle();

    expect(client.applied).toHaveLength(1);
    expect(client.applied[0]).toMatchObject({
      admin_email: "ada@example.com",
      admin_password: "correct horse battery staple",
    });
  });

  it("refuses to leave the account step on a password that is too short", async () => {
    const client = clientWith(status());
    await show(client);
    await skipConnect();
    await next(); // -> business
    await fill("setup-field-industry", "E-commerce — homeware");
    await next(); // -> sign-in
    await next(); // -> account
    await fill("setup-field-email", "ada@example.com");
    await fill("new-password", "short");
    await next();

    expect(find("new-password"), "still on the account step").toBeTruthy();
    expect(find("new-password-problem")?.textContent).toMatch(/12/);
  });

  /**
   * The bug this file exists for, in its new shape: whatever the host's mail,
   * the operator is signed in with the password they set. No inbox is named,
   * because none is involved.
   */
  it("signs the operator in with that password, whatever the host's mail", async () => {
    for (const mail of [
      { wired: true, echoes_code: false },
      { wired: false, echoes_code: true },
      { wired: false, echoes_code: false },
    ]) {
      const client = clientWith(status({ mail }));
      await show(client);
      await finish();

      expect(client.logins).toHaveLength(1);
      expect(client.logins[0]).toMatchObject({ email: "ada@example.com" });
      expect(find("setup-handoff-signed-in")?.textContent).toContain("ada@example.com");
      expect(container.textContent).not.toMatch(/check your email|inbox/i);
      expect(find("setup-open-console")).toBeTruthy();
      await act(async () => root.unmount());
      root = createRoot(container);
    }
  });

  it("says the account exists when the sign-in itself fails", async () => {
    // The apply landed — the account is there with the password they saw —
    // so the honest answer names the way in rather than announcing a failure.
    const client = clientWith(status(), {
      login: async () => {
        throw new Error("boom");
      },
    });
    await show(client);
    await finish();

    expect(find("setup-handoff-signed-in")).toBeNull();
    expect(find("setup-handoff-password")?.textContent).toMatch(/password/i);
    expect(find("setup-open-console")?.textContent).toMatch(/anyway/i);
  });

  it("arranges nothing on a host with no sign-in", async () => {
    const client = clientWith(status({ auth_modes: ["email", "none"] }));
    await show(client);
    await skipConnect();
    await next(); // -> business
    await fill("setup-field-industry", "E-commerce — homeware");
    await next(); // -> sign-in
    await click("auth-mode-none");
    await next(); // -> review (the account step is gone)
    await settle();
    await click("setup-finish");
    await settle();

    expect(client.logins).toHaveLength(0);
    expect(client.applied[0]).toMatchObject({ admin_password: null });
    expect(find("setup-open-console")).toBeTruthy();
  });
});
