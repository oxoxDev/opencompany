// @vitest-environment jsdom

import { act, createElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpenCompanyClient } from "@/api/client";
import { ConnectionScopeProvider } from "@/connections/ConnectionContext";
import { GENERAL_CHANNEL_ID } from "@/lib/chat";
import { TOUR } from "@/tour/steps";
import { RoomView } from "@/views/RoomView";

/**
 * The channel composer and the echo-brain notice that qualifies it, asked of
 * the DOM: this mounts the real `RoomView` against a stub client, since a grep
 * cannot tell a rendered control from a removed one.
 */

const DESK_DTO = {
  id: "general",
  name: "General",
  description: "The whole company",
  kind: "general" as const,
  mutable: false,
  members: [] as string[],
};

function stubClient(cognition: string | null): OpenCompanyClient {
  return {
    listDesks: vi.fn(async () => [DESK_DTO]),
    listTeam: vi.fn(async () => []),
    mentionables: vi.fn(async () => []),
    capabilityStatus: vi.fn(async () => ({ cognition })),
    chat: vi.fn(),
    reactToMessage: vi.fn(),
    getBudgetPause: vi.fn(async () => null),
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  // `useIsDesktop` reads `matchMedia`, which jsdom does not implement. A
  // desktop viewport keeps both panes mounted, which is the case under test.
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

/**
 * One run for the in-flight bar.
 *
 * Every other test here passes no `inflightRuns` at all, which is why the
 * sibling-order tests below could once claim the banner and the composer are
 * adjacent: `RoomView` gates `InflightRunBar` on the prop being defined, so a
 * harness that omits it never renders the row that actually sits between them
 * (codex and CodeRabbit, both on PR #2159).
 */
const INFLIGHT_RUN = {
  taskId: "t-1",
  key: "run-1",
  kind: "task",
  title: "Weekly pipeline review",
  agentId: "pm",
  startedAt: 0,
  pendingAction: null,
} as const;

function tree(
  client: OpenCompanyClient,
  sub: string,
  typing: string[] = [],
  inflight = false,
): ReactNode {
  const view = createElement(RoomView, {
    client,
    company: "acme",
    sub,
    onNavigate: vi.fn(),
    transcripts: {},
    setTranscripts: vi.fn(),
    // Who the shell says is at a keyboard in this channel. Empty by default;
    // the sibling-order test below supplies a name, because `TypingLine`
    // renders nothing at all when nobody is typing and the banner's placement
    // was only ever wrong when it renders something.
    resolveTypingNames: () => typing,
    // Undefined by default, because that is the shape most of these cases care
    // about — but a shell in production always passes both, so the in-flight
    // order test opts in.
    ...(inflight ? { inflightRuns: [INFLIGHT_RUN], onInflightSteered: vi.fn() } : {}),
    // The live-scope escape hatch `send` reads to decide whether a reply still
    // belongs to the company on screen. Nothing here sends.
    scopeRef: { current: { connection: "local", company: "acme", client } },
  });
  return createElement(ConnectionScopeProvider, {
    scope: { connection: "local", company: "acme" },
    children: view,
  });
}

/** Render (or re-render) this root at `sub`, then let the reads settle. */
async function renderAt(
  client: OpenCompanyClient,
  sub: string,
  typing: string[] = [],
  inflight = false,
) {
  await act(async () => {
    root.render(tree(client, sub, typing, inflight));
  });
  // Let the desks / capability reads settle.
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function mount(
  sub: string,
  cognition: string | null = null,
  typing: string[] = [],
  inflight = false,
) {
  const client = stubClient(cognition);
  await renderAt(client, sub, typing, inflight);
  return client;
}

/** The main channel composer's textarea — `MessageComposer` labels it. */
function composerInput() {
  return container.querySelector('textarea[aria-label^="Message "]');
}

function banner() {
  return container.querySelector('[data-testid="chat-cognition-banner"]');
}

describe("a channel renders the whole composer", () => {
  it("draws the input, the Send button and the controls", async () => {
    await mount("general");

    expect(composerInput()).not.toBeNull();
    expect(container.querySelector('[aria-label="Send"]')).not.toBeNull();
    // The intent chips ("Just chatting" / "Do it once" / "Build me the
    // automation") are behind `COMPOSER_INTENT_HIDDEN`, so the control that
    // opened them is absent. Asserted rather than dropped, in the idiom
    // `product-scope-hidden-surfaces.test.ts` uses: a hidden surface coming
    // back by accident is the failure, and it looks like a feature.
    expect(container.querySelector('[aria-label="What this message is for"]')).toBeNull();
    for (const label of ["Mention someone", "Formatting"]) {
      expect(container.querySelector(`[aria-label="${label}"]`)).not.toBeNull();
    }
    expect(container.textContent).toContain("to send");
    expect(container.textContent).not.toContain("There is nothing to reply to here");
  });

  it("still offers the empty-state cards", async () => {
    await mount("general");

    expect(container.textContent).toContain("Give the team a brief");
    expect(container.textContent).toContain("Add people");
  });
});

describe("the harness-unavailable notice sits next to the composer", () => {
  it("renders the notice on a writable channel, saying all three things", async () => {
    await mount("general", "unavailable");

    const strip = banner();
    expect(strip).not.toBeNull();
    expect(strip?.textContent).toContain(
      "This host cannot reach a model — no agent harness is available.",
    );
    // The sentence lost its directional word when the strip moved (see the
    // render site) and kept everything else: not the teammate they appear
    // under, from the offline echo brain, and no setting changes it.
    expect(strip?.textContent).toContain(
      "The replies in this conversation come from the offline echo brain rather than the " +
        "agent they appear under. No setting changes that: it takes a host built and " +
        "started with the harness.",
    );
  });

  it("shares the composer's own box, so nothing can come between them", async () => {
    await mount("general", "unavailable");

    const strip = banner()!;
    const input = composerInput()!;
    expect(strip).not.toBeNull();
    expect(input).not.toBeNull();

    // This used to be an order assertion over the pane's flex column: the
    // notice was a full-bleed strip in the flow, and the claim was that it sat
    // after the transcript and before the composer. It kept needing more cases
    // — the typing line, then the in-flight run bar — because every new row in
    // that column was a new thing that could land between them.
    //
    // The notice hovers now: it and the composer are in one `relative` box, and
    // it anchors to that box with `absolute bottom-full`. So the adjacency is
    // structural rather than ordered, and the run bar can render between them
    // in the DOM without coming between them on screen.
    const box = strip.parentElement!;
    expect(box.className).toContain("relative");
    expect(box.contains(input), "the notice and the composer share one box").toBe(true);
    expect(strip.className).toContain("absolute");
    expect(strip.className).toContain("bottom-full");
  });

  it("overlaps the transcript rather than displacing it", async () => {
    await mount("general", "unavailable");

    const strip = banner()!;
    // The trade the float makes, stated: it covers the last line of the
    // transcript instead of pushing it up. The transcript can be scrolled and
    // this cannot be missed, which is the right way round — but it is only
    // acceptable because the box takes no pointer events, so a click meant for
    // the message underneath still lands. The one thing here that IS clickable
    // puts them back on itself.
    expect(strip.className).toContain("pointer-events-none");
    const link = strip.querySelector("a");
    if (link) expect(strip.className).toContain("[&_a]:pointer-events-auto");
  });

  it("still hovers over the composer with a run in flight", async () => {
    // `InflightRunBar` renders inside the same box, between the notice's anchor
    // and the composer. That used to break the adjacency assertion; now it
    // cannot, and this is the case that proves it.
    await mount("general", "unavailable", ["Jane"], true);

    const strip = banner()!;
    const input = composerInput()!;
    const bar = container.querySelector('[data-testid="inflight-run-bar"]');
    expect(bar).not.toBeNull();

    const box = strip.parentElement!;
    expect(box.contains(input)).toBe(true);
    expect(box.contains(bar!)).toBe(true);
    expect(strip.className).toContain("bottom-full");
  });

});

/**
 * An address minted before `#general` had the id `general` — `#/chat/main`, or
 * `#/chat/General` in any casing — opens `#general` and is replaced, not pushed,
 * with `#/chat/general`.
 */
describe("a legacy #general address", () => {
  for (const legacy of ["main", "General", "GENERAL"]) {
    it(`opens #general from #/chat/${legacy} and rewrites the address`, async () => {
      window.history.replaceState(null, "", `#/chat/${legacy}?m=h7`);
      const before = window.history.length;

      await mount(legacy);

      expect(composerInput()?.getAttribute("aria-label")).toBe("Message #general");
      expect(container.textContent).not.toContain("isn't a channel here");
      expect(window.location.hash).toBe("#/chat/general?m=h7");
      expect(window.history.length).toBe(before);
    });
  }

  it("leaves #/chat/general alone", async () => {
    window.history.replaceState(null, "", "#/chat/general");

    await mount("general");

    expect(window.location.hash).toBe("#/chat/general");
  });
});

/**
 * The guided tour's composer stops land somewhere that has a composer. A stop
 * that names only `view: "chat"` inherits whichever channel was last open, and
 * a missing anchor skips the stop in silence.
 */
describe("the tour's composer stops address a writable channel", () => {
  const composerStops = TOUR.filter((s) => s.target === '[data-tour="chat-composer"]');

  it("finds the two stops that spotlight the composer", () => {
    expect(composerStops.length).toBe(2);
    expect(composerStops.map((s) => s.title)).toEqual(["Talk to your company", "You're all set"]);
  });

  it("names a channel outright rather than inheriting the last one", () => {
    for (const stop of composerStops) {
      expect(stop.view).toBe("chat");
      expect(stop.sub).toBeTruthy();
      // `#general` exists in every company and is writable in all of them.
      expect(stop.sub).toBe(GENERAL_CHANNEL_ID);
    }
  });

  it("mounts the spotlight anchor at that address", async () => {
    for (const stop of composerStops) {
      await renderAt(stubClient(null), stop.sub!);
      expect(container.querySelector('[data-tour="chat-composer"]')).not.toBeNull();
    }
  });
});
