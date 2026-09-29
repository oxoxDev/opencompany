// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { ConnectionScopeProvider } from "@/connections/ConnectionContext";
import { handleEvent, type CompanyStreamEvent } from "@/hooks/use-events";
import { ROSTER_REFETCH_DELAY_MS, RoomView } from "@/views/RoomView";

/**
 * `#general`'s members are the roster, kept by the host. When a teammate is
 * added or removed elsewhere, the host journals `teammate_added` and
 * `desk_members_changed`; the Room re-reads desks and roster on either, so the
 * member list follows without re-entering Room.
 */

describe("roster frames reach onRosterChanged", () => {
  it.each([
    { type: "teammate_added", seq: 1, atMillis: 1, agentId: "ada", role: "Engineer" },
    {
      type: "desk_members_changed",
      seq: 2,
      atMillis: 2,
      deskId: "general",
      added: [],
      removed: ["ada"],
    },
    {
      type: "desk_members_changed",
      seq: 3,
      atMillis: 3,
      deskId: "engineering",
      added: ["ada"],
      removed: [],
    },
  ] as CompanyStreamEvent[])("$type ($seq)", (event) => {
    const onRosterChanged = vi.fn();
    const onDeskRoutingConfigured = vi.fn();
    handleEvent(event, { onRosterChanged, onDeskRoutingConfigured });
    expect(onRosterChanged).toHaveBeenCalledWith(event);
    expect(onDeskRoutingConfigured).not.toHaveBeenCalled();
  });

  it("does not fire for an unrelated frame", () => {
    const onRosterChanged = vi.fn();
    handleEvent(
      { type: "desk_routing_configured", seq: 1, atMillis: 1, deskId: "x", reset: false },
      { onRosterChanged, onDeskRoutingConfigured: vi.fn() },
    );
    expect(onRosterChanged).not.toHaveBeenCalled();
  });
});

describe("RoomView re-reads desks and roster when rosterRevision moves", () => {
  let container: HTMLDivElement;
  let root: Root;
  let client: OpenCompanyClient;
  let listDesks: ReturnType<typeof vi.fn>;
  let listTeam: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
      true;
    window.matchMedia = ((query: string) => ({
      matches: query.includes("min-width"),
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    })) as unknown as typeof window.matchMedia;
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    listDesks = vi.fn(async () => [
      { id: "general", name: "General", kind: "general", mutable: false, members: ["ceo"] },
    ]);
    listTeam = vi.fn(async () => [{ id: "ceo", name: "Ada", role: "Chief" }]);
    client = {
      listDesks,
      listTeam,
      mentionables: vi.fn(async () => []),
      capabilityStatus: vi.fn(async () => ({ cognition: null })),
      chat: vi.fn(),
      reactToMessage: vi.fn(),
      getBudgetPause: vi.fn(async () => null),
    } as unknown as OpenCompanyClient;
    vi.useFakeTimers();
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.useRealTimers();
  });

  async function renderAt(rosterRevision: number) {
    await act(async () => {
      root.render(
        createElement(ConnectionScopeProvider, {
          scope: { connection: "local", company: "acme" },
          children: createElement(RoomView, {
            client,
            company: "acme",
            sub: "general",
            rosterRevision,
            onNavigate: vi.fn(),
            transcripts: {},
            setTranscripts: vi.fn(),
            scopeRef: { current: { connection: "local", company: "acme", client } },
          }),
        }),
      );
    });
  }

  async function elapse(ms: number) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ms);
    });
  }

  it("does not re-read on mount", async () => {
    await renderAt(0);
    await elapse(ROSTER_REFETCH_DELAY_MS * 2);
    expect(listDesks).toHaveBeenCalledTimes(1);
    expect(listTeam).toHaveBeenCalledTimes(1);
  });

  it("re-reads once after a revision bump", async () => {
    await renderAt(0);
    await renderAt(1);
    expect(listDesks).toHaveBeenCalledTimes(1);
    await elapse(ROSTER_REFETCH_DELAY_MS);
    expect(listDesks).toHaveBeenCalledTimes(2);
    expect(listTeam).toHaveBeenCalledTimes(2);
  });

  it("coalesces a burst of bumps into one re-read", async () => {
    await renderAt(0);
    await renderAt(1);
    await elapse(ROSTER_REFETCH_DELAY_MS / 2);
    await renderAt(2);
    await elapse(ROSTER_REFETCH_DELAY_MS / 2);
    await renderAt(3);
    await elapse(ROSTER_REFETCH_DELAY_MS);
    expect(listDesks).toHaveBeenCalledTimes(2);
    expect(listTeam).toHaveBeenCalledTimes(2);
  });

  it("shows the re-read membership in the member count", async () => {
    await renderAt(0);
    await elapse(0);
    const toggle = () => container.querySelector<HTMLButtonElement>("button[aria-pressed]");
    expect(toggle()?.textContent).toContain("1");

    listDesks.mockImplementation(async () => [
      { id: "general", name: "General", kind: "general", mutable: false, members: ["ceo", "eng"] },
    ]);
    listTeam.mockImplementation(async () => [
      { id: "ceo", name: "Ada", role: "Chief" },
      { id: "eng", name: "Blake", role: "Engineer" },
    ]);
    await renderAt(1);
    await elapse(ROSTER_REFETCH_DELAY_MS);
    expect(toggle()?.textContent).toContain("2");
  });
});
