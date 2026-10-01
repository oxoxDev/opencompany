// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { SkillPlaybook } from "@/views/skills/SkillPlaybook";

/**
 * The panel that serves a skill's `SKILL.md` and, where the host allows it,
 * rewrites it.
 *
 * `editable` is the host's word, never a guess from the row's source. These
 * tests pin that: a panel that decided for itself would offer Edit on a skill
 * the host then refuses, and the refusal would read as a bug.
 */

const DOC = "---\nname: Brand Voice\ndescription: How we sound.\n---\nStep one.\n";
const REWRITTEN = "---\nname: Brand Voice\ndescription: How we sound.\n---\nStep two.\n";

function clientWith(
  doc: { markdown: string; editable: boolean },
  put?: (body: unknown) => Promise<unknown>,
): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/companies/acme",
    get: (path: string) =>
      path.endsWith("/doc")
        ? Promise.resolve(doc)
        : Promise.reject(new Error(`unexpected GET ${path}`)),
    put: (_path: string, body: unknown) =>
      put ? put(body) : Promise.resolve({ id: "brand-voice" }),
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

async function show(client: OpenCompanyClient, canManage = true) {
  const onSaved = vi.fn();
  await act(async () => {
    root.render(
      createElement(SkillPlaybook, {
        client,
        company: "acme",
        slug: "brand-voice",
        canManage,
        onSaved,
      }),
    );
  });
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
  return onSaved;
}

function at(testid: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(`[data-testid="${testid}"]`);
}

async function click(testid: string) {
  await act(async () => {
    at(testid)?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function type(text: string) {
  const area = at("skill-doc-editor") as HTMLTextAreaElement;
  const setter = Object.getOwnPropertyDescriptor(
    window.HTMLTextAreaElement.prototype,
    "value",
  )?.set;
  await act(async () => {
    setter?.call(area, text);
    area.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

describe("the playbook panel at rest", () => {
  it("shows the whole document, frontmatter and all", async () => {
    await show(clientWith({ markdown: DOC, editable: true }));
    expect(at("skill-doc-text")?.textContent).toBe(DOC);
  });

  it("offers Edit when the host calls the skill editable", async () => {
    await show(clientWith({ markdown: DOC, editable: true }));
    expect(at("skill-doc-edit")).not.toBeNull();
    expect(at("skill-doc-read-only")).toBeNull();
  });

  it("names the reason instead of Edit when the host does not", async () => {
    await show(clientWith({ markdown: DOC, editable: false }));
    expect(at("skill-doc-edit"), "no editor to open").toBeNull();
    expect(at("skill-doc-read-only")?.textContent).toContain(
      "authored in the repository",
    );
    // Still readable: a member has to be able to see what a teammate is told.
    expect(at("skill-doc-text")?.textContent).toBe(DOC);
  });

  it("shows an editable document to a member without offering Edit", async () => {
    await show(clientWith({ markdown: DOC, editable: true }), false);
    expect(at("skill-doc-text")?.textContent).toBe(DOC);
    expect(at("skill-doc-edit")).toBeNull();
  });

  it("says why there is nothing to show when the read fails", async () => {
    const client = {
      scopeFor: () => "/api/v1/companies/acme",
      get: () => Promise.reject(new Error("not found: no document")),
    } as unknown as OpenCompanyClient;
    await show(client);
    expect(at("skill-doc-failed")?.textContent).toContain("no document");
    expect(at("skill-doc-text")).toBeNull();
  });
});

describe("rewriting the document", () => {
  it("sends the whole text and leaves the panel on what was stored", async () => {
    const put = vi.fn(() => Promise.resolve({ id: "brand-voice" }));
    const onSaved = await show(
      clientWith({ markdown: DOC, editable: true }, put),
    );

    await click("skill-doc-edit");
    await type(REWRITTEN);
    await click("skill-doc-save");

    expect(put).toHaveBeenCalledWith({ markdown: REWRITTEN, force: false });
    expect(at("skill-doc-editor"), "the editor closes").toBeNull();
    expect(at("skill-doc-text")?.textContent).toBe(REWRITTEN);
    // The row's name and description are read out of this document, so the
    // list has to be refetched or it keeps showing the old ones.
    expect(onSaved).toHaveBeenCalled();
  });

  it("will not send an unchanged document", async () => {
    await show(clientWith({ markdown: DOC, editable: true }));
    await click("skill-doc-edit");
    expect((at("skill-doc-save") as HTMLButtonElement).disabled).toBe(true);
  });

  it("keeps the draft and names the refusal when the host declines", async () => {
    const put = vi.fn(() =>
      Promise.reject(new Error("conflict: that skill is authored elsewhere")),
    );
    const onSaved = await show(
      clientWith({ markdown: DOC, editable: true }, put),
    );

    await click("skill-doc-edit");
    await type(REWRITTEN);
    await click("skill-doc-save");

    expect(at("skill-doc-problem")?.textContent).toContain(
      "authored elsewhere",
    );
    // The rewrite exists nowhere else yet, so closing the editor would lose it.
    expect((at("skill-doc-editor") as HTMLTextAreaElement).value).toBe(
      REWRITTEN,
    );
    expect(onSaved).not.toHaveBeenCalled();
  });

  it("drops the draft on Cancel, back to what is stored", async () => {
    await show(clientWith({ markdown: DOC, editable: true }));
    await click("skill-doc-edit");
    await type(REWRITTEN);
    await click("skill-doc-cancel");

    expect(at("skill-doc-editor")).toBeNull();
    expect(at("skill-doc-text")?.textContent).toBe(DOC);
  });
});
