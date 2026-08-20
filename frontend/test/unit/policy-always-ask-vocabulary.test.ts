// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { PolicyStatus } from "@/api/policy";
import { alwaysAskPlaceholder } from "@/components/policy-settings";

/**
 * Issue #1226: the always-ask field's worked example.
 *
 * It used to be `payment.send, filing.submit, external.publish` — the three
 * strings issue #684 deleted from the shipped default *because they gate
 * nothing*: on the harness path an `always_approve` entry is matched against
 * the tool name, and none of those three names a tool. An operator following
 * the field's own suggestion got a fence that was not there, and a
 * "list updated" toast confirming it.
 *
 * So the assertions here are about which vocabulary the control puts in front
 * of an operator: the tools this deployment actually wired, and never the
 * retired trio. The suggestions must stay *suggestions* — the effect namespace
 * is deliberately open (`src/policy/always_approve.rs`), so a `datalist` and
 * not a `select`, and nothing may reject typed text.
 */

const toasts = vi.hoisted(() => ({
  base: vi.fn(),
  success: vi.fn(),
  error: vi.fn(),
  warning: vi.fn(),
  info: vi.fn(),
}));

vi.mock("sonner", () => {
  const toast = Object.assign(toasts.base, {
    success: toasts.success,
    error: toasts.error,
    warning: toasts.warning,
    info: toasts.info,
  });
  return { toast };
});

const { PolicySettings } = await import("@/components/policy-settings");

const STATUS: PolicyStatus = {
  mode: "auto",
  alwaysApprove: [],
  manifestMode: "auto",
  manifestAlwaysApprove: [],
  overridden: false,
  takesEffect: "on the next turn",
  tiers: [
    { value: "auto", label: "Auto", description: "Works alone, stops before money." },
    { value: "full", label: "Full", description: "Acts without asking." },
  ],
} as unknown as PolicyStatus;

const WIRED = ["shell", "apply_patch", "git_operations", "web_fetch", "http_request"];

/** A client serving the policy and, optionally, the wired tool slugs. */
function makeClient({ slugs }: { slugs?: string[] | "unavailable" } = {}) {
  return {
    scopeFor: (company: string | null) => `/api/v1/${company ?? "company"}`,
    get: async (path: string) => {
      if (path.includes("/workflows/tool-slugs")) {
        if (slugs === "unavailable") throw new Error("this host predates the route");
        return { slugs: slugs ?? WIRED, unwired: [] };
      }
      if (path.endsWith("/policy")) return STATUS;
      return null;
    },
    put: async () => STATUS,
    del: async () => STATUS,
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

async function mount(client: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(PolicySettings, { client, company: "acme" }));
  });
}

const field = () => container.querySelector<HTMLInputElement>("#always-approve");
const options = () =>
  Array.from(container.querySelectorAll<HTMLOptionElement>("datalist option")).map(
    (o) => o.value,
  );

describe("what the always-ask field suggests", () => {
  it("never offers the three kinds #684 removed for gating nothing", async () => {
    await mount(makeClient());
    const text = container.textContent ?? "";
    const shown = `${field()?.placeholder ?? ""} ${options().join(" ")} ${text}`;
    for (const retired of ["payment.send", "filing.submit", "external.publish"]) {
      expect(shown, `still recommends ${retired}`).not.toContain(retired);
    }
  });

  it("grounds its example on the tools this deployment wired", async () => {
    await mount(makeClient());
    expect(options()).toEqual(WIRED);
    // The placeholder is drawn from the same set, so the one example an
    // operator reads without opening anything is also a working entry.
    const placeholder = field()?.placeholder ?? "";
    for (const suggested of placeholder.split(", ")) {
      expect(WIRED).toContain(suggested);
    }
  });

  it("picks examples worth gating, not merely the first three wired", () => {
    // The host's own order put `read_workspace_state` — a read — into the
    // worked example. A valid entry and a pointless suggestion.
    expect(alwaysAskPlaceholder(["read_workspace_state", "shell", "http_request"])).toBe(
      "shell, http_request, read_workspace_state",
    );
    // A deployment wiring nothing consequential still gets its own tools back,
    // in the host's order, rather than a name it does not have.
    expect(alwaysAskPlaceholder(["image_info", "csv_export"])).toBe(
      "image_info, csv_export",
    );
    expect(alwaysAskPlaceholder([])).toBe("shell, http_request, publish_artifact");
  });

  it("leaves the box free text — the effect namespace is open on purpose", async () => {
    await mount(makeClient());
    const input = field()!;
    // A `datalist`, never a `select`: a hosted brain may emit a kind this
    // repository has never seen, and the host deliberately does not validate.
    expect(input.tagName).toBe("INPUT");
    expect(input.getAttribute("list")).toBe("always-approve-tools");
    await act(async () => {
      input.value = "some.custom.kind";
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(field()?.value).toBe("some.custom.kind");
  });

  it("degrades to a plain box when the host cannot serve the tool set", async () => {
    await mount(makeClient({ slugs: "unavailable" }));
    expect(options()).toEqual([]);
    expect(field()?.getAttribute("list")).toBeNull();
    // Still a working example rather than a retired one.
    expect(field()?.placeholder).toBe("shell, http_request, publish_artifact");
    // And a failed suggestions read must not report the policy card as broken.
    expect(toasts.error).not.toHaveBeenCalled();
  });

  it("says what an entry is, including the prefix rule the matcher implements", async () => {
    await mount(makeClient());
    const text = container.textContent ?? "";
    expect(text).toContain("tool name");
    // The prefix rule, illustrated with a kind that is not one of the retired
    // three — explaining the rule must not double as recommending them.
    expect(text).toContain("invoice.send");
  });
});
