import { describe, expect, it, vi } from "vitest";

import type { ChatHistoryMessageDto } from "@/api/types";
import {
  GENERAL_CHANNEL_ID,
  dispatchMarkerPlacement,
  dispatchMarkerText,
  fromHistory,
  hostMessageId,
  type ChatMessage,
} from "@/lib/chat";
import { handleEvent, type CompanyStreamEvent } from "@/hooks/use-events";

/**
 * Issue #377 — what a completed dispatch shows in the channel it came from.
 *
 * A card raised from a channel can settle in `paused` or bounce back to `todo`,
 * and before this the channel showed only the agent's relay prose. A reader —
 * live, or arriving fresh after a reload — reasonably concluded the work had
 * finished. The marker is the structural fact the prose could not carry: the
 * run *stopped*, and here is where the card landed.
 *
 * Two properties are worth more than the rest and are pinned hardest here:
 *
 * 1. **Exactly one marker, live and after a reload.** The dedupe is on
 *    identity (`h<seq>`), never on content — that was issue #483's lesson, and
 *    a content check here would be the same bug wearing a new hat.
 * 2. **A marker never lands in a channel it did not come from.** A frame with
 *    no origin, or one naming a thread this company has no channel for, writes
 *    nothing at all. Falling back to "wherever the operator is looking" is
 *    issue #368's bug, and it would be *worse* here than a missing marker: it
 *    would tell someone a card settled in a conversation that never raised it.
 */

const AT = 1_700_000_000_000;

function terminal(over: Partial<CompanyStreamEvent & { type: "desk_task_completed" }> = {}) {
  return {
    type: "desk_task_completed" as const,
    seq: 4211,
    atMillis: AT,
    taskId: "t-1",
    desk: "engineer",
    column: "in_review",
    chatId: "engineering",
    ...over,
  };
}

function historyEntry(over: Partial<ChatHistoryMessageDto> = {}): ChatHistoryMessageDto {
  return {
    id: "4211",
    channel: "system",
    author: "system",
    text: "finished → In review",
    atMillis: AT,
    mine: false,
    taskId: "t-1",
    ...over,
  };
}

/** The host's addressing map: chat thread id → the channel that renders it. */
const CHANNELS = { engineering: "engineering", strategy: "strategy" };

describe("the marker's wording", () => {
  /**
   * Pinned to the **identical literals** the host's `dispatch_marker_text`
   * asserts (`src/server/chat_history.rs`). The sentence exists twice because
   * the live SSE frame is thin — it carries the raw column id, not prose — and
   * these two tests are the coupling that keeps the spellings together.
   *
   * Drift can only *reword* a marker across a reload, never double one, because
   * the dedupe below is on identity. That is what makes two copies a tolerable
   * cost rather than a returning bug.
   */
  it("names where the card landed, per column", () => {
    expect(dispatchMarkerText("in_review")).toBe("finished → In review");
    expect(dispatchMarkerText("paused")).toBe("finished → Paused");
    expect(dispatchMarkerText("todo")).toBe("finished → To-do");
    expect(dispatchMarkerText("done")).toBe("finished → Done");
    expect(dispatchMarkerText("planning")).toBe("finished → Planning");
    expect(dispatchMarkerText("in_progress")).toBe("finished → In progress");
  });

  /** A newer host's column reads a little raw rather than rendering blank. */
  it("passes an unknown column through verbatim", () => {
    expect(dispatchMarkerText("shipped_to_orbit")).toBe("finished → shipped_to_orbit");
  });
});

