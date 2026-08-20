// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { ApiError } from "@/api/types";
import type { WorkflowGraph } from "@/api/workflows";
import { WorkflowCreateDialog } from "@/views/WorkflowCreateDialog";

/**
 * How the create/edit dialog cascades a node rename and how it surfaces the
 * host's structured `workflow_invalid` refusal (issue #1016).
 *
 * Both properties live only in a mounted component holding an in-flight write:
 * a rename that must reach every edge before validation runs, and a per-node
 * `problems` array that must land on the right config field rather than in one
 * flat banner. `validate()` and the client parse are unit-tested elsewhere;
 * this is the wiring between them and the rendered form, so it earns a jsdom
 * render the same way `workflow-create-feedback` does.
 */

/** Stubs the two write verbs the dialog uses (create posts, edit puts) plus the
 * optional-picker GETs, each of which degrades to a free-text fallback. */
function stubClient(write: {
  post?: (path: string, body?: unknown) => Promise<unknown>;
  put?: (path: string, body?: unknown) => Promise<unknown>;
}) {
  return {
    scopeFor: () => "/api/v1/companies/acme",
    get: () => Promise.reject(new Error("not offered by this host")),
    listTeam: () => Promise.reject(new Error("not offered by this host")),
    post: write.post ?? (() => Promise.reject(new Error("no post expected"))),
    put: write.put ?? (() => Promise.reject(new Error("no put expected"))),
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;
let onOpenChange: ReturnType<typeof vi.fn>;

function inDialog<T extends Element>(selector: string): T | null {
  return document.querySelector<T>(`[data-slot="dialog-content"] ${selector}`);
}

function submitButton(): HTMLButtonElement {
  return inDialog<HTMLButtonElement>('[data-testid="workflow-dialog-submit"]')!;
}

/** Sets a controlled input the way a keystroke would. */
function type(selector: string, value: string, nth = 0) {
  const input = Array.from(
    document.querySelectorAll<HTMLInputElement>(`[data-slot="dialog-content"] ${selector}`),
  )[nth];
  expect(input, `no input matching ${selector} at index ${nth}`).toBeTruthy();
  const setter = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )!.set!;
  setter.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

async function openEditing(client: OpenCompanyClient, workflow: WorkflowGraph) {
  await act(async () => {
    root.render(
      createElement(WorkflowCreateDialog, {
        open: true,
        onOpenChange,
        client,
        company: "acme",
        workflow,
      }),
    );
  });
}

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
    true;
  Element.prototype.scrollIntoView = vi.fn();
  onOpenChange = vi.fn();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("renaming a node cascades to the edges that reference it", () => {
  it("rewrites an edge's endpoint so the save is not blocked by a dangling reference", async () => {
    // A saved graph whose only edge points at `greet` by id.
    const workflow: WorkflowGraph = {
      id: "greeter",
      name: "Greeter",
      version: "v1",
      nodes: [
        { id: "start", kind: "trigger", name: "Start" },
        { id: "greet", kind: "agent", name: "Greet", agent: "alice" },
      ],
      edges: [{ from: "start", to: "greet" }],
    };
    const put = vi.fn((_path: string, _body?: unknown) => Promise.resolve(workflow));
    await openEditing(stubClient({ put }), workflow);

    // Rename the second node (`greet` → `hello`) through its id control.
    await act(async () => {
      type('[aria-label="Node id"]', "hello", 1);
    });
    await act(async () => {
      submitButton().click();
    });

    // Pre-fix: the edge still says `to: "greet"`, `validate()` refuses the
    // dangling reference, and the write never fires. Post-fix: the edge tracked
    // the rename, validation passes, and the posted graph carries `start→hello`.
    expect(put).toHaveBeenCalledTimes(1);
    const graph = put.mock.calls[0][1] as WorkflowGraph;
    expect(graph.edges).toHaveLength(1);
    expect(graph.edges[0].from).toBe("start");
    expect(graph.edges[0].to).toBe("hello");
  });

  it("keeps cascading after the id was cleared before the replacement was typed", async () => {
    // Same fixture as above, but the rename goes through an intermediate
    // empty id — clearing the field first (as a user backspacing it out
    // does) before typing the replacement.
    const workflow: WorkflowGraph = {
      id: "greeter",
      name: "Greeter",
      version: "v1",
      nodes: [
        { id: "start", kind: "trigger", name: "Start" },
        { id: "greet", kind: "agent", name: "Greet", agent: "alice" },
      ],
      edges: [{ from: "start", to: "greet" }],
    };
    const put = vi.fn((_path: string, _body?: unknown) => Promise.resolve(workflow));
    await openEditing(stubClient({ put }), workflow);

    await act(async () => {
      type('[aria-label="Node id"]', "", 1);
    });
    await act(async () => {
      type('[aria-label="Node id"]', "hello", 1);
    });
    await act(async () => {
      submitButton().click();
    });

    // Pre-fix: the first edit cascades the edge to `""` (its rewrite still
    // fires, since the id going *into* that edit was `"greet"`, not `""`).
    // The second edit's rewrite is the one that used to be skipped — its
    // `prevId` reads back as `""`, which the old `prevId &&` guard treated
    // as "no previous id" and left the edge pointing at `""` forever.
    expect(put).toHaveBeenCalledTimes(1);
    const graph = put.mock.calls[0][1] as WorkflowGraph;
    expect(graph.edges).toHaveLength(1);
    expect(graph.edges[0].from).toBe("start");
    expect(graph.edges[0].to).toBe("hello");
  });
});

describe("the dialog consuming the host's structured workflow_invalid 400", () => {
  /** A saved graph whose `greet` node is an `http_request` (its config form has
   * a `url` field) with a valid url, so the client-side `validate()` passes and
   * the refusal below is the host's. */
  function httpWorkflow(): WorkflowGraph {
    return {
      id: "fetcher",
      name: "Fetcher",
      version: "v1",
      nodes: [
        { id: "start", kind: "trigger", name: "Start" },
        {
          id: "greet",
          kind: "http_request",
          name: "Fetch",
          config: { method: "GET", url: "https://example.com" },
        },
      ],
      edges: [],
    };
  }

  function apiError(problems: unknown): ApiError {
    const err = new ApiError(
      400,
      "workflow_invalid",
      "the workflow could not be validated",
      true,
    );
    (err as unknown as { problems: unknown }).problems = problems;
    return err;
  }

  it("lands a config-field problem on that node's field, not in the flat banner", async () => {
    const put = vi.fn(() =>
      Promise.reject(
        apiError([{ node_id: "greet", field: "config.url", message: "bad url" }]),
      ),
    );
    await openEditing(stubClient({ put }), httpWorkflow());

    await act(async () => {
      submitButton().click();
    });

    // The per-field surface for the url control shows the host's message...
    const urlField = inDialog<HTMLElement>('[data-testid="config-field-url"]');
    expect(urlField, "no url config control").toBeTruthy();
    expect(urlField!.getAttribute("aria-invalid")).toBe("true");
    const describedBy = urlField!.getAttribute("aria-describedby") ?? "";
    const errorNode = describedBy
      .split(/\s+/)
      .map((id) => document.getElementById(id))
      .find((el) => el?.textContent?.includes("bad url"));
    expect(errorNode, "the url field did not show the host message").toBeTruthy();

    // ...and the raw message is NOT dumped into the flat submit banner. A short
    // non-raw summary may sit there so Create does not read as dead, but the
    // per-field text must not be duplicated into it.
    const banner = inDialog<HTMLElement>('[data-testid="create-error"]');
    expect(banner?.textContent ?? "").not.toContain("bad url");
  });

  it("falls back to the flat banner when the problem names no current node", async () => {
    const put = vi.fn(() =>
      Promise.reject(
        apiError([{ node_id: "ghost", field: "config.url", message: "no such node" }]),
      ),
    );
    await openEditing(stubClient({ put }), httpWorkflow());

    await act(async () => {
      submitButton().click();
    });

    const banner = inDialog<HTMLElement>('[data-testid="create-error"]');
    expect(banner, "the flat banner did not render").toBeTruthy();
    expect(banner!.textContent).toContain("no such node");
  });

  it("matches a problem's node_id against the trimmed draft id, not the raw one", async () => {
    // The submit path trims every node id before sending it (`n.id.trim()`),
    // so the host's `problems` refer to the trimmed id even when the draft
    // row still holds surrounding whitespace the operator typed.
    const put = vi.fn(() =>
      Promise.reject(
        apiError([{ node_id: "greet", field: "config.url", message: "bad url" }]),
      ),
    );
    await openEditing(stubClient({ put }), httpWorkflow());

    await act(async () => {
      type('[aria-label="Node id"]', " greet ", 1);
    });
    await act(async () => {
      submitButton().click();
    });

    // Pre-fix: the raw draft id `" greet "` never equals the host's trimmed
    // `"greet"`, the lookup misses, and the message falls through to the
    // flat banner instead of landing on the url field.
    const urlField = inDialog<HTMLElement>('[data-testid="config-field-url"]');
    expect(urlField, "no url config control").toBeTruthy();
    expect(urlField!.getAttribute("aria-invalid")).toBe("true");
    const describedBy = urlField!.getAttribute("aria-describedby") ?? "";
    const errorNode = describedBy
      .split(/\s+/)
      .map((id) => document.getElementById(id))
      .find((el) => el?.textContent?.includes("bad url"));
    expect(errorNode, "the url field did not show the host message").toBeTruthy();

    const banner = inDialog<HTMLElement>('[data-testid="create-error"]');
    expect(banner?.textContent ?? "").not.toContain("bad url");
  });
});
