// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Login } from "@/views/Login";
import type { OpenCompanyClient } from "@/api/client";
import { ApiError } from "@/api/types";
import { MIN_PASSWORD_LENGTH } from "@/lib/generate-password";

/**
 * A company nobody has joined (`AuthConfig.claimable`) offers the first
 * person in the admin account, from the screen they are looking at. This is
 * how a fresh `docker compose up` gets its admin without a shell command
 * nobody told them about.
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

function client(
  config: { claimable: boolean; magicLink?: boolean },
  postSignIn: ReturnType<typeof vi.fn> = vi.fn(),
): OpenCompanyClient & { postSignIn: ReturnType<typeof vi.fn> } {
  return {
    scopeFor: () => "/api/v1/company",
    get: vi.fn().mockImplementation(async (path: string) => {
      if (path.endsWith("/auth/config")) {
        return {
          mode: "email",
          passwords: true,
          magicLink: config.magicLink ?? false,
          claimable: config.claimable,
        };
      }
      throw new Error(`unexpected GET ${path}`);
    }),
    post: vi.fn(),
    postSignIn,
  } as unknown as OpenCompanyClient & { postSignIn: ReturnType<typeof vi.fn> };
}

async function renderLogin(c: OpenCompanyClient, onSignedIn: () => void = () => {}) {
  await act(async () => {
    root.render(createElement(Login, { client: c, company: "acme", onSignedIn }));
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

const find = <T extends Element = HTMLElement>(testId: string) =>
  container.querySelector(`[data-testid="${testId}"]`) as T | null;

async function type(input: HTMLInputElement, value: string) {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

describe("the first-admin claim", () => {
  it("is offered instead of the sign-in form while nobody has joined", async () => {
    await renderLogin(client({ claimable: true }));

    expect(find("login-claim")).toBeTruthy();
    // Not beside it: with no users there is nobody who could pass that form.
    expect(container.querySelector("#password")).toBeNull();
    expect(container.querySelector("#email")).toBeNull();
  });

  it("is not offered once somebody has", async () => {
    await renderLogin(client({ claimable: false }));

    expect(find("login-claim")).toBeNull();
    expect(container.querySelector("#password")).toBeTruthy();
  });

  it("starts with a generated password the person can read and copy", async () => {
    await renderLogin(client({ claimable: true }));

    const password = find<HTMLInputElement>("new-password");
    expect(password?.type).toBe("text");
    expect(password?.value.length).toBeGreaterThanOrEqual(MIN_PASSWORD_LENGTH);
    expect(find("new-password-copy")).toBeTruthy();

    const before = password!.value;
    await act(async () => {
      find("new-password-generate")!.click();
    });
    expect(find<HTMLInputElement>("new-password")!.value).not.toBe(before);
  });

  it("creates the account with the chosen login and password, and signs in", async () => {
    const postSignIn = vi.fn().mockResolvedValue({ id: "u1", email: "admin", role: "admin" });
    const onSignedIn = vi.fn();
    await renderLogin(client({ claimable: true }, postSignIn), onSignedIn);

    await type(find<HTMLInputElement>("claim-login")!, "admin");
    await type(find<HTMLInputElement>("new-password")!, "correct horse battery staple");
    await act(async () => {
      find("claim-submit")!.click();
      await Promise.resolve();
    });

    expect(postSignIn).toHaveBeenCalledWith("/api/v1/company/auth/claim", {
      email: "admin",
      password: "correct horse battery staple",
    });
    expect(onSignedIn).toHaveBeenCalled();
  });

  it("calls out a short password before the round trip", async () => {
    const postSignIn = vi.fn();
    await renderLogin(client({ claimable: true }, postSignIn));

    await type(find<HTMLInputElement>("claim-login")!, "admin");
    await type(find<HTMLInputElement>("new-password")!, "short");
    await act(async () => {
      find("claim-submit")!.click();
    });

    expect(postSignIn).not.toHaveBeenCalled();
    expect(find("new-password-problem")?.textContent).toMatch(/12/);
  });

  it("falls back to the sign-in form when somebody got there first", async () => {
    const postSignIn = vi
      .fn()
      .mockRejectedValue(new ApiError(409, "already_claimed", "already claimed", true));
    await renderLogin(client({ claimable: true }, postSignIn));

    await type(find<HTMLInputElement>("claim-login")!, "admin");
    await act(async () => {
      find("claim-submit")!.click();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(find("login-claim")).toBeNull();
    expect(container.querySelector("#password")).toBeTruthy();
    expect(container.textContent).toMatch(/already set up/i);
  });
});
