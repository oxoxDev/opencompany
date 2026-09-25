// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { Episode } from "@/lib/episodes";
import { describeReason, EpisodeCompleteMarker } from "@/views/room/EpisodeCompleteMarker";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

function episode(over: Partial<Episode> = {}): Episode {
  return {
    id: "ep-1",
    participants: ["engineer", "ceo"],
    status: "completed",
    rounds: [],
    messageIds: [],
    roundCount: 3,
    referrals: [],
    conversations: [],
    live: false,
    completedBy: "ceo",
    ...over,
  };
}

function render(value: Episode) {
  act(() => {
    root.render(createElement(EpisodeCompleteMarker, { episode: value, agentNames: { ceo: "CEO" } }));
  });
  return host.querySelector('[data-testid="episode-complete"]') as HTMLElement;
}

describe("EpisodeCompleteMarker", () => {
  it("says how many rounds and who closed it", () => {
    const marker = render(episode());
    expect(marker.dataset.episodeId).toBe("ep-1");
    expect(marker.dataset.reason).toBe("complete_episode");
    expect(marker.textContent).toBe("Episode complete · 3 rounds · closed by CEO");
  });

  it("names a cut-off in words, and a single round in the singular", () => {
    expect(render(episode({ reason: "round_cap", roundCount: 1, completedBy: undefined })).textContent).toBe(
      "Episode complete · 1 round · round cap reached",
    );
    expect(render(episode({ reason: "timeout" })).textContent).toContain("timed out");
    expect(render(episode({ reason: "membership_changed" })).textContent).toContain("the desk changed");
    expect(render(episode({ reason: "failed" })).dataset.reason).toBe("failed");
  });

  it("passes an unknown reason through rather than hiding it", () => {
    expect(describeReason("budget_exhausted")).toBe("budget_exhausted");
    expect(describeReason(undefined)).toBeNull();
  });

  it("counts the rounds it saw when the host counted none", () => {
    const value = episode({ roundCount: 0, rounds: [{ episodeId: "ep-1", revision: 0, status: "committed", seats: [], messageIds: [] }] });
    expect(render(value).textContent).toContain("1 round ");
  });
});
