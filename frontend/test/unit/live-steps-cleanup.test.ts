import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const appShell = readFileSync("src/components/app-shell.tsx", "utf8");
const threadPanel = readFileSync("src/views/room/ThreadPanel.tsx", "utf8");
const chatView = readFileSync("src/views/RoomView.tsx", "utf8");

/**
 * The four gaps the Codex review on #2069 found in per-query live rows, each
 * pinned so it cannot come back.
 *
 * Filing rows per query fixed the case it was written for — two channel-rooted
 * queries on the built-in harness — and left four ways for a bucket to be
 * written that nothing renders, or rendered that nothing clears. All four are
 * invisible in the happy path, which is exactly why they need pinning.
 */

describe("a threaded query's rows have somewhere to render", () => {
  /**
   * `buildTimeline` keeps every parented line out of the channel timeline
   * (`if (!m.parentId) continue`), so a query typed into an open thread renders
   * through `ThreadPanel` or nowhere at all. Passing the per-query map only to
   * `MessageTimeline` left such a turn with no render path — and, because its
   * frames now carry `messageSeq`, no per-thread fallback either.
   */
  it("ThreadPanel takes the per-query map", () => {
    expect(threadPanel).toContain("liveStepsByMessage?: Record<string, TurnStep[]>;");
  });

  /**
   * …and resolves it to ONE row at the foot rather than one per line.
   *
   * Position in a transcript is chronology. A "happening now" row placed back
   * at the asking message claims the work finished before every reply beneath
   * it — false the moment anything is journaled in between, which in a thread
   * is every follow-up. The panel also said the two things in two tenses at
   * once: rows against the body lines, and a foot row that could only manage
   * "Replying…" because it was handed no steps.
   */
  it("resolves the per-query map to one row at the foot, not one per line", () => {
    expect(threadPanel).toContain("const openTurnSteps");
    expect(threadPanel).toContain("steps={openTurnSteps}");
    // No line in the body may carry live rows again.
    expect(threadPanel).not.toContain("liveSteps={liveStepsByMessage");
    expect(threadPanel).not.toContain("liveSteps?: readonly TurnStep[];");
  });

  it("names live activity, and shows the live rows behind it", () => {
    expect(threadPanel).toContain("<WorkingIndicator");
    // Narrowed on the same terms as `raw-turns-toggle`'s own ban, and for the
    // same reason: what that rule protects is one renderer for a **stored**
    // message's steps, which chat must not restate. A running turn's rows are
    // not that claim — they exist only while the turn is open, and the reply's
    // durable steps replace them the instant it settles. Banning them outright
    // left the panel able to say a turn was running and never what it had done.
    expect(threadPanel).toContain("<StepTimeline steps={[...openTurnSteps]}");
    // What stays banned: the panel reaching for a message's own steps.
    expect(threadPanel).not.toContain("message.steps");
    expect(threadPanel).not.toContain("reply.steps");
  });

  it("RoomView supplies it, so the panel is never handed an empty map", () => {
    expect(chatView).toMatch(/<ThreadPanel[\s\S]{0,600}liveStepsByMessage=\{liveStepsByMessage\}/);
  });
});

describe("cleanup is addressed by the message that was answered", () => {
  /**
   * `AcceptedTurn::thread_root` is explicit that "a reply is parented to its
   * question's parent, never to the question", so a reply's `parentId` names
   * the thread ROOT for any follow-up typed inside a thread.
   *
   * Clearing by it would leave the follow-up's own bucket resident and — far
   * worse — delete the root's. With the root's own turn still running that
   * erases a live sibling's timeline: precisely the failure per-query rows
   * exist to prevent, reintroduced by the cleanup path.
   */
  it("never keys the per-query clear on a reply's placement parent", () => {
    const reply = appShell.slice(appShell.indexOf("const renderAgentReply"));
    const body = reply.slice(0, reply.indexOf("setTranscripts("));
    expect(body).not.toContain("setLiveStepsByMessage(");
    expect(body).not.toMatch(/hostMessageId\(event\.parentId\)[\s\S]{0,200}delete/);
  });

  /**
   * A turn that answers grows durable steps on its own message, which is a fact
   * about that message rather than about where its reply was placed — so the
   * swap is driven by their arrival.
   */
  it("retires a bucket once its message carries durable steps", () => {
    expect(appShell).toMatch(/if \(m\.steps && m\.steps\.length > 0\) done\.add\(m\.id\)/);
    // Driven from the history hydrate, so a mid-turn reload re-converges too.
    expect(appShell).toMatch(/const hydrated = fromHistory\(entries\);[\s\S]{0,400}clearLiveRowsSettledBy\(hydrated\)/);
  });

  /**
   * A turn that FAILS journals a `TurnFailed` line and no reply, so it never
   * grows steps to swap for. Without this its bucket outlives the turn holding
   * a row still marked `running` — a result that never arrived cannot flip it.
   */
  it("retires a failed turn's bucket on the terminal settle, inside the guard", () => {
    // Located by text, so the string tracks the source. The guard read
    // `openTurnsRef.current` while the shell mirrored its own state into a ref;
    // that state moved to `room/store.ts` and the mirror went with it, so the
    // read is now the store's synchronous one. Same guard, and strictly fresher:
    // the ref was written in an effect and so lagged a commit behind, which is
    // the direction that MISSES a turn just added.
    const guardAt = appShell.indexOf(
      "if (!hasOtherOpenTurns(room.readRoom().openTurns, liveKey, settledTurnId)) {",
    );
    expect(guardAt, "the settle guard must be present").toBeGreaterThan(-1);
    // Inside the guard: a queued sibling still running owns its rows.
    const block = appShell.slice(guardAt, guardAt + 1600);
    expect(block).toContain("clearLiveRowsSettledBy(hydrated, hydrated.map((m) => m.id))");
  });
});

describe("a thread's live agent retires with its rows", () => {
  /**
   * A thread key is reused by every turn a conversation ever runs, and
   * `liveAgentByTurn` is keyed by it for any frame the host did not stamp with
   * a `messageSeq`. Clearing the rows without the agent leaves the previous
   * turn's teammate on the key, so the next turn's row names whoever answered
   * last until a frame happens to carry a new id — and on a turn that never
   * reports one, that is the whole turn (CodeRabbit on #2423).
   *
   * Pinned as one helper rather than as three call sites, because the failure
   * mode is a *fourth* clear site added later that forgets the second half.
   */
  it("clears both halves through one helper", () => {
    expect(appShell).toContain("const clearLiveThread = useCallback(");
    const helper = appShell.slice(
      appShell.indexOf("const clearLiveThread = useCallback("),
      appShell.indexOf("const onSendStart = useCallback("),
    );
    expect(helper).toContain("setLiveStepsByThread(");
    expect(helper).toContain("setLiveAgentByTurn(");
  });

  it("is what every thread-bucket clear goes through", () => {
    // The three terminal paths: a send arming, a send settling, and a reply
    // landing for a turn this console did not start.
    expect(appShell).toContain("clearLiveThread(threadId, true)");
    expect(appShell).toContain("clearLiveThread(threadId)");
    expect(appShell).toContain("clearLiveThread(event.chatId)");
    // And no path left writing the rows directly, which would skip the agent.
    expect(appShell).not.toContain("setLiveStepsByThread((prev) => ({ ...prev, [threadId]: [] }))");
  });
});
