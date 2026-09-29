// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Login } from "@/views/Login";
import type { OpenCompanyClient } from "@/api/client";

/**
 * The dead-link recovery path keeps the destination a dead link was carrying.
 *
 * A setup hand-off link (`…/login?code=…#/company?from=setup`) that has
 * expired or been consumed lands here with the fragment still in the address:
 * `App` strips the single-use code, preserves the hash, and falls back to this
 * form. A replacement link asked for from here must carry the same fragment,
 * or following it lands on Overview and can show the tour welcome instead of
 * the roster setup just built. `askForLink` forwards the marker when — and only
 * when — the current address carries it.
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

/** A host answering `/auth/config`, recording every `get`/`post`. */
function hostReporting(
  config: Record<string, unknown>,
  post: ReturnType<typeof vi.fn>,
): OpenCompanyClient & { get: ReturnType<typeof vi.fn> } {
  const get = vi.fn().mockImplementation(async (path: string) => {
    const base = path.split("?")[0];
    if (base.endsWith("/auth/config")) return config;
    throw new Error(`unexpected GET ${path}`);
  });
  return { scopeFor: () => "/api/v1/company", get, post } as unknown as OpenCompanyClient & {
    get: ReturnType<typeof vi.fn>;
  };
}

async function render(client: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(Login, { client, company: "acme", onSignedIn: () => {} }));
    await Promise.resolve();
  });
}

/** Fills the form and sends it, the recovery path after a refused link. */
async function sendLinkRequest() {
  const input = container.querySelector("#email") as HTMLInputElement | null;
  expect(input, "no email field").toBeTruthy();
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!;
    setter.call(input!, "ada@example.com");
    input!.dispatchEvent(new Event("input", { bubbles: true }));
  });
  const submit = Array.from(container.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Email me a link"),
  );
  expect(submit, "no send button").toBeTruthy();
  await act(async () => {
    submit!.click();
    await Promise.resolve();
  });
}

describe("the dead-link recovery path", () => {
  it("forwards the setup destination when the address carries it", async () => {
    window.location.hash = "#/company?from=setup";
    const post = vi.fn().mockResolvedValue({ sent: true });
    const client = hostReporting({ mode: "email", passwords: true, magicLink: true }, post);
    await render(client);

    await sendLinkRequest();

    const [path, body] = post.mock.calls[0];
    expect(path).toBe("/api/v1/company/auth/request");
    expect(body).toEqual({ email: "ada@example.com", redirect: "#/company?from=setup" });
  });

  it("asks for nothing extra when the address carries no destination", async () => {
    window.location.hash = "#/overview";
    const post = vi.fn().mockResolvedValue({ sent: true });
    await render(hostReporting({ mode: "email", passwords: true, magicLink: true }, post));

    await sendLinkRequest();

    const [, body] = post.mock.calls[0];
    // `undefined` is dropped by JSON.stringify on the wire; the body is
    // unchanged from an ordinary sign-in request.
    expect(body).toEqual({ email: "ada@example.com" });
  });
});
