// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { TeamMember } from "@/lib/team";
import { AGENT_FIELDS } from "@/lib/agent";
import { AddMemberDialog } from "@/views/room/AddMemberDialog";
import { MembersPane } from "@/views/room/MembersPane";

/**
 * Put an existing agent onto a channel's desk from the chat member pane
 * (issue #2224). `MembersPane` no longer creates or removes a teammate
 * itself — hiring lives on the empty-desk prompt and the Team page, and
 * dropping one from the roster entirely is a Team-page-only action now —
 * this pane's only mutation is `onAddExisting`, offered solely on an
 * "Everyone else" row when there is a real desk to add into.
 */

const MEMBER: TeamMember = {
  id: "m1",
  name: "Ada",
  role: "engineer",
  description: "",
  tone: "blue",
  avatar: "ada",
  inboxEnabled: false,
  effectiveTools: [],
  desks: [],
};

function paneProps(overrides: Record<string, unknown> = {}) {
  return {
    channelMembers: [],
    others: [MEMBER],
    people: [],
    loading: false,
    fromHost: true,
    onMessage: vi.fn(),
    onAddExisting: vi.fn(),
    ...overrides,
  };
}

let container: HTMLDivElement;
let root: Root;

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

describe("MembersPane's add-existing action, on an Everyone-else row", () => {
  it("offers a + for Ada when there is a real desk to add her to", async () => {
    await act(async () => {
      root.render(createElement(MembersPane, paneProps()));
    });

    const add = container.querySelector('[aria-label="Add Ada to this channel"]') as HTMLButtonElement;
    expect(add).not.toBeNull();
  });

  it("wires the + straight to onAddExisting with the member's id", async () => {
    const onAddExisting = vi.fn();
    await act(async () => {
      root.render(createElement(MembersPane, paneProps({ onAddExisting })));
    });

    const add = container.querySelector('[aria-label="Add Ada to this channel"]') as HTMLButtonElement;
    await act(async () => add.click());

    expect(onAddExisting).toHaveBeenCalledWith("m1");
  });

  it("offers no + at all when the caller has no channel to add into (onAddExisting absent)", async () => {
    await act(async () => {
      root.render(createElement(MembersPane, paneProps({ onAddExisting: undefined })));
    });

    expect(container.querySelector('[aria-label="Add Ada to this channel"]')).toBeNull();
  });

  it("offers no + on a DM — real, non-null channelMembers that is not a desk", async () => {
    const onAddExisting = vi.fn();
    await act(async () => {
      // The caller (RoomView) gates `onAddExisting` on `activeIsDesk`, never on
      // `channelMembers` alone: a DM has real, non-null membership too (issue
      // #2224's DM regression). This pane must not re-derive that signal —
      // an absent `onAddExisting` renders no + no matter what channelMembers is.
      root.render(createElement(MembersPane, paneProps({ onAddExisting: undefined, channelMembers: [MEMBER] })));
    });

    expect(container.querySelector('[aria-label="Add Ada to this channel"]')).toBeNull();
    expect(onAddExisting).not.toHaveBeenCalled();
  });
});

describe("AddMemberDialog, when the write is refused", () => {
  function clientAs(): OpenCompanyClient {
    return {
      scopeFor: () => "/api/v1/companies/acme",
      get: (path: string) =>
        path.endsWith("/inference") ? Promise.resolve({ cognition: "echo" }) : Promise.resolve({}),
    } as unknown as OpenCompanyClient;
  }

  async function flush() {
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
  }

  it("keeps the dialog open and the typed fields intact for a retry, rather than closing on a write that never landed", async () => {
    const onAdd = vi.fn(async () => false);
    const onOpenChange = vi.fn();
    await act(async () => {
      root.render(
        createElement(AddMemberDialog, {
          open: true,
          onOpenChange,
          onAdd,
          client: clientAs(),
          company: "acme",
        }),
      );
    });
    await flush();

    const setInput = (el: HTMLInputElement, text: string) => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      act(() => {
        setter.call(el, text);
        el.dispatchEvent(new Event("input", { bubbles: true }));
      });
    };
    setInput(document.body.querySelector("#agent-add-name") as HTMLInputElement, "Nova");
    setInput(document.body.querySelector("#agent-add-role") as HTMLInputElement, "Growth Marketer");

    const create = Array.from(document.body.querySelectorAll("button")).find(
      (b) => b.textContent === "Add agent" || b.textContent === "Adding…",
    ) as HTMLButtonElement;
    await act(async () => create.click());
    await flush();

    // `inbox` is gone with the per-agent inbox; `avatar` and `landOnProfile`
    // are what the reduced dialog adds. `avatar` is undefined because nobody
    // picked a face, which is not the same as picking the hashed mascot.
    expect(onAdd).toHaveBeenCalledWith({
      name: "Nova",
      role: "Growth Marketer",
      description: "",
      instructions: "",
      avatar: undefined,
      landOnProfile: true,
    });
    // Not closed on a failed write — the caller's own toast (RoomView.addMember)
    // is the visible error; this dialog's honest half is staying open and
    // retryable rather than claiming the write landed.
    expect(onOpenChange).not.toHaveBeenCalledWith(false);
    expect((document.body.querySelector("#agent-add-name") as HTMLInputElement).value).toBe("Nova");
    const retry = Array.from(document.body.querySelectorAll("button")).find(
      (b) => b.textContent === "Add agent",
    ) as HTMLButtonElement | undefined;
    expect(retry).not.toBeUndefined();
    expect(retry?.disabled).toBe(false);
  });
});

