// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { SetupStatus } from "@/api/setup";
import { SetupWizard } from "@/views/setup/SetupWizard";

/**
 * The zero-company dead end (CodeRabbit review on #908): a host with no
 * companies must not be able to finish setup without a company to finish
 * *into*, because that is exactly the "no companies running, no way back into
 * setup" dead end the flow exists to remove.
 *
 * The **condition** changed when the two setups merged — it used to be "a
 * template was picked", and is now "a roster was designed and reviewed" —
 * but the invariant is the same one and is why this file still exists.
 *
 * A pure test cannot reach this — the claim is about a *button's disabled
 * state* changing as the operator moves through the wizard, which only
 * exists once the component is mounted and rendering. Same earned exception
 * as `provider-detail-render` and `working-indicator`.
 */

function status(over: Partial<SetupStatus> = {}): SetupStatus {
  return {
    complete: false,
    config_path: "/data/config.toml",
    fields: [],
    templates: [
      { id: "starter", name: "Starter", agent_count: 2, output: null },
    ],
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
    ...over,
  };
}

/**
 * `over.post` answers the **connection test** only.
 *
 * Routed by path rather than replacing `post` wholesale, because the wizard
 * makes three different calls through it — the test, the roster design, and the
 * apply — and a blanket override would silently change what the other two see.
 */
function clientWith(
  s: SetupStatus,
  over: { post?: (path: string, body: unknown) => Promise<unknown> } = {},
): OpenCompanyClient {
  return {
    get: async () => s,
    post: async (path: string, body: unknown) => {
      if (over.post && path.includes("/inference/test")) return over.post(path, body);
      return {
        complete: true,
        config_path: s.config_path,
        restart_required: [],
        seeded_company: null,
      };
    },
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

async function show(client: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(SetupWizard, { client, onDone: () => {} }));
  });
}

function button(label: string): HTMLButtonElement {
  const buttons = Array.from(container.querySelectorAll("button"));
  // The advance button is labelled "Looks good" on the Advanced step, because
  // there is nothing to answer there — same control, different word.
  const wanted = label === "Next" ? ["Next", "Looks good"] : [label];
  const match = buttons.find((b) => wanted.includes(b.textContent?.trim() ?? ""));
  expect(match, `no button labeled "${label}"`).toBeTruthy();
  return match as HTMLButtonElement;
}

