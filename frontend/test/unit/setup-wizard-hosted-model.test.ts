// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { SetupStatus } from "@/api/setup";
import { SetupWizard } from "@/views/setup/SetupWizard";

/**
 * A host that already reaches a model must not ask its operator for one.
 *
 * `GET /api/v1/setup` answers `inference.ready` whenever a platform credential
 * resolves from the environment — a hosted tenant's projected token, or a
 * static key on the process. That operator has no key of their own and no way
 * to obtain one, so the model step is a question with a single possible answer
 * and a key field nobody can fill.
 *
 * Three things are pinned here, and the third is the one that matters most:
 * the step is gone and the progress bar is a step shorter; the apply stores no
 * inference provider, because the host resolves the managed endpoint from its
 * own environment and writing one would record a decision the operator never
 * made; and a host that is *not* ready still gets exactly the flow it has
 * today.
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

/** What the control plane's projected credential looks like on the wire. */
const hosted = () =>
  status({
    inference: {
      ready: true,
      provider: "managed",
      base_url: "https://api.tinyhumans.ai/openai/v1",
    },
  });

/**
 * Routes by path, because the wizard makes three different calls through
 * `post` — the connection test, the roster design, and the apply — and the
 * apply body is what the payload assertions read.
 */
function clientWith(s: SetupStatus, seen: { body?: unknown } = {}): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/company",
    get: async () => s,
    post: async (path: string, body: unknown) => {
      if (path.includes("/inference/test")) {
        return { ok: true, baseUrl: s.inference.base_url, model: "some-model" };
      }
      if (path.includes("/setup/roster")) {
        return { source: "model", agents: [{ role: "Operations", description: "Runs things" }] };
      }
      seen.body = body;
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

const all = (testId: string) => container.querySelectorAll(`[data-testid="${testId}"]`);

/** The slots the progress bar is actually drawing. */
const slots = () =>
  Array.from(container.querySelectorAll("[data-testid^='step-']")).map((el) =>
    el.getAttribute("data-testid"),
  );

/** The wizard's own progress line, e.g. `Review · step 4 of 4`. */
function stepLabel(): string {
  const match = container.textContent?.match(/(\w[\w -]*) · step \d+ of \d+/);
  return match ? match[0] : "";
}

function labelled(...wanted: string[]): HTMLButtonElement {
  const match = Array.from(container.querySelectorAll("button")).find((b) =>
    wanted.includes(b.textContent?.trim() ?? ""),
  );
  expect(match, `no button labeled ${wanted.join("/")}`).toBeTruthy();
  return match as HTMLButtonElement;
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

const settle = async () =>
  act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });

/**
 * Gets past step 0 onto step 1, and is a no-op where step 0 is absent.
 *
 * The flow opens on the setup-way choice, and the add-provider sequence sits
 * behind "Set it up yourself".
 */
async function chooseSelfManaged() {
  const option = find("setup-way-self-managed") as HTMLElement | null;
  if (!option) return;
  await act(async () => {
    option.click();
  });
  await next();
}

/**
 * Business -> sign-in -> account -> review, with no model step in front of it.
 *
 * Nothing here answers a model question, which is the assertion this helper
 * carries into every test that uses it: the walk completes without one.
 */
async function goToReview() {
  await fill("setup-field-industry", "E-commerce — homeware");
  await next(); // -> sign-in
  await next(); // -> account
  await fill("setup-field-email", "ada@example.com");
  await next(); // -> review
  await settle();
}

describe("a host that already reaches a model", () => {
  it("opens on the business question, with no model step and no slot for one", async () => {
    await show(clientWith(hosted()));

    expect(slots()).toEqual([
      "step-business",
      "step-signin",
      "step-account",
      "step-review",
    ]);
    // The count must drop with the step. A four-screen flow that says "of 5" is
    // telling the operator about a screen they will never be shown.
    expect(container.textContent).toContain("step 1 of 4");
    expect(find("setup-add-provider"), "step 1 must not render").toBeNull();
  });

  it("finishes without ever showing a key field or a connection test", async () => {
    await show(clientWith(hosted()));
    await goToReview();

    // Landed on Review, asserted before anything about what is absent —
    // otherwise the three checks below would also pass if the walk had stalled.
    expect(stepLabel(), "the walk should have reached Review").toMatch(/^Review · step/);
    expect(container.querySelector("#setup-key")).toBeNull();
    expect(find("setup-field-key")).toBeNull();
    expect(find("setup-test-connection")).toBeNull();
    expect((find("setup-finish") as HTMLButtonElement | null)?.disabled).toBe(false);
  });

  it("stores no inference provider for the company it builds", async () => {
    const seen: { body?: unknown } = {};
    await show(clientWith(hosted(), seen));
    await goToReview();

    await act(async () => {
      (find("setup-finish") as HTMLElement).click();
    });
    await settle();

    const body = seen.body as
      | { company?: { inference?: unknown }; fields?: Record<string, unknown> }
      | undefined;
    expect(body, "setup should have been applied").toBeTruthy();
    // The host resolves the managed endpoint from its own environment at
    // runtime. Writing a provider here would record a choice nobody made.
    expect(body?.company?.inference ?? null).toBeNull();
    expect(body?.fields ?? {}).not.toHaveProperty("tinyhumans_api_key");
  });

  it("says once, on review, that the model came with the host", async () => {
    await show(clientWith(hosted()));
    await goToReview();

    // Skipping silently is the failure mode: an operator who was never asked
    // for a model should find out where theirs came from in the flow.
    expect(all("setup-host-model")).toHaveLength(1);
    expect(find("setup-host-model")?.textContent).toMatch(/comes with this host/);
  });
});

describe("a host that reaches no model of its own", () => {
  it("still asks how this company connects, now behind the setup-way choice", async () => {
    await show(clientWith(status()));

    expect(find("setup-add-provider"), "step 1 is not step 0").toBeNull();

    await chooseSelfManaged();

    expect(slots()).toEqual([
      "step-setup-way",
      "step-self-managed-connect",
      "step-business",
      "step-signin",
      "step-account",
      "step-review",
    ]);
    expect(container.textContent).toContain("step 2 of 6");
    expect(find("setup-add-provider"), "step 1 should render").toBeTruthy();
  });

  it("still gates the managed branch on a verdict", async () => {
    await show(clientWith(status()));
    await act(async () => {
      (find("setup-way-managed") as HTMLElement).click();
    });
    await next();

    await next();
    expect(find("setup-problem"), "an untested connection must hold the step").toBeTruthy();
    expect(find("setup-field-key"), "and must not have left it").toBeTruthy();
  });

  it("says nothing about a host-provided model", async () => {
    await show(clientWith(status()));
    await chooseSelfManaged();

    // Connecting nothing answers the step, which is what lets this walk reach
    // Review without a credential.
    await next(); // -> business
    await goToReview();

    expect(stepLabel(), "the walk should have reached Review").toMatch(/^Review · step/);
    expect(all("setup-host-model")).toHaveLength(0);
  });
});