describe("the add-agent dialog asks what they do", () => {
  const spec = AGENT_FIELDS.find((f) => f.key === "description")!;

  function clientAs(): OpenCompanyClient {
    return {
      scopeFor: () => "/api/v1/companies/acme",
      get: (path: string) =>
        path.endsWith("/inference")
          ? Promise.resolve({ cognition: "echo" })
          : Promise.resolve({}),
    } as unknown as OpenCompanyClient;
  }

  async function flush() {
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
  }

  async function openDialog(onAdd: ReturnType<typeof vi.fn>) {
    await act(async () => {
      root.render(
        createElement(AddMemberDialog, {
          open: true,
          onOpenChange: vi.fn(),
          onAdd,
          client: clientAs(),
          company: "acme",
        }),
      );
    });
    await flush();
  }

  function setValue(el: HTMLInputElement | HTMLTextAreaElement, text: string) {
    const proto =
      el instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(proto, "value")!.set!;
    act(() => {
      setter.call(el, text);
      el.dispatchEvent(new Event("input", { bubbles: true }));
    });
  }

  function field() {
    return document.body.querySelector(
      "#agent-add-description",
    ) as HTMLTextAreaElement | null;
  }

  it("offers the field, labelled the way the edit form labels it", async () => {
    await openDialog(vi.fn(() => Promise.resolve(true)));
    expect(field(), "the dialog asks for it").not.toBeNull();
    expect(field()?.placeholder).toBe(spec.placeholder);
    expect(document.body.textContent).toContain(spec.label);
  });

  it("does not repeat that label as the post's hint", async () => {
    // Two controls both headed "What they do" is what made the field look
    // missing rather than removed.
    await openDialog(vi.fn(() => Promise.resolve(true)));
    const occurrences = (
      document.body.textContent?.match(new RegExp(spec.label, "g")) ?? []
    ).length;
    expect(occurrences).toBe(1);
  });

  it("sends what was typed, trimmed", async () => {
    const onAdd = vi.fn(() => Promise.resolve(true));
    await openDialog(onAdd);

    setValue(
      document.body.querySelector("#agent-add-name") as HTMLInputElement,
      "Nova",
    );
    setValue(
      document.body.querySelector("#agent-add-role") as HTMLInputElement,
      "Growth Marketer",
    );
    setValue(field()!, "  Runs paid acquisition.  ");

    const create = Array.from(document.body.querySelectorAll("button")).find(
      (b) => b.textContent === "Add agent",
    ) as HTMLButtonElement;
    await act(async () => create.click());
    await flush();

    expect(onAdd).toHaveBeenCalledWith(
      expect.objectContaining({ description: "Runs paid acquisition." }),
    );
  });

  it("does not require it: a name and a post are still enough", async () => {
    const onAdd = vi.fn(() => Promise.resolve(true));
    await openDialog(onAdd);

    setValue(
      document.body.querySelector("#agent-add-name") as HTMLInputElement,
      "Nova",
    );
    setValue(
      document.body.querySelector("#agent-add-role") as HTMLInputElement,
      "Growth Marketer",
    );

    const create = Array.from(document.body.querySelectorAll("button")).find(
      (b) => b.textContent === "Add agent",
    ) as HTMLButtonElement;
    expect(create.disabled, "creatable without a description").toBe(false);
    await act(async () => create.click());
    await flush();
    expect(onAdd).toHaveBeenCalledWith(
      expect.objectContaining({ description: "" }),
    );
  });
});
