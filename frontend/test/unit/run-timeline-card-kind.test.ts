import { describe, expect, it } from "vitest";

import type { TimelineEntry } from "@/api/tasks";
import { groupTimeline, rowIcon } from "@/views/runs/RunTimeline";

const T0 = 1_700_000_000_000;

function cardEntry(seq: number, label: string): TimelineEntry {
  return {
    seq,
    atMillis: T0 + seq,
    kind: "card",
    label,
  };
}

describe("groupTimeline with the card kind", () => {
  it("keeps consecutive card entries as their own rows, unlike a failure run", () => {
    const items = groupTimeline([
      cardEntry(1, "Card opened → todo"),
      cardEntry(2, "Card opened → todo"),
      cardEntry(3, "Card opened → todo"),
    ]);
    const groups = items.filter((item) => item.row === "group");
    expect(groups).toHaveLength(3);
    expect(groups.every((g) => g.group.kind === "card")).toBe(true);
  });

  it("keeps card entries with different labels as separate rows", () => {
    const items = groupTimeline([
      cardEntry(1, "Card opened → todo"),
      cardEntry(2, "Card updated → in_progress"),
    ]);
    const groups = items.filter((item) => item.row === "group");
    expect(groups).toHaveLength(2);
  });
});

describe("rowIcon for the card kind", () => {
  it("renders a defined icon rather than undefined", () => {
    const icon = rowIcon("card");
    expect(icon).toBeDefined();
    expect(icon.type).toBeDefined();
  });
});
