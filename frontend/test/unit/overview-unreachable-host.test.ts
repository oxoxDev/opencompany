// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { DeskDto, TeamMemberDto } from "@/api/types";

/**
 * Issue #1219: an unreachable host used to draw an empty company.
 *
 * `Overview` reads six independent sources in one `Promise.all`, and each was
 * individually `.catch()`-ed to an empty fallback. When the host cannot be
 * reached at all, every one of the six "fails" into the same empty value the
 * component also uses for a company that genuinely has nothing in it — so a
 * total outage and an empty company were indistinguishable, and the snapshot
 * clock re-stamped itself over a fetch that never actually landed.
 *
 * `KnowledgeGraph` — the lazy-loaded graph itself — is mocked out below. It
 * drives a force simulation off `requestAnimationFrame` and reads
 * `window.matchMedia`, neither of which jsdom provides, and none of that
 * machinery is under test here: what is under test is the snapshot corner
 * `Overview` renders around it.
 */

vi.mock("@/views/overview/kg/KnowledgeGraph", () => ({
  KnowledgeGraph: () => null,
}));

const { Overview } = await import("@/views/Overview");

function desk(over: Partial<DeskDto> & Pick<DeskDto, "id" | "name">): DeskDto {
  return { members: [], ...over } as DeskDto;
}

function member(id: string, role = "Analyst"): TeamMemberDto {
  return { id, role };
}

/** Every `client.get` path this component reads, keyed by its suffix. */
const HEALTHY_GET: Record<string, unknown[]> = {
  "/tasks": [],
  "/users": [],
  "/memory": [],
  "/workflows": [],
};

/**
 * A fake host, built from three raw mocks so a test can reconfigure them
 * later (a company switching from healthy to unreachable, say) without losing
 * the mock identity a plain object literal cast to `OpenCompanyClient` would.
 *
 * `get` is one mock dispatched by path suffix, since `listTasks`,
 * `listPeople`, `listMemory` and `listWorkflows` all route through it rather
 * than through their own client method.
 */
function fakeClient(over?: { desks?: DeskDto[]; team?: TeamMemberDto[] }) {
  const get = vi.fn((path: string) => {
    const suffix = Object.keys(HEALTHY_GET).find((k) => path.endsWith(k));
    return Promise.resolve(suffix ? HEALTHY_GET[suffix] : []);
  });
  const listDesks = vi.fn().mockResolvedValue(over?.desks ?? []);
  const listTeam = vi.fn().mockResolvedValue(over?.team ?? []);
  const client = {
    scopeFor: () => "/api/v1/company/acme",
    get,
    listDesks,
    listTeam,
  } as unknown as OpenCompanyClient;
  return { client, get, listDesks, listTeam };
}

/** Makes every one of the six reads fail at the transport, in place. */
function goUnreachable(mocks: ReturnType<typeof fakeClient>) {
  const fail = () => Promise.reject(new Error("ERR_CONNECTION_REFUSED"));
  mocks.get.mockImplementation(fail);
  mocks.listDesks.mockImplementation(fail);
  mocks.listTeam.mockImplementation(fail);
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
    root.render(createElement(Overview, { client: host, company: "acme" }));
  });
  // The six sources resolve as one `Promise.all`(-Settled), but the state it
  // sets lands a tick later; give React that tick rather than assuming one
  // flush covers it.
  await act(async () => {});
  await act(async () => {});
}

function snapshotText(): string {
  return (
    [...container.querySelectorAll(".text-2xs")]
      .map((el) => el.textContent ?? "")
      .find((t) => t.includes("Snapshot") || t.includes("No snapshot") || t.includes("Loading")) ?? ""
  );
}

function alertText(): string | undefined {
  return container.querySelector('[role="alert"]')?.textContent ?? undefined;
}

function clickRefresh() {
  const button = [...container.querySelectorAll("button")].find((b) =>
    b.textContent?.includes("Refresh"),
  );
  button?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
}

describe("a host that cannot be reached at all", () => {
  it("says so, and does not draw an empty company", async () => {
    const unreachable = fakeClient();
    goUnreachable(unreachable);
    await render(unreachable.client);

    expect(alertText()).toContain("Could not reach the company");
    // Never a company with nothing in it: the corner says there was no
    // snapshot to draw, not that one was taken and came back empty.
    expect(snapshotText()).not.toContain("Snapshot");
    expect(snapshotText()).toContain("No snapshot yet");
  });

  it("keeps the previous snapshot's time rather than re-stamping it", async () => {
    const mocks = fakeClient({
      desks: [desk({ id: "research", name: "Research Desk", members: ["maya"] })],
      team: [member("maya")],
    });
    await render(mocks.client);
    expect(alertText()).toBeUndefined();
    const firstSnapshot = snapshotText();
    expect(firstSnapshot).toContain("Snapshot");

    // The same outage the issue reproduces: every one of the six reads now
    // fails at the transport. Reconfigure the mocks in place and press the
    // console's own Refresh control, rather than reaching into internals.
    goUnreachable(mocks);
    await act(async () => {
      clickRefresh();
    });
    await act(async () => {});
    await act(async () => {});

    expect(alertText()).toContain("Could not reach the company");
    expect(alertText()).toContain("Showing the last snapshot");
    // The whole point: the time on screen is the one the healthy load
    // produced, not a new one stamped over a graph that never actually
    // re-read anything.
    expect(snapshotText()).toBe(firstSnapshot);
  });
});

describe("a host that answers some sources and not others", () => {
  it("keeps drawing what it has, and raises no outage notice", async () => {
    const mocks = fakeClient({
      desks: [desk({ id: "research", name: "Research Desk", members: ["maya"] })],
      team: [member("maya")],
    });
    // A single failed source (desks) must not read as a total outage — the
    // other five still answered.
    mocks.listDesks.mockRejectedValue(new Error("desks unavailable"));

    await render(mocks.client);

    expect(alertText()).toBeUndefined();
    expect(snapshotText()).toContain("Snapshot");
  });
});
