// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { Episode, EpisodeRound } from "@/lib/episodes";
import { RoundBand } from "@/views/room/RoundBand";
import type { TimelineItem } from "@/views/room/timeline";

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

const NAMES = { engineer: "Engineer", ceo: "CEO", writer: "Writer" };

function round(over: Partial<EpisodeRound> = {}): EpisodeRound {
  return {
    episodeId: "ep-1",
    revision: 0,
    status: "open",
    seats: [
      { agentId: "engineer", status: "working" },
      { agentId: "ceo", status: "working" },
    ],
    messageIds: [],
    ...over,
  };
}

function episode(rounds: EpisodeRound[], over: Partial<Episode> = {}): Episode {
  return {
    id: "ep-1",
    chatId: "engineering",
    participants: ["engineer", "ceo"],
    status: "open",
    rounds,
    messageIds: [],
    roundCount: rounds.length,
    referrals: [],
    conversations: [],
    live: true,
    plan: { kind: "hive", primaryId: "engineer", invitedIds: ["ceo"] },
    ...over,
  };
}

function render(
  value: EpisodeRound,
  ep: Episode,
  items: TimelineItem[] = [],
  agentNames: Readonly<Record<string, string>> | undefined = NAMES,
) {
  const rendered: string[] = [];
  act(() => {
    root.render(
      createElement(RoundBand, {
        episode: ep,
        round: value,
        items,
        renderRow: (item) => {
          rendered.push(item.key);
          return createElement("p", { key: item.key, "data-testid": "row" }, item.key);
        },
        agentNames,
      }),
    );
  });
  return { band: host.querySelector('[data-testid="round-band"]') as HTMLElement, rendered };
}

const seats = (band: HTMLElement) =>
  [...band.querySelectorAll<HTMLElement>('[data-testid="round-seat"]')].map(
    (seat) => `${seat.dataset.agentId}:${seat.dataset.seatStatus}`,
  );

