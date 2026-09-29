// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { CompanyCredentialStatus } from "@/api/credential";
import { ApiError } from "@/api/types";
import { captureKeyLink } from "@/lib/pending-key-link";

// KR-ACCT-01: `ApiKeyView`'s restart action calls the same
// `restartInference` the LLM page's own "Restart now" button does — mocked
// here so the cancel-race and restart-toast tests can assert on it without a
// real inference route. Sonner is mocked the same way
// `credential-clear-confirm.test.ts` mocks it: no `<Toaster/>` is mounted in
// this jsdom harness, so a real `toast.success` call would render nothing,
// and asserting on the toast's own headline/action requires seeing the call.
const api = vi.hoisted(() => ({
  restartInference: vi.fn(),
}));

vi.mock("@/api/inference", () => ({
  restartInference: api.restartInference,
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), warning: vi.fn(), info: vi.fn() },
}));

const { ApiKeyView } = await import("@/views/connections/ApiKeyView");
const { toast } = await import("sonner");

let container: HTMLDivElement;
let root: Root;

/** The hub this host is on, as the host reports it — staging, deliberately,
 * so a production constant leaking into a link would be caught. */
const STAGING_KEYS_URL = "https://staging.tinyhumans.ai/dashboard?tab=api-keys";

function credential(overrides: Partial<CompanyCredentialStatus> = {}): CompanyCredentialStatus {
  return {
    configured: true,
    source: "company",
    notice: "notice",
    hubLink: false,
    account: {
      manageKeysUrl: STAGING_KEYS_URL,
      topUpUrl: "https://staging.tinyhumans.ai/dashboard?tab=billing",
    },
    ...overrides,
  };
}

/** A client whose `/auth/me` always answers as a non-admin, so the credential
 * card's own network calls stay quiet and out of scope for these tests. */
function clientFor(handlers: {
  credential: () => Promise<CompanyCredentialStatus>;
  billing: () => Promise<unknown>;
}): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/companies/acme",
    get: async (path: string) => {
      if (path.endsWith("/credential/billing")) return handlers.billing();
      if (path.endsWith("/auth/me")) return { role: "member" };
      if (path.endsWith("/credential")) return handlers.credential();
      throw new Error(`unexpected GET ${path}`);
    },
  } as unknown as OpenCompanyClient;
}

/** An admin client, so the controls a member never sees are actually rendered.
 * `writes` collects every PUT body, which is how the confirmation tests tell a
 * cleared key from an offered one. */
function adminClient(
  credentialFor: () => Promise<CompanyCredentialStatus>,
  writes: unknown[] = [],
): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/companies/acme",
    get: async (path: string) => {
      if (path.endsWith("/credential/billing")) return { configured: false };
      if (path.endsWith("/auth/me")) return { role: "admin" };
      if (path.endsWith("/credential")) return credentialFor();
      throw new Error(`unexpected GET ${path}`);
    },
    put: async (_path: string, body: unknown) => {
      writes.push(body);
      return { status: await credentialFor(), note: "" };
    },
  } as unknown as OpenCompanyClient;
}

/** Clicks a rendered control and lets the resulting state settle.
 *
 * Menus and dialogs render into a portal on `document.body`, not into the
 * container, so the queries below deliberately ask the document. */
async function press(selector: string) {
  const el = document.querySelector(selector);
  if (el === null) throw new Error(`nothing to press at ${selector}`);
  await act(async () => {
    (el as HTMLElement).click();
  });
  await act(async () => {});
}

/** Clicks the button whose visible text matches exactly — for the dialog's
 * plain `Cancel`/`Save model` controls, which carry no `data-testid` of their
 * own beyond the ones already asserted on. */
async function pressButtonNamed(text: string) {
  const button = Array.from(document.querySelectorAll("button")).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  if (!button) throw new Error(`no button named "${text}"`);
  await act(async () => {
    button.click();
  });
  await act(async () => {});
}

async function mount(client: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(ApiKeyView, { client, company: "acme" }));
  });
  await act(async () => {});
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  api.restartInference.mockReset();
  vi.mocked(toast.success).mockClear();
  vi.mocked(toast.error).mockClear();
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("ApiKeyView billing failures stay distinguishable from no key", () => {
  // The regression this covers (CodeRabbit + Codex, PR #2216): the old catch
  // converted every billing rejection into `{ configured: false }`, so a
  // company that HAS a key saw the balance vanish entirely — the same outcome
  // as never having set one, even though `billing.unavailable` exists
  // precisely to say "the key is set, the hub just would not answer".
  it("keeps the balance row in its unavailable state when billing rejects but a key is configured", async () => {
    const client = clientFor({
      credential: async () => credential({ configured: true, source: "company" }),
      billing: async () => {
        throw new Error("network blip");
      },
    });

    await mount(client);

    const text = container.textContent ?? "";
    expect(container.querySelector('[data-testid="account-balance"]')).not.toBeNull();
    expect(text).toContain("Balance unknown");
    expect(text).toContain("The key is set");
    // Must NOT have fallen through to the empty state.
    expect(container.querySelector('[data-testid="account-empty"]')).toBeNull();
  });

  it("reports no account at all when billing rejects and nothing resolves", async () => {
    const client = clientFor({
      credential: async () => credential({ configured: false, source: "none" }),
      billing: async () => {
        throw new Error("network blip");
      },
    });

    await mount(client);

    // No balance row at all — nothing to be "unavailable" about.
    expect(container.querySelector('[data-testid="account-balance"]')).toBeNull();
    expect(container.querySelector('[data-testid="account-empty"]')).not.toBeNull();
    expect(container.textContent ?? "").toContain("No account connected yet.");
  });
});