describe("where a settled dispatch's marker goes", () => {
  it("into the channel the card was raised in, carrying the card", () => {
    const placement = dispatchMarkerPlacement(terminal(), CHANNELS);

    expect(placement).not.toBeNull();
    expect(placement!.threadId).toBe("engineering");
    expect(placement!.channelId).toBe("engineering");
    expect(placement!.message.from).toBe("system");
    expect(placement!.message.text).toBe("finished → In review");
    expect(placement!.message.taskId).toBe("t-1");
    expect(placement!.message.at).toBe(AT);
  });

  /**
   * A board-created card names no conversation. The host declines to file it
   * into any desk's history, so a console that injected one anyway would show a
   * marker live that vanished on reload — worse than showing none.
   */
  it("nowhere, for a card no conversation raised", () => {
    expect(dispatchMarkerPlacement(terminal({ chatId: undefined }), CHANNELS)).toBeNull();
  });

  /** An unaddressed message lands in `#general`, so an empty origin is `#general`. */
  it("into #general when the origin is the empty string", () => {
    const placement = dispatchMarkerPlacement(terminal({ chatId: "" }), CHANNELS);

    expect(placement).not.toBeNull();
    expect(placement?.threadId).toBe(GENERAL_CHANNEL_ID);
  });

  it("into #general for a card raised there", () => {
    const placement = dispatchMarkerPlacement(terminal({ chatId: "general" }), {
      ...CHANNELS,
      general: "general",
    });

    expect(placement?.threadId).toBe("general");
    expect(placement?.channelId).toBe("general");
  });

  it("nowhere for a legacy `main` origin, which no longer folds onto #general", () => {
    const placement = dispatchMarkerPlacement(terminal({ chatId: "main" }), {
      ...CHANNELS,
      general: "general",
    });

    expect(placement?.channelId).toBeNull();
  });

  /**
   * **Issue #368's bug, refused.** An unmatched thread yields no channel — not
   * the first desk, not the channel the operator last had open. Filing one
   * conversation's settle into another is the failure this shape exists to make
   * impossible.
   */
  /**
   * Issue #1890 B — the thread inside the channel.
   *
   * A card raised in a thread used to settle flat in the channel, so the thread
   * that asked for the work never showed it finishing. The host names the root
   * by its own sequence, in the same namespace `seq` lives in, so the console
   * has to give it the same `h` prefix — an unprefixed value would point at a
   * line no console id matches.
   */
  it("into the thread inside that channel, under the host's own id namespace", () => {
    const placement = dispatchMarkerPlacement(terminal({ parentId: "41" }), CHANNELS);

    expect(placement).not.toBeNull();
    expect(placement!.channelId).toBe("engineering");
    expect(placement!.message.parentId).toBe(hostMessageId("41"));
    // And it matches what a reload mints for the same root, which is the whole
    // point: a marker that threaded live and flattened on reload would be the
    // live-vs-history split the identity dedupe exists to close.
    const [rehydrated] = fromHistory([historyEntry({ parentId: "41" })]);
    expect(placement!.message.parentId).toBe(rehydrated.parentId);
  });

  /**
   * A card raised straight into a channel carries no root, and its marker stays
   * flat — which is where every marker sat before B, and still the common case.
   */
  it("flat in the channel for a card raised at channel level", () => {
    const placement = dispatchMarkerPlacement(terminal(), CHANNELS);

    expect(placement!.message.parentId).toBeUndefined();
  });

  it("into no channel at all when the thread matches none — never a fallback", () => {
    const placement = dispatchMarkerPlacement(terminal({ chatId: "a-desk-that-left" }), CHANNELS);

    expect(placement).not.toBeNull();
    expect(placement!.channelId).toBeNull();
    // The thread store still keys directly on the origin, so a parked
    // Conversation on that very thread would still get the line; what must not
    // happen is a *different* channel receiving it.
    expect(placement!.threadId).toBe("a-desk-that-left");
  });

  /** An empty addressing map is the pre-hydration state, not a licence to guess. */
  it("into no channel before the addressing map has landed", () => {
    expect(dispatchMarkerPlacement(terminal(), {})!.channelId).toBeNull();
  });
});

