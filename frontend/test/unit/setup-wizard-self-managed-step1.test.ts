// @vitest-environment jsdom

/**
 * The self-managed branch's step 1: Connections → LLM's own add-provider
 * sequence, mounted, and what it collects going where that page's adds go.
 *
 * The step used to be the model picker — a provider dropdown, a raw key field
 * and a Test button — which is a different mechanism from the one the LLM page
 * uses, produced a different shape on the wire, and could not offer the models
 * an endpoint actually publishes. This file pins that there is now one: the
 * real two dialogs, the real draft probe, and a submit that carries the add's
 * own body rather than a manifest inference block.
 *
 * Mounted rather than pure: every claim here is about what a submit carries
 * after the operator has walked the branch.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { SetupStatus } from "@/api/setup";
import { SetupWizard } from "@/views/setup/SetupWizard";

const TEMPLATE = {
  id: "law_firm",
  name: "Agentic Law Firm",
  agent_count: 5,
  output: "Filings and advice",
};

const KEY = "sk-not-a-real-key";
const MODEL = "acme/small";

function status(over: Partial<SetupStatus> = {}): SetupStatus {
  return {
    complete: false,
    config_path: "/data/config.toml",
    fields: [
      {
        key: "tinyhumans_api_key",
        value: null,
        layer: "default",
        editable: true,
        requires_restart: true,
        secret: true,
      },
    ],
    templates: [TEMPLATE],
    // `none` keeps the walk short — it removes the address step, the only one
    // that would demand an answer this file is not about.
    auth_modes: ["none", "email"],
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

interface Sent {
  /** Every path posted, in order. */
  paths: string[];
  probe?: Record<string, unknown>;
  composioCheck?: Record<string, unknown>;
  roster?: Record<string, unknown>;
  body?: Record<string, unknown>;
}