describe("RoundBand", () => {
  it("draws two working lanes for a round running two seats at once", () => {
    const value = round();
    const { band } = render(value, episode([value]));
    expect(band.dataset.roundStatus).toBe("open");
    expect(band.dataset.episodeId).toBe("ep-1");
    expect(band.dataset.roundRevision).toBe("0");
    expect(band.querySelector('[data-testid="round-running"]')).not.toBeNull();
    expect(seats(band)).toEqual(["engineer:working", "ceo:working"]);
    // A count of the desk's own waves, not a revision number: conversation
    // waves take revisions of their own, so the raw number is not a count.
    expect(band.textContent).toContain("1 round");
    expect(band.textContent).toContain("0/2 seats");
  });

  it("shows the plan on the first round only", () => {
    const first = round();
    const second = round({ revision: 1 });
    expect(render(first, episode([first, second])).band.querySelector('[data-testid="routing-plan-chip"]')).not.toBeNull();
    expect(render(second, episode([first, second])).band.querySelector('[data-testid="routing-plan-chip"]')).toBeNull();
  });

  it("renders its rows through the timeline's own renderer, inside the band", () => {
    const value = round({
      status: "committed",
      seats: [
        { agentId: "engineer", status: "committed", utterance: { kind: "post" } },
        { agentId: "ceo", status: "committed", utterance: { kind: "dm", to: ["engineer"] } },
      ],
      messageIds: ["h2", "h3"],
    });
    const items = [
      { kind: "message", key: "h2", at: 1, entry: {} },
      { kind: "message", key: "h3", at: 2, entry: {} },
    ] as unknown as TimelineItem[];
    const { band, rendered } = render(value, episode([value]), items);
    expect(rendered).toEqual(["h2", "h3"]);
    expect(band.querySelectorAll('[data-testid="row"]')).toHaveLength(2);
    expect(band.dataset.roundStatus).toBe("committed");
    expect(band.querySelector('[data-testid="round-running"]')).toBeNull();
    expect(seats(band)).toEqual(["engineer:committed", "ceo:committed"]);
    expect(band.textContent).toContain("2/2 seats");
    // A committed seat's lane names its speech act.
    const lanes = [...band.querySelectorAll<HTMLElement>('[data-testid="round-seat"]')];
    expect(lanes[0].textContent).toContain("Posted");
    expect(lanes[1].textContent).toContain("Private note");
  });

  it("names an unnamed seat a teammate and words an unknown act plainly, never raw", () => {
    const value = round({
      status: "committed",
      seats: [
        { agentId: "stranger-5e0d", status: "committed", utterance: { kind: "post" } },
        { agentId: "ceo", status: "committed", utterance: { kind: "handoff" as never } },
      ],
    });
    const { band } = render(value, episode([value]));
    const lanes = [...band.querySelectorAll<HTMLElement>('[data-testid="round-seat"]')];
    expect(lanes[0].textContent).toContain("a teammate");
    expect(lanes[0].textContent).not.toContain("stranger-5e0d");
    expect(lanes[0].title).toBe("a teammate: done");
    expect(lanes[1].textContent).toContain("Replied");
    expect(lanes[1].textContent).not.toContain("handoff");
  });

  it("words a seat that ended without speaking", () => {
    const value = round({
      status: "committed",
      seats: [
        { agentId: "engineer", status: "timed_out" },
        { agentId: "ceo", status: "no_utterance" },
        { agentId: "writer", status: "failed" },
      ],
    });
    const { band } = render(value, episode([value]));
    expect(seats(band)).toEqual(["engineer:timed_out", "ceo:no_utterance", "writer:failed"]);
    expect(band.textContent).toContain("timed out");
    expect(band.textContent).toContain("said nothing");
    expect(band.textContent).toContain("failed");
  });

  it("says which desk was asked when the episode referred a question", () => {
    const value = round();
    const { band } = render(
      value,
      episode([value], {
        referrals: [
          { toDesk: "content", target: "writer", asker: "engineer", direct: false, returning: false, sequence: 5, atMillis: 1 },
          { toDesk: "content", target: "writer", asker: "engineer", direct: false, returning: true, sequence: 6, atMillis: 2 },
        ],
      }),
    );
    const referrals = band.querySelectorAll('[data-testid="round-referral"]');
    expect(referrals).toHaveLength(1);
    expect(referrals[0].textContent).toBe("asked #content");
  });

  it("names a directly-asked teammate by roster, never by id", () => {
    const value = round();
    const { band } = render(
      value,
      episode([value], {
        referrals: [
          { toDesk: "content", target: "writer", asker: "engineer", direct: true, returning: false, sequence: 5, atMillis: 1 },
        ],
      }),
    );
    const referrals = band.querySelectorAll('[data-testid="round-referral"]');
    expect(referrals).toHaveLength(1);
    expect(referrals[0].textContent).toBe("asked @Writer");
  });

  it("falls back to 'a teammate' rather than the raw id when the roster has no name for it", () => {
    const value = round();
    const { band } = render(
      value,
      episode([value], {
        referrals: [
          { toDesk: "content", target: "writer", asker: "engineer", direct: true, returning: false, sequence: 5, atMillis: 1 },
        ],
      }),
      [],
      {},
    );
    const referrals = band.querySelectorAll('[data-testid="round-referral"]');
    expect(referrals[0].textContent).toBe("asked @a teammate");
  });

  /**
   * A finished episode draws no band, and loses none of its rows.
   *
   * The band is the live instrument -- who ran together, who is still
   * thinking. None of that is news once the episode is over, and the
   * completion marker already carries the round count and who closed it. So
   * the frame goes and the transcript stays: hiding the rows with it would
   * be hiding what the seats actually said.
   */
  it("draws no band once the episode has completed, but still draws its rows", () => {
    const row = {
      kind: "message" as const,
      key: "m1",
      at: 1,
      entry: { message: { id: "m1" } },
    } as unknown as TimelineItem;
    const { band, rendered } = render(
      round({ status: "committed" }),
      episode([round({ status: "committed" })], { status: "completed" }),
      [row],
    );
    expect(band).toBeNull();
    expect(rendered).toEqual(["m1"]);
  });
});
