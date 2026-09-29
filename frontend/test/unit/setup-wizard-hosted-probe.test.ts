// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { SetupStatus } from "@/api/setup";
import { SetupWizard } from "@/views/setup/SetupWizard";

/**
 * Hiding the model step is a claim about a live endpoint, so something has to
 * have called one.
 *
 * `inference.ready` is built from the environment — a credential and a URL
 * resolve — and says nothing about whether the endpoint answers. The model step
 * is the only live connection check in first run, so removing it on readiness
 * alone lets an expired key or a stale URL finish setup looking healthy: the
 * roster request then fails, the host falls back to its curated team, and the
 * operator ends up with a plausible company whose agents cannot think and no
 * screen anywhere pointing at the model.
 *
 * Three states, and each has to be said honestly:
 *
 * - **reachable** — the step goes, and review says the model came with the host
 * - **resolved but unreachable** — the step stays, carrying the failure
 * - **no credential** — the manual flow, untouched
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

interface ProbeBody {
  provider: string;
  key?: string | null;
  baseUrl?: string | null;
}

interface Seen {
  /** Every call to the connection test, in order. */
  probes: ProbeBody[];
  /** The apply body, once setup is submitted. */
  apply?: { company?: { inference?: unknown }; fields?: Record<string, unknown> };
}

const DEAD = "The credential was rejected.";

/**
 * A host whose model is reachable only with a key the operator supplies.
 *
 * Models the failure this exists for: the injected credential still resolves,
 * so `ready` is true, and it no longer works.
 */
const onlyWithOwnKey = (body: ProbeBody) =>
  body.key?.trim()
    ? { ok: true, baseUrl: "https://openrouter.ai/api/v1", model: "some-model" }
    : { ok: false, baseUrl: "https://api.tinyhumans.ai/openai/v1", error: DEAD };