describe("ApiKeyView describes a fallback platform identity honestly", () => {
  // Codex P2: a host with no company key but a live instance identity
  // (`attested` / `static`) already lets agents think and providers connect —
  // "agents cannot think and no app can be connected" is simply false there
  // and would send an operator to reconnect something that already works. The
  // row is keyed on what `resolve` returned, not on `configured`, which is
  // false in exactly this case.
  it("names the server's identity rather than saying nothing is configured", async () => {
    const client = clientFor({
      credential: async () => credential({ configured: false, source: "attested" }),
      billing: async () => ({ configured: false }),
    });

    await mount(client);

    const subline = container.querySelector('[data-testid="account-row-subline"]');
    expect(subline?.textContent).toBe("Acting as the account of whoever runs this server");
    expect(container.querySelector('[data-testid="account-empty"]')).toBeNull();
  });

  it("still says plainly when there is no identity at all", async () => {
    const client = clientFor({
      credential: async () => credential({ configured: false, source: "none" }),
      billing: async () => ({ configured: false }),
    });

    await mount(client);

    expect(container.querySelector('[data-testid="account-empty"]')).not.toBeNull();
    expect(container.textContent ?? "").toContain("No account connected yet.");
    expect(container.textContent ?? "").toContain("Apps cannot be connected");
  });
});

describe("ApiKeyView never overstates what a missing account breaks", () => {
  // QA, 2026-09-11. The old page said "Until one is set, agents cannot think
  // and no provider can be connected", and it is **false** on a company whose
  // LLM page holds a provider key of its own: `inference/key` resolves without
  // this credential, so such a company thinks perfectly well at `source:
  // "none"` — and the sentence sends its operator to fix something that is not
  // broken. The page may say what this key governs; it may not claim the whole
  // company has stopped.
  it("never claims agents cannot think, in any state", async () => {
    for (const source of ["none", "attested", "static", "company"] as const) {
      const client = clientFor({
        credential: async () => credential({ configured: source === "company", source }),
        billing: async () => ({ configured: false }),
      });

      await mount(client);

      const text = (container.textContent ?? "").toLowerCase();
      expect(text, `source=${source}`).not.toContain("agents cannot think");
      expect(text, `source=${source}`).not.toContain("cannot think");
    }
  });

  // The exception is named rather than denied — an operator who has set a
  // provider key on the LLM page must be able to see that it still applies.
  it("names the LLM-page provider key as the thing that still works", async () => {
    const client = clientFor({
      credential: async () => credential({ configured: false, source: "none" }),
      billing: async () => ({ configured: false }),
    });

    await mount(client);

    expect(container.textContent ?? "").toContain(
      "a provider key set on the LLM page still works",
    );
  });

  // The billing consequence belongs to the control it is true of. `PUT
  // …/credential` — what the paste dialog submits — writes `tinyhumans/key`
  // and stops; only `finish_link` also writes `inference/key` and declares the
  // managed provider. So the header card, which carries the Connect button,
  // states the move, and the dialog must not: telling someone that pasting a
  // key moved their model spend is the same defect pointing the other way.
  //
  // Keys rework (#2306, slice 4b): the sentence was reworded around Q7 (a
  // save never overwrites a key set on the LLM or Composio page's own) rather
  // than the old precedence chain. `git grep -n "keeps precedence"
  // frontend/src/views/connections` must print nothing.
  it("puts the fan-out consequence on the connect path, not on the paste field", async () => {
    await mount(
      adminClient(async () => credential({ configured: false, source: "none", hubLink: true })),
    );

    // The header card says it, beside the button it is true of.
    expect(container.textContent ?? "").toContain(
      "One key for the apps your agents act through and the models they think with.",
    );
    expect(container.textContent ?? "").toContain(
      "Saving copies it to the LLM, Composio, and Search pages wherever they hold no key of their own.",
    );
    expect(container.textContent ?? "").not.toContain("keeps precedence");
  });

  // The dialog is kept minimal at the operator's request (2026-09-14): a
  // heading, the field, the link and its controls — no explanatory paragraph,
  // and so no billing claim that could be wrong either way.
  it("keeps the API-key dialog to its heading, field and link", async () => {
    await mount(
      adminClient(async () => credential({ configured: false, source: "none", hubLink: false })),
    );

    await press('[data-testid="account-add-key"]');

    const dialog = document.body.textContent ?? "";
    expect(dialog).toContain("Add your API key");
    expect(dialog).toContain("Don't have an API key?");
    expect(dialog).not.toContain("connects apps as");
    expect(dialog).not.toContain("It does not choose a model provider");
    expect(dialog).not.toContain("moves every agent turn");
  });
});