function clientWith(
  s: SetupStatus,
  sent: Sent,
  over: {
    probe?: () => Promise<unknown>;
    composioCheck?: () => Promise<unknown>;
    providerNote?: string | null;
    composioNote?: string | null;
  } = {},
): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/company",
    get: async () => s,
    post: async (path: string, body: unknown) => {
      sent.paths.push(path);
      if (path === "/api/v1/setup/inference/probe") {
        sent.probe = body as Record<string, unknown>;
        if (over.probe) return over.probe();
        return { ok: true, modelCount: 2, models: [MODEL, "acme/large"] };
      }
      if (path === "/api/v1/setup/composio/api-key/test") {
        sent.composioCheck = body as Record<string, unknown>;
        if (over.composioCheck) return over.composioCheck();
        return { ok: true };
      }
      if (path.includes("/setup/roster")) {
        sent.roster = body as Record<string, unknown>;
        return {
          agents: [{ name: "Partner", role: "Partner", description: "Advises." }],
          template: TEMPLATE.id,
          source: "preset",
          jobs: [],
          uncovered: [],
          reason: "ok",
        };
      }
      if (path === "/api/v1/setup") {
        sent.body = body as Record<string, unknown>;
        return {
          complete: true,
          config_path: s.config_path,
          restart_required: [],
          seeded_company: "agentic-law-firm",
          provider_note:
            "providerNote" in over ? over.providerNote : "Acme is connected and answering.",
          composio_note: "composioNote" in over ? over.composioNote : null,
        };
      }
      return {};
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

/** The wizard's own chrome lives in `container`; both dialogs portal to the body. */
const find = (testId: string) => container.querySelector(`[data-testid="${testId}"]`);
const anywhere = (testId: string) => document.querySelector(`[data-testid="${testId}"]`);

async function click(el: Element | null, what: string) {
  expect(el, `no element ${what}`).toBeTruthy();
  await act(async () => {
    (el as HTMLElement).click();
  });
  await act(async () => {});
}

const clickId = (testId: string) => click(anywhere(testId), testId);

async function typeInto(selector: string, value: string) {
  const el = document.querySelector(selector) as HTMLInputElement | null;
  expect(el, `no field at ${selector}`).toBeTruthy();
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  await act(async () => {
    setter?.call(el, value);
    el!.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function labelled(...wanted: string[]): HTMLButtonElement {
  const match = Array.from(container.querySelectorAll("button")).find((b) =>
    wanted.includes(b.textContent?.trim() ?? ""),
  );
  expect(match, `no button labeled ${wanted.join("/")}`).toBeTruthy();
  return match as HTMLButtonElement;
}

const next = async () => {
  await act(async () => {
    labelled("Next", "Looks good").click();
  });
  await act(async () => {});
};

const back = async () =>
  act(async () => {
    labelled("Back").click();
  });

async function pickTemplate(id: string) {
  const select = container.querySelector("select") as HTMLSelectElement;
  expect(select, "no template dropdown").toBeTruthy();
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!.call(select, id);
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

async function show(client: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(SetupWizard, { client, onDone: () => {} }));
  });
  await act(async () => {});
}

/** Step 0 (self-managed) -> step 1. */
async function selfManaged(client: OpenCompanyClient) {
  await show(client);
  await clickId("setup-way-self-managed");
  await next();
}

/**
 * The whole add sequence, through the two real dialogs: pick a provider, type
 * a key, let the probe answer, choose from the list it returned.
 */
async function connectProvider() {
  await clickId("setup-add-provider");
  expect(anywhere("inference-add-provider"), "the real picker should open").toBeTruthy();

  // The picker's cloud list is a base-ui `Select` whose popup only mounts once
  // the trigger is opened; the custom option is a plain button beside it and
  // is the cheapest honest route through the same `onChoose`.
  await clickId("inference-add-custom");
  expect(anywhere("inference-connect-provider"), "the real connect dialog should open").toBeTruthy();
  expect(
    anywhere("inference-add-provider"),
    "and the picker must close underneath it (KR-L1-02)",
  ).toBeNull();

  await typeInto("#inference-connect-name", "Acme");
  await typeInto("#inference-connect-url", "https://acme.test/v1");
  await typeInto("#inference-connect-key", KEY);
  await clickId("inference-connect-submit"); // details -> probe -> model step

  expect(anywhere("inference-connect-model-step"), "the model step should open").toBeTruthy();
  await click(document.querySelector("#inference-connect-model"), "the model field");
  const option = Array.from(document.querySelectorAll('[role="option"]')).find((row) =>
    row.textContent?.includes(MODEL),
  );
  await click(option ?? null, `the ${MODEL} option`);
  await clickId("inference-connect-submit"); // model -> staged
}

/** …and on through business -> sign-in -> review -> build. */
async function build() {
  await next();
  await pickTemplate(TEMPLATE.id);
  await next();
  await clickId("auth-mode-none");
  await next();
  await act(async () => {
    labelled("Build my company").click();
  });
  await act(async () => {});
}

describe("the self-managed branch's step 1", () => {
  it("does not hold the step — both of its connections are optional", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(clientWith(status(), sent));

    expect(find("setup-add-provider"), "the add entry point is the step").toBeTruthy();
    await next();

    expect(find("setup-problem"), "nothing here may gate Next").toBeNull();
    expect(find("setup-field-template"), "the walk should have left step 1").toBeTruthy();
    expect(find("setup-add-provider"), "and must not still be on it").toBeNull();
  });

  it("mounts the real dialogs, and probes through the first-run route", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(clientWith(status(), sent));
    await connectProvider();

    // Not the wizard's own picker, and not `POST /setup/inference/test`: the
    // same probe the LLM page's add runs, reached through the first-run gate.
    expect(sent.probe).toMatchObject({
      baseUrl: "https://acme.test/v1",
      key: KEY,
      kind: "custom",
    });
    expect(
      sent.paths.filter((path) => path.endsWith("/setup/inference/test")),
      "nothing here asks the one-model test",
    ).toHaveLength(0);
    expect(find("setup-provider-staged")?.textContent).toContain(MODEL);
  });

  it("stops on a rejected key rather than opening the model step over it", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(
      clientWith(status(), sent, {
        probe: async () => ({
          ok: false,
          class: "auth",
          message: "The provider rejected the credential.",
          modelCount: 0,
        }),
      }),
    );

    await clickId("setup-add-provider");
    await clickId("inference-add-custom");
    await typeInto("#inference-connect-name", "Acme");
    await typeInto("#inference-connect-url", "https://acme.test/v1");
    await typeInto("#inference-connect-key", "wrong-key");
    await clickId("inference-connect-submit");

    expect(anywhere("inference-connect-model-step"), "a rejected key must not advance").toBeNull();
    expect(anywhere("inference-connect-error")?.textContent).toContain("rejected the credential");
  });
});