describe("a live marker and its rehydrated twin", () => {
  /**
   * The property that keeps the count at one. The stream frame's `seq` and the
   * history entry's `id` are the same host identifier, so the live line and the
   * rehydrated one resolve to the same console id and `hydrateChannel`'s filter
   * recognises the line it already has.
   */
  it("resolve to one console id", () => {
    const live = dispatchMarkerPlacement(terminal(), CHANNELS)!.message;
    const [rehydrated] = fromHistory([historyEntry()]);

    expect(live.id).toBe(rehydrated.id);
  });

  it("leave exactly one line in the channel after a reload", () => {
    // Live: the frame arrives and the shell appends the marker.
    const channel: ChatMessage[] = [dispatchMarkerPlacement(terminal(), CHANNELS)!.message];

    // Reload: `hydrateChannel` prepends everything it does not already know,
    // by id — the filter written verbatim in shape.
    const hydrated = fromHistory([historyEntry()]);
    const known = new Set(channel.map((m) => m.id));
    const fresh = hydrated.filter((m) => !known.has(m.id));

    expect(fresh).toHaveLength(0);
    expect([...fresh, ...channel]).toHaveLength(1);
  });

  /**
   * The dedupe must not over-reach either: two cards settling into the same
   * column produce the same *sentence*, and collapsing them would silently
   * swallow a real settle. Identity is what tells them apart; content could
   * not.
   */
  it("still separate two different settles that read identically", () => {
    const first = dispatchMarkerPlacement(terminal({ seq: 1, taskId: "t-1" }), CHANNELS)!.message;
    const second = dispatchMarkerPlacement(terminal({ seq: 2, taskId: "t-2" }), CHANNELS)!.message;

    expect(first.text).toBe(second.text);
    expect(first.id).not.toBe(second.id);
  });
});

describe("a rehydrated marker", () => {
  /**
   * The host authors the marker as `system`, and `mine` is false on it — so
   * without reading the author a reload rendered it as a **company bubble**,
   * i.e. a settle that looked like something an agent had said. This is the
   * half of the feature that only shows up after a refresh.
   */
  it("comes back as a system line, not a company bubble", () => {
    const [marker] = fromHistory([historyEntry()]);

    expect(marker.from).toBe("system");
    expect(marker.text).toBe("finished → In review");
  });

  /** …still carrying its card, which is the whole reason the pill links out. */
  it("keeps the card it names", () => {
    expect(fromHistory([historyEntry()])[0].taskId).toBe("t-1");
  });

  /** An ordinary reply is untouched by the system branch. */
  it("does not change how a company reply is read", () => {
    const [reply] = fromHistory([
      historyEntry({ author: "engineering", channel: "engineering", text: "on it", taskId: undefined }),
    ]);

    expect(reply.from).toBe("company");
    expect(reply.channel).toBe("engineering");
  });

  /** …nor how your own message is, which must never grow a card chip. */
  it("does not change how your own message is read", () => {
    const [mine] = fromHistory([
      historyEntry({ author: "ada", mine: true, text: "ship it", taskId: "t-1" }),
    ]);

    expect(mine.from).toBe("you");
    expect(mine.taskId).toBeUndefined();
  });
});

describe("the terminal frame's routing", () => {
  /**
   * One frame, two genuinely different reactions — and this file has been
   * bitten before by an event reaching only one subscriber (#464, #371, #384).
   * The board still needs its refetch tick because a settle moves a card
   * between columns; the channel needs the marker.
   */
  it("reaches the board and the channel", () => {
    const onTaskEvent = vi.fn();
    const onDispatchTerminal = vi.fn();

    handleEvent(terminal(), { onTaskEvent, onDispatchTerminal });

    expect(onTaskEvent).toHaveBeenCalledTimes(1);
    expect(onDispatchTerminal).toHaveBeenCalledTimes(1);
    expect(onDispatchTerminal.mock.calls[0][0]).toMatchObject({
      taskId: "t-1",
      column: "in_review",
      chatId: "engineering",
    });
  });

  /**
   * A board-created card still ticks the board — its column moved — and simply
   * carries no origin for the channel half to act on.
   */
  it("still ticks the board for a card no conversation raised", () => {
    const onTaskEvent = vi.fn();
    const onDispatchTerminal = vi.fn();

    handleEvent(terminal({ chatId: undefined }), { onTaskEvent, onDispatchTerminal });

    expect(onTaskEvent).toHaveBeenCalledTimes(1);
    expect(dispatchMarkerPlacement(onDispatchTerminal.mock.calls[0][0], CHANNELS)).toBeNull();
  });

  /**
   * A card *changing* is not a card *settling*. Only the terminal posts a
   * marker; if this ever routed both, every column drag would leave a
   * "finished" line in a channel.
   */
  it("does not post a marker for an ordinary card write", () => {
    const onDispatchTerminal = vi.fn();

    handleEvent(
      { type: "task_card_changed", seq: 9, atMillis: AT, taskId: "t-1", change: "updated", column: "todo" },
      { onDispatchTerminal },
    );

    expect(onDispatchTerminal).not.toHaveBeenCalled();
  });
});