function clientWith(
  s: SetupStatus,
  opts: { probe?: (body: ProbeBody) => unknown; seen?: Seen } = {},
): OpenCompanyClient {
  const seen = opts.seen ?? { probes: [] };
  return {
    scopeFor: () => "/api/v1/company",
    get: async () => s,
    post: async (path: string, body: unknown) => {
      if (path.includes("/inference/test")) {
        seen.probes.push(body as ProbeBody);
        return (
          opts.probe?.(body as ProbeBody) ?? {
            ok: true,
            baseUrl: s.inference.base_url,
            model: "some-model",
          }
        );
      }
      if (path.includes("/setup/roster")) {
        return { source: "model", agents: [{ role: "Operations", description: "Runs things" }] };
      }
      seen.apply = body as Seen["apply"];
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
  await settle();
}

const find = (testId: string) => container.querySelector(`[data-testid="${testId}"]`);

const all = (testId: string) => container.querySelectorAll(`[data-testid="${testId}"]`);

const text = (testId: string) => find(testId)?.textContent ?? "";

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

const click = async (testId: string) => {
  const el = find(testId);
  expect(el, `no element ${testId}`).toBeTruthy();
  await act(async () => {
    (el as HTMLElement).click();
  });
};

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

/** Business -> sign-in -> account -> review, from the business question. */
async function goToReview() {
  await fill("setup-field-industry", "E-commerce — homeware");
  await next(); // -> sign-in
  await next(); // -> account
  await fill("setup-field-email", "ada@example.com");
  await next(); // -> review
  await settle();
}

/**
 * Gets past step 0 onto step 1, and is a no-op where step 0 is absent.
 *
 * The flow opens on the setup-way choice, and the add-provider sequence sits
 * behind "Set it up yourself".
 */
async function chooseSelfManaged() {
  if (!find("setup-way-self-managed")) return;
  await click("setup-way-self-managed");
  await next();
}

/** Onto the managed step 1, which is the branch a verdict still gates. */
async function chooseManaged() {
  await click("setup-way-managed");
  await next();
}

describe("a host whose model answers", () => {
  it("proves the credential before the step is taken away", async () => {
    const seen: Seen = { probes: [] };
    await show(clientWith(hosted(), { seen }));

    // The whole defect in one assertion: the step may only be removed on the
    // strength of a call that was actually made.
    expect(seen.probes).toHaveLength(1);
    expect(seen.probes[0].provider).toBe("managed");
    // No key and no URL, so the host resolves its own injected credential
    // rather than one this operator was asked for.
    expect(seen.probes[0].key ?? null).toBeNull();
    expect(seen.probes[0].baseUrl ?? null).toBeNull();
  });

  it("then hides the step and shortens the bar", async () => {
    await show(clientWith(hosted()));

    expect(slots()).toEqual(["step-business", "step-signin", "step-account", "step-review"]);
    expect(container.textContent).toContain("step 1 of 4");
    expect(find("setup-provider-select")).toBeNull();
  });

  it("carries no inference block and no host key into the apply", async () => {
    const seen: Seen = { probes: [] };
    await show(clientWith(hosted(), { seen }));
    await goToReview();

    expect(stepLabel(), "the walk should have reached Review").toMatch(/^Review · step/);
    await click("setup-finish");
    await settle();

    expect(seen.apply, "setup should have been applied").toBeTruthy();
    expect(seen.apply?.company?.inference ?? null).toBeNull();
    expect(seen.apply?.fields ?? {}).not.toHaveProperty("tinyhumans_api_key");
  });

  it("says on review that the model came with the host", async () => {
    await show(clientWith(hosted()));
    await goToReview();

    expect(all("setup-host-model")).toHaveLength(1);
  });
});

describe("a host whose credential resolves but no longer reaches", () => {
  const unreachable = (seen?: Seen) =>
    clientWith(hosted(), { probe: onlyWithOwnKey, seen });

  it("keeps the model step rather than skipping the only live check", async () => {
    await show(unreachable());
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
    expect(find("setup-add-provider"), "step 1 must render").toBeTruthy();
  });

  it("shows the failure on the branch a credential is typed on", async () => {
    await show(unreachable());
    await chooseManaged();

    expect(text("setup-test-failed")).toContain(DEAD);
    // Not "This host already has a model" — it has a credential that no longer
    // works, and the operator needs to know which of the two is true.
    expect(all("setup-host-model")).toHaveLength(0);
  });

  it("still gates the managed branch, so the failure cannot be walked past", async () => {
    await show(unreachable());
    await chooseManaged();

    await next();
    expect(find("setup-problem"), "a failed connection must hold the step").toBeTruthy();
    expect(find("setup-field-key"), "and must not have left it").toBeTruthy();
  });

  it("is completable once the operator supplies a key of their own", async () => {
    const seen: Seen = { probes: [] };
    await show(unreachable(seen));
    await chooseManaged();

    await fill("setup-field-key", "sk-mine");
    await click("setup-test-connection");
    await settle();

    expect(find("setup-test-ok"), "their own key should pass").toBeTruthy();
    await next(); // -> business
    await goToReview();

    expect(stepLabel(), "the walk should have reached Review").toMatch(/^Review · step/);
    await click("setup-finish");
    await settle();
    expect(seen.apply, "setup should have been applied").toBeTruthy();
  });

  it("is completable without a model at all", async () => {
    const seen: Seen = { probes: [] };
    await show(unreachable(seen));

    await chooseSelfManaged();
    await next(); // -> business
    await goToReview();

    expect(stepLabel(), "the walk should have reached Review").toMatch(/^Review · step/);
    await click("setup-finish");
    await settle();
    expect(seen.apply, "setup should have been applied").toBeTruthy();
  });

  it("never claims on review that the host supplies the model", async () => {
    await show(unreachable());

    await chooseSelfManaged();
    await next(); // -> business
    await goToReview();

    expect(stepLabel(), "the walk should have reached Review").toMatch(/^Review · step/);
    // The line would be a promise the endpoint just refused to keep.
    expect(all("setup-host-model")).toHaveLength(0);
  });
});

describe("a host that reaches no model of its own", () => {
  it("probes nothing on its own, and asks the question as it always has", async () => {
    const seen: Seen = { probes: [] };
    await show(clientWith(status(), { seen }));

    expect(seen.probes, "nothing to prove, so nothing to call").toHaveLength(0);
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
    // Read off the page rather than a test id, so this says the same thing
    // against the flow as it stands today.
    expect(container.textContent).toContain("Connect what your team thinks with");
  });

  it("still gates the managed branch on a verdict the operator earns", async () => {
    await show(clientWith(status()));
    await chooseManaged();

    await next();
    expect(find("setup-problem"), "an untested connection must hold the step").toBeTruthy();
    expect(find("setup-field-key"), "and must not have left it").toBeTruthy();
  });
});
