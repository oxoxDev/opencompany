// @vitest-environment jsdom

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

/**
 * A system-attributed live frame must never trigger `renderAgentReply`'s
 * "the reply is the end of that turn" live-step cleanup (Codex review, PR
 * #2052).
 *
 * B-101's mention-ambiguity note is emitted mid-turn, before the cycle that
 * answers has even run — it is never itself a completion signal. But
 * `onSendFailed` can release a held system frame (via `PendingSyncPosts`)
 * synchronously, before its own async `listRuns` lookup below it has had a
 * chance to install the still-running turn into the Room store. At that
 * instant `openTurns` legitimately knows nothing about the turn yet,
 * `hasOtherOpenTurns` reads `false`, and treating the advisory as "the turn
 * is over" would erase the live tool trace of a turn that — per the very
 * lookup racing it — is still running on the host.
 *
 * `AppShell` is too large to mount in a unit test (SSE, the authenticated
 * client, routing — see `chat-receipt-scope-reset.test.ts`'s own doc for the
 * precedent this file follows).
 */

const here = dirname(fileURLToPath(import.meta.url));
const appShell = readFileSync(resolve(here, "../../src/components/app-shell.tsx"), "utf8");

function renderAgentReplyBody(): string {
  const start = appShell.indexOf("const renderAgentReply = useCallback(");
  expect(start, "renderAgentReply's declaration").toBeGreaterThan(-1);
  const end = appShell.indexOf("// `useEvents` holds its callbacks in refs", start);
  expect(end, "renderAgentReply's trailing comment before its dependency array").toBeGreaterThan(
    start,
  );
  return appShell.slice(start, end);
}

describe("renderAgentReply never treats a system frame as turn completion", () => {
  it("computes `from` via replyVoice before the cleanup guard reads it", () => {
    const body = renderAgentReplyBody();
    expect(body).toMatch(/const from = replyVoice\(event\.agentId\);/);
  });

  it("guards the live-step cleanup on `from !== \"system\"`", () => {
    const body = renderAgentReplyBody();
    expect(body).toMatch(
      // `room.readRoom().openTurns`, not the old `openTurnsRef.current`: the
      // shell's mirror refs are gone and the Room store is the single source of
      // truth for open turns. What this pins is unchanged — which expression
      // supplies `openTurns` is incidental, the `from !== "system"` guard in
      // front of it is the point.
      /if \(from !== "system" && !hasOtherOpenTurns\(room\.readRoom\(\)\.openTurns, event\.chatId\)\) \{/,
    );
    // The bug this pins: the guard existing anywhere in the file would not
    // prove it protects the right call — confirm the specific cleanup call is
    // the one immediately gated by it. Since #2423 that call is
    // `clearLiveThread`, which retires the thread's rows and the agent they
    // named together; `live-steps-cleanup.test.ts` pins what it does.
    const guardAt = body.indexOf(
      'if (from !== "system" && !hasOtherOpenTurns(room.readRoom().openTurns, event.chatId)) {',
    );
    const cleanupAt = body.indexOf("clearLiveThread(event.chatId)", guardAt);
    expect(cleanupAt, "the live-steps clear inside the guarded block").toBeGreaterThan(guardAt);
  });
});