describe("where the provider the self-managed branch connected goes", () => {
  it("is submitted as an add, not as the manifest's inference block", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(clientWith(status(), sent));
    await connectProvider();
    await build();

    // The same body `POST …/inference/providers` takes, so the host runs the
    // real add — its slot guard, its first-provider default, its rollback pair.
    expect(sent.body?.provider_draft).toMatchObject({
      kind: "custom",
      label: "Acme",
      baseUrl: "https://acme.test/v1",
      key: KEY,
      model: MODEL,
    });
    // A declared manifest provider is the legacy shape: a row nothing on the
    // LLM page put there, with no key written through the store it reads.
    const company = sent.body?.company as { inference?: unknown } | null | undefined;
    expect(company?.inference ?? null).toBeNull();
    expect(sent.body?.tinyhumans_key ?? null, "and not an account key either").toBeNull();
    // The seed decision has to agree with the payload: a connected provider is
    // a credential this submit will write, so the designed path is taken and
    // the template is not handed back whole.
    expect(company, "a designed company, not a template slug").toBeTruthy();
    expect(sent.body?.template ?? null).toBeNull();
  });

  it("hands the template back whole when nothing was connected", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(clientWith(status(), sent));
    await build();

    // The other half of the same decision. With no credential to write there
    // is nothing to trade the template's own roster, tool belt and prompts for.
    expect(sent.body?.template).toBe(TEMPLATE.id);
    expect(sent.body?.company ?? null).toBeNull();
  });

  it("shows the host's own account of what the add did", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(clientWith(status(), sent));
    await connectProvider();
    await build();

    expect(find("setup-done"), "the apply landed").toBeTruthy();
    expect(find("setup-provider-note")?.textContent).toBe("Acme is connected and answering.");
  });

  it("says nothing where the host said nothing", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(clientWith(status(), sent, { providerNote: null }));
    await next();
    await pickTemplate(TEMPLATE.id);
    await next();
    await clickId("auth-mode-none");
    await next();
    await act(async () => {
      labelled("Build my company").click();
    });
    await act(async () => {});

    expect(find("setup-done")).toBeTruthy();
    expect(find("setup-provider-note"), "no note, no line invented for it").toBeNull();
  });
});

describe("what a connected provider is worth before the company exists", () => {
  it("powers the design brief, rather than leaving the roster generic", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(clientWith(status(), sent));
    await connectProvider();
    await build();

    // The regression this catches has no symptom on screen: the design pass
    // falls back to the curated team in silence, so an operator with a working
    // key gets a generic roster and is told nothing.
    expect(sent.roster).toMatchObject({
      inferenceProvider: "custom",
      inferenceBaseUrl: "https://acme.test/v1",
      inferenceModel: MODEL,
      inferenceKey: KEY,
    });
    expect(sent.roster?.forceCurated, "there is a model, so nothing is forced").toBe(false);
  });

  it("asks the design brief's own questions once there is something to read them", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(clientWith(status(), sent));
    await connectProvider();
    await next(); // -> business

    expect(find("setup-field-automate"), "what to automate").toBeTruthy();
    expect(find("setup-field-teamHint"), "who else is needed").toBeTruthy();
  });

  it("asks only what kind of company it is when nothing was connected", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(clientWith(status(), sent));
    await next(); // -> business

    expect(find("setup-field-template"), "the one question that still counts").toBeTruthy();
    expect(find("setup-field-automate")).toBeNull();
    expect(find("setup-field-teamHint")).toBeNull();
  });
});

/**
 * Composio's own half of the step: the Connections card, the Connections
 * dialog, and two credentials that are not the same credential.
 */
async function connectComposio(row: "managed" | "byok") {
  // The real row list, from `composioRows(null)` — the managed route offers a
  // token to add, and the own-account route offers to be chosen by supplying
  // the key that makes it resolve.
  await clickId(row === "managed" ? "composio-row-managed-add" : "composio-row-byok-select");
  expect(anywhere("composio-form-dialog"), "the real credential dialog should open").toBeTruthy();
  await typeInto(
    row === "managed" ? "#composio-token" : "#composio-api-key",
    row === "managed" ? "th-not-a-real-token" : "ak-not-a-real-key",
  );
  await clickId("composio-form-save");
}

