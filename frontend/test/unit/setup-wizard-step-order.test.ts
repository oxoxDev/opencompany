// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { SetupStatus } from "@/api/setup";
import { SetupWizard } from "@/views/setup/SetupWizard";

/**
 * The sign-in question comes before its consequences.
 *
 * The wizard used to ask "what's your email?" one screen before it offered the
 * choice that decides whether an address is needed at all — and it offered that
 * choice buried inside Advanced, under copy inviting the operator to press
 * straight past it. So an operator on a laptop was asked for an address they
 * would never have had to supply had they seen that "no sign-in" was on the
 * table.
 *
 * What this file pins is therefore about *order and visibility*, not about any
 * one screen: the mode is asked first, and a host told it needs no sign-in is
 * never shown the address step at all — not disabled, not optional, absent,
 * with no slot of its own in the progress bar.
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
 * "Set it up yourself" — so a walk starting at the model step answers that
 * first, while one that has navigated back to it must not answer it again.
 */
async function chooseSelfManaged() {
  if (!find("setup-way-self-managed")) return;
  await click("setup-way-self-managed");
  await next();
}

/**
 * Gets onto the managed step 1, which is where a credential is still typed and
 * proved — the self-managed branch stages a provider instead.
 */
async function connectManaged() {
  await click("setup-way-managed");
  await next();
  await fill("setup-field-key", "th-not-a-real-key");
  await click("setup-test-connection");
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

/** "Looks good" is the same control under a different word, on Advanced. */
/** The wizard's own progress line, e.g. `Review · step 4 of 4`. */
function stepLabel(): string {
  const match = container.textContent?.match(/(\w[\w -]*) · step \d+ of \d+/);
  return match ? match[0] : "";
}

const next = async () =>
  act(async () => {
    labelled("Next", "Looks good").click();
  });

const back = async () =>
  act(async () => {
    labelled("Back").click();
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
  await next(); // -> business
  await fill("setup-field-industry", "E-commerce — homeware");
  await next(); // -> sign-in
}

/** The slots the progress bar is actually drawing. */
const slots = () =>
  Array.from(container.querySelectorAll("[data-testid^='step-']")).map((el) =>
    el.getAttribute("data-testid"),
  );

describe("where the sign-in question sits", () => {
  it("asks how people sign in before it asks for an address", async () => {
    await show(clientWith(status()));
    await goToSignIn();

    expect(find("auth-mode-email")).toBeTruthy();
    // The consequence has not been asked for yet.
    expect(find("setup-field-email")).toBeNull();

    await next();
    expect(find("setup-field-email")).toBeTruthy();
  });

  it("gives the sign-in question its own heading, not Advanced's", async () => {
    // It was written to sit inside the Advanced accordion, where the group
    // header supplied the question. Standing on its own it has to ask it.
    await show(clientWith(status()));
    await goToSignIn();

    expect(find("setup-question")?.textContent).toContain("sign in");
    expect(find("setup-advanced")).toBeNull();
  });

  it("has no Advanced step left to offer the mode a second time in", async () => {
    // Advanced held one group — how this host runs — and hiding it leaves the
    // step with nothing on it, so the step goes too rather than rendering blank.
    await show(clientWith(status()));
    await goToSignIn();
    await next(); // -> account
    await fill("setup-field-email", "ada@example.com");
    await next(); // -> review

    // Landed on Review, asserted before anything about what is absent. Without
    // this the three checks below would also pass if the press had gone nowhere
    // — a test that cannot fail for the reason it exists.
    //
    // Read off the step label rather than `setup-review`: entering Review kicks
    // off the roster design, so the screen is its spinner first and the review
    // body only once that settles. The label is true in both.
    expect(stepLabel(), "the press should have reached Review").toMatch(/^Review · step/);
    expect(find("setup-advanced")).toBeNull();
    expect(find("auth-mode-email")).toBeNull();
    expect(find("field-auth_mode")).toBeNull();
  });
});

describe("a step this host does not need gets no slot", () => {
  it("draws six steps on a host that asks people to sign in", async () => {
    await show(clientWith(status()));
    await goToSignIn();

    expect(slots()).toEqual([
      "step-setup-way",
      "step-self-managed-connect",
      "step-business",
      "step-signin",
      "step-account",
      "step-review",
    ]);
    expect(container.textContent).toContain("step 4 of 6");
  });

  it("draws five, and never the address field, once no sign-in is chosen", async () => {
    await show(clientWith(status()));
    await goToSignIn();
    await click("auth-mode-none");

    expect(slots()).toEqual([
      "step-setup-way",
      "step-self-managed-connect",
      "step-business",
      "step-signin",
      "step-review",
    ]);
    // The bar must renumber too: a five-step flow that says "of 6" is telling
    // the operator about a screen they will never be shown.
    expect(container.textContent).toContain("step 4 of 5");

    await next();
    // Review, not "wherever the press left us": absence proves nothing about a
    // screen that never changed.
    expect(stepLabel(), "the press should have reached Review").toMatch(/^Review · step/);
    expect(find("setup-advanced")).toBeNull();
    expect(find("setup-field-email")).toBeNull();
  });
});

describe("the position survives the list changing under it", () => {
  /**
   * The reason the wizard holds *which step* rather than *which index*.
   *
   * Choosing a mode adds or removes a screen behind the operator's back, and an
   * index means a different screen the moment the list changes length. Here the
   * flow goes forward under `none`, comes back, and switches to `email` — after
   * which the very next press must land on the address it now needs, not skip
   * over it into Advanced.
   */
  it("lands on the address step after switching back to email sign-in", async () => {
    await show(clientWith(status()));
    await goToSignIn();
    await click("auth-mode-none");
    await next(); // -> review, with no address step in the list
    await back(); // -> sign-in
    expect(find("auth-mode-email")).toBeTruthy();

    await click("auth-mode-email");
    await next();

    expect(find("setup-field-email")).toBeTruthy();
    expect(find("setup-advanced")).toBeNull();
  });
});

/**
 * What the Business step asks for depends on whether anything will read it.
 *
 * `automate` and `teamHint` are the design brief: the first becomes a numbered
 * job list the roster is designed against and checked for coverage, the second
 * a request added on top. Neither is acted on without a model — the host falls
 * back to a curated team matched on keywords, where the two score one point
 * each against `industry`'s three.
 *
 * So a run with no model asks neither, rather than collecting a description of
 * somebody's business under copy promising their team is built around it and
 * then staffing them from a keyword match.
 */
describe("the questions a modelless run does not ask", () => {
  it("asks only what kind of company it is when there is no model", async () => {
    await show(clientWith(status()));
    await skipConnect();
    await next(); // -> business

    expect(find("setup-field-industry"), "the one question that still counts").toBeTruthy();
    expect(find("setup-field-automate")).toBeNull();
    expect(find("setup-field-teamHint")).toBeNull();
  });

  it("asks all three once a model answers for them", async () => {
    // A passing test on step 1 is what makes the design brief real, so this
    // client has to answer the probe rather than the apply.
    const client = {
      scopeFor: () => "/api/v1/company",
      get: async () => status(),
      post: async (path: string) =>
        path.endsWith("/inference/test")
          ? { ok: true, baseUrl: "https://api.example/v1", model: "m" }
          : { complete: true, config_path: "/data/config.toml", restart_required: [] },
    } as unknown as OpenCompanyClient;
    await show(client);
    await connectManaged();
    await next(); // -> business

    expect(find("setup-field-industry")).toBeTruthy();
    expect(find("setup-field-automate")).toBeTruthy();
    expect(find("setup-field-teamHint")).toBeTruthy();
  });
});

/**
 * What "No model" is worth against a host that has one anyway.
 *
 * Sending no credential does not mean "no model": the host reads an absent one
 * as "use whatever you have", and `RosterBuilder::for_setup` falls through to
 * `harness_inference_from_env` — so a hosted tenant with an injected key would
 * design a roster with the model its operator had just declined, and now
 * without the design brief this step stops asking for. `forceCurated` is what
 * makes the promise on screen true on that host too.
 */
describe("what a modelless run asks the host for", () => {
  /** The roster request the wizard actually sent. */
  async function rosterRequestFrom(walk: () => Promise<void>): Promise<Record<string, unknown>> {
    const sent: Record<string, unknown>[] = [];
    const client = {
      scopeFor: () => "/api/v1/company",
      get: async () => status(),
      post: async (path: string, body: unknown) => {
        if (path.endsWith("/setup/roster")) {
          sent.push(body as Record<string, unknown>);
          return {
            agents: [{ name: "Ada", role: "Operations", description: "Runs the desk." }],
            template: "ecommerce",
            source: "fallback",
          };
        }
        if (path.endsWith("/inference/test")) {
          return { ok: true, baseUrl: "https://api.example/v1", model: "m" };
        }
        return { complete: true, config_path: "/data/config.toml", restart_required: [] };
      },
    } as unknown as OpenCompanyClient;
    await show(client);
    await walk();
    expect(sent.length, "the wizard should have asked for a roster").toBeGreaterThan(0);
    return sent[sent.length - 1];
  }

  it("asks for the curated team outright, rather than by saying nothing", async () => {
    const body = await rosterRequestFrom(async () => {
      await skipConnect();
      await next(); // -> business
      await fill("setup-field-industry", "E-commerce — homeware");
      await next(); // -> sign-in
      await click("auth-mode-none");
      await next(); // -> review, designing on the way in
    });

    expect(body.forceCurated, "no model means no model").toBe(true);
  });

  it("sends neither half of the design brief once there is no model to read it", async () => {
    const body = await rosterRequestFrom(async () => {
      // Answered first, then taken back: the drafts survive in state, and must
      // not steer the curated pick they are no longer an answer to. Taking it
      // back is now switching the way, which throws the verdict away.
      await connectManaged();
      await next(); // -> business, with a model
      await fill("setup-field-industry", "E-commerce — homeware");
      await fill("setup-field-automate", "Meta ads, order dispatch");
      await fill("setup-field-teamHint", "someone chasing invoices");
      await back(); // -> step 1
      await back(); // -> the way
      await skipConnect();
      await next(); // -> business
      await next(); // -> sign-in
      await click("auth-mode-none");
      await next(); // -> review
    });

    expect(body.automate).toBe("");
    expect(body.teamHint).toBe("");
    expect(body.forceCurated).toBe(true);
  });

  it("does not submit a roster designed under an answer that has since changed", async () => {
    // A model designed one, the operator went back and connected nothing. The
    // wizard designs only when it holds none, so the old one would otherwise
    // ride through Review under copy promising a standard team.
    const body = await rosterRequestFrom(async () => {
      await connectManaged();
      await next(); // -> business
      await fill("setup-field-industry", "E-commerce — homeware");
      await next(); // -> sign-in
      await click("auth-mode-none");
      await next(); // -> review, designs with the model
      await back(); // -> sign-in
      await back(); // -> business
      await back(); // -> step 1
      await back(); // -> the way
      await skipConnect();
      await next(); // -> business
      await next(); // -> sign-in
      await next(); // -> review, must design again
    });

    expect(body.forceCurated, "the second design is the modelless one").toBe(true);
  });
});

/**
 * A verdict is only true of the answers it was asked with.
 *
 * The key field stays live while a test is in flight, so an answer can land
 * after the operator has changed the thing it was asked about. Clearing the
 * field mid-test is the case that bites: a late `ok` would release the step on
 * a credential nobody supplied, and submit an empty one as the company's
 * account key.
 */
describe("a connection verdict that arrives after the question changed", () => {
  it("is discarded when the key it was asked about has been cleared", async () => {
    let release!: (value: unknown) => void;
    const inFlight = new Promise((resolve) => {
      release = resolve;
    });
    const client = {
      scopeFor: () => "/api/v1/company",
      get: async () => status(),
      post: async (path: string) =>
        path.endsWith("/inference/test")
          ? inFlight
          : { complete: true, config_path: "/data/config.toml", restart_required: [] },
    } as unknown as OpenCompanyClient;

    await show(client);
    await connectManaged(); // in flight, unresolved
    await fill("setup-field-key", "");

    // The late answer, for a key that is no longer in the field.
    await act(async () => {
      release({ ok: true, baseUrl: "https://api.example/v1", model: "m" });
      await inFlight;
    });

    expect(find("setup-test-ok"), "a verdict about a cleared key must not land").toBeNull();
    await next();
    expect(find("setup-problem"), "and the step must still be held").toBeTruthy();
  });
});
