// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { PageManifestDto } from "@/api/types";
import { PagesView } from "@/views/PagesView";

/**
 * `PagesView`'s postMessage bridge is the actual security boundary between an
 * agent-authored page and the console's real GraphQL endpoint
 * (docs/spec/runtime/pages.md §6): the sandboxed iframe holds no session
 * cookie, so `event.source === iframe.contentWindow` is the only thing that
 * tells the console this request really came from its own embedded page and
 * not some other frame or tab posting the same shape of message. This is the
 * one piece of that view worth a unit test — everything else is either a
 * plain fetch-and-render list or the iframe element itself, which needs a
 * real browser to say anything about.
 */

const PAGE: PageManifestDto = {
  slug: "metrics",
  title: "Metrics",
  description: "The daily numbers.",
  icon: "chart",
  navVisible: true,
};

function clientWith(graphqlRequest: OpenCompanyClient["graphqlRequest"]): OpenCompanyClient {
  return {
    listPages: () => Promise.resolve([PAGE]),
    pageUrl: (slug: string) => `/api/v1/company/pages/${slug}`,
    graphqlRequest,
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

async function show(client: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(PagesView, { client, company: "acme" }));
  });
}

function iframe(): HTMLIFrameElement | null {
  return container.querySelector("iframe");
}

/** Fires the iframe `load` handler, minting the current document capability. */
function loadFrame(frame: HTMLIFrameElement): void {
  frame.dispatchEvent(new Event("load"));
}

/** Returns the capability the view hands to the loaded iframe document. */
function mintedCapability(frame: HTMLIFrameElement): string {
  const contentWindow = frame.contentWindow as Window;
  const postMessage = vi.spyOn(contentWindow, "postMessage").mockImplementation(() => {});
  loadFrame(frame);
  const init = postMessage.mock.calls.find(([msg]) => (msg as { type?: string })?.type === "oc:init");
  return (init?.[0] as { capability: string }).capability;
}

/** Lets any pending `.then`/`.catch` microtasks (the bridge's own reply) run. */
async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
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
  vi.restoreAllMocks();
});

describe("PagesView bridge", () => {
  it("embeds the page in an opaque-origin sandbox (allow-scripts, no allow-same-origin)", async () => {
    // The `sandbox` attribute is the actual isolation boundary: without
    // `allow-same-origin` the frame is opaque-origin and holds no session
    // cookie, so `event.source` / capability checks in the bridge are
    // meaningful. A regression that drops the attribute — or quietly adds
    // `allow-same-origin` — must be caught by the suite, not shipped.
    const graphqlRequest = vi.fn();
    await show(clientWith(graphqlRequest));

    const frame = iframe();
    expect(frame).not.toBeNull();
    const sandbox = frame!.getAttribute("sandbox");
    expect(sandbox).toContain("allow-scripts");
    expect(sandbox).not.toContain("allow-same-origin");
  });

  it("ignores an oc:graphql message whose source isn't the embedded iframe", async () => {
    const graphqlRequest = vi.fn();
    await show(clientWith(graphqlRequest));

    const frame = iframe();
    expect(frame).not.toBeNull();

    // Same shape as a legitimate request, but posted as if from `window`
    // itself rather than the iframe's own `contentWindow` — exactly what a
    // spoofing frame/tab would send.
    window.dispatchEvent(
      new MessageEvent("message", {
        data: { type: "oc:graphql", id: "spoofed", capability: "cap", query: "{ ping }" },
        source: window,
      }),
    );
    await flush();

    expect(graphqlRequest).not.toHaveBeenCalled();
  });

  it("forwards an oc:graphql message from the embedded iframe and replies on its contentWindow", async () => {
    const graphqlRequest = vi.fn().mockResolvedValue({ data: { ping: "pong" }, errors: undefined });
    await show(clientWith(graphqlRequest));

    const frame = iframe();
    expect(frame).not.toBeNull();
    const contentWindow = frame!.contentWindow;
    expect(contentWindow).toBeTruthy();
    const capability = mintedCapability(frame as HTMLIFrameElement);
    const postMessage = vi.spyOn(contentWindow as Window, "postMessage").mockImplementation(() => {});

    window.dispatchEvent(
      new MessageEvent("message", {
        data: { type: "oc:graphql", id: "req-1", capability, query: "{ ping }", variables: { a: 1 } },
        source: contentWindow,
        origin: "null",
      }),
    );
    await flush();

    expect(graphqlRequest).toHaveBeenCalledWith("{ ping }", { a: 1 });
    expect(postMessage).toHaveBeenCalledWith(
      { type: "oc:graphql:result", id: "req-1", data: { ping: "pong" }, errors: undefined },
      "*",
    );
  });

  it("ignores a same-source message that isn't the oc:graphql shape", async () => {
    const graphqlRequest = vi.fn();
    await show(clientWith(graphqlRequest));

    const contentWindow = iframe()?.contentWindow;
    window.dispatchEvent(
      new MessageEvent("message", {
        data: { type: "some-other-message" },
        source: contentWindow,
        origin: "null",
      }),
    );
    await flush();

    expect(graphqlRequest).not.toHaveBeenCalled();
  });

  it("rejects a same-source oc:graphql message carrying a stale capability", async () => {
    const graphqlRequest = vi.fn();
    await show(clientWith(graphqlRequest));

    const frame = iframe();
    expect(frame).not.toBeNull();
    const contentWindow = frame!.contentWindow as Window;
    // Mint a capability for the current document.
    const current = mintedCapability(frame as HTMLIFrameElement);
    // The page then navigates itself to a new document; the parent rotates the
    // capability on the resulting load. The forged message replays the OLD one.
    loadFrame(frame as HTMLIFrameElement);

    window.dispatchEvent(
      new MessageEvent("message", {
        data: { type: "oc:graphql", id: "stale", capability: current, query: "{ secrets }" },
        source: contentWindow,
        origin: "null",
      }),
    );
    await flush();

    expect(graphqlRequest).not.toHaveBeenCalled();
  });

  it("mints a fresh capability and hands it to the loaded iframe document via oc:init", async () => {
    const graphqlRequest = vi.fn();
    await show(clientWith(graphqlRequest));
    const frame = iframe();
    expect(frame).not.toBeNull();

    const contentWindow = frame!.contentWindow as Window;
    const postMessage = vi.spyOn(contentWindow, "postMessage").mockImplementation(() => {});
    const caps: string[] = [];
    loadFrame(frame as HTMLIFrameElement);
    loadFrame(frame as HTMLIFrameElement);
    for (const call of postMessage.mock.calls) {
      const msg = call[0] as { type?: string; capability?: string };
      if (msg?.type === "oc:init") caps.push(msg.capability as string);
    }
    expect(caps.length).toBe(2);
    expect(caps[0]).toBeTruthy();
    // A second load must rotate the token — a stale one must not stay live.
    expect(caps[0]).not.toEqual(caps[1]);
  });
});
