// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { SetupField, SetupStatus } from "@/api/setup";
import { SetupWizard } from "@/views/setup/SetupWizard";

/**
 * The first screen is a choice, not a model-picker.
 *
 * "How would you like to set this up?" decides which of two step-1 screens the
 * operator sees, and the branch is expressed as a filter over the flat step
 * list: the unchosen branch has no slot in the progress bar, the same way an
 * address step a host will never need has none.
 *
 * What this file pins is the branch itself — that a way has to be answered,
 * that exactly one step-1 follows from it, that changing the answer does not
 * carry the other branch's credential across, and that the question is not put
 * to hosts for whom it has only one answer.
 */

function status(over: Partial<SetupStatus> = {}): SetupStatus {
  return {
    complete: false,
    config_path: "/data/config.toml",
    fields: [],
    templates: [],
    auth_modes: ["email", "none"],
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

/** The host's own `tinyhumans_api_key` row, as `GET /setup` reports it. */
function keyField(over: Partial<SetupField> = {}): SetupField {
  return {
    key: "tinyhumans_api_key",
    value: null,
    layer: "config.toml",
    editable: true,
    requires_restart: false,
    secret: true,
    ...over,
  };
}

function clientWith(
  s: SetupStatus,
  probe: (() => Promise<unknown>) | null = null,
): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/company",
    get: async () => s,
    post: async (path: string) => {
      if (path.endsWith("/inference/test")) {
        return probe
          ? probe()
          : { ok: true, baseUrl: "https://api.example/v1", model: "m" };
      }
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

async function click(testId: string) {
  const el = find(testId) as HTMLElement | null;
  expect(el, `no element ${testId}`).toBeTruthy();
  await act(async () => {
    el!.click();
  });
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

const back = async () =>
  act(async () => {
    labelled("Back").click();
  });

const settle = async () =>
  act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });

async function fill(testId: string, value: string) {
  const field = find(testId) as HTMLInputElement | null;
  expect(field, `no field ${testId}`).toBeTruthy();
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(field, value);
    field!.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

/** The slots the progress bar is actually drawing. */
const slots = () =>
  Array.from(container.querySelectorAll("[data-testid^='step-']")).map((el) =>
    el.getAttribute("data-testid"),
  );

describe("the setup-way choice", () => {
  it("is the first screen, and holds the flow until it is answered", async () => {
    await show(clientWith(status()));

    expect(find("setup-question")?.textContent).toContain("set this up");
    expect(find("setup-way-managed")).toBeTruthy();
    expect(find("setup-way-self-managed")).toBeTruthy();
    // Neither step-1 has a slot yet: which one follows is not known until the
    // question is answered, and a bar drawing both would count a screen the
    // operator will never be shown.
    expect(slots()).toEqual([
      "step-setup-way",
      "step-business",
      "step-signin",
      "step-account",
      "step-review",
    ]);

    await next();
    expect(find("setup-problem"), "an unanswered choice must hold the step").toBeTruthy();
    expect(find("setup-way-managed"), "and must not have left it").toBeTruthy();
  });

  it("follows Managed with the managed step-1 only", async () => {
    await show(clientWith(status()));
    await click("setup-way-managed");

    expect(slots()).toEqual([
      "step-setup-way",
      "step-managed-login",
      "step-business",
      "step-signin",
      "step-account",
      "step-review",
    ]);

    await next();
    // The managed step-1 is the Connect-to-TinyHumans screen, which has no
    // provider to pick — taking the managed way already picked it.
    expect(find("setup-key-get-link"), "the press should have reached step 1").toBeTruthy();
    expect(find("setup-provider-select"), "and managed has no provider choice").toBeNull();
  });

  it("follows Set it up yourself with the self-managed step-1 only", async () => {
    await show(clientWith(status()));
    await click("setup-way-self-managed");

    expect(slots()).toEqual([
      "step-setup-way",
      "step-self-managed-connect",
      "step-business",
      "step-signin",
      "step-account",
      "step-review",
    ]);

    await next();
    // The self-managed step-1 is Connections → LLM's own add-provider
    // sequence, so what lands is its entry point rather than a picker of the
    // wizard's own.
    expect(find("setup-add-provider"), "the press should have reached step 1").toBeTruthy();
  });

  it("does not carry one branch's credential into the other", async () => {
    // Out through the other way and back. A key typed against one branch would
    // otherwise be presented to it again under a verdict that was thrown away
    // in between — and the step would release on a tick nobody re-earned.
    await show(clientWith(status()));
    await click("setup-way-managed");
    await next();
    await fill("setup-field-key", "th-not-a-real-key");
    await click("setup-test-connection");
    await settle();
    expect(find("setup-test-ok"), "the key should have passed").toBeTruthy();

    await back();
    await click("setup-way-self-managed");
    await back();
    await click("setup-way-managed");
    await next();

    expect((find("setup-field-key") as HTMLInputElement | null)?.value).toBe("");
    expect(find("setup-test-ok"), "the other branch's verdict must not survive").toBeNull();
    await next();
    expect(find("setup-problem"), "an untested connection must hold the step").toBeTruthy();
  });

  it("ignores a connection test that settles after the way has changed", async () => {
    // The test is asked under Managed, the operator backs out and picks Set it
    // up yourself before it settles, and only then does the response arrive.
    // The verdict is about a branch the operator already left, so it must not
    // apply to the one they are on now — and the step's own staleness rule
    // cannot catch this, because switching away unmounts the step with exactly
    // the answers the test was asked with.
    let resolveTest!: (value: unknown) => void;
    const pending = new Promise((resolve) => {
      resolveTest = resolve;
    });
    await show(clientWith(status(), () => pending));
    await click("setup-way-managed");
    await next();
    await fill("setup-field-key", "th-not-a-real-key");
    await click("setup-test-connection");

    await back();
    await click("setup-way-self-managed");

    await act(async () => {
      resolveTest({ ok: true, baseUrl: "https://api.example/v1", model: "m" });
      await pending;
    });
    await settle();

    // Read off the consequence rather than the verdict, because the verdict is
    // not on screen here: a live `ok` would mean this branch believes it has a
    // model, and the Business step would ask for the design brief that only a
    // model reads — from an operator who has connected nothing.
    await next(); // -> step 1, nothing connected
    await next(); // -> business
    expect(
      find("setup-field-automate"),
      "a verdict asked under the abandoned branch must not land on this one",
    ).toBeNull();
    expect(find("setup-field-teamHint")).toBeNull();
  });
});

describe("a host the managed way cannot be completed on", () => {
  it("is not offered it, and is not asked a question with one answer", async () => {
    // The host owns the TinyHumans key from its environment, so a pasted one
    // would be refused — the branch would dead-end on its own step 1.
    await show(clientWith(status({ fields: [keyField({ layer: "env", editable: false })] })));

    expect(find("setup-way-managed")).toBeNull();
    expect(slots()).toEqual([
      "step-self-managed-connect",
      "step-business",
      "step-signin",
      "step-account",
      "step-review",
    ]);
    expect(find("setup-add-provider"), "it opens on step 1 instead").toBeTruthy();
  });

  it("is offered it when the host will take a key", async () => {
    // The control for the case above: same field, editable.
    await show(clientWith(status({ fields: [keyField()] })));

    expect(find("setup-way-managed")).toBeTruthy();
    expect(slots()[0]).toBe("step-setup-way");
  });
});

describe("an instance that has already been configured", () => {
  it("is not asked the way question again", async () => {
    // A re-run is an edit of a configuration that exists. How it was set up the
    // first time is not back on the table, and the wizard reopens at the first
    // visible step — which must not be the branch point.
    await show(clientWith(status({ complete: true })));

    expect(find("setup-way-managed")).toBeNull();
    expect(find("setup-way-self-managed")).toBeNull();
    expect(slots()).toEqual([
      "step-self-managed-connect",
      "step-business",
      "step-signin",
      "step-account",
      "step-review",
    ]);
    expect(find("setup-add-provider"), "step 1 is still reachable").toBeTruthy();
  });
});

describe("a host whose own model answers", () => {
  it("is asked the way while the probe is in flight, and not once it lands", async () => {
    // The probe settles `hosted`, which takes the whole branch away — step 0
    // and both step-1s, not just the model screen. Until it settles the host's
    // model is an unproven claim, so the question stands rather than a bar
    // changing length under someone already reading it.
    let release!: (value: unknown) => void;
    const inFlight = new Promise((resolve) => {
      release = resolve;
    });
    await show(
      clientWith(
        status({ inference: { ready: true, provider: "managed", base_url: "https://api.example/v1" } }),
        () => inFlight,
      ),
    );

    expect(find("setup-way-managed"), "the way is asked while nothing is proven").toBeTruthy();

    await act(async () => {
      release({ ok: true, baseUrl: "https://api.example/v1", model: "m" });
      await inFlight;
    });

    expect(find("setup-way-managed")).toBeNull();
    expect(slots()).toEqual([
      "step-business",
      "step-signin",
      "step-account",
      "step-review",
    ]);
    expect(find("setup-field-industry"), "it lands on step 2").toBeTruthy();
  });
});