/** Types into a React-controlled input the way a person would. */
async function typeInto(selector: string, value: string) {
  const el = document.querySelector(selector) as HTMLInputElement | null;
  if (el === null) throw new Error(`nothing to type into at ${selector}`);
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  await act(async () => {
    setter?.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

describe("ApiKeyView offers one way to connect", () => {
  // The sign-in option was removed at the operator's request (2026-09-14):
  // even on a host with a hub, the header card offers Connect to TinyHumans
  // alone.
  it("shows Connect to TinyHumans and no sign-in option where the host has a hub", async () => {
    await mount(
      adminClient(async () => credential({ configured: false, source: "none", hubLink: true })),
    );

    expect(container.querySelector('[data-testid="account-add-key"]')?.textContent).toContain(
      "Connect to TinyHumans",
    );
    expect(container.querySelector('[data-testid="connect-tinyhumans"]')).toBeNull();
    expect(container.textContent ?? "").not.toContain("Sign in with TinyHumans");
  });

  it("shows the same single option where there is no hub", async () => {
    await mount(
      adminClient(async () => credential({ configured: false, source: "none", hubLink: false })),
    );

    expect(container.querySelector('[data-testid="account-add-key"]')).not.toBeNull();
    expect(container.querySelector('[data-testid="connect-tinyhumans"]')).toBeNull();
  });

  // Connected: the row's menu carries Replace and Remove; a Connect CTA above
  // it would be a second route to the same write.
  it("offers no connect CTA once this company has a key of its own", async () => {
    await mount(adminClient(async () => credential({ source: "company", hubLink: true })));

    expect(container.querySelector('[data-testid="account-add-key"]')).toBeNull();
    expect(container.querySelector('[data-testid="connect-tinyhumans"]')).toBeNull();
    expect(container.querySelector('[data-testid="account-row-menu"]')).not.toBeNull();
  });
});

describe("ApiKeyView's Connect to TinyHumans dialog", () => {
  function recordingClient(
    writes: { path: string; body: unknown }[],
    put?: () => Promise<unknown>,
  ): OpenCompanyClient {
    return {
      scopeFor: () => "/api/v1/companies/acme",
      get: async (path: string) => {
        if (path.endsWith("/credential/billing")) return { configured: false };
        if (path.endsWith("/auth/me")) return { role: "admin" };
        if (path.endsWith("/credential")) {
          return credential({ configured: false, source: "none", hubLink: true });
        }
        throw new Error(`unexpected GET ${path}`);
      },
      put: async (path: string, body: unknown) => {
        writes.push({ path, body });
        if (put) return put();
        return { status: credential({ source: "company" }), note: "" };
      },
    } as unknown as OpenCompanyClient;
  }

  it("asks for the API key and links to where one is created", async () => {
    await mount(recordingClient([]));
    await press('[data-testid="account-add-key"]');

    const input = document.querySelector('[data-testid="account-key-input"]') as HTMLInputElement;
    expect(input).not.toBeNull();
    // Write-only: never echoed.
    expect(input.type).toBe("password");
    // With a hub the grant leads and the field is the "or"; the label says so.
    expect(document.body.textContent ?? "").toContain("Paste an API key");
    expect(document.body.textContent ?? "").toContain("Don't have an API key?");

    const link = document.querySelector('[data-testid="account-key-get-link"]') as HTMLAnchorElement;
    expect(link.textContent).toContain("Get an API key");
    // The hub THIS host is on, as the status reports it — never a production
    // constant: a key minted there would be refused by a staging host.
    expect(link.getAttribute("href")).toBe(STAGING_KEYS_URL);
    expect(link.getAttribute("target")).toBe("_blank");
  });

  it("offers no key link when the host derives no hub site", async () => {
    await mount(
      adminClient(async () =>
        credential({ configured: false, source: "none", hubLink: false, account: undefined }),
      ),
    );
    await press('[data-testid="account-add-key"]');
    expect(document.querySelector('[data-testid="account-key-get-link"]')).toBeNull();
    // The paste field is still the whole of the dialog.
    expect(document.querySelector('[data-testid="account-key-input"]')).not.toBeNull();
  });

  it("offers the one-click grant ahead of the paste field where the host has a hub", async () => {
    const posts: string[] = [];
    const client = {
      ...recordingClient([]),
      post: async (path: string) => {
        posts.push(path);
        return { authorizeUrl: "https://staging.tinyhumans.ai/connect?x=1" };
      },
    } as unknown as OpenCompanyClient;
    const assign = vi.fn();
    const original = window.location;
    Object.defineProperty(window, "location", {
      configurable: true,
      value: { ...original, assign },
    });
    try {
      await mount(client);
      await press('[data-testid="account-add-key"]');

      const connect = document.querySelector('[data-testid="connect-tinyhumans"]');
      expect(connect?.textContent).toContain("Connect with TinyHumans");
      // Still there for the person who would rather paste.
      expect(document.querySelector('[data-testid="account-key-input"]')).not.toBeNull();

      await press('[data-testid="connect-tinyhumans"]');
      expect(posts).toEqual(["/api/v1/companies/acme/credential/link/start"]);
      // A browser: the hub is a top-level navigation.
      expect(assign).toHaveBeenCalledWith("https://staging.tinyhumans.ai/connect?x=1");
    } finally {
      Object.defineProperty(window, "location", { configurable: true, value: original });
    }
  });

  it("offers no grant button on a host with no hub", async () => {
    await mount(
      adminClient(async () => credential({ configured: false, source: "none", hubLink: false })),
    );
    await press('[data-testid="account-add-key"]');
    expect(document.querySelector('[data-testid="connect-tinyhumans"]')).toBeNull();
  });

  it("writes the typed key to the company credential route and closes", async () => {
    const writes: { path: string; body: unknown }[] = [];
    await mount(recordingClient(writes));
    await press('[data-testid="account-add-key"]');
    await typeInto('[data-testid="account-key-input"]', "th-not-a-real-key");
    await press('[data-testid="account-key-save"]');

    // The Account page's own slot (`tinyhumans/key`), never the inference
    // managed-key route.
    expect(writes).toEqual([
      { path: "/api/v1/companies/acme/credential", body: { key: "th-not-a-real-key" } },
    ]);
    expect(document.querySelector('[data-testid="account-key-input"]')).toBeNull();
  });

  it("shows a refused save inside the dialog and keeps it open", async () => {
    const writes: { path: string; body: unknown }[] = [];
    await mount(
      recordingClient(writes, async () => {
        throw new ApiError(400, "invalid", "that key was not accepted", true);
      }),
    );
    await press('[data-testid="account-add-key"]');
    await typeInto('[data-testid="account-key-input"]', "th-not-a-real-key");
    await press('[data-testid="account-key-save"]');

    expect(writes).toHaveLength(1);
    expect(document.querySelector('[data-testid="account-key-input"]')).not.toBeNull();
    expect(document.querySelector('[data-testid="account-key-error"]')?.textContent).toBe(
      "that key was not accepted",
    );
  });
});

describe("ApiKeyView confirms before clearing a credential", () => {
  // QA matrix X8, and the standing rule behind it — an operator lost a live
  // key to an unconfirmed clear. `store_key("")` is how the store spells a
  // delete, it is irreversible from this console (the hub shows a key's value
  // once), and the menu item cannot show what it costs.
  it("offers Remove key as a confirmation, never as a direct write", async () => {
    const writes: unknown[] = [];
    await mount(adminClient(async () => credential({ source: "company" }), writes));

    // Mounting and rendering the row must never have written anything.
    expect(writes).toHaveLength(0);
    // At rest the menu is closed, so there is no one-press path to a cleared
    // key on the page at all.
    expect(document.querySelector('[data-testid="account-remove-key"]')).toBeNull();

    // Open it and press the destructive item. This is the press that used to
    // clear the key outright, and the assertion that matters is that it still
    // has not written anything.
    await press('[data-testid="account-row-menu"]');
    await press('[data-testid="account-remove-key"]');
    expect(writes).toHaveLength(0);
    expect(document.body.textContent ?? "").toContain("Remove this company's account key?");

    // Only the second, deliberate press reaches the host — and reaches it once,
    // with the empty value that is how the store spells a delete.
    await press('[data-testid="account-remove-key-confirm"]');
    expect(writes).toEqual([{ key: "" }]);
  });
});

describe("ApiKeyView's Remove-key dialog names dependents (KR-L3-01)", () => {
  /** A client whose `PUT …/credential` answers from a fixed queue, one
   * response per call — some entries `throw` to model a refused attempt. */
  function queuedRemoveClient(
    writes: unknown[],
    responses: (unknown | (() => never))[],
    status: CompanyCredentialStatus,
  ): OpenCompanyClient {
    let call = 0;
    return {
      scopeFor: () => "/api/v1/companies/acme",
      get: async (path: string) => {
        if (path.endsWith("/credential/billing")) return { configured: false };
        if (path.endsWith("/auth/me")) return { role: "admin" };
        if (path.endsWith("/credential")) return status;
        throw new Error(`unexpected GET ${path}`);
      },
      put: async (_path: string, body: unknown) => {
        writes.push(body);
        const response = responses[Math.min(call, responses.length - 1)];
        call += 1;
        if (typeof response === "function") return (response as () => never)();
        return response;
      },
    } as unknown as OpenCompanyClient;
  }

  // State 1: nothing depends on the key when the dialog opens. The generic
  // two sentences show, no reason line renders, and the confirm sends no
  // `confirmInUse` at all — this is `api-key-view.test.ts`'s pre-existing
  // "offers Remove key as a confirmation" test, restated here to pin the
  // absence of the reason element alongside it.
  it("plain remove: no reason known, confirms with no confirmInUse", async () => {
    const writes: unknown[] = [];
    await mount(
      queuedRemoveClient(writes, [{ status: credential({ source: "none" }), note: "Key removed." }], credential({
        source: "company",
      })),
    );

    await press('[data-testid="account-row-menu"]');
    await press('[data-testid="account-remove-key"]');
    expect(document.querySelector('[data-testid="account-remove-key-reason"]')).toBeNull();

    await press('[data-testid="account-remove-key-confirm"]');
    expect(writes).toEqual([{ key: "" }]);
    // The dialog closes on a landed, unconfirmed clear.
    expect(document.body.textContent ?? "").not.toContain("Remove this company's account key?");
  });

  // State 2: the status the page already read carries `usedBy` — the dialog
  // names dependents the moment it opens, with no round trip, and the first
  // (and only) press already sends `confirmInUse: true`.
  it("in-use, known up front: names dependents on open and confirms with confirmInUse", async () => {
    const writes: unknown[] = [];
    await mount(
      queuedRemoveClient(
        writes,
        [{ status: credential({ source: "none" }), note: "Key removed.", usedBy: { surfaces: ["llm", "composio"] } }],
        credential({ source: "company", usedBy: { surfaces: ["llm", "composio"] } }),
      ),
    );

    await press('[data-testid="account-row-menu"]');
    await press('[data-testid="account-remove-key"]');
    expect(document.querySelector('[data-testid="account-remove-key-reason"]')?.textContent).toBe(
      "Used by TinyHumans on the LLM page and by Composio.",
    );

    await press('[data-testid="account-remove-key-confirm"]');
    expect(writes).toEqual([{ key: "", confirmInUse: true }]);
  });

  // State 3: nothing known at open — the generic text shows, no reason line —
  // but something starts depending on the key before the confirm reaches the
  // host. The stale, uninformed first attempt is refused `409 in_use`; the
  // dialog must reopen with the server's own reason (KR-L3-01's actual bug)
  // rather than closing on a toast, and a second press then sends
  // `confirmInUse: true` and lands.
  it("stale then 409: reopens with the server's reason, then confirms and clears", async () => {
    const writes: unknown[] = [];
    const refusal = () => {
      throw new ApiError(
        409,
        "in_use",
        "Used by Composio.",
        true,
      );
    };
    await mount(
      queuedRemoveClient(
        writes,
        [refusal, { status: credential({ source: "none" }), note: "Key removed." }],
        credential({ source: "company" }),
      ),
    );

    await press('[data-testid="account-row-menu"]');
    await press('[data-testid="account-remove-key"]');
    expect(document.querySelector('[data-testid="account-remove-key-reason"]')).toBeNull();

    // First, uninformed press: refused, and the dialog stays open and now
    // shows the host's own reason instead of closing with a bare toast.
    await press('[data-testid="account-remove-key-confirm"]');
    expect(writes).toEqual([{ key: "" }]);
    expect(document.body.textContent ?? "").toContain("Remove this company's account key?");
    expect(document.querySelector('[data-testid="account-remove-key-reason"]')?.textContent).toBe(
      "Used by Composio.",
    );

    // Second, now-informed press: confirms, and lands.
    await press('[data-testid="account-remove-key-confirm"]');
    expect(writes).toEqual([{ key: "" }, { key: "", confirmInUse: true }]);
    expect(document.body.textContent ?? "").not.toContain("Remove this company's account key?");
  });
});

describe("ApiKeyView never renders an unreadable store as an empty one", () => {
  // `company_key::resolve` propagates a secret-store read error rather than
  // falling through to the instance identity, because a connection made under
  // a silently-borrowed identity belongs to the wrong account invisibly and
  // permanently. The console has to spend a state on that, or the distinction
  // the host paid for is thrown away at the last step.
  it("says the host could not answer, and offers no empty state", async () => {
    const client = clientFor({
      credential: async () => {
        throw new Error("secret store unavailable");
      },
      billing: async () => ({ configured: false }),
    });

    await mount(client);

    const subline = container.querySelector('[data-testid="account-row-subline"]');
    expect(subline?.textContent).toContain("not the same as having no key");
    expect(container.querySelector('[data-testid="account-empty"]')).toBeNull();
    // And no balance under a row that has just said it does not know whose
    // account this is.
    expect(container.querySelector('[data-testid="account-balance"]')).toBeNull();
  });

  // The same honesty, applied to the controls rather than the words. An admin
  // reading "the host could not say" must not be offered a key field beside
  // it: the write it opens overwrites a write-only credential this console has
  // just admitted it cannot see, and the value it replaces cannot be read back
  // from the hub, which shows a key's plaintext once.
  it("offers no way to overwrite a key it cannot read", async () => {
    const client = {
      scopeFor: () => "/api/v1/companies/acme",
      get: async (path: string) => {
        if (path.endsWith("/credential/billing")) return { configured: false };
        if (path.endsWith("/auth/me")) return { role: "admin" };
        if (path.endsWith("/credential")) throw new Error("secret store unavailable");
        throw new Error(`unexpected GET ${path}`);
      },
    } as unknown as OpenCompanyClient;

    await mount(client);

    // The row is there, saying it does not know — that part is the point above.
    expect(container.querySelector('[data-testid="account-row"]')).not.toBeNull();
    // The header card's action is gone rather than disabled: there is no state
    // in which it is the right offer, so a greyed one would only invite a
    // retry.
    expect(container.querySelector('[data-testid="account-add-key"]')).toBeNull();
    // And the row menu, which carries the same "Add a key" item, cannot open.
    const menu = container.querySelector('[data-testid="account-row-menu"]');
    expect(menu).not.toBeNull();
    // Either spelling counts — the trigger is a `Button` rendered through the
    // menu primitive, and which of the two it forwards is the primitive's
    // business rather than this page's.
    const shut =
      menu?.hasAttribute("disabled") === true || menu?.getAttribute("aria-disabled") === "true";
    expect(shut).toBe(true);
  });
});

describe("ApiKeyView offers no control that cannot act", () => {
  // The rule that removed a toggle from the Managed inference row. Remove key
  // clears `tinyhumans/key`, which a company on the instance's identity does
  // not have — so offering it would be a destructive control that changes
  // nothing.
  it("does not offer Remove key when the identity is the instance's", async () => {
    const writes: unknown[] = [];
    // An **admin**, so the gate under test is the source rather than the role:
    // a member's menu is disabled whatever the tier, and asserting through one
    // would pass for the wrong reason.
    await mount(adminClient(async () => credential({ configured: false, source: "static" }), writes));

    await press('[data-testid="account-row-menu"]');

    // The menu opens — the admin gets the other item — and the destructive one
    // is simply not in it. `tinyhumans/key` is not what resolved here, so a
    // Remove would clear nothing.
    expect(document.body.textContent ?? "").toContain("Add a key");
    expect(document.querySelector('[data-testid="account-remove-key"]')).toBeNull();
    expect(writes).toHaveLength(0);
  });
});

describe("ApiKeyView redeems a returning grant whatever else failed", () => {
  // The grant comes back as a top-level navigation: `App` takes the code off
  // the URL before the first render, strips the address bar because it is a
  // live single-use credential, and hands it to a module-local box that a
  // reload empties. `useRedeemKeyGrant`, which `ApiKeyView` calls
  // unconditionally, is the only thing on this page that spends it.
  //
  // So that call must not be gated on the credential read. It was, briefly:
  // the header's action is chosen from `status`, which is null while the read
  // is in flight and stays null when it fails — and a company whose secret
  // store hiccuped on exactly that page load would have lost the key it had
  // just minted, with nothing on screen to try again with. The sign-in button
  // is gone from this page (2026-09-14); the redemption must not have gone
  // with it. Only what is *shown* may depend on the read.
  it("finishes the link even when the credential read fails", async () => {
    const finished: unknown[] = [];
    captureKeyLink({ state: "st", code: "cd" }, false);

    const client = {
      scopeFor: () => "/api/v1/companies/acme",
      get: async (path: string) => {
        if (path.endsWith("/credential/billing")) return { configured: false };
        if (path.endsWith("/auth/me")) return { role: "admin" };
        if (path.endsWith("/credential")) throw new Error("secret store unavailable");
        throw new Error(`unexpected GET ${path}`);
      },
      post: async (path: string, body: unknown) => {
        finished.push({ path, body });
        return { status: credential({ source: "company" }), note: "" };
      },
    } as unknown as OpenCompanyClient;

    await mount(client);
    await act(async () => {});

    expect(finished).toHaveLength(1);
    expect((finished[0] as { path: string }).path).toContain("/credential/link/finish");
    expect((finished[0] as { body: unknown }).body).toEqual({ state: "st", code: "cd" });
  });

  // And the box is emptied by the redemption rather than by the mount, so a
  // page that never had a grant never calls the route.
  it("calls nothing when no grant is pending", async () => {
    const finished: unknown[] = [];
    captureKeyLink(null, false);

    const client = {
      scopeFor: () => "/api/v1/companies/acme",
      get: async (path: string) => {
        if (path.endsWith("/credential/billing")) return { configured: false };
        if (path.endsWith("/auth/me")) return { role: "admin" };
        if (path.endsWith("/credential")) return credential({ source: "company" });
        throw new Error(`unexpected GET ${path}`);
      },
      post: async () => {
        finished.push(true);
        return {};
      },
    } as unknown as OpenCompanyClient;

    await mount(client);
    await act(async () => {});

    expect(finished).toHaveLength(0);
  });
});

describe("ApiKeyView's account-key dialog two-step flow (keys rework #2306, slice 4b)", () => {
  /** A client whose `PUT …/credential` answers from a fixed queue, one
   * response per call — the two-step flow posts the key, then (on a
   * `needsModel` answer) posts it again with a model. `status` is the fixed
   * `GET …/credential` answer, matching what the real host would report
   * before a save (the fill line is a property of the *read*, not the
   * write). */
  function queuedClient(
    writes: unknown[],
    responses: unknown[],
    status: CompanyCredentialStatus = credential({ configured: false, source: "none", hubLink: true }),
  ): OpenCompanyClient {
    let call = 0;
    return {
      scopeFor: () => "/api/v1/companies/acme",
      get: async (path: string) => {
        if (path.endsWith("/credential/billing")) return { configured: false };
        if (path.endsWith("/auth/me")) return { role: "admin" };
        if (path.endsWith("/credential")) return status;
        throw new Error(`unexpected GET ${path}`);
      },
      put: async (_path: string, body: unknown) => {
        writes.push(body);
        const response = responses[Math.min(call, responses.length - 1)];
        call += 1;
        return response;
      },
    } as unknown as OpenCompanyClient;
  }

  it("the fill line names only the slots saving fills", async () => {
    await mount(
      queuedClient(
        [],
        [{ status: credential({ source: "company" }), note: "" }],
        credential({
          configured: false,
          source: "none",
          hubLink: true,
          inferenceHasOwnKey: true,
          composioHasOwnKey: false,
          searchHasOwnKey: true,
          defaultSet: false,
        }),
      ),
    );
    await press('[data-testid="account-add-key"]');

    expect(document.querySelector('[data-testid="account-key-fill-line"]')?.textContent).toContain(
      "Saving also connects TinyHumans for Composio.",
    );
    expect(document.querySelector('[data-testid="account-key-llm-link"]')).toBeNull();
    expect(document.querySelector('[data-testid="account-key-composio-link"]')).not.toBeNull();
  });

  it("no fill line when both pages hold their own keys", async () => {
    await mount(
      queuedClient(
        [],
        [{ status: credential({ source: "company" }), note: "" }],
        credential({
          configured: false,
          source: "none",
          hubLink: true,
          inferenceHasOwnKey: true,
          composioHasOwnKey: true,
          searchHasOwnKey: true,
          defaultSet: false,
        }),
      ),
    );
    await press('[data-testid="account-add-key"]');

    expect(document.querySelector('[data-testid="account-key-fill-line"]')).toBeNull();
  });

  it("needs_model opens step two, then reposts the key and the chosen model", async () => {
    const writes: unknown[] = [];
    await mount(
      queuedClient(writes, [
        {
          status: credential({ source: "none" }),
          note: "Key saved. Choose a model to finish setting up TinyHumans for LLM.",
          needsModel: true,
          setsDefault: true,
          models: ["acme/test-model"],
        },
        {
          status: credential({ source: "company" }),
          note: "Key saved. TinyHumans is set up for LLM with acme/test-model. It is now the default for new work.",
        },
      ]),
    );
    await press('[data-testid="account-add-key"]');
    await typeInto('[data-testid="account-key-input"]', "th-not-a-real-key");
    await press('[data-testid="account-key-save"]');

    expect(document.querySelector('[data-testid="account-key-model-step"]')).not.toBeNull();
    expect(document.body.textContent ?? "").toContain("Choose the model new work uses");
    expect(document.querySelector('[data-testid="account-key-note"]')?.textContent).toBe(
      "Key saved. Choose a model to finish setting up TinyHumans for LLM.",
    );

    // Switch the model field to free text rather than driving the catalog
    // combobox's popover through jsdom — the e2e spec exercises the real
    // combobox in a real browser.
    await press('[data-testid="inference-model-enter-id"]');
    await typeInto("#account-key-model", "acme/test-model");
    await press('[data-testid="account-key-model-save"]');

    expect(writes).toEqual([
      { key: "th-not-a-real-key" },
      { key: "th-not-a-real-key", model: "acme/test-model" },
    ]);
    expect(document.querySelector('[data-testid="account-key-model-step"]')).toBeNull();
    expect(document.querySelector('[data-testid="account-key-input"]')).toBeNull();
  });

  it("closing step two forgets the pending key", async () => {
    const writes: unknown[] = [];
    await mount(
      queuedClient(writes, [
        {
          status: credential({ source: "none" }),
          note: "note",
          needsModel: true,
          setsDefault: false,
          models: ["acme/test-model"],
        },
      ]),
    );
    await press('[data-testid="account-add-key"]');
    await typeInto('[data-testid="account-key-input"]', "th-not-a-real-key");
    await press('[data-testid="account-key-save"]');
    expect(document.querySelector('[data-testid="account-key-model-step"]')).not.toBeNull();

    await pressButtonNamed("Cancel");
    expect(document.querySelector('[data-testid="account-key-model-step"]')).toBeNull();

    // Reopening starts over at step one, with an empty key field — and sends
    // no second request for a key that closing already forgot.
    await press('[data-testid="account-add-key"]');
    expect(document.querySelector('[data-testid="account-key-model-step"]')).toBeNull();
    expect(
      (document.querySelector('[data-testid="account-key-input"]') as HTMLInputElement | null)
        ?.value,
    ).toBe("");
    expect(writes).toHaveLength(1);
  });

  it("a response without needsModel closes at once", async () => {
    const writes: unknown[] = [];
    await mount(
      queuedClient(writes, [{ status: credential({ source: "company" }), note: "Key saved." }]),
    );
    await press('[data-testid="account-add-key"]');
    await typeInto('[data-testid="account-key-input"]', "th-not-a-real-key");
    await press('[data-testid="account-key-save"]');

    expect(writes).toEqual([{ key: "th-not-a-real-key" }]);
    expect(document.querySelector('[data-testid="account-key-model-step"]')).toBeNull();
    expect(document.querySelector('[data-testid="account-key-input"]')).toBeNull();
  });

  // Round-3b review, P2-4: a client whose PUT never resolves until the test
  // says so — the only way to actually get a Cancel press to land while the
  // save it started is still in flight.
  function deferredClient(writes: unknown[]): {
    client: OpenCompanyClient;
    resolve: (value: unknown) => void;
  } {
    const pending: { resolve: (value: unknown) => void } = { resolve: () => {} };
    const client = {
      scopeFor: () => "/api/v1/companies/acme",
      get: async (path: string) => {
        if (path.endsWith("/credential/billing")) return { configured: false };
        if (path.endsWith("/auth/me")) return { role: "admin" };
        if (path.endsWith("/credential")) return credential({ configured: false, source: "none" });
        throw new Error(`unexpected GET ${path}`);
      },
      put: async (_path: string, body: unknown) => {
        writes.push(body);
        return new Promise((resolve) => {
          pending.resolve = resolve;
        });
      },
    } as unknown as OpenCompanyClient;
    return { client, resolve: (value: unknown) => pending.resolve(value) };
  }

  // The bug this whole item is about: Cancel pressed while step one's save was
  // still in flight let its late `needsModel` answer land after the dialog
  // had already closed, silently reopening it on step two the next time it
  // was opened. The fix is the busy guard on `closeKeyDialog` (Escape and the
  // backdrop go through it too) plus disabling Cancel itself; the `attempt`
  // counter is the backstop for whatever that guard does not catch. This test
  // asserts the outcome rather than which mechanism produced it: pressed
  // during the save, Cancel must do nothing at all, and the save must still
  // land normally on step two once it settles.
  it("Cancel does nothing while the save is in flight, and the dialog still lands on step two once it settles (round-3b review, P2-4)", async () => {
    const writes: unknown[] = [];
    const { client, resolve } = deferredClient(writes);
    await mount(client);

    await press('[data-testid="account-add-key"]');
    await typeInto('[data-testid="account-key-input"]', "th-not-a-real-key");
    await press('[data-testid="account-key-save"]');

    const cancelButton = Array.from(document.querySelectorAll("button")).find(
      (candidate) => candidate.textContent?.trim() === "Cancel",
    );
    expect(cancelButton?.disabled).toBe(true);

    await pressButtonNamed("Cancel");
    expect(document.querySelector('[data-testid="account-key-input"]')).not.toBeNull();
    expect(writes).toHaveLength(1);

    await act(async () => {
      resolve({
        status: credential({ source: "none" }),
        note: "note",
        needsModel: true,
        setsDefault: false,
        models: ["acme/test-model"],
      });
    });
    await act(async () => {});

    expect(document.querySelector('[data-testid="account-key-model-step"]')).not.toBeNull();
  });
});

describe("ApiKeyView offers a restart action when the host says one is needed (KR-ACCT-01)", () => {
  function queuedClient(
    writes: unknown[],
    responses: unknown[],
    status: CompanyCredentialStatus = credential({ configured: false, source: "none" }),
  ): OpenCompanyClient {
    let call = 0;
    return {
      scopeFor: () => "/api/v1/companies/acme",
      get: async (path: string) => {
        if (path.endsWith("/credential/billing")) return { configured: false };
        if (path.endsWith("/auth/me")) return { role: "admin" };
        if (path.endsWith("/credential")) return status;
        throw new Error(`unexpected GET ${path}`);
      },
      put: async (_path: string, body: unknown) => {
        writes.push(body);
        const response = responses[Math.min(call, responses.length - 1)];
        call += 1;
        return response;
      },
    } as unknown as OpenCompanyClient;
  }

  it("save: renames the toast and wires Restart now to the same endpoint the LLM page uses", async () => {
    const writes: unknown[] = [];
    await mount(
      queuedClient(writes, [
        { status: credential({ source: "company" }), note: "Key saved.", restartRequired: true },
      ]),
    );
    await press('[data-testid="account-add-key"]');
    await typeInto('[data-testid="account-key-input"]', "th-not-a-real-key");
    await press('[data-testid="account-key-save"]');

    expect(toast.success).toHaveBeenCalledWith(
      "Key saved — restart required to use it.",
      expect.objectContaining({ action: expect.objectContaining({ label: "Restart now" }) }),
    );

    const [, options] = vi.mocked(toast.success).mock.calls.at(-1) ?? [];
    api.restartInference.mockResolvedValueOnce({});
    await act(async () => {
      (options as { action?: { onClick: () => void } })?.action?.onClick();
    });
    await act(async () => {});

    expect(api.restartInference).toHaveBeenCalledWith(expect.anything(), "acme");
  });

  it("does not rename the toast or offer a restart when the host does not say one is needed", async () => {
    const writes: unknown[] = [];
    await mount(
      queuedClient(writes, [{ status: credential({ source: "company" }), note: "Key saved." }]),
    );
    await press('[data-testid="account-add-key"]');
    await typeInto('[data-testid="account-key-input"]', "th-not-a-real-key");
    await press('[data-testid="account-key-save"]');

    expect(toast.success).toHaveBeenCalledWith(
      "Key saved.",
      expect.objectContaining({ action: undefined }),
    );
  });

  it("step two also offers Restart now when the host says one is needed there", async () => {
    const writes: unknown[] = [];
    await mount(
      queuedClient(writes, [
        {
          status: credential({ source: "none" }),
          note: "note",
          needsModel: true,
          setsDefault: false,
          models: ["acme/test-model"],
        },
        { status: credential({ source: "company" }), note: "Key saved.", restartRequired: true },
      ]),
    );
    await press('[data-testid="account-add-key"]');
    await typeInto('[data-testid="account-key-input"]', "th-not-a-real-key");
    await press('[data-testid="account-key-save"]');
    await press('[data-testid="inference-model-enter-id"]');
    await typeInto("#account-key-model", "acme/test-model");
    await press('[data-testid="account-key-model-save"]');

    expect(toast.success).toHaveBeenCalledWith(
      "Key saved — restart required to use it.",
      expect.objectContaining({ action: expect.objectContaining({ label: "Restart now" }) }),
    );
  });
});

describe("ApiKeyView does not claim a key works because it is stored", () => {
  const HUB = {
    manageKeysUrl: "https://hub.example/keys",
    topUpUrl: "https://hub.example/top-up",
  };

  /** A page whose credential resolves to this company's own key, with the hub's
   * billing read answering however the case needs. */
  function page(billing: () => Promise<unknown>) {
    return clientFor({
      credential: async () => credential({ configured: true, source: "company", account: HUB }),
      billing,
    });
  }

  const state = () =>
    container.querySelector('[data-testid="account-card-state"]')?.textContent?.trim();
  const subline = () =>
    container.querySelector('[data-testid="account-row-subline"]')?.textContent?.trim();

  it("says action is needed, and why, when the hub refused the stored key", async () => {
    await mount(
      page(async () => ({
        configured: true,
        unavailable: "TinyHumans refused this company's key.",
        unavailableReason: "rejected",
        unavailableCode: "http_401",
      })),
    );

    expect(state()).toBe("Action needed");
    expect(subline()).toContain("refusing it");
    expect(subline()).not.toContain("Acting as this company's own");
  });

  // Money does not repair a refused credential, and a Top up beside "replace it
  // to reconnect" offers the wrong fix on the one row that is telling somebody
  // what to do.
  it("offers no top-up link against a refused key", async () => {
    await mount(
      page(async () => ({
        configured: true,
        unavailable: "TinyHumans refused this company's key.",
        unavailableReason: "rejected",
      })),
    );

    expect(container.querySelector('[data-testid="billing-top-up"]')).toBeNull();
  });

  // The control. An outage is not a dead key: the row stays connected and the
  // host-derived top-up link stays offered.
  it("keeps the connected row and its top-up link through an outage", async () => {
    await mount(
      page(async () => ({
        configured: true,
        unavailable: "TinyHumans could not be reached just now.",
        unavailableReason: "unreachable",
      })),
    );

    expect(state()).toBe("Connected");
    expect(subline()).toContain("Acting as this company's own");
    expect(
      container.querySelector('[data-testid="billing-top-up"]')?.getAttribute("href"),
    ).toBe(HUB.topUpUrl);
  });

  // The hub's response body is what put raw JSON under a balance. Nothing it
  // returns is rendered anywhere on the page.
  it("renders none of the hub's response body", async () => {
    const body = '{"success":false,"error":"Invalid API key","statusCode":401}';
    await mount(
      page(async () => ({
        configured: true,
        unavailable: body,
        unavailableReason: "rejected",
      })),
    );

    const text = container.textContent ?? "";
    expect(text).not.toContain("Invalid API key");
    expect(text).not.toContain("success");
    expect(text).toContain("Balance unknown");
  });
});
