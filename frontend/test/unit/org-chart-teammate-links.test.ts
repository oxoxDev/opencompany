// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { DeskDto, TeamMemberDto } from "@/api/types";
import { OrgChartView } from "@/views/company/OrgChartView";

/**
 * Teammates on the Company page open (issue #1102).
 *
 * The chart drew every teammate it knew about and wired none of them: a staffed
 * row's name was plain text, and the "Not on a desk" chips were bordered pills
 * with no `href`, no handler and no tooltip — every visual signal of a control
 * and none of the behaviour. `AgentDetailView` had existed at `#/team/<agentId>`
 * since #264, so the destination was never missing; only the link was.
 *
 * These assertions read the DOM the operator actually gets, because the failure
 * is invisible to a type check and to every derivation test in
 * `org-tree.test.ts`: `buildOrgTree` was already correct, and stayed correct,
 * the whole time the surface was inert. What has to be pinned is that the
 * element is an **anchor with a real href** — not a `div` with an `onClick`,
 * which would type-check, look identical, and still break middle-click,
 * cmd-click, keyboard focus and the browser's own hover preview.
 */

function desk(over: Partial<DeskDto> & Pick<DeskDto, "id" | "name">): DeskDto {
  return { members: [], ...over } as DeskDto;
}

function member(id: string, name: string, role = "Analyst"): TeamMemberDto {
  return { id, name, role };
}

/** A fake host: desks, roster, users and status, and nothing else. */
function client(over: {
  desks?: DeskDto[];
  team?: TeamMemberDto[];
  users?: { id: string; email: string; displayName?: string; role: string }[];
}): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/company/acme",
    listDesks: vi.fn().mockResolvedValue(over.desks ?? []),
    listTeam: vi.fn().mockResolvedValue(over.team ?? []),
    get: vi.fn().mockResolvedValue(over.users ?? []),
    status: vi.fn().mockResolvedValue({ name: "Acme" }),
  } as unknown as OpenCompanyClient;
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
});

async function render(host: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(OrgChartView, { client: host, company: "acme" }));
  });
  // The chart's four reads resolve as one `Promise.all`, but the state it sets
  // lands a tick later; give React that tick rather than assuming one flush.
  await act(async () => {});
}

/** Every anchor on the page, as `[text, href]`. */
function links(): [string, string][] {
  return [...container.querySelectorAll("a")].map((a) => [
    a.textContent ?? "",
    a.getAttribute("href") ?? "",
  ]);
}

/** The anchor whose text names `who`, or `undefined` when they are not linked. */
function linkFor(who: string): [string, string] | undefined {
  return links().find(([text]) => text.includes(who));
}

describe("a staffed seat", () => {
  it("is an anchor to that teammate's detail page", async () => {
    await render(
      client({
        desks: [desk({ id: "research", name: "Research Desk", members: ["maya", "ravi"] })],
        team: [member("maya", "Maya", "Research Lead"), member("ravi", "Ravi")],
      }),
    );

    // The href is the whole assertion: the id routed on is the desk's own
    // member id, which is the id `AgentDetailView` resolves against the host.
    expect(linkFor("Maya")?.[1]).toBe("#/team/maya");
    expect(linkFor("Ravi")?.[1]).toBe("#/team/ravi");
  });

  it("does not swallow the row's drag handle", async () => {
    await render(
      client({
        desks: [desk({ id: "research", name: "Research Desk", members: ["maya"] })],
        team: [member("maya", "Maya", "Research Lead")],
      }),
    );

    // Re-ordering a desk is a drag on the row. A link is draggable by default,
    // and the browser's drag-a-link gesture would take precedence over the
    // row's — so the anchor opts out and the drag falls through to the row.
    const anchor = container.querySelector<HTMLAnchorElement>('a[href="#/team/maya"]');
    expect(anchor?.getAttribute("draggable")).toBe("false");
    expect(container.querySelector('[data-seat-id="maya"]')?.getAttribute("draggable")).toBe(
      "true",
    );
  });

  it("leaves a seat the roster cannot resolve unlinked", async () => {
    await render(
      client({
        desks: [desk({ id: "research", name: "Research Desk", members: ["ghost"] })],
        team: [],
      }),
    );

    // `#/team/ghost` would land on "no such teammate" — a dead end that only
    // repeats what the badge beside the name already says.
    expect(container.textContent).toContain("Not on the roster");
    expect(linkFor("ghost")).toBeUndefined();
  });
});

describe("a Not-on-a-desk chip", () => {
  it("is an anchor to the same detail page a seat links to", async () => {
    await render(
      client({
        desks: [desk({ id: "research", name: "Research Desk", members: ["maya"] })],
        team: [
          member("maya", "Maya", "Research Lead"),
          member("priya", "Priya"),
          member("sam", "Sam"),
        ],
      }),
    );

    expect(container.textContent).toContain("Not on a desk");
    expect(linkFor("Priya")?.[1]).toBe("#/team/priya");
    expect(linkFor("Sam")?.[1]).toBe("#/team/sam");
  });

  it("never renders #/team/undefined for a teammate with no id", async () => {
    await render(
      client({
        desks: [],
        team: [member("", "Nameless id")],
      }),
    );

    // The guard this test exists for: an id-less teammate must be flat text
    // with an explanation, not a pill pointing at a page that cannot exist.
    expect(links().map(([, href]) => href)).not.toContain("#/team/undefined");
    expect(linkFor("Nameless id")).toBeUndefined();
    expect(container.textContent).toContain("Nameless id");
  });
});

describe("a People chip", () => {
  it("is inert, and drops the pill treatment that said otherwise", async () => {
    await render(
      client({
        desks: [],
        team: [],
        users: [{ id: "u1", email: "dana@acme.test", displayName: "Dana", role: "admin" }],
      }),
    );

    expect(container.textContent).toContain("Dana");
    // A person is a console user, not an agent: `#/team/u1` would 404, and
    // there is no person page to link to instead. So it must not be a link —
    // and must not keep the border that made the inert version read as one.
    expect(linkFor("Dana")).toBeUndefined();
    const chip = [...container.querySelectorAll("span[title]")].find((el) =>
      el.textContent?.includes("Dana"),
    );
    expect(chip?.className).not.toContain("border");
    expect(chip?.getAttribute("title")).toBeTruthy();
  });
});
