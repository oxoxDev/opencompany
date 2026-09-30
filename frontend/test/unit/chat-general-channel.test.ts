// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import type { DeskDto, NotificationDto, ReadMarker } from "@/api/types";
import { ConnectionScopeProvider } from "@/connections/ConnectionContext";
import { GENERAL_CHANNEL_ID, isGeneralChannel, migrateLegacyGeneralId } from "@/lib/chat";
import { defaultDesks, isGeneralDesk, type Desk } from "@/lib/desks";
import { readLastChannel, writeLastChannel } from "@/lib/last-channel";
import { notificationHref } from "@/lib/notification-links";
import type { TeamMember } from "@/lib/team";
import { mergeReadFloors } from "@/lib/unread";
import {
  buildChannels,
  channelIdForThread,
  deskFromDto,
  dmThreadId,
} from "@/views/room/model";
import { RoomView } from "@/views/RoomView";

/**
 * `#general` is a real channel: the host lists it first in `GET .../desks` as
 * `{ id: "general-channel", kind: "general", mutable: false }`, with every non-retired
 * teammate as a member. The console renders it from that entry, pins it first,
 * offers no membership, order or delete control on it, and migrates the ids it
 * used to keep for it (`main`, `General`) to `general`.
 */

function member(over: Partial<TeamMember> & Pick<TeamMember, "id" | "name">): TeamMember {
  return {
    role: "Engineer",
    description: "",
    tone: "sky",
    avatar: "green",
    inboxEnabled: false,
    effectiveTools: [],
    desks: [],
    ...over,
  };
}

const ROSTER: TeamMember[] = [
  member({ id: "ceo", name: "Ada", role: "Chief", isOrchestrator: true }),
  member({ id: "eng", name: "Blake", role: "Engineer" }),
];

const GENERAL_DTO: DeskDto = {
  id: GENERAL_CHANNEL_ID,
  name: "General",
  kind: "general",
  members: ["ceo", "eng"],
  mutable: false,
};

const ENGINEERING_DTO: DeskDto = {
  id: "engineering",
  name: "Engineering",
  kind: "desk",
  members: ["eng"],
  mutable: true,
};

function channels(members: TeamMember[], desks: Desk[]) {
  return buildChannels(members, desks, {}).find((s) => s.id === "channels")!.channels;
}

describe("#general from the API entry", () => {
  it("renders the host's entry, pinned first even when listed later", () => {
    const rail = channels(ROSTER, [deskFromDto(ENGINEERING_DTO), deskFromDto(GENERAL_DTO)]);

    expect(rail.map((c) => c.id)).toEqual([GENERAL_CHANNEL_ID, "engineering"]);
    expect(rail[0].name).toBe("general");
    expect(rail[0].kind).toBe("channel");
    expect(rail[0].memberIds).toEqual(["ceo", "eng"]);
  });

  it("carries no mutation affordance and no lead", () => {
    const [general, engineering] = channels(ROSTER, [
      deskFromDto(GENERAL_DTO),
      deskFromDto(ENGINEERING_DTO),
    ]);

    expect(general.mutable).toBe(false);
    expect(general.leadless).toBe(true);
    expect(engineering.mutable).toBe(true);
    expect(engineering.leadless).toBeUndefined();
  });

  it("names the orchestrator as who picks up an unmentioned message", () => {
    const [general] = channels(ROSTER, [deskFromDto(GENERAL_DTO)]);
    expect(general.purpose).toBe("Everyone's here. Ada picks up anything you don't @-mention.");
    expect(general.voice).toBe("Ada");
  });

  it("makes no claim about who answers when the host does not say", () => {
    const roster = ROSTER.map((m) => ({ ...m, isOrchestrator: undefined }));
    const [general] = channels(roster, [deskFromDto(GENERAL_DTO)]);
    expect(general.purpose).toBe("Everyone's here — the whole company on one line");
    expect(general.voice).toBe("General");
  });

  it("is not fabricated for a host that does not list it", () => {
    expect(channels(ROSTER, [deskFromDto(ENGINEERING_DTO)]).map((c) => c.id)).toEqual([
      "engineering",
    ]);
    expect(channels(ROSTER, defaultDesks()).some((c) => isGeneralChannel(c.id))).toBe(false);
    expect(defaultDesks().some(isGeneralDesk)).toBe(false);
  });

  it("reads an older host's entry, with no kind or mutable, as an ordinary desk", () => {
    const desk = deskFromDto({ id: "ops", name: "Ops", members: [] });
    expect(desk.kind).toBe("desk");
    expect(desk.mutable).toBe(true);
    expect(isGeneralDesk(desk)).toBe(false);
  });

  it("follows the roster the host sends: a teammate added or removed moves with it", () => {
    const added = channels(ROSTER, [
      deskFromDto({ ...GENERAL_DTO, members: ["ceo", "eng", "new"] }),
    ])[0];
    const removed = channels(ROSTER, [deskFromDto({ ...GENERAL_DTO, members: ["ceo"] })])[0];

    expect(added.memberIds).toContain("new");
    expect(removed.memberIds).not.toContain("eng");
  });
});

