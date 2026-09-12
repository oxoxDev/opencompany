// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { InferenceStatus } from "@/api/inference";
import { InferenceSection } from "@/views/connections/InferenceSection";

/**
 * The Inference card must not say two things at once (issues #1736, #1737).
 *
 * Both defects are the same defect: the card knew a fact about the host or
 * about the stored configuration and rendered something that contradicted it.
 * It offered a "Restart now" button on hosts where the route behind it can only
 * fail, and it rendered a Provider select that was a constant while the header
 * beside it rendered whatever the host actually holds.
 */

let container: HTMLDivElement;
let root: Root;

/** A status with everything nailed down; each test varies only what it is about. */
function status(over: Partial<InferenceStatus> = {}): InferenceStatus {
  return {
    provider: "managed",
    slug: "managed",
    baseUrl: "https://openrouter.ai/api/v1",
    models: {},
    defaultTierModels: {},
    source: "runtime",
    keyConfigured: true,
    cognition: "echo",
    usageMetering: "none",
    restartRequired: true,
    harnessReachable: true,
    canRebuildInPlace: true,
    ...over,
  };
}

/**
 * A client stub answering `GET …/inference` from a queue, so a test can stage
 * what the host holds before a save and what it holds after one.
 */
function stubClient(replies: InferenceStatus[], mutation?: InferenceStatus) {
  let reads = 0;
  const read = () => replies[Math.min(reads++, replies.length - 1)];
  const settled = () => mutation ?? replies[replies.length - 1];
  return {
    scopeFor: (company: string | null) =>
      company ? `/api/v1/companies/${company}` : "/api/v1/company",
    // The catalog route answers with an object naming the endpoint that was
    // read, not a bare array (`InferenceModelCatalog`). These tests assert
    // nothing about the picker, so the stub answers the host's *unreadable
    // catalog* reply — a 200 carrying `error`, with no `tierVocabulary`,
    // because "we could not ask" is not the same fact as `"unknown"`.
    //
    // Deliberately not `{models: [], tierVocabulary: "unknown"}`: the host
    // cannot produce that pairing. `list_models` only sets a vocabulary in its
    // success arm, and `catalog_models` treats an empty catalog as a failure,
    // so an empty list always arrives with `error` set and no vocabulary. A
    // double that answered a shape the host cannot emit would let these tests
    // pass on behaviour nothing real can reach.
    get: async (path: string) =>
      path.endsWith("/inference/models")
        ? {
            baseUrl: "https://openrouter.ai/api/v1",
            models: [],
            tierDefaults: {},
            error:
              "Could not list models from https://openrouter.ai/api/v1: connection refused. " +
              "Enter model ids directly.",
          }
        : read(),
    put: async () => ({ status: settled(), note: "" }),
    del: async () => ({ status: settled(), note: "" }),
    post: async () => ({ status: settled(), note: "" }),
  } as unknown as OpenCompanyClient;
}

async function mount(client: OpenCompanyClient, canManage = true) {
  await act(async () => {
    root.render(createElement(InferenceSection, { client, company: "acme", canManage }));
  });
}

function testId(id: string) {
  return container.querySelector(`[data-testid="${id}"]`);
}

/**
 * What the Provider select currently reads, as the operator sees it. The
 * trigger renders its own chevron glyph into the same text node, so strip
 * anything that is not part of a provider label.
 */
