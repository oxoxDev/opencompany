// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { RunsPage, WorkflowGraph, WorkflowRunOutcome } from "@/api/workflows";

/**
 * A stale "Load older" page from a superseded company cannot land on the
 * current one (PR #1090 review, issue #1012 follow-up).
 *
 * `create_company_workflow` only checks id-uniqueness against the requesting
 * company's own seed/overlay/manifest ids (`src/company/workflow_create.rs`) —
 * never across companies — so two companies can genuinely share a workflow id,
 * most commonly a seed workflow shipped identically to every company. Before
 * this fix, `loadOlder`'s only staleness guard was `runsForRef.current !==
 * workflow`: a workflow id, not a scope. An older-page request started against
 * company A, held in flight while the operator switches to company B — which
 * happens to have a workflow of the same id already selected — sails through
 * that check and appends A's rows onto B's history.
 *
 * This file renders the view, the way `workflow-run-failure.test.ts` earns its
 * exception to the pure-function rule: the claim is about what ends up in the
 * DOM after two overlapping fetches race, which the pure pagination helpers
 * next door cannot pin.
 */

vi.mock("sonner", () => {
  const noop = vi.fn();
  const toast = Object.assign(noop, { success: noop, error: noop, warning: noop, info: noop });
  return { toast };
});

vi.mock("next-themes", () => ({ useTheme: () => ({ resolvedTheme: "light" }) }));

// React Flow measures its container on mount; jsdom has no layout and no
// `ResizeObserver`, so these stubs are what let the view render at all. None is
// under test. (Same three as `workflow-run-failure.test.ts`.)
class NoopResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}
Object.assign(globalThis, {
  ResizeObserver: NoopResizeObserver,
  DOMMatrixReadOnly: class {
    m22 = 1;
  },
});
Object.defineProperties(globalThis.HTMLElement.prototype, {
  offsetHeight: { get: () => 400 },
  offsetWidth: { get: () => 800 },
});

const { WorkflowsView } = await import("@/views/WorkflowsView");

/** The id both companies' workflow happen to share — e.g. an identical seed. */
const WF_ID = "shared-wf";

const GRAPH: WorkflowGraph = {
  id: WF_ID,
  name: "Shared workflow",
  version: null,
  nodes: [{ id: "start", kind: "trigger", name: "Start" }],
  edges: [],
};

function run(seq: number, runId: string): WorkflowRunOutcome {
  return {
    seq,
    atMillis: seq * 1_000,
    workflowId: WF_ID,
    scheduled: false,
    runId,
    deliveries: [],
    pendingApprovals: [],
  };
}

/** A resolver the test controls, so a fetch can be held open across renders. */
function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

/**
 * Two companies, `acme` and `beta`, both list a workflow with id `WF_ID`.
 * `acme`'s first page has more runs past it; the "older" fetch it drives is
 * held open on `acmeOlder` until the test releases it. `beta`'s first page
 * answers immediately with exactly one run and nothing older.
 */
function makeClient(acmeOlder: { promise: Promise<RunsPage> }): OpenCompanyClient {
  return {
    scopeFor: (company: string | null) => `/api/v1/${company ?? "company"}`,
    get: async (path: string) => {
      if (path.endsWith("/workflows")) return [{ id: WF_ID, name: GRAPH.name }];
      if (path.includes("/workflows/tool-slugs")) return { slugs: [], unwired: [] };
      if (path.includes("/workflows/wired-channels")) return { channels: [] };
      if (path.includes("/workflows/runs")) {
        const url = new URL(path, "http://test");
        const workflow = url.searchParams.get("workflow");
        const beforeSeq = url.searchParams.get("before_seq");
        if (workflow === WF_ID && path.startsWith("/api/v1/acme/") && beforeSeq == null) {
          return {
            runs: [run(20, "acme-r2"), run(10, "acme-r1")],
            hasMore: true,
            nextBeforeSeq: 10,
            // Both cursor halves — `cursorFromPage` in WorkflowsView.tsx needs
            // both non-null before `loadOlder` will fire at all.
            nextBeforeAtMillis: 10_000,
          } satisfies RunsPage;
        }
        if (workflow === WF_ID && path.startsWith("/api/v1/acme/") && beforeSeq === "10") {
          // The in-flight "Load older" request — held until the test resolves it.
          return acmeOlder.promise;
        }
        if (workflow === WF_ID && path.startsWith("/api/v1/beta/") && beforeSeq == null) {
          return { runs: [run(5, "beta-r1")], hasMore: false } satisfies RunsPage;
        }
        // Any other/unscoped runs read (e.g. the company-wide index fetch,
        // inert here since the view stays on the detail page throughout).
        return { runs: [], hasMore: false } satisfies RunsPage;
      }
      const m = path.match(/\/workflows\/([^/?]+)$/);
      if (m) return GRAPH;
      return null;
    },
    post: async () => ({}),
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
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

function rows(): NodeListOf<Element> {
  return container.querySelectorAll('[data-testid="workflow-run-row"]');
}

describe("run history pagination race across a company switch", () => {
  it("drops a superseded company's stale older page instead of appending it (decisive)", async () => {
    const acmeOlder = deferred<RunsPage>();
    const client = makeClient(acmeOlder);

    await act(async () => {
      root.render(createElement(WorkflowsView, { client, company: "acme", sub: WF_ID }));
    });

    // Open History: acme's first page (2 runs, more available) is on screen.
    await act(async () => {
      (
        container.querySelector('[data-testid="workflow-history-toggle"]') as HTMLButtonElement
      ).click();
    });
    expect(rows()).toHaveLength(2);
    expect(
      container.querySelector('[data-testid="workflow-run-load-older"]'),
    ).not.toBeNull();

    // Fire "Load older" for acme. The request is held open — nothing resolves
    // it until the test does, below.
    await act(async () => {
      (
        container.querySelector('[data-testid="workflow-run-load-older"]') as HTMLButtonElement
      ).click();
    });

    // The operator switches to `beta`, which happens to have a workflow of the
    // SAME id already selected (`sub` unchanged — `WorkflowsView` never resets
    // `selectedId` on a company prop change, and the deep-link-apply effect
    // will not reselect an id it already applied). Beta's own first page
    // answers immediately: one run, nothing older.
    await act(async () => {
      root.render(createElement(WorkflowsView, { client, company: "beta", sub: WF_ID }));
    });
    expect(rows()).toHaveLength(1);
    // Beta has no older page of its own, so the button is gone.
    expect(container.querySelector('[data-testid="workflow-run-load-older"]')).toBeNull();

    // Now let acme's stale older-page answer land.
    await act(async () => {
      acmeOlder.resolve({ runs: [run(1, "acme-older")], hasMore: false });
    });

    // Pre-fix: `runsForRef.current !== workflow` alone passed (the id is the
    // same string in both companies), so this response was appended onto
    // beta's page — 1 + 1 = 2 rows, one of them acme's stale row, and the
    // stale response's own (absent) cursor would even overwrite beta's
    // pagination metadata. Post-fix: the company and generation guards also
    // fail, so the response is dropped and beta's page is exactly what it was.
    expect(rows()).toHaveLength(1);
  });
});
