// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { OpenCompanyClient } from "@/api/client";
import { SERVER_FETCH_TIMEOUT_MS, type ComposioStatus } from "@/api/composio";
import type {
  StreamHandlers,
  Transport,
  TransportRequest,
  TransportResponse,
} from "@/api/transport";
import { OAuthView } from "@/views/OAuthView";

/**
 * What the Apps page renders while the host is still reading a cold catalog.
 *
 * `composio-catalog-deadline.test.ts` pins the read. This pins the page, which
 * is where the two warnings the operator actually saw were assembled — and
 * where they turn out to be one failure: abandoning the read set the status to
 * `null`, so the catalog warning and the "couldn't check the grant" warning
 * both came from the same abandoned request rather than from two.
 *
 * A slow host is the case under test throughout. `SERVER_FETCH_TIMEOUT_MS` is
 * what the host may spend upstream before it degrades, so an answer just past
 * it is the ordinary cold-catalog shape, not a pathological one.
 */

/** The reply staged for one URL. `delayMs: null` never answers at all. */
interface Staged extends Partial<TransportResponse> {
  delayMs?: number | null;
}

class RoutedTransport implements Transport {
  readonly aborted: string[] = [];

  constructor(private readonly route: (path: string) => Staged) {}

  async request(req: TransportRequest): Promise<TransportResponse> {
    const staged = this.route(new URL(req.url).pathname);
    if (staged.delayMs === null) {
      await new Promise<never>((_, reject) => {
        req.signal?.addEventListener(
          "abort",
          () => {
            this.aborted.push(new URL(req.url).pathname);
            reject(new DOMException("The operation was aborted.", "AbortError"));
          },
          { once: true },
        );
      });
    }
    if (staged.delayMs) {
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(resolve, staged.delayMs!);
        req.signal?.addEventListener(
          "abort",
          () => {
            clearTimeout(timer);
            this.aborted.push(new URL(req.url).pathname);
            reject(new DOMException("The operation was aborted.", "AbortError"));
          },
          { once: true },
        );
      });
    }
    return {
      status: staged.status ?? 200,
      statusText: staged.statusText ?? "",
      url: req.url,
      text: staged.text ?? "",
      header: () => null,
    };
  }

  subscribe(_url: string, _handlers: StreamHandlers): () => void {
    return () => {};
  }
}

function status(overrides: Partial<ComposioStatus> = {}): ComposioStatus {
  return {
    inBuild: true,
    granted: true,
    credentialSource: "company",
    backendUrl: "https://api.example.test",
    toolkits: [],
    openMode: true,
    effectiveToolkits: ["gmail", "slack"],
    effectiveCatalog: [
      { slug: "gmail", name: "Gmail", description: "Send and read email.", logo: null, categories: ["email"] },
      { slug: "slack", name: "Slack", description: "Post to channels.", logo: null, categories: ["communication"] },
    ],
    catalogSource: "backend",
    catalogNotice: null,
    ...overrides,
  };
}

/** Gmail already connected, so the grant warning has something to warn about. */
const CONNECTIONS = JSON.stringify([
  { provider: "gmail", connected: true, via: ["composio"] },
]);

/**
 * A host whose Composio status answers after `answersIn`, and which serves
 * nothing else this page asks for.
 *
 * The 404 is deliberate rather than lazy: every other section on this page
 * treats it as "not served here" and hides, which keeps the render down to the
 * grid this test is about.
 */
function hostAnswering(composio: Staged, connections: string = CONNECTIONS): RoutedTransport {
  return new RoutedTransport((path) => {
    if (path.endsWith("/composio")) return composio;
    if (path.endsWith("/composio/connections")) return { text: JSON.stringify([]) };
    if (path.endsWith("/connections")) return { text: connections };
    if (path.endsWith("/auth/me")) return { text: JSON.stringify({ role: "admin" }) };
    return { status: 404, text: "" };
  });
}

function clientOn(transport: Transport): OpenCompanyClient {
  return new OpenCompanyClient(
    { baseUrl: "https://host.test", company: null, operatorToken: null, sessionHeader: null },
    transport,
  );
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.useFakeTimers();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
});

async function mount(client: OpenCompanyClient, company: string | null = null) {
  await act(async () => {
    root.render(createElement(OAuthView, { client, company }));
  });
}