describe("the self-managed branch's Composio half", () => {
  it("submits the own-account key and the managed token as different credentials", async () => {
    const byok: Sent = { paths: [] };
    await selfManaged(clientWith(status(), byok));
    await connectComposio("byok");
    await build();
    expect(byok.body?.composio_draft).toEqual({
      credential: "composio-api-key",
      value: "ak-not-a-real-key",
    });
    // The own-account key is the one the host can check, and it was.
    expect(byok.composioCheck).toEqual({ apiKey: "ak-not-a-real-key" });

    const managed: Sent = { paths: [] };
    await act(async () => root.unmount());
    root = createRoot(container);
    await selfManaged(clientWith(status(), managed));
    await connectComposio("managed");
    await build();
    expect(managed.body?.composio_draft).toEqual({
      credential: "composio-token",
      value: "th-not-a-real-token",
    });
    // The managed route's token is a bearer the TinyHumans backend issues, and
    // no cheap call tells a bad one apart from a backend that is down — so
    // there is no check to run, and none is run.
    expect(managed.composioCheck, "a token has no check to fail").toBeUndefined();
  });

  it("holds the dialog open on a rejected key rather than staging it", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(
      clientWith(status(), sent, {
        composioCheck: async () => ({
          ok: false,
          probeClass: "auth",
          message: "Composio rejected that key.",
        }),
      }),
    );
    await connectComposio("byok");

    expect(anywhere("composio-form-dialog"), "a rejected key must not close the form").toBeTruthy();
    expect(find("setup-composio-staged")).toBeNull();
    expect(document.body.textContent).toContain("Composio rejected that key.");
  });

  it("stages a key the check could not reach, and says so beside it", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(
      clientWith(status(), sent, {
        composioCheck: async () => ({
          ok: false,
          probeClass: "unknown",
          message: "The check did not complete.",
        }),
      }),
    );
    await connectComposio("byok");

    // Non-destructive: Composio was unreachable, or this build has no client to
    // check with. The Connections page stores on that class and reports the
    // reason, and refusing here would leave a default build unable to stage a
    // key at all — the check can only ever answer this there.
    expect(anywhere("composio-form-dialog"), "the form is done with").toBeNull();
    expect(find("setup-composio-staged"), "the key is staged").toBeTruthy();
    expect(find("setup-composio-advisory")?.textContent).toBe("The check did not complete.");
  });

  it("is skippable on its own, and so is the provider", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(clientWith(status(), sent));

    // Connect a provider, then say "later" to Composio. The two answers stand
    // apart: deferring one must leave the other exactly as it was.
    await connectProvider();
    await clickId("setup-composio-later");
    expect(find("setup-composio-later-note")).toBeTruthy();
    expect(
      find("setup-provider-staged"),
      "deferring Composio must not unstage the provider",
    ).toBeTruthy();

    await build();
    expect(sent.body?.provider_draft).toBeTruthy();
    expect(sent.body?.composio_draft ?? null).toBeNull();
  });

  it("finishes with both skipped, recording neither", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(clientWith(status(), sent));
    await clickId("setup-provider-later");
    await clickId("setup-composio-later");
    await build();

    expect(find("setup-done"), "skipping both must still finish").toBeTruthy();
    // `null` is the honest state of a company minutes old. There is no
    // "explicitly deferred" to record, and nothing downstream could act on it.
    expect(sent.body?.provider_draft ?? null).toBeNull();
    expect(sent.body?.composio_draft ?? null).toBeNull();
    // And with nothing written, nothing is claimed on the way out.
    expect(find("setup-composio-note"), "no note, no line invented for it").toBeNull();
  });

  it("shows the host's own account of what the credential did", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(
      clientWith(status(), sent, {
        composioNote: "This company reaches its tools through its own Composio account.",
      }),
    );
    await connectComposio("byok");
    await build();

    expect(find("setup-composio-note")?.textContent).toBe(
      "This company reaches its tools through its own Composio account.",
    );
  });
});

describe("a draft that belongs to a branch the operator has left", () => {
  it("is cleared when the setup way changes", async () => {
    const sent: Sent = { paths: [] };
    await selfManaged(clientWith(status(), sent));
    await connectProvider();
    await connectComposio("byok");
    expect(find("setup-provider-staged"), "staged before the switch").toBeTruthy();
    expect(find("setup-composio-staged"), "and so was Composio").toBeTruthy();

    await back();
    await clickId("setup-way-managed");
    await back();
    await clickId("setup-way-self-managed");
    await next();

    expect(
      find("setup-provider-staged"),
      "a provider connected under the other way must not survive",
    ).toBeNull();
    expect(
      find("setup-composio-staged"),
      "and neither must a Composio credential",
    ).toBeNull();
  });

  /**
   * The step unmounts with the branch, so the probe's answer has nowhere to
   * land and the re-entered step is a fresh one. Stated as a test rather than
   * left to be re-derived: the staging call is synchronous *today*, and the
   * generation guard on it is insurance against the day it is not.
   */
  it("leaves nothing staged when its probe settles after the way has changed", async () => {
    let release!: (value: unknown) => void;
    const inFlight = new Promise((resolve) => {
      release = resolve;
    });
    const sent: Sent = { paths: [] };
    await selfManaged(clientWith(status(), sent, { probe: () => inFlight }));

    await clickId("setup-add-provider");
    await clickId("inference-add-custom");
    await typeInto("#inference-connect-name", "Acme");
    await typeInto("#inference-connect-url", "https://acme.test/v1");
    await typeInto("#inference-connect-key", KEY);
    await clickId("inference-connect-submit"); // probe in flight

    // Out of the branch before it settles.
    await back();
    await clickId("setup-way-managed");
    await act(async () => {
      release({ ok: true, modelCount: 1, models: [MODEL] });
      await inFlight;
    });
    await act(async () => {});

    await back();
    await clickId("setup-way-self-managed");
    await next();

    expect(
      find("setup-provider-staged"),
      "a probe asked under the abandoned branch must not stage anything",
    ).toBeNull();
    expect(
      anywhere("inference-connect-model-step"),
      "and the model step it would have opened is gone with it",
    ).toBeNull();
  });
});
