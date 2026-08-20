// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { BillingView } from "@/views/BillingView";

/**
 * The conditional surfaces of `BillingView`, which are where its whole job is.
 *
 * The view's own doc comment explains why it reports four failure modes
 * separately rather than as one "Connected" badge: a single green tick is right
 * for one of them and actively misleading for the other three, sending an
 * operator hunting through a form for a problem that is not in it. That is a
 * behaviour, and none of it was exercised — the existing suite covers only the
 * company-switch remount. A regression that collapsed the not-granted alert into
 * the not-in-build one, or that dropped the connected badge, would have shipped
 * green.
 *
 * The one that most needed a test is `load`: Chargebee and PayPal are unrelated
 * integrations loaded together, and under `Promise.all` a PayPal failure blanked
 * the Chargebee form too.
 */

const CHARGEBEE_OK = {
  apiKeyConfigured: true,
  site: "acme-test",
  webhookConfigured: true,
  webhookUrl: "https://oc.example/hooks/acme/chargebee",
  granted: true,
  inBuild: true,
};

const PAYPAL_OK = {
  clientIdConfigured: true,
  clientSecretConfigured: true,
  environment: "sandbox",
  granted: true,
  inBuild: true,
};

/** A client answering each billing read with a value or a rejection. */
function clientWith(answers: {
  chargebee?: unknown | Error;
  paypal?: unknown | Error;
}): OpenCompanyClient {
  const answer = (value: unknown) =>
    value instanceof Error ? Promise.reject(value) : Promise.resolve(value);
  return {
    scopeFor: () => "/api/v1/companies/acme",
    get: (path: string) =>
      path.endsWith("/billing/paypal")
        ? answer(answers.paypal ?? PAYPAL_OK)
        : answer(answers.chargebee ?? CHARGEBEE_OK),
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

async function show(client: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(BillingView, { client, company: "acme" }));
  });
}