describe("addressing #general by id", () => {
  const desks = [deskFromDto(GENERAL_DTO), deskFromDto(ENGINEERING_DTO)];

  it("resolves the `general-channel` thread to the `general-channel` channel", () => {
    expect(channelIdForThread(GENERAL_CHANNEL_ID, desks, ROSTER)).toBe(GENERAL_CHANNEL_ID);
  });

  it("no longer folds legacy spellings onto it", () => {
    for (const spelling of ["", "main", "general", "General", "GENERAL"]) {
      expect(isGeneralChannel(spelling)).toBe(false);
      expect(channelIdForThread(spelling, desks, ROSTER)).toBeNull();
    }
  });

  it("addresses a teammate whose id is `general` on its prefixed DM thread only", () => {
    const namesake = member({ id: "general", name: "Gen" });
    expect(dmThreadId(namesake)).toBe("dm:general");
    expect(dmThreadId(member({ id: GENERAL_CHANNEL_ID, name: "Gc" }))).toBe(
      `dm:${GENERAL_CHANNEL_ID}`,
    );
    expect(dmThreadId(ROSTER[0])).toBe("ceo");
    expect(channelIdForThread("dm:general", desks, [...ROSTER, namesake])).toBe("dm:general");
  });

  it("addresses a legacy teammate called `main` on its prefixed DM thread", () => {
    for (const id of ["main", "Main", "General"]) {
      expect(dmThreadId(member({ id, name: "Legacy" }))).toBe(`dm:${id}`);
    }
  });
});

