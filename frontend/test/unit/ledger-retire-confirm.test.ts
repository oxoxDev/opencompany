// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { LedgerList, LedgerRead, LedgerSummary } from "@/api/ledgers";

/**
 * Issue #1216: "Retire" on the Ledgers view used to delete the whole ledger on
 * a single unconfirmed click, 8px from "Record" — unlike row deletion on the
 * same screen, which asks first.
 *
 * The assertion the issue explicitly calls for is that the retire API is NOT
 * called until the confirm button is pressed, which is a claim about what the
 * DOM does across a click sequence — not something the pure helpers in
 * `api/ledgers.ts` can pin on their own. So this file renders the view, the
 * way `workflow-run-failure.test.ts` does for the same reason.
 */

const toasts = vi.hoisted(() => ({
  base: vi.fn(),
  success: vi.fn(),
  error: vi.fn(),
  warning: vi.fn(),
  info: vi.fn(),
}));

vi.mock("sonner", () => {
  const toast = Object.assign(toasts.base, {
    success: toasts.success,
    error: toasts.error,
    warning: toasts.warning,
    info: toasts.info,
  });
  return { toast };
});

const { LedgersView } = await import("@/views/LedgersView");

const LEDGER: LedgerSummary = {
  slug: "customer-promises",
  title: "Customer promises",
  purpose: "What we told a customer we would do.",
  source: "events",
  derived: "derived/CUSTOMER_PROMISES.md",
  writtenBy: "",
  builtin: false,
  fields: [],
  statuses: [{ name: "open" }, { name: "kept", closed: true }],
  sections: [],
  open: 6,
  closed: 0,
};

const LIST: LedgerList = { ledgers: [LEDGER], remaining: 3 };
const EMPTY_LIST: LedgerList = { ledgers: [], remaining: 4 };
const READ: LedgerRead = { ledger: LEDGER, entries: [], matched: 0 };

/** A client whose retire (`del`) call is whatever the test hands it. */
function fakeClient(del: (path: string) => Promise<unknown>): OpenCompanyClient {
  let retired = false;
  return {
    scopeFor: (company: string | null) => `/api/v1/${company ?? "company"}`,
    get: async (path: string) => {
      if (path.endsWith("/ledgers")) return retired ? EMPTY_LIST : LIST;
      return READ;
    },
    del: async (path: string) => {
      const result = await del(path);
      retired = true;
      return result;
    },
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

// The AlertDialog renders through a portal onto `document.body`, not inside
// `container` — so button lookups search the whole document, the same way
// `workflow-index-first.test.ts` reaches `workflow-delete-confirm`.
function button(label: string): HTMLButtonElement {
  const found = Array.from(document.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === label,
  );
  if (!found) throw new Error(`no “${label}” button in:\n${document.body.innerHTML}`);
  return found as HTMLButtonElement;
}

function maybeButton(label: string): HTMLButtonElement | undefined {
  return Array.from(document.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === label,
  ) as HTMLButtonElement | undefined;
}

beforeEach(async () => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
    true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  vi.clearAllMocks();
});

afterEach(async () => {
  await act(async () => {
    root.unmount();
  });
  container.remove();
});

async function mount(del: (path: string) => Promise<unknown>, onOpenLedger = vi.fn()) {
  await act(async () => {
    root.render(
      createElement(LedgersView, {
        client: fakeClient(del),
        company: "acme",
        sub: LEDGER.slug,
        onOpenLedger,
      }),
    );
  });
  return onOpenLedger;
}

describe("Retire asks before it deletes a ledger (issue #1216)", () => {
  it("opens a confirm dialog on click and does not call the retire API", async () => {
    const del = vi.fn(async (_path: string) => undefined);
    await mount(del);

    await act(async () => {
      button("Retire").click();
    });

    expect(del).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain(`Retire “${LEDGER.title}”?`);
    expect(
      document.querySelector('[data-testid="ledger-retire-confirm"]'),
    ).not.toBeNull();
  });

  it("Keep it dismisses the dialog without ever calling the retire API", async () => {
    const del = vi.fn(async (_path: string) => undefined);
    await mount(del);

    await act(async () => {
      button("Retire").click();
    });
    expect(maybeButton("Keep it")).toBeDefined();

    await act(async () => {
      button("Keep it").click();
    });

    expect(del).not.toHaveBeenCalled();
    expect(
      document.querySelector('[data-testid="ledger-retire-confirm"]'),
    ).toBeNull();
  });

  it("only calls the retire API once the confirm button is pressed", async () => {
    const del = vi.fn(async (_path: string) => undefined);
    const onOpenLedger = await mount(del);

    await act(async () => {
      button("Retire").click();
    });
    await act(async () => {
      button("Retire ledger").click();
    });

    expect(del).toHaveBeenCalledTimes(1);
    expect(del.mock.calls[0][0]).toContain(`/ledgers/${LEDGER.slug}`);
    expect(toasts.success).toHaveBeenCalledWith(
      expect.stringContaining(`Retired ${LEDGER.slug}`),
    );
    // The address named this ledger; it must stop naming it once retired
    // rather than keep pointing at a dead slug (issue #1216).
    expect(onOpenLedger).toHaveBeenCalledWith(null);
  });
});