function at(testid: string): HTMLElement | null {
  return container.querySelector<HTMLElement>(`[data-testid="${testid}"]`);
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

describe("BillingView load failures", () => {
  it("keeps the Chargebee form usable when only PayPal fails to load", async () => {
    // The regression this pins: `Promise.all` rejected the pair, so a PayPal
    // read that failed sent the whole page to an error card and an operator
    // could not reach the Chargebee settings at all — for a problem that had
    // nothing to do with Chargebee.
    await show(clientWith({ paypal: new Error("paypal is having a moment") }));

    expect(at("billing-load-error")).toBeNull();
    expect(at("billing-view")).not.toBeNull();
    expect(at("billing-site")).not.toBeNull();

    // And the failure is reported where the failure is.
    const failed = at("paypal-load-error");
    expect(failed?.textContent).toContain("paypal is having a moment");
    // The PayPal form is withheld rather than shown against unknown state: with
    // no stored `environment` to compare against, a Save would post fields the
    // operator never touched.
    expect(at("paypal-client-id")).toBeNull();
  });

  it("shows the page-level error when Chargebee itself fails", async () => {
    await show(clientWith({ chargebee: new Error("store unreachable") }));
    expect(at("billing-load-error")?.textContent).toContain(
      "store unreachable",
    );
  });

  it("renders both halves when both load", async () => {
    // The control: the two assertions above are only worth having if the
    // ordinary path does NOT produce either error card.
    await show(clientWith({}));
    expect(at("billing-load-error")).toBeNull();
    expect(at("paypal-load-error")).toBeNull();
    expect(at("billing-connected")).not.toBeNull();
    expect(at("paypal-connected")).not.toBeNull();
  });
});

describe("BillingView status surfaces", () => {
  it("names the manifest, not the form, when the company does not grant chargebee", async () => {
    // Both credentials stored and still nothing reaches an agent. The remedy is
    // `company.toml`, so saying "not connected" here would send the operator
    // back through a form that is already correct.
    await show(clientWith({ chargebee: { ...CHARGEBEE_OK, granted: false } }));
    expect(at("billing-not-granted")?.textContent).toContain("[tools].allow");
    expect(at("billing-not-in-build")).toBeNull();
    // Both credentials are configured (CHARGEBEE_OK) and the badge must still
    // not agree with a page that just said this integration does nothing.
    expect(at("billing-connected")).toBeNull();
  });

  it("says the host lacks the feature, and says only that", async () => {
    // Not-in-build outranks not-granted: granting `chargebee` in the manifest
    // fixes nothing on a host compiled without it, and showing both alerts
    // gives two remedies for one problem.
    await show(
      clientWith({
        chargebee: { ...CHARGEBEE_OK, granted: false, inBuild: false },
      }),
    );
    expect(at("billing-not-in-build")).not.toBeNull();
    expect(at("billing-not-granted")).toBeNull();
    expect(at("billing-connected")).toBeNull();
  });

  it("withholds the connected badges when the host lacks the feature, even with credentials configured", async () => {
    // The regression this pins: a green badge next to a banner saying the
    // feature is inert on this build is exactly the disagreement the file's
    // own doc comment says must not happen.
    await show(
      clientWith({
        chargebee: { ...CHARGEBEE_OK, inBuild: false, granted: false },
        paypal: { ...PAYPAL_OK, inBuild: false, granted: false },
      }),
    );
    expect(at("billing-not-in-build")).not.toBeNull();
    expect(at("paypal-not-in-build")).not.toBeNull();
    expect(at("billing-connected")).toBeNull();
    expect(at("paypal-connected")).toBeNull();
  });

  it("withholds the connected badges when the manifest does not grant the tool, even with credentials configured", async () => {
    await show(
      clientWith({
        chargebee: { ...CHARGEBEE_OK, granted: false },
        paypal: { ...PAYPAL_OK, granted: false },
      }),
    );
    expect(at("billing-not-granted")).not.toBeNull();
    expect(at("paypal-not-granted")).not.toBeNull();
    expect(at("billing-connected")).toBeNull();
    expect(at("paypal-connected")).toBeNull();
  });

  it("withholds the connected badge until BOTH the key and the site are stored", async () => {
    // A key with no site produces requests against no host and a site with no
    // key produces unauthenticated ones; either alone is not a connection.
    await show(clientWith({ chargebee: { ...CHARGEBEE_OK, site: null } }));
    expect(at("billing-connected")).toBeNull();

    await show(
      clientWith({ chargebee: { ...CHARGEBEE_OK, apiKeyConfigured: false } }),
    );
    expect(at("billing-connected")).toBeNull();
  });

  it("explains a missing public URL instead of showing a loopback webhook", async () => {
    // Chargebee cannot deliver to a loopback address, so a box containing one
    // sends an operator to configure a webhook that silently never arrives.
    await show(
      clientWith({ chargebee: { ...CHARGEBEE_OK, webhookUrl: null } }),
    );
    expect(at("billing-webhook-url")).toBeNull();
    expect(at("billing-no-webhook-url")?.textContent).toContain(
      "OPENCOMPANY_PUBLIC_URL",
    );
  });

  it("offers Disconnect only once something is actually stored", async () => {
    await show(
      clientWith({
        chargebee: {
          ...CHARGEBEE_OK,
          apiKeyConfigured: false,
          webhookConfigured: false,
        },
        paypal: {
          ...PAYPAL_OK,
          clientIdConfigured: false,
          clientSecretConfigured: false,
        },
      }),
    );
    expect(at("billing-clear")).toBeNull();
    expect(at("paypal-clear")).toBeNull();

    await show(clientWith({}));
    expect(at("billing-clear")).not.toBeNull();
    expect(at("paypal-clear")).not.toBeNull();
  });

  it("reports the PayPal grant and build gaps on their own terms", async () => {
    await show(clientWith({ paypal: { ...PAYPAL_OK, granted: false } }));
    expect(at("paypal-not-granted")?.textContent).toContain("[tools].allow");
    expect(at("paypal-connected")).toBeNull();

    await show(
      clientWith({ paypal: { ...PAYPAL_OK, granted: false, inBuild: false } }),
    );
    expect(at("paypal-not-in-build")).not.toBeNull();
    expect(at("paypal-not-granted")).toBeNull();
    expect(at("paypal-connected")).toBeNull();
  });

  it("seeds the site box from what is stored and the environment from PayPal", async () => {
    // The site is the one non-secret field, so an operator correcting a typo
    // should not have to retype it — while the credentials stay empty, because
    // an input pre-filled with dots invites somebody to submit the dots.
    await show(clientWith({ paypal: { ...PAYPAL_OK, environment: "live" } }));
    expect(
      container.querySelector<HTMLInputElement>('[data-testid="billing-site"]')
        ?.value,
    ).toBe("acme-test");
    expect(
      container.querySelector<HTMLSelectElement>(
        '[data-testid="paypal-environment"]',
      )?.value,
    ).toBe("live");
    expect(
      container.querySelector<HTMLInputElement>(
        '[data-testid="billing-api-key"]',
      )?.value,
    ).toBe("");
  });
});
