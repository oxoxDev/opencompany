// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterAll, afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { WorkflowGraph } from "@/api/workflows";
import { WorkflowCreateDialog } from "@/views/WorkflowCreateDialog";

/**
 * Issue #1006: the edit dialog must not destroy unsaved graph edits, and must
 * not be able to lock itself shut.
 *
 * Both claims are about a mounted dialog reacting to a click — what a Cancel
 * press does when the form has been touched, and whether Cancel still works
 * after a failed save. Neither is reachable from a pure helper, so this earns
 * the same jsdom exception as `setup-wizard-finish-gate`: mount it, click it,
 * assert what came back out.
 *
 * The serialisation half is driven through a mocked `configFromDraft` on
 * purpose. Its failure branch means the form's validation and the serializer
 * DISAGREED, which by construction no keystroke can produce — so the only
 * honest way to pin "and then the operator can still get out" is to make them
 * disagree.
 */

const { configFromDraft } = vi.hoisted(() => ({ configFromDraft: vi.fn() }));

vi.mock("@/lib/workflow-node-config", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/workflow-node-config")>();
  configFromDraft.mockImplementation(actual.configFromDraft);
  return { ...actual, configFromDraft };
});

/** A saved graph the dialog hydrates from: valid, so `validate()` passes and
 * Save reaches the serialisation step. */
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

/** A host that answers every read the dialog makes on open, and records writes.
 * `put` rejects if it is ever called: a save that failed to serialise must not
 * have reached the host. */
function stubClient(): OpenCompanyClient & { puts: unknown[] } {
  const puts: unknown[] = [];
  return {
    puts,
    scopeFor: () => "/api/companies/acme",
    listTeam: async () => [],
    get: async (path: string) =>
      path.endsWith("/wired-channels") ? { channels: [] } : [],
    put: async (_path: string, body: unknown) => {
      puts.push(body);
      return savedGraph();
    },
  } as unknown as OpenCompanyClient & { puts: unknown[] };
}

/**
 * jsdom implements no scrolling at all, so `Element.prototype.scrollIntoView`
 * simply does not exist. The dialog scrolls its error banner into view from a
 * `requestAnimationFrame` after a failed save, which puts the resulting
 * TypeError outside every test body — Vitest reports 899 passing tests and
 * then fails the run on one unhandled error. Supplying the missing method is
 * the honest fix: the production call is correct in a browser, and guarding it
 * in `WorkflowCreateDialog` would only be guarding against jsdom.
 */
const originalScrollIntoView = Object.getOwnPropertyDescriptor(
  Element.prototype,
  "scrollIntoView",
);

if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}

let container: HTMLDivElement;
let root: Root;
let confirmed: boolean;
let confirms: string[];

/** The dialog portals into `document.body`, not into the mount container. */
function find<T extends Element>(selector: string): T {
  const el = document.body.querySelector<T>(selector);
  expect(el, `no element matching ${selector}`).toBeTruthy();
  return el as T;
}

function button(label: string): HTMLButtonElement {
  const buttons = Array.from(document.body.querySelectorAll("button"));
  const match = buttons.find((b) => b.textContent?.trim() === label);
  expect(match, `no button labeled "${label}"`).toBeTruthy();
  return match as HTMLButtonElement;
}

