import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DEFAULT_REQUEST_TIMEOUT_MS, OpenCompanyClient } from "@/api/client";
import {
  CATALOG_READ_TIMEOUT_MS,
  SERVER_FETCH_TIMEOUT_MS,
  getComposioStatus,
  type ComposioStatus,
} from "@/api/composio";
import type {
  StreamHandlers,
  Transport,
  TransportRequest,
  TransportResponse,
} from "@/api/transport";
import { ApiError } from "@/api/types";
import { classifyLoadFailure } from "@/lib/section-load";

/**
 * The deadline on `GET …/composio`, and the ordering it has to keep.
 *
 * The console's budget for this read used to equal the host's own budget for
 * the upstream catalog fetch *inside* it — two five-second bounds, nested. On a
 * cold catalog the host spends up to its whole budget upstream and then answers
 * with a flagged fallback, so the console gave up at exactly the moment the host
 * was about to explain itself: the operator saw "couldn't check" on their first
 * visit and a full catalog on their second, once the host's fifteen-minute cache
 * was warm.
 *
 * These pin the ordering that makes that impossible, that a host answering past
 * its own budget is still heard out, and that the three ways this read can fail
 * stay three different facts.
 */

/** A transport whose reply the test stages, per URL, with an optional delay. */
class DelayedTransport implements Transport {
  readonly seen: TransportRequest[] = [];
  readonly aborted: string[] = [];

  constructor(
    private readonly reply: (
      req: TransportRequest,
    ) => { delayMs?: number | null } & Partial<TransportResponse>,
  ) {}

  async request(req: TransportRequest): Promise<TransportResponse> {
    this.seen.push(req);
    const staged = this.reply(req);
    if (staged.delayMs === null) await new Promise<never>(() => {});
    if (staged.delayMs) {
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(resolve, staged.delayMs!);
        req.signal?.addEventListener(
          "abort",
          () => {
            clearTimeout(timer);
            this.aborted.push(req.url);
            reject(new DOMException("The operation was aborted.", "AbortError"));
          },
          { once: true },
        );
      });
    }
    return {
      status: staged.status ?? 200,
      statusText: staged.statusText ?? "",
      url: staged.url ?? req.url,
      text: staged.text ?? "",
      header: staged.header ?? (() => null),
    };
  }

  subscribe(_url: string, _handlers: StreamHandlers): () => void {
    return () => {};
  }
}

function clientOn(transport: Transport): OpenCompanyClient {
  return new OpenCompanyClient(
    { baseUrl: "https://host.test", company: null, operatorToken: null, sessionHeader: null },
    transport,
  );
}

/** The degraded answer a host gives when its own catalog fetch ran out of time. */
const DEGRADED: ComposioStatus = {
  inBuild: true,
  granted: true,
  credentialSource: "company",
  backendUrl: "https://api.example.test",
  toolkits: [],
  openMode: true,
  effectiveToolkits: ["gmail"],
  effectiveCatalog: [
    { slug: "gmail", name: "Gmail", description: "", logo: null, categories: [] },
  ],
  catalogSource: "fallback",
  catalogNotice: "the Composio backend did not answer within 5s",
};

describe("the catalog read's deadline", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("strictly dominates the host's own budget, and stays under the shared default", () => {
    // The invariant, not the literals. Re-equalising the two — which is the
    // state this issue was filed from — fails here first.
    expect(SERVER_FETCH_TIMEOUT_MS).toBeLessThan(CATALOG_READ_TIMEOUT_MS);
    expect(CATALOG_READ_TIMEOUT_MS).toBeLessThan(DEFAULT_REQUEST_TIMEOUT_MS);
  });

  it("hears out a host that answers past its own catalog budget", async () => {
    // A second past the host's upstream budget: the shape of a cold catalog
    // whose upstream ran long, where the host degrades gracefully and says so.
    const answersAt = SERVER_FETCH_TIMEOUT_MS + 1_000;
    const client = clientOn(
      new DelayedTransport(() => ({ delayMs: answersAt, text: JSON.stringify(DEGRADED) })),
    );

    const read = getComposioStatus(client, null, { timeoutMs: CATALOG_READ_TIMEOUT_MS });
    await vi.advanceTimersByTimeAsync(answersAt);

    // The whole point: the host's own honesty marker reaches the console.
    await expect(read).resolves.toMatchObject({
      catalogSource: "fallback",
      catalogNotice: "the Composio backend did not answer within 5s",
    });
  });

  it("gives up at its own deadline and not a moment earlier", async () => {
    const client = clientOn(new DelayedTransport(() => ({ delayMs: null })));

    let settled = false;
    const read = getComposioStatus(client, null, { timeoutMs: CATALOG_READ_TIMEOUT_MS }).catch(
      (err: unknown) => {
        settled = true;
        return err;
      },
    );

    await vi.advanceTimersByTimeAsync(CATALOG_READ_TIMEOUT_MS - 1);
    expect(settled, "a read must not be abandoned before its deadline").toBe(false);

    await vi.advanceTimersByTimeAsync(1);
    expect(await read).toMatchObject({ status: 0, code: "timeout" });
  });
});

describe("the three ways the catalog read can fail stay three facts", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("reads a 404 as a host with no Composio surface", async () => {
    const client = clientOn(new DelayedTransport(() => ({ status: 404, text: "" })));

    const err = await getComposioStatus(client, null).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(404);
    expect(classifyLoadFailure(err)).toBe("unavailable");
  });

  it("reads a host that never answered as unknown, not absent", async () => {
    const client = clientOn(new DelayedTransport(() => ({ delayMs: null })));

    const read = getComposioStatus(client, null, { timeoutMs: CATALOG_READ_TIMEOUT_MS }).catch(
      (e: unknown) => e,
    );
    await vi.advanceTimersByTimeAsync(CATALOG_READ_TIMEOUT_MS);

    const err = await read;
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ status: 0, code: "timeout" });
    // The sentinel the hand-rolled race needed is gone; this is what carries
    // "we could not check" now, and it must not read as "not served here".
    expect(classifyLoadFailure(err)).toBe("error");
  });

  it("keeps a caller's own cancellation apart from both of them", async () => {
    const transport = new DelayedTransport(() => ({ delayMs: 60_000 }));
    const client = clientOn(transport);
    const abort = new AbortController();

    const read = getComposioStatus(client, null, {
      timeoutMs: CATALOG_READ_TIMEOUT_MS,
      signal: abort.signal,
    }).catch((e: unknown) => e);
    abort.abort();

    const err = await read;
    expect(err).toBeInstanceOf(Error);
    expect((err as Error).name).toBe("AbortError");
    expect(err).not.toBeInstanceOf(ApiError);
    // And the socket is actually released, rather than the answer merely
    // ignored when it eventually arrives.
    expect(transport.aborted).toEqual(["https://host.test/api/v1/company/composio"]);
  });
});