/** Types into a step's field, so a required one can be left. */
async function fill(testId: string, value: string) {
  const field = container.querySelector(`[data-testid="${testId}"]`) as
    | HTMLInputElement
    | HTMLTextAreaElement;
  expect(field, `no field ${testId}`).toBeTruthy();
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(
      field instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype,
      "value",
    )!.set!;
    setter.call(field, value);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

const next = async () =>
  act(async () => {
    button("Next").click();
  });

/**
 * Skips the model step, which is now FIRST and is a gate.
 *
 * The skip is the honest path for a test with no provider to reach: the step
 * refuses to advance on an untested credential, which is the whole reason it
 * moved to the front.
 */
async function skipModel() {
  await act(async () => {
    (
      container.querySelector('[data-testid="setup-skip-model"]') as HTMLElement
    ).click();
  });
  await next(); // -> business
}

/** model -> business -> account -> advanced -> review. */
async function goToReview() {
  await skipModel();
  await fill("setup-field-industry", "E-commerce — homeware");
  await next(); // -> account
  // The address is required on any host that asks people to sign in — leaving
  // it blank holds the wizard here, which is its own assertion below.
  await fill("setup-field-email", "ada@example.com");
  await next(); // -> advanced
  await next(); // -> review
  // Entering Review kicks off the design call. Let it settle, or the assertions
  // below run against the spinner rather than the outcome.
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

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

const finishButton = () =>
  container.querySelector('[data-testid="setup-finish"]') as HTMLButtonElement | null;

describe("finishing setup with no companies on the host", () => {
  /**
   * The design call fails in this environment (no host behind the client), so
   * Review renders its error rather than a roster — which is precisely the
   * state that must not be finishable: there is no team to build, and applying
   * would leave a configured instance with nothing to sign in to.
   */
  it("refuses to finish when no team was designed", async () => {
    await show(clientWith(status()));
    await goToReview();

    expect(container.querySelector('[data-testid="setup-design-error"]')).toBeTruthy();
    expect(finishButton()?.disabled).toBe(true);
  });

  /**
   * The first question is the only required one, and it gates leaving step one
   * — so an operator cannot skip past every screen into a company with no
   * description behind it.
   */
  it("will not leave the first question empty", async () => {
    await show(clientWith(status()));
    await skipModel();

    await act(async () => {
      button("Next").click();
    });
    expect(container.querySelector('[data-testid="setup-problem"]')).toBeTruthy();
    // Still on the first question.
    expect(
      container.querySelector('[data-testid="setup-field-industry"]'),
    ).toBeTruthy();
  });

  /**
   * The other half of the dead end, and the one that was actually reachable in
   * shipped code: an operator who finishes setup on an email-sign-in host
   * without an address can then sign in as nobody, because no shipped template
   * invites anyone.
   */
  it("will not pass the email step on a host that asks people to sign in", async () => {
    await show(clientWith(status()));
    await skipModel();
    await fill("setup-field-industry", "E-commerce — homeware");
    await next(); // -> account

    // Pressing on with no address must not leave this step.
    await next();
    expect(container.querySelector('[data-testid="setup-problem"]')).toBeTruthy();
    expect(container.querySelector('[data-testid="setup-field-email"]')).toBeTruthy();
  });

  /**
   * A host that already serves a company is not at risk of the dead end: there
   * is somewhere to sign in to regardless of what this flow does, so an
   * operator reconfiguring one may finish without designing anything.
   */
  it("does not gate finishing when the host already has a company", async () => {
    await show(clientWith(status({ companies: ["acme"] })));
    await goToReview();

    expect(finishButton()?.disabled).toBe(false);
  });

  // -------------------------------------------------------------------------
  // The model gate (step one)
  // -------------------------------------------------------------------------

  /**
   * The reason this step moved to the front.
   *
   * The design pass is silent about credentials — it falls back to a curated
   * team on any failure — so an untested key produces a *plausible* company
   * rather than an error, and the operator finds out several screens later, if
   * at all. Untested therefore holds the flow here.
   */
  it("will not pass the model step on an untested connection", async () => {
    await show(clientWith(status()));

    await act(async () => {
      button("Next").click();
    });
    expect(container.querySelector('[data-testid="setup-problem"]')).toBeTruthy();
    // Still on the model step, not the first question.
    expect(container.querySelector('[data-testid="setup-field-key"]')).toBeTruthy();
    expect(container.querySelector('[data-testid="setup-field-industry"]')).toBeNull();
  });

  /**
   * Nobody gets stuck (decision D3). A hosted operator has no key of their own
   * and no way to get one, so a credential must never be the single thing that
   * traps them — the curated team exists for exactly this path.
   */
  it("lets an operator continue without a model, explicitly", async () => {
    await show(clientWith(status()));
    await skipModel();

    expect(container.querySelector('[data-testid="setup-field-industry"]')).toBeTruthy();
  });

  /**
   * A failed test is not a passed one. The step reports the reason and still
   * holds, because "we could not reach that" is exactly when carrying on
   * silently would produce the wrong company.
   */
  it("holds, and says why, when the connection fails", async () => {
    await show(
      clientWith(status(), {
        post: async () => ({
          ok: false,
          baseUrl: "https://api.tinyhumans.ai/openai/v1",
          error: "That key was rejected by the provider.",
        }),
      }),
    );

    await act(async () => {
      (
        container.querySelector('[data-testid="setup-test-connection"]') as HTMLElement
      ).click();
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const failure = container.querySelector('[data-testid="setup-test-failed"]');
    expect(failure?.textContent).toContain("rejected by the provider");
    await act(async () => {
      button("Next").click();
    });
    expect(container.querySelector('[data-testid="setup-field-industry"]')).toBeNull();
  });

  /**
   * A passing test releases the gate, and names the endpoint it reached — a
   * tick earned against the default endpoint, on a host meant to point
   * elsewhere, is a wrong answer the operator watched us produce.
   */
  it("releases the gate on a passing test, naming the endpoint", async () => {
    await show(
      clientWith(status(), {
        post: async () => ({ ok: true, baseUrl: "https://example.test/v1" }),
      }),
    );

    await act(async () => {
      (
        container.querySelector('[data-testid="setup-test-connection"]') as HTMLElement
      ).click();
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(
      container.querySelector('[data-testid="setup-test-ok"]')?.textContent,
    ).toContain("https://example.test/v1");
    await next();
    expect(container.querySelector('[data-testid="setup-field-industry"]')).toBeTruthy();
  });


  /**
   * "This host has a key" must read as **answered**, not as an unanswered
   * question.
   *
   * It shipped as an empty password box with "Using this host's key" as grey
   * placeholder text, and it read exactly like a field nobody had filled in —
   * which on a hosted tenant is the one impression it must not give, because the
   * operator has no key to put there and nothing is wrong. There is no value to
   * pre-fill with and there never will be: the host reports a secret's presence,
   * never its bytes.
   */
  it("states the host's key as settled rather than drawing an empty field", async () => {
    await show(
      clientWith({
        ...status(),
        inference: {
          ready: true,
          provider: "managed",
          base_url: "https://api.tinyhumans.ai/openai/v1",
        },
      }),
    );

    expect(
      container.querySelector('[data-testid="setup-key-on-the-house"]'),
    ).toBeTruthy();
    // No empty input pretending to be the question.
    expect(container.querySelector('[data-testid="setup-field-key"]')).toBeNull();

    // Someone who wants their own key can still get the field.
    await act(async () => {
      (
        container.querySelector('[data-testid="setup-key-override"]') as HTMLElement
      ).click();
    });
    expect(container.querySelector('[data-testid="setup-field-key"]')).toBeTruthy();
  });

  /**
   * A regression guard for a bug that reached a screenshot: `\u2014` written
   * into JSX text, where nothing interprets it, so the operator read a literal
   * escape sequence where an em dash belonged.
   *
   * Asserted over the rendered text rather than the source, because that is
   * where the defect was visible and where any future one will be.
   */
  it("renders no un-interpreted escape sequences", async () => {
    await show(clientWith(status()));
    const text = container.textContent ?? "";
    expect(text).not.toMatch(/\\u[0-9a-fA-F]{4}/);
    expect(text.length).toBeGreaterThan(0);
  });

});