/** Type into a controlled `<input>`/`<textarea>` the way React reads it. */
async function type(el: HTMLInputElement | HTMLTextAreaElement, value: string) {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(
      el instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype,
      "value",
    )!.set!;
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function click(el: HTMLElement) {
  await act(async () => {
    el.click();
  });
}

async function open(client: OpenCompanyClient, onOpenChange: (o: boolean) => void) {
  await act(async () => {
    root.render(
      createElement(WorkflowCreateDialog, {
        client,
        company: "acme",
        open: true,
        onOpenChange,
        workflow: savedGraph(),
      }),
    );
  });
}

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
    true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  confirmed = true;
  confirms = [];
  vi.stubGlobal("confirm", (message: string) => {
    confirms.push(message);
    return confirmed;
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
  configFromDraft.mockClear();
});

// The shim stays up for the whole FILE, not per test: a failed save scrolls the
// error banner from a `requestAnimationFrame`, and the callback can fire after
// a test body has already returned. Removing the shim in `afterEach` strands
// that callback with no `scrollIntoView` and Vitest reports an unhandled error.
// `afterAll` keeps it present throughout the file and still restores the
// prototype so it does not leak into a later jsdom test sharing this worker
// (same discipline as `chat-scroll-anchor.test.ts`).
afterAll(() => {
  if (originalScrollIntoView) {
    Object.defineProperty(Element.prototype, "scrollIntoView", originalScrollIntoView);
  } else {
    delete (Element.prototype as unknown as Record<string, unknown>).scrollIntoView;
  }
});

describe("unsaved graph edits are not thrown away silently (#1006)", () => {
  it("names the workflow being edited, so a swapped selection is visible", async () => {
    await open(stubClient(), () => {});
    expect(document.body.textContent).toContain("Weekly report");
  });

  it("closes without asking when nothing has been edited", async () => {
    const closes: boolean[] = [];
    await open(stubClient(), (o) => closes.push(o));

    await click(button("Cancel"));

    expect(confirms).toEqual([]);
    expect(closes).toEqual([false]);
  });

  it("asks before Cancel discards an edit, and stays open when declined", async () => {
    const closes: boolean[] = [];
    await open(stubClient(), (o) => closes.push(o));
    const name = find<HTMLInputElement>('input[id$="-name"]');
    await type(name, "Weekly report v2");

    confirmed = false;
    await click(button("Cancel"));

    expect(confirms).toHaveLength(1);
    // Not closed — and the edit is still in the form, which is the whole point.
    expect(closes).toEqual([]);
    expect(find<HTMLInputElement>('input[id$="-name"]').value).toBe("Weekly report v2");

    confirmed = true;
    await click(button("Cancel"));
    expect(closes).toEqual([false]);
  });

  it("asks before Esc discards an edit", async () => {
    const closes: boolean[] = [];
    await open(stubClient(), (o) => closes.push(o));
    await type(find<HTMLTextAreaElement>('textarea[id$="-desc"]'), "changed");

    confirmed = false;
    await act(async () => {
      document.body
        .querySelector('[data-slot="dialog-content"]')!
        .dispatchEvent(
          new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
        );
    });

    expect(confirms).toHaveLength(1);
    expect(closes).toEqual([]);
  });

  it("guards a reload while dirty, and stops guarding once it is not", async () => {
    await open(stubClient(), () => {});
    const beforeUnload = () => {
      const e = new Event("beforeunload", { cancelable: true });
      window.dispatchEvent(e);
      return e.defaultPrevented;
    };
    // A pristine form has nothing to warn about.
    expect(beforeUnload()).toBe(false);

    await type(find<HTMLInputElement>('input[id$="-name"]'), "Weekly report v2");
    expect(beforeUnload()).toBe(true);

    // Typed back to what was saved: no longer dirty, so no warning.
    await type(find<HTMLInputElement>('input[id$="-name"]'), "Weekly report");
    expect(beforeUnload()).toBe(false);
  });
});

describe("a serialisation failure leaves the dialog closable (#1006)", () => {
  it("reports it, keeps the draft, sends nothing, and lets Cancel out", async () => {
    const client = stubClient();
    const closes: boolean[] = [];
    await open(client, (o) => closes.push(o));
    await type(find<HTMLInputElement>('input[id$="-name"]'), "Weekly report v2");

    configFromDraft.mockReturnValue({ ok: false, error: "Arguments must be valid JSON." });
    await click(find<HTMLButtonElement>('[data-testid="workflow-dialog-submit"]'));

    // The failure is reported against the node that caused it…
    const banner = find('[data-testid="create-error"]');
    expect(banner.textContent).toContain("Search");
    expect(banner.textContent).toContain("Arguments must be valid JSON.");
    // …nothing was written…
    expect(client.puts).toEqual([]);
    // …the draft survived…
    expect(find<HTMLInputElement>('input[id$="-name"]').value).toBe("Weekly report v2");
    // …and the way out is not disabled, which is what `submitting` stuck true
    // used to do. Before the fix this button stayed disabled forever.
    const cancel = button("Cancel");
    expect(cancel.disabled).toBe(false);

    confirmed = true;
    await click(cancel);
    expect(closes).toEqual([false]);
  });
});
