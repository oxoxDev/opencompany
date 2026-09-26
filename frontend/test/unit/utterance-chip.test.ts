// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  recipientNames,
  roundTitle,
  UTTERANCE_LABEL,
  utteranceLead,
  UtteranceChip,
} from "@/components/episode/UtteranceChip";
import type { MessageEpisodeDto } from "@/api/types";

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

function render(episode: MessageEpisodeDto, audience?: string[]) {
  act(() => {
    root.render(createElement(UtteranceChip, { episode, audience, agentNames: { engineer: "Engineer", ceo: "CEO" } }));
  });
  return host.querySelector('[data-testid="utterance-chip"]') as HTMLElement;
}

const KINDS = ["post", "broadcast", "dm", "complete_episode"] as const;
const TOOL_WORD = /\b(post|broadcast|dm|complete|complete_episode)\b/i;

describe("utterance chip wording", () => {
  it("labels every kind in plain words, never the tool's name", () => {
    for (const kind of KINDS) {
      expect(UTTERANCE_LABEL[kind]).not.toMatch(TOOL_WORD);
      expect(utteranceLead(kind, false)).not.toMatch(TOOL_WORD);
      expect(utteranceLead(kind, true)).not.toMatch(TOOL_WORD);
    }
    expect(utteranceLead("dm", true)).toBe("Sent to");
    expect(utteranceLead("dm", false)).toBe("Private note");
  });

  it("catches a tool name regardless of how it's capitalized", () => {
    expect("DM").toMatch(TOOL_WORD);
    expect("Post").toMatch(TOOL_WORD);
    expect("Complete_Episode").toMatch(TOOL_WORD);
  });

  it("titles the round without the episode id", () => {
    expect(roundTitle(0)).toBe("Round 1");
    expect(roundTitle(2)).toBe("Round 3");
  });

  it("names a recipient by display name, and an unnamed one as a teammate", () => {
    expect(recipientNames(["engineer", "ceo"], { engineer: "Engineer", ceo: "CEO" })).toEqual(["Engineer", "CEO"]);
    expect(recipientNames(["writer-7f3a"], { engineer: "Engineer" })).toEqual(["a teammate"]);
    expect(recipientNames(["writer-7f3a", "editor-9b2c"])).toEqual(["a teammate"]);
  });

  it("keeps two distinct recipients apart even when they share a configured name", () => {
    expect(
      recipientNames(["engineer-1", "engineer-2"], { "engineer-1": "Sam", "engineer-2": "Sam" }),
    ).toEqual(["Sam", "Sam"]);
  });

  it("does not drop a named recipient that repeats after an unnamed one", () => {
    expect(recipientNames(["writer-7f3a", "engineer"], { engineer: "Engineer" })).toEqual([
      "a teammate",
      "Engineer",
    ]);
  });
});

describe("UtteranceChip", () => {
  it("names each speech act in plain words, and stamps the episode and round", () => {
    for (const kind of KINDS) {
      const chip = render({ id: "ep-1", revision: 2, kind });
      expect(chip.dataset.kind).toBe(kind);
      expect(chip.dataset.episodeId).toBe("ep-1");
      expect(chip.dataset.roundRevision).toBe("2");
      expect(chip.textContent).toContain(UTTERANCE_LABEL[kind]);
      expect(chip.textContent).not.toMatch(TOOL_WORD);
    }
  });

  it("titles the chip by round, never by episode id", () => {
    const chip = render({ id: "ep-9c1e", revision: 1, kind: "post" });
    const titled = chip.querySelector("[title]") as HTMLElement;
    expect(titled.title).toBe("Round 2");
    expect(chip.innerHTML.replace(/data-episode-id="[^"]*"/, "")).not.toContain("ep-9c1e");
  });

  it("addresses a dm to its recipients by display name", () => {
    const chip = render({ id: "ep-1", revision: 1, kind: "dm", to: ["engineer"] });
    expect(chip.textContent).toContain("Sent to");
    // The lead and the recipient are separate nodes; textContent must not
    // concatenate them into "Sent toEngineer".
    expect(chip.textContent).toContain("Sent to Engineer");
    expect(chip.querySelector('[data-testid="utterance-audience"]')?.textContent).toBe("Engineer");
  });

  it("shows the dm's own recipients, not the desk's narrower audience, when the episode named them", () => {
    // `audience` is a coordination device for who a desk routed the row to,
    // not access control (api/types.ts) — the episode's own `to` is the DM's
    // actual recipients and is what the chip reports, even when this seat's
    // session narrowed delivery to a subset of them.
    const chip = render({ id: "ep-1", revision: 1, kind: "dm", to: ["engineer", "ceo"] }, ["engineer"]);
    expect(chip.querySelector('[data-testid="utterance-audience"]')?.textContent).toBe("Engineer, CEO");
  });

  it("falls back to the audience when a dm names no recipients, and never shows a raw id", () => {
    const chip = render({ id: "ep-1", revision: 1, kind: "dm" }, ["writer"]);
    expect(chip.querySelector('[data-testid="utterance-audience"]')?.textContent).toBe("a teammate");
    expect(chip.textContent).not.toContain("writer");
    expect(render({ id: "ep-1", revision: 0, kind: "post" }, ["writer"]).querySelector('[data-testid="utterance-audience"]')).toBeNull();
  });

  it("calls a dm with no known recipient a private note", () => {
    const chip = render({ id: "ep-1", revision: 0, kind: "dm" });
    expect(chip.textContent).toBe("Private note");
  });

  it("shows how a broadcast was routed onward", () => {
    const chip = render({
      id: "ep-1",
      revision: 1,
      kind: "broadcast",
      routedBy: { plan: { kind: "hive", primaryId: "ceo", invitedIds: ["engineer"] }, router: "jev" },
    });
    const plan = chip.querySelector('[data-testid="routing-plan-chip"]') as HTMLElement;
    expect(plan.dataset.planKind).toBe("hive");
    expect(plan.dataset.router).toBe("jev");
    expect(plan.textContent).toContain("→ CEO + Engineer");
  });
});
