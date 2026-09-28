// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Login } from "@/views/Login";
import type { OpenCompanyClient } from "@/api/client";

/**
 * `AuthConfig.magicLink` false is a host with no mail transport: the mode is
 * still `email` and a password still signs people in, but a link asked for
 * here reaches nobody. Nothing else in the flow reveals that — `auth/request`
 * answers `sent: true` exactly as a host that delivered would — so this screen
 * must not offer the link at all. The password is the way in, and the only
 * form drawn is the one for it.
 */

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

function client(config: { magicLink: boolean; passwords: boolean }): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/company",
    get: vi.fn().mockImplementation(async (path: string) => {
      if (path.endsWith("/auth/config")) {
        return {
          mode: "email",
          passwords: config.passwords,
          magicLink: config.magicLink,
          claimable: false,
        };
      }
      throw new Error(`unexpected GET ${path}`);
    }),
    post: vi.fn(),
  } as unknown as OpenCompanyClient;
}

async function renderLogin(c: OpenCompanyClient) {
  await act(async () => {
    root.render(
      createElement(Login, { client: c, company: "acme", onSignedIn: () => {} }),
    );
    // Flush the microtasks the config-fetching effect resolves on.
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

const labels = () =>
  Array.from(container.querySelectorAll("button")).map((b) => b.textContent?.trim() ?? "");

const submitLabel = () =>
  (container.querySelector('button[type="submit"]')?.textContent ?? "").trim();

describe("a host that cannot deliver a magic link", () => {
  it("draws the password form and nothing about a link", async () => {
    await renderLogin(client({ magicLink: false, passwords: true }));

    expect(container.querySelector("#password")).toBeTruthy();
    expect(submitLabel()).toContain("Sign in");
    // No toggle to a link, no "email me a link" anywhere: the link is not a
    // sign-in on this host and offering it was the confusion this exists to
    // remove.
    expect(labels().some((l) => /link/i.test(l))).toBe(false);
    expect(container.textContent).not.toMatch(/email me a link/i);
  });

  it("takes a username, not only a mailbox", async () => {
    // The first admin on a host with no mail may have chosen `admin` as their
    // login. An `email` input would refuse it before the host ever saw it.
    await renderLogin(client({ magicLink: false, passwords: true }));

    const login = container.querySelector("#email") as HTMLInputElement | null;
    expect(login?.type).toBe("text");
    expect(container.querySelector('label[for="email"]')?.textContent).toMatch(/username/i);
  });

  it("leaves a host that mails alone", async () => {
    await renderLogin(client({ magicLink: true, passwords: true }));

    expect(submitLabel()).toContain("Email me a link");
    expect(labels().some((l) => l.includes("Use a password instead"))).toBe(true);
    expect((container.querySelector("#email") as HTMLInputElement | null)?.type).toBe("email");
  });
});
