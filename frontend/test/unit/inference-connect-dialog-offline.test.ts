// @vitest-environment jsdom

// The claim the wizard's self-managed step 1 rests on, turned into a contract.
//
// `ProviderConnectDialog` takes a `client` and a `company`, which reads as
// "this needs a company" — and on the add path it does not. Its only
// company-scoped call is the edit step's draft probe, gated on
// `step !== "edit"`, and its `ModelField` is handed `slug={null}` with the
// catalogue already in hand, so nothing is fetched. That is what lets the
// first-run wizard mount it before any company exists.
//
// Read off the source it is an observation; here it is a test that fails if a
// later edit reaches for the client on this path. The client below throws from
// every method, so any call at all is the failure.

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { ProviderConnectDialog } from "@/inference/ProviderConnectDialog";
import type { ConnectDraft, ModelAsk } from "@/inference/ProviderConnectDialog";

let container: HTMLDivElement;
let root: Root;

/**
 * Every method records itself and then throws.
 *
 * Recorded as well as thrown because a throw alone is not enough: the edit
 * step's probe is debounced inside a timer, so a call that should not happen
 * would surface as an uncaught timer error at some later moment rather than as
 * this test failing. The list is checked directly.
 */
function hostileClient(calls: string[]): OpenCompanyClient {
  const refuse = (name: string) => () => {
    calls.push(name);
    throw new Error(`the connect dialog called ${name} with no company`);
  };
  return {
    scopeFor: refuse("scopeFor"),
    get: refuse("get"),
    post: refuse("post"),
    put: refuse("put"),
    del: refuse("del"),
    patch: refuse("patch"),
  } as unknown as OpenCompanyClient;
}

/** Past the connect dialog's own 400ms probe debounce. */
async function pastTheDebounce() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 450));
  });
}

const CATALOGUE: ModelAsk = { models: ["acme/small", "acme/large"], freeTextOnly: false };

function testId(id: string) {
  return document.querySelector(`[data-testid="${id}"]`);
}

function field(selector: string) {
  return document.querySelector(selector) as HTMLInputElement | null;
}

async function click(el: Element | null) {
  expect(el, "nothing to click").toBeTruthy();
  await act(async () => {
    (el as HTMLElement).click();
  });
  await act(async () => {});
}

async function typeInto(selector: string, value: string) {
  const el = field(selector);
  expect(el, `nothing to type into at ${selector}`).toBeTruthy();
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  await act(async () => {
    setter?.call(el, value);
    el!.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

/** Mounts the dialog the way the wizard mounts it: adding, no company. */
async function mount(over: {
  optionSlug: string;
  calls: string[];
  modelAsk?: ModelAsk | null;
  onSubmit?: (draft: ConnectDraft) => void;
}) {
  await act(async () => {
    root.render(
      createElement(ProviderConnectDialog, {
        client: hostileClient(over.calls),
        company: null,
        optionSlug: over.optionSlug,
        providers: [],
        editing: null,
        busy: false,
        error: null,
        offerAddAnyway: false,
        modelAsk: over.modelAsk ?? null,
        noDefaultYet: true,
        onCancel: () => {},
        onBack: () => {},
        onSubmit: over.onSubmit ?? (() => {}),
      }),
    );
  });
  await act(async () => {});
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

describe("the connect dialog on the add path, with no company behind it", () => {
  it("renders the details step and asks the host for nothing", async () => {
    const calls: string[] = [];
    await mount({ optionSlug: "openai", calls });

    expect(testId("inference-connect-provider"), "the dialog should be open").toBeTruthy();
    expect(field("#inference-connect-key"), "a cloud provider asks for a key").toBeTruthy();

    // A typed key is what would arm the edit step's own draft probe, if that
    // probe were not gated on the edit step. Waited out rather than assumed:
    // it is debounced, so a call would otherwise land after this test ended.
    await typeInto("#inference-connect-key", "sk-not-a-real-key");
    await pastTheDebounce();
    expect(calls, "the add path is offline").toEqual([]);
  });

  it("walks details -> model on a supplied catalogue, still calling nothing", async () => {
    const calls: string[] = [];
    const submitted: ConnectDraft[] = [];
    await mount({ optionSlug: "openai", calls, onSubmit: (draft) => submitted.push(draft) });

    await typeInto("#inference-connect-key", "sk-not-a-real-key");
    await click(testId("inference-connect-submit"));
    expect(submitted, "step 1 hands the draft back to its caller").toHaveLength(1);

    // The caller probes and re-renders with the answer, which is the move the
    // wizard's step makes. The model field is in list mode from here, so it
    // reads no catalogue of its own.
    await mount({
      optionSlug: "openai",
      calls,
      modelAsk: CATALOGUE,
      onSubmit: (draft) => submitted.push(draft),
    });
    expect(testId("inference-connect-model-step"), "the model step should open").toBeTruthy();

    // The catalogue is offered as a list because one was supplied — which is
    // the whole point: the field is in list mode and reads nothing itself.
    await click(document.querySelector("#inference-connect-model"));
    const option = Array.from(document.querySelectorAll('[role="option"]')).find(
      (row) => row.textContent?.includes("acme/small"),
    );
    await click(option ?? null);

    await click(testId("inference-connect-submit"));
    expect(submitted).toHaveLength(2);
    expect(submitted[1].model).toBe("acme/small");
    // The details step stays mounted behind the model step, so its key travels
    // with the second submit rather than having to be typed again.
    expect(submitted[1].key).toBe("sk-not-a-real-key");

    await pastTheDebounce();
    expect(calls, "nothing on this path reads the host").toEqual([]);
  });

  it("holds the details step until a local runtime has an endpoint", async () => {
    // The gate the wizard's step no longer owns: it moved here with the
    // dialog, and an Ollama with no address is the case it was written for.
    const calls: string[] = [];
    await mount({ optionSlug: "ollama", calls });

    const submit = testId("inference-connect-submit") as HTMLButtonElement;
    const url = field("#inference-connect-url")!;
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      setter?.call(url, "");
      url.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(submit.disabled, "no endpoint, nothing to connect").toBe(true);

    await typeInto("#inference-connect-url", "http://127.0.0.1:11434/v1");
    expect((testId("inference-connect-submit") as HTMLButtonElement).disabled).toBe(false);

    await pastTheDebounce();
    expect(calls).toEqual([]);
  });
});
