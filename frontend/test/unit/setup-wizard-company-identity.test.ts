// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { SetupStatus } from "@/api/setup";
import {
  SetupWizard,
  offeredAuthModes,
  shouldSeedTemplate,
  suggestedCompanyName,
} from "@/views/setup/SetupWizard";

/**
 * What the wizard sends back about the company itself: its **name**, and
 * whether the thing to build is a template or a designed team.
 *
 * Both were decided silently before. The name was derived host-side from the
 * *industry* answer — a field labelled "what kind of company are you setting
 * up?" — and it mints the company id, which is then permanent and has no
 * rename anywhere in the product. And a picked template was never sent: the
 * wizard only ever posted a designed company, so choosing "Agentic Marketing
 * Agency" and skipping the model produced a rebuilt approximation of it,
 * without the roster, tool belt or prompts that template ships.
 *
 * Mounted rather than pure, for the same earned reason the sibling gate test
 * gives: the claim is about what a submit *carries* after the operator has
 * walked the steps, which only exists once the component is rendering.
 */

const TEMPLATE = {
  id: "marketing_agency",
  name: "Agentic Marketing Agency",
  agent_count: 8,
  output: "Campaigns across every channel",
};

const OTHER_TEMPLATE = {
  id: "law_firm",
  name: "Agentic Law Firm",
  agent_count: 5,
  output: "Filings and advice",
};