function providerSelect(): string {
  const text = container.querySelector("#inference-provider")?.textContent ?? "";
  return text.replace(/[^\w\s()-]/g, "").trim();
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

describe("the restart notice offers an action only where one exists (issue #1736)", () => {
  it("offers Restart now when the host can rebuild a runtime in place", async () => {
    await mount(stubClient([status({ canRebuildInPlace: true })]));

    expect(testId("inference-restart-required")).not.toBeNull();
    expect(testId("inference-restart-now")).not.toBeNull();
    expect(testId("inference-restart-manual")).toBeNull();
  });

  it("withholds the button and names the real remedy when the host cannot", async () => {
    // `POST …/inference/restart` needs a `RuntimeRebuilder` wired into the
    // host. Where none is, it fails unconditionally with "this host cannot
    // rebuild a company runtime in place" — so a button here is a control whose
    // only possible outcome is a toast the operator can do nothing about.
    await mount(stubClient([status({ canRebuildInPlace: false })]));

    expect(testId("inference-restart-required")).not.toBeNull();
    expect(testId("inference-restart-now")).toBeNull();

    const manual = testId("inference-restart-manual");
    expect(manual).not.toBeNull();
    // The remedy, in both spellings the host could mean — the capability comes
    // from the host, which does not know which shell it is packaged in.
    expect(manual?.textContent).toContain("quit and reopen the app");
    expect(manual?.textContent).toContain("restart the server process");
  });

  it("keeps the notice itself either way — the restart is still required", async () => {
    await mount(stubClient([status({ canRebuildInPlace: false })]), false);
    expect(testId("inference-restart-required")?.textContent).toContain("Restart required.");
  });
});

describe("the Provider select shows the provider the host holds (issue #1737)", () => {
  it("opens on the stored provider rather than a hardcoded default", async () => {
    // The select was `useState("managed")` with nothing ever writing it back, so
    // it read "Managed (TinyHumans)" whatever was stored — including after a
    // full process restart, which just re-runs the same initializer. The header
    // beside it renders the host's provider, so the two disagreed on one card.
    await mount(stubClient([status({ provider: "openrouter" })]));
    expect(providerSelect()).toBe("OpenRouter");
  });

  it("follows the host to a provider it normalized on the way in", async () => {
    // `managed` is a legacy alias the host resolves to `openrouter`. The value
    // the select shows has to be the value the host came back with, or an
    // operator sees one vendor named in the header and another in the select
    // while their key is stored against exactly one of them.
    await mount(stubClient([status({ provider: "openai_compatible" })]));
    expect(providerSelect()).toBe("Custom (OpenAI-compatible)");
  });

  it("names the managed route in both places when the host reports the proxy", async () => {
    // The header and the select read the same status from two different fields:
    // the header from the label, the select from `provider`. `managed` is
    // normalized to `openrouter` on the way in, so a company on the managed card
    // came back as `openrouter` and the select rested there while the header
    // named TinyHumans — one card, two answers, for one configuration.
    //
    // `slug` is the host saying which route the traffic actually takes, so both
    // read it and cannot disagree.
    await mount(stubClient([status({ provider: "openrouter", slug: "subscription" })]));

    expect(providerSelect()).toBe("Managed (TinyHumans)");
    expect(testId("inference-current-provider")?.textContent).toBe("Managed (TinyHumans)");
  });

  it("still names OpenRouter when the traffic really is direct", async () => {
    await mount(stubClient([status({ provider: "openrouter", slug: "openrouter" })]));

    expect(providerSelect()).toBe("OpenRouter");
    expect(testId("inference-current-provider")?.textContent).toBe("OpenRouter");
  });

  it("says nothing is unsaved when the form still matches the host", async () => {
    await mount(stubClient([status({ provider: "openrouter", slug: "subscription" })]));
    expect(testId("inference-unsaved")).toBeNull();
  });

  it("marks the form unsaved once the draft leaves what is running", async () => {
    // The header reports the running config and the select reports the draft,
    // so the two legitimately differ mid-edit. Without this cue that gap is
    // indistinguishable from the card contradicting itself, which it used to do.
    await mount(stubClient([status({ provider: "openrouter", slug: "subscription" })]));
    expect(testId("inference-unsaved")).toBeNull();

    const field = container.querySelector("#inference-key") as HTMLInputElement;
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )?.set;
      setter?.call(field, "sk-or-typed-but-not-saved");
      field.dispatchEvent(new Event("input", { bubbles: true }));
    });

    expect(testId("inference-unsaved")?.textContent).toContain("Unsaved");
  });

  it("rehydrates after a save rather than snapping back to the default", async () => {
    // The reported sequence: save under one provider, and the select goes on
    // reading the initializer's value while the header reads the saved one.
    //
    // Was staged from `managed`, which this console no longer offers as a route
    // — its select row is the disabled "Not configured" stand-in and Save is
    // withheld there, so the save under test could not fire. The defect was
    // never about which two providers: it was the select not re-reading the
    // host, which two offered providers exercise exactly as well.
    const client = stubClient(
      [status({ provider: "openai_compatible" }), status({ provider: "openrouter" })],
      status({ provider: "openrouter" }),
    );
    await mount(client);
    expect(providerSelect()).toBe("Custom (OpenAI-compatible)");

    await act(async () => {
      (testId("inference-save") as HTMLButtonElement).click();
    });
    await act(async () => {});

    expect(providerSelect()).toBe("OpenRouter");
  });

  it("names the key that belongs in the field, for the provider it is stored against", async () => {
    // This line asked for a TinyHumans key — true when `managed` was a provider
    // of its own, and never updated when it stopped being one. It is what the
    // reported 401 actually was.
    //
    // Asserted against OpenRouter rather than `managed`: the key field is
    // withheld entirely for a route this console does not offer, so the note
    // has no rendering there to check. What the test is for — the note naming
    // the key of the provider it is stored against — is unchanged.
    await mount(stubClient([status({ provider: "openrouter" })]));
    expect(testId("inference-key-note")?.textContent).toContain("an OpenRouter key");
  });
});
