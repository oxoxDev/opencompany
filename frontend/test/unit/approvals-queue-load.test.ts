// @vitest-environment jsdom

import { act, createElement, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { ApprovalSummary, CompanyStatus } from "@/api/types";
import { useCompany, type CompanyFeed } from "@/hooks/use-company";

/**
 * "Nothing is parked" and "we could not read what is parked" (issue #1229).
 *
 * `approvals: []` is both of those, and the Approvals page rendered the first
 * for both: **"All clear — nothing is waiting on you"** over a read that never
 * answered, beside a sidebar badge — carried over from the bootstrap status
 * read — saying fourteen things were waiting. On the one surface whose job is
 * to catch what needs a person, that is a confident instruction to stop
 * looking, and every parked request has a deadline after which it is declined
 * on its own.
 *
 * `queue` is the distinction the array cannot make. These tests are about when
 * it says each of its three words.
 */

const STATUS: CompanyStatus = {
  id: "acme",
  name: "Acme",
  lifecycle: "running",
  pending_approvals: 0,
};

function approvals(n: number): ApprovalSummary[] {
  return Array.from({ length: n }, (_, i) => ({
    id: `a${i}`,
    kind: "web_fetch",
    amount_usd: null,
    at_millis: 1_700_000_000_000 + i,
    agent: "seo",
    thread: "desk-marketing",
  }));
}

let container: HTMLDivElement;
let root: Root;

/** Renders `hook` and hands back the latest value it returned. */
function probe<T>(hook: () => T): () => T {
  let latest: T | undefined;
  const Probe = (): ReactElement | null => {
    latest = hook();
    return null;
  };
  act(() => root.render(createElement(Probe)));
  return () => {
    if (latest === undefined) throw new Error("the hook never rendered");
    return latest;
  };
}

/**
 * Let the mount fetch settle before asserting.
 *
 * `Promise.allSettled` resolves a few microtasks later than the `Promise.all`
 * it replaced, and `useCompany` also arms a visibility-gated poll, so a single
 * flush is not enough and an unbounded wait would hang on a hook that never
 * settles. Flush until the hook says it has left `loading`, with a ceiling.
 */
async function settle(feed: () => CompanyFeed) {
  for (let i = 0; i < 20 && feed().queue === "loading"; i++) {
    await act(async () => {
      await Promise.resolve();
    });
  }
}

/**
 * A client whose two reads can each be made to fail independently — which is
 * the point: they are two requests, and the bug was one of them taking the
 * other down with it.
 */
function client(opts: {
  status?: () => Promise<CompanyStatus>;
  approvals?: () => Promise<ApprovalSummary[]>;
}): OpenCompanyClient {
  return {
    status: opts.status ?? (async () => STATUS),
    approvals: opts.approvals ?? (async () => []),
  } as unknown as OpenCompanyClient;
}

const refuse = () => Promise.reject(new Error("connection refused"));

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

describe("useCompany — an unread queue is not an empty one (#1229)", () => {
  it("starts as loading, before either read has answered", () => {
    // Hoisted, not built inside the render: `useCompany` keys its poll on the
    // client's identity, so a fresh object per render re-arms the effect
    // forever. Same for the status object it is seeded with.
    const c = client({});
    const seed = { ...STATUS, pending_approvals: 14 };
    const feed = probe<CompanyFeed>(() => useCompany(c, "acme", seed));

    expect(feed().queue).toBe("loading");
    // And the array it would have rendered "All clear" from.
    expect(feed().approvals).toEqual([]);
  });

  it("says the queue could not be read when the first read fails", async () => {
    const c = client({
      approvals: refuse,
      status: async () => ({ ...STATUS, pending_approvals: 14 }),
    });
    const seed = { ...STATUS, pending_approvals: 14 };
    const feed = probe<CompanyFeed>(() => useCompany(c, "acme", seed));
    await settle(feed);

    expect(feed().queue).toBe("error");
    // The reported symptom: this empty array was rendering as "All clear"
    // while the badge beside it read 14.
    expect(feed().approvals).toEqual([]);
    expect(feed().status.pending_approvals).toBe(14);
  });

  it("says ready when the host answers with nothing parked", async () => {
    const c = client({});
    const feed = probe<CompanyFeed>(() => useCompany(c, "acme", STATUS));
    await settle(feed);

    // The genuinely-clear case, which must keep reading as clear.
    expect(feed().queue).toBe("ready");
    expect(feed().approvals).toEqual([]);
  });

  it("keeps the rows and stays ready when a later poll fails", async () => {
    let fail = false;
    const c = client({ approvals: () => (fail ? refuse() : Promise.resolve(approvals(3))) });
    const feed = probe<CompanyFeed>(() => useCompany(c, "acme", STATUS));
    await settle(feed);
    expect(feed().queue).toBe("ready");
    expect(feed().approvals).toHaveLength(3);

    // A dropped poll must not blank rows that are real, only possibly stale.
    fail = true;
    await act(async () => {
      await feed().refresh();
    });

    expect(feed().queue).toBe("ready");
    expect(feed().approvals).toHaveLength(3);
  });

  it("keeps a queue that arrived even though the status beside it failed", async () => {
    const c = client({ status: refuse, approvals: async () => approvals(2) });
    const feed = probe<CompanyFeed>(() => useCompany(c, "acme", STATUS));
    await settle(feed);

    // `Promise.all` rejected on the status and discarded this queue with it,
    // which is how the badge and the page came to be reading two moments.
    expect(feed().queue).toBe("ready");
    expect(feed().approvals).toHaveLength(2);
  });

  it("reconciles the badge to the queue only when both arrived", async () => {
    const c = client({
      status: async () => ({ ...STATUS, pending_approvals: 18 }),
      approvals: async () => approvals(14),
    });
    const feed = probe<CompanyFeed>(() => useCompany(c, "acme", STATUS));
    await settle(feed);

    expect(feed().status.pending_approvals).toBe(14);
  });
});
