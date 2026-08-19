// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterAll, afterEach, beforeEach, describe, expect, it } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { PrefilledDraft, WorkflowGraph } from "@/api/workflows";
import { WorkflowCreateDialog } from "@/views/WorkflowCreateDialog";

/**
 * Issue #1053: the id derives itself from the name — and stops the moment the
 * id is somebody's.
 *
 * `workflow-id.test.ts` pins the two helpers. This pins the WIRING, which is
 * where the bug actually lives: which handler calls the slugger, when the
 * `idTouched` latch closes, and that edit mode never derives. None of that is
 * reachable from a pure helper — a latch that never closes and a latch that
 * closes on open both leave `slugifyWorkflowId` perfectly correct, and both
 * ship the bug.
 *
 * The dialog earned a jsdom harness in #1006, so the honest test is now
 * available: mount it, type into it, read the id field back out. Before that
 * landed, this file could not have been written, and the PR said so rather
 * than claiming the coverage.
 */

/** A saved graph for edit mode: its id must survive every name keystroke. */
function savedGraph(): WorkflowGraph {
  return {
    id: "weekly_report",
    name: "Weekly report",
    description: "Assemble and send the Monday summary.",
    version: "v1",
    nodes: [
      { id: "start", kind: "trigger", name: "Start" },
      { id: "search", kind: "tool_call", name: "Search", config: { slug: "web_search" } },
    ],
    edges: [{ from: "start", to: "search" }],
  } as WorkflowGraph;
}

/** A host that answers every read the dialog makes on open. */
function stubClient(): OpenCompanyClient {
  return {
    scopeFor: () => "/api/companies/acme",
    listTeam: async () => [],
    get: async (path: string) =>
      path.endsWith("/wired-channels") ? { channels: [] } : [],
  } as unknown as OpenCompanyClient;
}

// Same jsdom gap `workflow-editor-unsaved-work` documents: `scrollIntoView`
// does not exist, and the dialog calls it from a `requestAnimationFrame` that
// can outlive a test body. Kept up for the whole file, restored in `afterAll`.
const originalScrollIntoView = Object.getOwnPropertyDescriptor(
  Element.prototype,
  "scrollIntoView",
);

if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}

let container: HTMLDivElement;
let root: Root;

/** The dialog portals into `document.body`, not into the mount container. */
function field(suffix: string): HTMLInputElement {
  const el = document.body.querySelector<HTMLInputElement>(`input[id$="-${suffix}"]`);
  expect(el, `no input matching id$="-${suffix}"`).toBeTruthy();
  return el as HTMLInputElement;
}

/** Type into a controlled `<input>` the way React reads it. */
async function type(el: HTMLInputElement, value: string) {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!;
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function render(opts: {
  open: boolean;
  workflow?: WorkflowGraph | null;
  prefilledDraft?: PrefilledDraft | null;
}) {
  await act(async () => {
    root.render(
      createElement(WorkflowCreateDialog, {
        client: stubClient(),
        company: "acme",
        open: opts.open,
        onOpenChange: () => {},
        workflow: opts.workflow ?? null,
        prefilledDraft: opts.prefilledDraft ?? null,
      }),
    );
  });
}

async function open(opts: {
  workflow?: WorkflowGraph | null;
  prefilledDraft?: PrefilledDraft | null;
} = {}) {
  await render({ ...opts, open: true });
}

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
    true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

afterAll(() => {
  if (originalScrollIntoView) {
    Object.defineProperty(Element.prototype, "scrollIntoView", originalScrollIntoView);
  } else {
    delete (Element.prototype as unknown as Record<string, unknown>).scrollIntoView;
  }
});

describe("the create form derives the id from the name (#1053)", () => {
  it("fills the id in as the name is typed, so a bare name is not rejected", async () => {
    await open();

    await type(field("name"), "Weekly digest");

    // The reported bug verbatim: this used to be "" and the form answered
    // "Give the workflow an id."
    expect(field("id").value).toBe("weekly-digest");
  });

  it("keeps deriving while the id is nobody's, so it tracks the name", async () => {
    await open();
    const name = field("name");

    await type(name, "Weekly");
    expect(field("id").value).toBe("weekly");
    await type(name, "Weekly digest v2");
    expect(field("id").value).toBe("weekly-digest-v2");
  });

  it("leaves the field alone when the name derives to nothing", async () => {
    await open();

    await type(field("name"), "Campaign pipeline");
    expect(field("id").value).toBe("campaign-pipeline");
    // "???" has nothing usable in it. Writing the empty derivation would blank
    // a good id on a keystroke — the clobber the guard exists to prevent.
    await type(field("name"), "???");
    expect(field("id").value).toBe("campaign-pipeline");
  });
});

describe("deriving stops the moment the id is somebody's (#1053)", () => {
  it("never writes over an id the operator typed", async () => {
    await open();

    await type(field("id"), "chosen-by-hand");
    await type(field("name"), "Weekly digest");

    expect(field("id").value).toBe("chosen-by-hand");
  });

  it("treats clearing the id back to empty as a decision too", async () => {
    await open();

    // Derive one, then take it away. An operator who empties the field meant
    // to; the next keystroke in Name must not quietly refill it.
    await type(field("name"), "Weekly digest");
    expect(field("id").value).toBe("weekly-digest");
    await type(field("id"), "");
    await type(field("name"), "Weekly digest final");

    expect(field("id").value).toBe("");
  });

  it("starts derivable again on the next open, so the latch is not one-way", async () => {
    await open();
    await type(field("id"), "the-first-one");
    await type(field("name"), "First workflow");
    expect(field("id").value).toBe("the-first-one");

    // The latch is deliberately sticky WITHIN an open — clobbering a chosen id
    // is the worse bug. That makes failing to reset it on the next open the
    // matching failure: the second workflow an operator creates in a session
    // would silently stop deriving, and only the second one.
    await render({ open: false });
    await open();

    await type(field("name"), "Second workflow");
    expect(field("id").value).toBe("second-workflow");
  });

  it("never writes over an id a copilot draft supplied", async () => {
    // Create mode with a drafted graph: the copilot chose the id, so it is
    // already somebody's before the operator has touched anything.
    await open({
      prefilledDraft: { workflow: { ...savedGraph(), id: "copilot-chose-this" } },
    });
    expect(field("id").value).toBe("copilot-chose-this");

    await type(field("name"), "Renamed by the operator");

    expect(field("id").value).toBe("copilot-chose-this");
  });
});

describe("edit mode never derives (#1053)", () => {
  it("leaves the saved id alone however the name is edited", async () => {
    await open({ workflow: savedGraph() });
    expect(field("id").value).toBe("weekly_report");

    await type(field("name"), "Weekly report v2");

    // Re-slugging here would be a rename, and the id keys the saved graph, its
    // schedule and its run history. The field is `readOnly` in edit mode, but
    // that is the UI's guard; this pins the handler's own.
    expect(field("id").value).toBe("weekly_report");
  });
});