describe("migrating stored #general ids", () => {
  it("maps `main`, `general` and #general's own id, in any casing, to it, and nothing else", () => {
    for (const legacy of ["main", "MAIN", "General", "GENERAL", "general", "General-Channel"]) {
      expect(migrateLegacyGeneralId(legacy)).toBe(GENERAL_CHANNEL_ID);
    }
    for (const other of ["engineering", "dm:main", "", "maintenance"]) {
      expect(migrateLegacyGeneralId(other)).toBe(other);
    }
  });

  it("reads a floor stored under `main` or `General` as #general's", () => {
    const markers: ReadMarker[] = [
      { channelId: "main", lastReadAt: 10 },
      { channelId: "General", lastReadAt: 30 },
      { channelId: "engineering", lastReadAt: 5 },
    ] as ReadMarker[];

    expect(mergeReadFloors({ [GENERAL_CHANNEL_ID]: 20 }, markers)).toEqual({
      [GENERAL_CHANNEL_ID]: 30,
      engineering: 5,
    });
  });

  describe("the remembered last channel", () => {
    const scope = { connection: "local", company: "acme" };

    beforeEach(() => window.localStorage.clear());

    it("rewrites a remembered `main` to #general's id once, on read", () => {
      writeLastChannel(scope, "main");
      const setItem = vi.spyOn(Storage.prototype, "setItem");

      expect(readLastChannel(scope)).toBe(GENERAL_CHANNEL_ID);
      expect(setItem).toHaveBeenCalledTimes(1);
      setItem.mockClear();
      expect(readLastChannel(scope)).toBe(GENERAL_CHANNEL_ID);
      expect(setItem).not.toHaveBeenCalled();
      setItem.mockRestore();
    });

    it("rewrites a remembered `general` to #general's id", () => {
      writeLastChannel(scope, "general");
      expect(readLastChannel(scope)).toBe(GENERAL_CHANNEL_ID);
    });

    it("leaves any other channel as it was", () => {
      writeLastChannel(scope, "engineering");
      expect(readLastChannel(scope)).toBe("engineering");
    });

    it("answers null rather than throwing when storage throws", () => {
      const getItem = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
        throw new Error("denied");
      });
      expect(readLastChannel(scope)).toBeNull();
      expect(() => writeLastChannel(scope, GENERAL_CHANNEL_ID)).not.toThrow();
      getItem.mockRestore();
    });
  });
});

describe("a notification from #general", () => {
  it("links to #general", () => {
    const n: NotificationDto = {
      id: "n1",
      kind: "mention",
      subjectKind: "message",
      subjectId: "41",
      title: "Ada mentioned you",
      createdAt: 1,
      context: GENERAL_CHANNEL_ID,
    };
    expect(notificationHref(n)).toBe(`#/chat/${GENERAL_CHANNEL_ID}?m=h41`);
  });
});

describe("RoomView offers no membership control on #general", () => {
  let container: HTMLDivElement;
  let root: Root;

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
    Object.defineProperty(window, "innerWidth", { value: 1440, writable: true });
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  function client(): OpenCompanyClient {
    return {
      listDesks: vi.fn(async () => [{ ...GENERAL_DTO, members: ["ceo"] }, ENGINEERING_DTO]),
      listTeam: vi.fn(async () => [
        { id: "ceo", name: "Ada", role: "Chief", isOrchestrator: true },
        { id: "eng", name: "Blake", role: "Engineer" },
      ]),
      mentionables: vi.fn(async () => []),
      capabilityStatus: vi.fn(async () => ({ cognition: null })),
      chat: vi.fn(),
      reactToMessage: vi.fn(),
      getBudgetPause: vi.fn(async () => null),
    } as unknown as OpenCompanyClient;
  }

  async function openMembers(sub: string) {
    const c = client();
    await act(async () => {
      root.render(
        createElement(ConnectionScopeProvider, {
          scope: { connection: "local", company: "acme" },
          children: createElement(RoomView, {
            client: c,
            company: "acme",
            sub,
            onNavigate: vi.fn(),
            transcripts: {},
            setTranscripts: vi.fn(),
            scopeRef: { current: { connection: "local", company: "acme", client: c } },
          }),
        }),
      );
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });
    const toggle = container.querySelector<HTMLButtonElement>("button[aria-pressed]");
    expect(toggle).not.toBeNull();
    await act(async () => toggle!.click());
  }

  it("draws neither an add button nor the org-chart link on #general", async () => {
    await openMembers(GENERAL_CHANNEL_ID);

    expect(container.querySelector('textarea[aria-label="Message #general"]')).not.toBeNull();
    expect(container.textContent).toContain("Everyone else");
    expect(container.querySelector('[aria-label^="Add "][aria-label$=" to this channel"]')).toBeNull();
    expect(container.textContent).not.toContain("Manage on the org chart");
  });

  it("still draws both on an ordinary desk, off the same fixture", async () => {
    await openMembers("engineering");

    expect(container.querySelector('[aria-label="Add Ada to this channel"]')).not.toBeNull();
    expect(container.textContent).toContain("Manage on the org chart");
  });
});
