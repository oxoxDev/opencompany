// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { SkillsView } from "@/views/SkillsView";

/**
 * Two things this screen owes an operator, neither of which a
 * `querySelector`-shaped assertion can see.
 *
 * The admin-only notice has to render *once*: every existing test reaches for
 * the first match, so a second copy of the same alert — with a body
 * contradicting the first — sat on the page unremarked. Count, don't find.
 *
 * And the authoring dialog has to send what it collects: the copy promises a
 * playbook, so a skill that reaches the agent as a title and one line is the
 * dialog quietly dropping the only field that carries the procedure.
 */

const INSTALLED: unknown[] = [];
const REGISTRY: unknown[] = [];

interface Posted {
  path: string;
  body: Record<string, unknown>;
}

function clientAs(role: "admin" | "member", posted: Posted[] = []): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/companies/acme",
    get: (path: string) => {
      if (path.endsWith("/auth/me")) {
        return Promise.resolve({
          id: "u1",
          email: "a@b.c",
          role,
          company: "acme",
          hasPassword: true,
        });
      }
      if (path.endsWith("/skills/registry")) return Promise.resolve(REGISTRY);
      if (path.endsWith("/skills")) return Promise.resolve(INSTALLED);
      return Promise.reject(new Error(`unexpected GET ${path}`));
    },
    post: (path: string, body: Record<string, unknown>) => {
      posted.push({ path, body });
      return Promise.resolve({
        id: "press-outreach",
        name: String(body.name),
        description: String(body.description),
        category: String(body.category ?? "Marketing"),
        source: "custom",
        enabled: true,
      });
    },
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

async function show(client: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(SkillsView, { client, company: "acme" }));
  });
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

/** Every match, not the first — the whole point of this file. */
function all(testid: string): HTMLElement[] {
  return [...container.querySelectorAll<HTMLElement>(`[data-testid="${testid}"]`)];
}

function click(el: Element | null | undefined) {
  return act(async () => {
    (el as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function button(label: string, within: ParentNode = document.body): HTMLButtonElement {
  const found = [...within.querySelectorAll("button")].find((b) => b.textContent?.includes(label));
  if (!found) throw new Error(`no button labelled ${label}`);
  return found;
}

/** The dialog's own submit, not the page header's identically-labelled trigger. */
function submitButton(): HTMLButtonElement {
  const dialog = document.body.querySelector('[role="dialog"]');
  if (!dialog) throw new Error("the add-skill dialog is not open");
  return button("Add skill", dialog);
}

/** Radix portals the dialog onto `document.body`, so fields live there. */
async function type(id: string, value: string) {
  const field = document.body.querySelector<HTMLInputElement | HTMLTextAreaElement>(`#${id}`);
  if (!field) throw new Error(`no field #${id}`);
  const proto =
    field instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  // React installs its own value setter on the element; go through the
  // prototype's so the change event carries the new value.
  Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(field, value);
  await act(async () => {
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function openDialog(posted: Posted[]) {
  await show(clientAs("admin", posted));
  await click(button("Add skill")); // the page header trigger
}

beforeEach(() => {
  window.location.hash = "";
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

describe("SkillsView admin-only notice", () => {
  it("renders exactly one notice for a member", async () => {
    await show(clientAs("member"));

    expect(all("skills-admin-only")).toHaveLength(1);
  });

  it("tells a member what they can actually do, which includes browsing the registry", async () => {
    await show(clientAs("member"));

    // Both skills reads are member-level on the host, so the registry is
    // genuinely open to them — the wording this replaced said otherwise, and
    // must not reappear anywhere on the page.
    const [notice] = all("skills-admin-only");
    expect(notice.textContent).toContain("browse the registry");
    expect(container.textContent).not.toContain("installed and enabled");
  });

  it("renders no notice at all for an admin", async () => {
    await show(clientAs("admin"));

    expect(all("skills-admin-only")).toHaveLength(0);
  });
});

describe("SkillsView authoring", () => {
  it("sends the playbook the operator wrote", async () => {
    const posted: Posted[] = [];
    await openDialog(posted);

    await type("skill-name", "Press Outreach");
    await type("skill-desc", "Pitch journalists a story.");
    await type("skill-body", "1. Shortlist five reporters.\n2. Send the pitch.");
    await click(submitButton());

    expect(posted).toHaveLength(1);
    expect(posted[0].body).toMatchObject({
      name: "Press Outreach",
      description: "Pitch journalists a story.",
      category: "Marketing",
      body: "1. Shortlist five reporters.\n2. Send the pitch.",
    });
  });

  it("still adds a skill when no playbook is written", async () => {
    const posted: Posted[] = [];
    await openDialog(posted);

    await type("skill-name", "Press Outreach");
    await type("skill-desc", "Pitch journalists a story.");
    await click(submitButton());

    expect(posted).toHaveLength(1);
    expect(posted[0].body.name).toBe("Press Outreach");
    // Absent rather than empty — the host treats the two identically, and an
    // empty string would read like content the operator wrote.
    expect(posted[0].body).not.toHaveProperty("body");
  });

  it("opens empty after a successful add, rather than carrying the last skill's playbook", async () => {
    const posted: Posted[] = [];
    await openDialog(posted);

    await type("skill-name", "Press Outreach");
    await type("skill-desc", "Pitch journalists a story.");
    await type("skill-body", "1. Shortlist five reporters.");
    await click(submitButton());

    // The view closes the dialog by flipping `open`, which never reaches
    // `onOpenChange` — so a reset that only runs on dismiss never runs at all,
    // and the next skill inherits a whole procedure nobody wrote for it.
    await click(button("Add skill"));
    for (const id of ["skill-name", "skill-desc", "skill-body"]) {
      const field = document.body.querySelector<HTMLInputElement>(`#${id}`);
      expect(field?.value).toBe("");
    }
  });

  it("keeps the submit button live with the playbook empty, and dead without a description", async () => {
    await openDialog([]);

    await type("skill-name", "Press Outreach");
    const submit = submitButton();
    expect(submit.disabled).toBe(true);

    await type("skill-desc", "Pitch journalists a story.");
    expect(submit.disabled).toBe(false);
  });
});