/** Let `ms` of staged latency elapse, and React settle the state it produced. */
async function elapse(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

const PROBE_FAILED = '[data-testid="providers-probe-failed"]';
const NOT_GRANTED = '[data-testid="providers-not-granted"]';
const GRANT_UNKNOWN = "Couldn't check whether this company grants";

function text(): string {
  return container.textContent ?? "";
}

describe("the Apps page against a host reading a cold catalog", () => {
  it("waits for an answer that arrives past the host's own upstream budget", async () => {
    const answersIn = SERVER_FETCH_TIMEOUT_MS + 1_000;
    const client = clientOn(hostAnswering({ delayMs: answersIn, text: JSON.stringify(status()) }));

    await mount(client);
    await elapse(answersIn);

    // The catalog the host actually sent, on the FIRST visit — no reload, no
    // warm cache.
    expect(text()).toContain("Gmail");
    expect(text()).toContain("Slack");
    // And neither warning. They were one failure: abandoning the read nulled
    // the status, which is what produced both.
    expect(container.querySelectorAll(PROBE_FAILED)).toHaveLength(0);
    expect(text()).not.toContain(GRANT_UNKNOWN);
  });

  it("renders the host's own fallback notice when the host degraded", async () => {
    const answersIn = SERVER_FETCH_TIMEOUT_MS + 1_000;
    const notice = "the Composio backend did not answer within 5s";
    const client = clientOn(
      hostAnswering({
        delayMs: answersIn,
        text: JSON.stringify(status({ catalogSource: "fallback", catalogNotice: notice })),
      }),
    );

    await mount(client);
    await elapse(answersIn);

    // The graceful degradation the inversion was hiding: the host explains
    // itself, and the console shows what it said rather than "couldn't check".
    expect(text()).toContain(notice);
    expect(container.querySelectorAll(PROBE_FAILED)).toHaveLength(0);
  });

  it("keeps a false grant a false grant on a slow read", async () => {
    const answersIn = SERVER_FETCH_TIMEOUT_MS + 1_000;
    const client = clientOn(
      hostAnswering({ delayMs: answersIn, text: JSON.stringify(status({ granted: false })) }),
    );

    await mount(client);
    await elapse(answersIn);

    // `granted` survives as a real boolean. Abandoning the read left it
    // `undefined`, which renders as "we could not check" — the second warning
    // in the report, and collateral of the first rather than a second failure.
    expect(container.querySelectorAll(NOT_GRANTED)).toHaveLength(1);
    expect(text()).not.toContain(GRANT_UNKNOWN);
  });

  it("still says so honestly about a host that never answers", async () => {
    const client = clientOn(hostAnswering({ delayMs: null }));

    await mount(client);
    await elapse(60_000);

    // The warning is not weakened — only moved off the healthy path.
    expect(container.querySelectorAll(PROBE_FAILED)).toHaveLength(1);
  });

  it("reads a 404 as a host with no Composio surface, not as a failed check", async () => {
    // Nothing connected, so the empty state is the one this host can honestly
    // reach — the distinction is between "nothing here" and "could not check".
    const client = clientOn(hostAnswering({ status: 404, text: "" }, "[]"));

    await mount(client);
    await elapse(1_000);

    expect(container.querySelectorAll(PROBE_FAILED)).toHaveLength(0);
    expect(container.querySelectorAll('[data-testid="providers-empty"]')).toHaveLength(1);
  });

  it("cancels a superseded read instead of ignoring it, and warns about neither", async () => {
    const transport = hostAnswering({ delayMs: null });
    const client = clientOn(transport);

    await mount(client, "acme");
    // A company switch while the first read is still open.
    await act(async () => {
      root.render(createElement(OAuthView, { client, company: "globex" }));
    });
    await elapse(1_000);

    // The socket is released rather than left running for an answer nobody
    // will read.
    expect(transport.aborted).toContain("/api/v1/companies/acme/composio");
    // And the cancellation says nothing about the host, so it leaves no
    // warning behind for the company now on screen.
    expect(container.querySelectorAll(PROBE_FAILED)).toHaveLength(0);
    expect(text()).not.toContain(GRANT_UNKNOWN);
  });
});
