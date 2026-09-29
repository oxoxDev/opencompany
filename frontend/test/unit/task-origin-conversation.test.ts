import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { originConversation } from "@/lib/task-origin";

/**
 * "Open the conversation" on a card used to land on `#/conversation` — the
 * legacy two-pane surface with its own thread rail — and select the thread
 * with a state setter the address knows nothing about. Chats are the Room tab
 * now (`#/chat/<channelId>`), so the card's host thread id is not an address
 * on its own: it has to be resolved through the shell's thread → channel map,
 * and that resolution can come back empty.
 *
 * The pure half of that decision lives in `lib/task-origin.ts` so the three
 * outcomes can be named here rather than inferred from a rendered button, and
 * so the "no channel carries it" case is a branch with a test rather than a
 * button that navigates nowhere.
 */
describe("where a card's origin conversation lives", () => {
  it("is nowhere for a card that was never opened from one", () => {
    expect(originConversation(undefined, { t1: "desk-eng" })).toEqual({ kind: "none" });
    expect(originConversation("", { t1: "desk-eng" })).toEqual({ kind: "none" });
  });

  it("is the channel that carries the origin thread", () => {
    expect(originConversation("t1", { t1: "desk-eng" })).toEqual({
      kind: "channel",
      channelId: "desk-eng",
    });
  });

  it("is unreachable when no channel carries the thread", () => {
    // A desk retired since the card was opened. The row must state the origin
    // and offer no jump — the pre-fix handler offered one unconditionally and
    // navigated to a surface that had nothing to show.
    expect(originConversation("gone", { t1: "desk-eng" })).toEqual({ kind: "unreachable" });
  });

  it("is unreachable before the channel map has loaded", () => {
    // The shell starts with an empty map and fills it once `/desks` answers.
    expect(originConversation("t1", {})).toEqual({ kind: "unreachable" });
    expect(originConversation("t1", undefined)).toEqual({ kind: "unreachable" });
  });

  it("finds #general for a card raised there", () => {
    expect(originConversation("general", { general: "general" })).toEqual({
      kind: "channel",
      channelId: "general",
    });
  });

  it("no longer folds a legacy General spelling onto #general", () => {
    expect(originConversation("MAIN", { general: "general" })).toEqual({ kind: "unreachable" });
  });

  it("carries the origin thread's own id, when the card was raised inside one", () => {
    // `h`-prefixed: `Room`'s transcript keys every host-journaled message as
    // `h<seq>` (`hostMessageId`) to tell it apart from a locally-minted
    // optimistic id, so a bare seq here would never match a loaded message.
    expect(originConversation("t1", { t1: "desk-eng" }, 41)).toEqual({
      kind: "channel",
      channelId: "desk-eng",
      threadId: "h41",
    });
  });

  it("carries no thread id for a card raised straight into the channel", () => {
    expect(originConversation("t1", { t1: "desk-eng" })).toEqual({
      kind: "channel",
      channelId: "desk-eng",
      threadId: undefined,
    });
  });
});

/**
 * The resolved thread id has to actually ride the jump, not just come back
 * from `originConversation` — pinned by source-text, the idiom
 * `chat-general-channel.test.ts` already uses for wiring a full `AppShell` /
 * `RoomView` render is too heavy to stand up.
 */
describe("the origin thread rides the card → Room jump, not just the channel", () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const shell = readFileSync(resolve(here, "../../src/components/app-shell.tsx"), "utf8");
  const chatView = readFileSync(resolve(here, "../../src/views/RoomView.tsx"), "utf8");

  it("the shell carries the resolved thread id into the chat navigation's query", () => {
    expect(shell).toContain('navigate("chat", channelId, { thread: threadId ?? null })');
  });

  it("Room reads the thread the query names and opens it", () => {
    // Read reactively since #2130: `useHashView` parses only the path segments,
    // so a link that changes nothing but `?thread=` re-renders nothing an effect
    // keyed on the channel would see. The read moved into its own `hashchange`
    // subscription — the one `useHashFlag` uses — and the effect opens whatever
    // it names.
    expect(chatView).toContain('new URLSearchParams(query).get("thread")');
    expect(chatView).toContain("setOpenThreadId(threadQuery.value)");
  });
});