function status(over: Partial<SetupStatus> = {}): SetupStatus {
  return {
    complete: false,
    config_path: "/data/config.toml",
    fields: [],
    templates: [TEMPLATE],
    // `none` keeps the walk short: it removes the address step, which is the
    // only one that would demand an answer this file is not about.
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

/** The roster the host proposes, and the apply body it is asked for. */
function clientWith(
  s: SetupStatus,
  source: "preset" | "fallback",
  applied: { body?: unknown },
): OpenCompanyClient {
  return {
    get: async () => s,
    post: async (path: string, body: unknown) => {
      if (path.includes("/setup/roster")) {
        return {
          agents: [
            { name: "Creative Director", role: "Creative Director", description: "Concepts." },
            { name: "Copywriter", role: "Copywriter", description: "Words." },
          ],
          template: TEMPLATE.id,
          source,
          jobs: [],
          uncovered: [],
          reason: "no_model",
        };
      }
      if (path === "/api/v1/setup") {
        applied.body = body;
        return {
          complete: true,
          config_path: s.config_path,
          restart_required: [],
          seeded_company: "whatever-they-called-it",
        };
      }
      return {};
    },
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

function button(label: string): HTMLButtonElement {
  const wanted = label === "Next" ? ["Next", "Looks good"] : [label];
  const match = Array.from(container.querySelectorAll("button")).find((b) =>
    wanted.includes(b.textContent?.trim() ?? ""),
  );
  expect(match, `no button labeled "${label}"`).toBeTruthy();
  return match as HTMLButtonElement;
}

const next = async () =>
  act(async () => {
    button("Next").click();
  });

async function fill(testId: string, value: string) {
  const field = container.querySelector(`[data-testid="${testId}"]`) as
    | HTMLInputElement
    | HTMLTextAreaElement;
  expect(field, `no field ${testId}`).toBeTruthy();
  await act(async () => {
    const proto =
      field instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(field, value);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function pickTemplate(id: string) {
  const select = container.querySelector("select") as HTMLSelectElement;
  expect(select, "no template dropdown").toBeTruthy();
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!.call(select, id);
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

/** How many company-name fields are on screen right now. */
const nameFields = () =>
  container.querySelectorAll('[data-testid="setup-company-name"]').length;

/** Render, and walk as far as the business step without answering it. */
async function walkToBusiness(client: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(SetupWizard, { client, onDone: () => {} }));
  });
  // Step 0 is the setup-way choice; the add-provider sequence sits behind "Set
  // it up yourself", and connecting nothing is a first-class answer to it.
  await act(async () => {
    (container.querySelector('[data-testid="setup-way-self-managed"]') as HTMLElement).click();
  });
  await next(); // -> step 1, where connecting nothing is a first-class answer
  await next(); // -> business
}

/** business -> sign-in (none) -> review, with the business step answered. */
async function walkToReview(client: OpenCompanyClient, template: string | null) {
  await walkToBusiness(client);
  if (template) await pickTemplate(template);
  else await fill("setup-field-industry", "E-commerce — homeware online");
  await next(); // -> sign-in
  // "No sign-in", which also removes the address step.
  await act(async () => {
    (container.querySelector('[data-testid="auth-mode-none"]') as HTMLElement).click();
  });
  await next(); // -> review
}

describe("what a finished wizard says the company is", () => {
  it("asks what to call it on the business step, not at the end", async () => {
    await walkToBusiness(clientWith(status(), "preset", {}));
    const field = container.querySelector(
      '[data-testid="setup-company-name"]',
    ) as HTMLInputElement;
    expect(field, "the business step must ask what to call it").toBeTruthy();
    expect(field.tagName).toBe("INPUT");
  });

  it("suggests the template's name there, and sends it as the company's", async () => {
    const applied: { body?: unknown } = {};
    await walkToBusiness(clientWith(status(), "preset", applied));
    await pickTemplate(TEMPLATE.id);

    const field = container.querySelector(
      '[data-testid="setup-company-name"]',
    ) as HTMLInputElement;
    expect(field.value, "a name is offered before the operator is asked for one").toBe(
      TEMPLATE.name,
    );

    await next(); // -> sign-in
    await act(async () => {
      (container.querySelector('[data-testid="auth-mode-none"]') as HTMLElement).click();
    });
    await next(); // -> review
    await act(async () => {
      button("Build my company").click();
    });
    expect((applied.body as { name?: string }).name).toBe(TEMPLATE.name);
  });

  it("sends a name typed on the business step, over the suggestion", async () => {
    const applied: { body?: unknown } = {};
    await walkToBusiness(clientWith(status(), "preset", applied));
    await pickTemplate(TEMPLATE.id);
    await fill("setup-company-name", "Northwind Studio");

    await next(); // -> sign-in
    await act(async () => {
      (container.querySelector('[data-testid="auth-mode-none"]') as HTMLElement).click();
    });
    await next(); // -> review
    await act(async () => {
      button("Build my company").click();
    });
    const body = applied.body as { name?: string; template?: string | null };
    expect(body.name).toBe("Northwind Studio");
    // Renaming is not designing: the template is still what gets seeded.
    expect(body.template).toBe(TEMPLATE.id);
  });

  it("never offers to rename the company on the review step", async () => {
    const client = clientWith(status(), "preset", {});
    await walkToBusiness(client);
    await pickTemplate(TEMPLATE.id);
    expect(nameFields(), "asked once, on the step that asks it").toBe(1);
    await next(); // -> sign-in
    expect(nameFields()).toBe(0);
    await act(async () => {
      (container.querySelector('[data-testid="auth-mode-none"]') as HTMLElement).click();
    });
    await next(); // -> review
    expect(
      nameFields(),
      "D-name-once: the review step's own name field is deleted, not hidden",
    ).toBe(0);
  });

  it("offers no name field on review even when nothing has named the company", async () => {
    // A host that already serves a company never gates the business step on a
    // name, so this is the one walk that reaches review with no name at all —
    // and the one where a review field merely *hidden* behind "already named?"
    // would come back.
    await walkToBusiness(clientWith(status({ companies: ["acme"] }), "preset", {}));
    await next(); // -> sign-in, nothing answered
    await act(async () => {
      (container.querySelector('[data-testid="auth-mode-none"]') as HTMLElement).click();
    });
    await next(); // -> review
    expect(container.querySelector('[data-testid="setup-review"]')).toBeTruthy();
    expect(nameFields(), "review never renders an editable company name").toBe(0);
    expect(container.querySelector('[data-testid="setup-review-name"]')).toBeNull();
  });

  it("sends an untouched template roster back as the template itself", async () => {
    const applied: { body?: unknown } = {};
    await walkToReview(clientWith(status(), "preset", applied), TEMPLATE.id);

    await act(async () => {
      button("Build my company").click();
    });
    const body = applied.body as { template?: string | null; company?: unknown };
    expect(body.template).toBe(TEMPLATE.id);
    expect(
      body.company,
      "a template the host can seed whole must not be rebuilt from this screen",
    ).toBeNull();
  });

  it("still sends a designed company when the roster was matched, not picked", async () => {
    const applied: { body?: unknown } = {};
    // No templates offered, so the step asks for the business in the operator's
    // own words — which is the path the curated roster is matched from.
    await walkToReview(clientWith(status({ templates: [] }), "fallback", applied), null);

    await act(async () => {
      button("Build my company").click();
    });
    const body = applied.body as { template?: string | null; company?: { agents: unknown[] } };
    expect(body.template).toBeNull();
    expect(body.company?.agents).toHaveLength(2);
  });

  it("re-suggests the name when the template changes, unless it was typed", async () => {
    const client = clientWith(status({ templates: [TEMPLATE, OTHER_TEMPLATE] }), "preset", {});
    await walkToBusiness(client);
    await pickTemplate(TEMPLATE.id);
    const field = () =>
      container.querySelector('[data-testid="setup-company-name"]') as HTMLInputElement;
    expect(field().value).toBe(TEMPLATE.name);

    await pickTemplate(OTHER_TEMPLATE.id);
    expect(
      field().value,
      "a suggestion nobody typed must not name the company they did pick",
    ).toBe(OTHER_TEMPLATE.name);
  });

  it("keeps a typed name across a change of template", async () => {
    const client = clientWith(status({ templates: [TEMPLATE, OTHER_TEMPLATE] }), "preset", {});
    await walkToBusiness(client);
    await pickTemplate(TEMPLATE.id);
    await fill("setup-company-name", "Northwind Studio");
    await pickTemplate(OTHER_TEMPLATE.id);

    const field = container.querySelector(
      '[data-testid="setup-company-name"]',
    ) as HTMLInputElement;
    expect(field.value, "a name the operator typed is theirs").toBe("Northwind Studio");
  });

  it("keeps a typed name across a change of setup way", async () => {
    // Switching ways clears what belongs to a branch — the key, its verdict,
    // the roster designed under it. The name is an answer about the company,
    // the same under either way.
    const client = clientWith(status(), "preset", {});
    await walkToBusiness(client);
    await pickTemplate(TEMPLATE.id);
    await fill("setup-company-name", "Northwind Studio");

    for (let i = 0; i < 2; i += 1) {
      await act(async () => {
        button("Back").click();
      });
    }
    await act(async () => {
      (container.querySelector('[data-testid="setup-way-managed"]') as HTMLElement).click();
    });
    await act(async () => {
      (container.querySelector('[data-testid="setup-way-self-managed"]') as HTMLElement).click();
    });
    await next(); // -> step 1, whose answers the switch cleared
    await next(); // -> business

    const field = container.querySelector(
      '[data-testid="setup-company-name"]',
    ) as HTMLInputElement;
    expect(field.value, "the company is called what it is called under either way").toBe(
      "Northwind Studio",
    );
  });

  it("keeps a typed name across leaving the step and coming back", async () => {
    await walkToBusiness(clientWith(status(), "preset", {}));
    await pickTemplate(TEMPLATE.id);
    await fill("setup-company-name", "Northwind Studio");
    await next(); // -> sign-in
    await act(async () => {
      button("Back").click();
    });

    const field = container.querySelector(
      '[data-testid="setup-company-name"]',
    ) as HTMLInputElement;
    expect(field.value).toBe("Northwind Studio");
  });

  it("will not leave the business step with the name cleared", async () => {
    await walkToBusiness(clientWith(status(), "preset", {}));
    await pickTemplate(TEMPLATE.id);
    await fill("setup-company-name", "");
    await next();

    expect(
      container.querySelector('[data-testid="setup-company-name"]'),
      "a cleared name keeps the operator on the step that asks for it",
    ).toBeTruthy();
    expect(container.querySelector('[data-testid="setup-problem"]')?.textContent).toBe(
      "Give your company a name.",
    );
  });

  it("names the question they skipped when nothing on the step is answered", async () => {
    // Ordering, not wording: with no template picked the name is empty too, and
    // being told to name a company nobody has chosen the shape of is an answer
    // to the wrong question.
    await walkToBusiness(clientWith(status(), "preset", {}));
    await next();
    expect(container.querySelector('[data-testid="setup-problem"]')?.textContent).toBe(
      "Choose the kind of company you want to start with.",
    );
  });

  it("demands no name from a host that already serves a company", async () => {
    await walkToBusiness(clientWith(status({ companies: ["acme"] }), "preset", {}));
    await next();
    expect(
      container.querySelector('[data-testid="setup-problem"]'),
      "the name is never used where no company is being minted",
    ).toBeNull();
    expect(container.querySelector('[data-testid="setup-company-name"]')).toBeNull();
  });

  it("states the name on review without offering to change it", async () => {
    await walkToBusiness(clientWith(status(), "preset", {}));
    await pickTemplate(TEMPLATE.id);
    await fill("setup-company-name", "Northwind Studio");
    await next(); // -> sign-in
    await act(async () => {
      (container.querySelector('[data-testid="auth-mode-none"]') as HTMLElement).click();
    });
    await next(); // -> review

    const echo = container.querySelector('[data-testid="setup-review-name"]');
    expect(echo, "review still says what the company will be called").toBeTruthy();
    expect(echo?.textContent).toContain("Northwind Studio");
    expect(echo?.querySelector("input"), "said, not asked").toBeNull();
  });

  it("clamps the name to the sixty code points the host keeps", async () => {
    // Code points, not UTF-16 units: `maxLength` would cut an astral-script
    // name at thirty, and the profile rename's own clamp would let 200 through
    // for the host to truncate silently.
    await walkToBusiness(clientWith(status(), "preset", {}));
    await pickTemplate(TEMPLATE.id);
    const field = () =>
      container.querySelector('[data-testid="setup-company-name"]') as HTMLInputElement;

    const sixty = "\u{1D49C}".repeat(60);
    await fill("setup-company-name", sixty);
    expect(field().value, "sixty astral code points are sixty characters").toBe(sixty);

    await fill("setup-company-name", "\u{1D49C}".repeat(61));
    expect(Array.from(field().value)).toHaveLength(60);
  });

  it("does not offer to edit a roster it is going to seed whole", async () => {
    await walkToReview(clientWith(status(), "preset", {}), TEMPLATE.id);
    expect(
      container.querySelector('[data-testid="setup-review-remove"]'),
      "an edited template roster could only go back as a designed company, which is capped at six",
    ).toBeNull();
  });
});

describe("the name offered for a company nobody has named", () => {
  it("prefers a picked template's own name", () => {
    expect(suggestedCompanyName("anything at all", "Agentic Law Firm")).toBe("Agentic Law Firm");
  });

  it("takes the first clause of the industry answer, as the host does", () => {
    expect(suggestedCompanyName("E-commerce — I sell homeware online", null)).toBe("E-commerce");
    expect(suggestedCompanyName("Consulting, mostly public sector", null)).toBe("Consulting");
  });

  it("never splits a hyphenated word", () => {
    expect(suggestedCompanyName("E-commerce", null)).toBe("E-commerce");
  });

  it("offers nothing when there is nothing to offer", () => {
    expect(suggestedCompanyName("   ", null)).toBe("");
  });
});

describe("whether a finished wizard seeds a template or a designed company", () => {
  const picked = {
    hasCompany: false,
    source: "preset" as const,
    rosterEdited: false,
    template: TEMPLATE.id,
    credentialTested: false,
    writesInference: true,
  };

  it("seeds the template an operator picked and did not edit", () => {
    expect(shouldSeedTemplate(picked)).toBe(true);
  });

  it("designs instead once the roster has been edited", () => {
    expect(shouldSeedTemplate({ ...picked, rosterEdited: true })).toBe(false);
  });

  it("designs instead for a curated roster, which is no template's", () => {
    expect(shouldSeedTemplate({ ...picked, source: "fallback" })).toBe(false);
    expect(shouldSeedTemplate({ ...picked, source: "model" })).toBe(false);
  });

  it("designs instead when a credential has to be carried", () => {
    expect(shouldSeedTemplate({ ...picked, credentialTested: true })).toBe(false);
  });

  it("still seeds the template when nothing will be written anyway", () => {
    // The submit omits inference where the host already reaches it, or where the
    // credential that passed was the house's rather than this operator's — so
    // the designed path would trade the template's roster, belt and prompts for
    // nothing at all.
    //
    // Asked as `writesInference`, not `provider === "managed"`. That literal
    // answered the same question only while the model step adopted a provider it
    // no longer offers, and went quietly wrong when it stopped.
    expect(
      shouldSeedTemplate({ ...picked, credentialTested: true, writesInference: false }),
    ).toBe(true);
  });

  it("seeds nothing onto a host that already has a company", () => {
    expect(shouldSeedTemplate({ ...picked, hasCompany: true })).toBe(false);
  });

  it("designs instead when no template id is actually carried", () => {
    // `source: "preset"` names the picker's answer, not proof a template id
    // rode along with it — an empty or whitespace-only `template` has nothing
    // for the host to seed from, so this must fall to the designed path the
    // same as any other incomplete pick.
    expect(shouldSeedTemplate({ ...picked, template: "" })).toBe(false);
    expect(shouldSeedTemplate({ ...picked, template: "   " })).toBe(false);
  });
});

describe("the sign-in modes a first run may offer", () => {
  const host = (modes: string[]) => status({ auth_modes: modes });

  it("withholds wallet, which this flow cannot finish", () => {
    // `[users].wallets` is what a wallet company is bootstrapped by, and
    // nothing here can collect one — so finishing on `wallet` produces a
    // company with no eligible administrator and no anonymous way back in.
    expect(offeredAuthModes(host(["none", "email", "wallet"]), "")).toEqual(["none", "email"]);
  });

  it("still shows a wallet host the mode it is already running", () => {
    expect(offeredAuthModes(host(["email", "wallet"]), "wallet")).toEqual(["email", "wallet"]);
  });

  it("reports every mode, wallet included, when env owns the field", () => {
    // `FieldDto.value` is read from `config.toml` alone, so an
    // `OPENCOMPANY_AUTH_MODE=wallet` never reaches `current`. The picker is
    // locked in that state and is reporting rather than offering — filtering
    // there would show an operator a disabled list whose every option is wrong.
    const envOwned = status({
      auth_modes: ["none", "email", "wallet"],
      fields: [
        {
          key: "auth_mode",
          value: null,
          layer: "env",
          editable: false,
          requires_restart: false,
          secret: false,
        },
      ],
    });
    expect(offeredAuthModes(envOwned, "")).toEqual(["none", "email", "wallet"]);
  });

  it("still withholds wallet when the field is the wizard's to write", () => {
    const editable = status({
      auth_modes: ["none", "email", "wallet"],
      fields: [
        {
          key: "auth_mode",
          value: "email",
          layer: "config.toml",
          editable: true,
          requires_restart: false,
          secret: false,
        },
      ],
    });
    expect(offeredAuthModes(editable, "")).toEqual(["none", "email"]);
  });

  it("leaves every other mode exactly as the host offered it", () => {
    expect(offeredAuthModes(host(["none", "email"]), "")).toEqual(["none", "email"]);
    expect(offeredAuthModes(host(["email"]), "email")).toEqual(["email"]);
  });
});
