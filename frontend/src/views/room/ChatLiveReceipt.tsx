import { useEffect, useState } from "react";

import type { TurnStep } from "@/api/types";
import { GENERAL_CHANNEL_ID } from "@/lib/chat";
import { cn } from "@/lib/utils";
import { TeammateAvatar } from "@/components/teammate-avatar";
import { StepTimeline } from "./StepTimeline";
import { runningStepLabel } from "./WorkingIndicator";
import type { Channel } from "./model";

/**
 * The live receipt for a chat instruction the operator just sent (issue #1934).
 *
 * Between hitting send and the reply landing there used to be a dead gap — the
 * composer cleared, and nothing said the turn had been taken until the whole
 * answer arrived, which for a long turn is many silent seconds. This is the row
 * that fills that gap: **Sent** the instant the POST is armed, **Picked up by
 * <teammate>** once the first live frame names who answered, **On step <label>**
 * while a step is in flight — each with a ticking elapsed readout so the wait is
 * legible rather than a frozen spinner.
 *
 * It is deliberately reversible. If no frame arrives (or advances) for
 * {@link RECEIPT_STALL_AFTER_MS}, it adds a soft "still waiting" note — not an
 * error, not a terminal state. Any new frame, or the reply itself, clears it.
 * The receipt clears the moment the real reply bubble lands (`AppShell`'s
 * `onSendEnd`), so there is never a frame where both the reply and the receipt
 * are absent.
 *
 * The state line reuses {@link runningStepLabel} — the same source
 * `WorkingIndicator` derives its line from — so the two surfaces never phrase
 * the same step differently (the #264 drift rule). The teammate is always shown
 * by name; a raw agent id is never rendered.
 */
export const RECEIPT_STALL_AFTER_MS = 30_000;

/**
 * What `AppShell` tracks per thread for the duration of a synchronous chat
 * turn. `startedAt` fixes the elapsed clock; `lastFrameAt` seeds to `startedAt`
 * and bumps on every live frame, so the stall check is "no frame for 30s"
 * rather than "no reply for 30s". `agentId` is captured off the first frame
 * that names one and never rendered raw — it is resolved to a display name.
 *
 * `gen` is the generation the send that armed this receipt was stamped with
 * (issue #1935 review). Host thread ids like `main` recur across companies,
 * so without it a slow POST from a company the operator has since left can
 * land after a *new* send has re-armed the same thread id and delete that
 * newer receipt out from under the company actually on screen. See
 * {@link shouldClearReceipt}.
 */
export interface ChatReceipt {
  startedAt: number;
  lastFrameAt: number;
  agentId?: string;
  gen?: number;
}

/**
 * Whether a clear request for a thread's receipt should actually delete it.
 *
 * The bug this guards (issue #1935 review, codex 3892523790 / coderabbit
 * 3892517512, and its sibling codex 3892702774): thread ids are reused across
 * companies (`main` above all), and `AppShell`'s
 * `onSendStale`/`onSendEnd`/`onSendDetached`/`onSendFailed` all clear a
 * receipt by thread id alone. Send it from company A, switch to company B,
 * send again on the same thread id — B's send arms a *new* receipt — and when
 * A's slow POST finally settles, its own clear call must not delete B's
 * receipt just because they share a thread id.
 *
 * Each armed receipt is stamped with the generation counter value current at
 * arm time; each terminal callback is handed the generation its own
 * `onSendStart` call returned. A clear only proceeds when the receipt
 * currently on file carries that same generation — if a newer send has
 * re-armed the slot in between, the generations differ and the clear is a
 * no-op, leaving the newer receipt alone.
 *
 * `gen === undefined` REFUSES to clear (issue #1935 review, codex
 * 3892702774 — reversing the original "clears unconditionally" reading of
 * this branch). A send surface that omits the generation is one whose slow
 * POST from company A can delete a newer receipt on company B through the
 * reused-thread-id race, which is exactly what this guard exists to close.
 *
 * Every current caller supplies a defined `gen`, so this branch is not
 * load-bearing for them. It stays fail-closed rather than
 * being deleted so a FUTURE send surface that forgets to capture and forward
 * `onSendStart`'s return value cannot reintroduce this exact leak by omission
 * — the failure mode of forgetting becomes a receipt that lingers until the
 * next send or company switch clears it, never a live receipt deleted out
 * from under a different company.
 */
export function shouldClearReceipt(
  current: Pick<ChatReceipt, "gen"> | undefined,
  gen: number | undefined,
): boolean {
  if (!current) return false;
  if (gen === undefined) return false;
  return current.gen === gen;
}

/**
 * Elapsed since a turn was sent — `Ns` under a minute, `m:ss` beyond it. Kept a
 * pure function so the format is unit-testable without a clock.
 */
export function formatElapsed(ms: number): string {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return minutes > 0 ? `${minutes}:${String(seconds).padStart(2, "0")}` : `${seconds}s`;
}

/**
 * Which agent the receipt should name, given the one a live frame just
 * reported and the one it is already showing.
 *
 * **The newest frame's agent wins.** One query can span several agents: a desk
 * hand-off runs the delegate's turn under the same `messageSeq`, and a hive
 * episode passes the floor between seats for the whole deliberation — the
 * episode's trigger seq is fixed at its start while `agent_id` is a per-turn
 * argument. Latching the first agent seen therefore pinned the receipt to
 * whoever spoke first and left it there while somebody else was visibly
 * working, which is exactly the question this row exists to answer.
 *
 * **A frame with no agent changes nothing.** Absence is not a hand-back, and
 * blanking the name mid-turn would drop the line to "Sent", reading as though
 * the turn had been un-picked-up.
 *
 * Extracted from `AppShell.onTurnEvent` so a test can call the rule instead of
 * restating it — the same reason `foldLiveFrame` lives apart from the shell,
 * and the trap the #2068 review caught when a test duplicated a conditional
 * and would have kept passing through a regression in the branch that runs.
 */
export function receiptAgentAfter(
  current: string | undefined,
  frameAgentId: string | undefined,
): string | undefined {
  return frameAgentId || current;
}

/**
 * The teammate on the other end of this receipt, by name — never a raw id.
 *
 * Resolves the captured `agentId` against the roster's name map, falling back
 * to the channel's own voice and then a neutral "a teammate" so an unresolved
 * id is still shown as a person rather than an opaque token. `undefined` only
 * when no frame has named an agent yet, which is the "Sent" state.
 */
export function resolveReceiptAgentName(
  receipt: ChatReceipt,
  agentNames: Record<string, string> | undefined,
  channel: Channel,
): string | undefined {
  if (!receipt.agentId) return undefined;
  return agentNames?.[receipt.agentId] ?? channel.voice ?? "an agent";
}

/**
 * The one visible state line, progressing Queued → Picked up by <name> → On step
 * <label>. A running step is the most specific thing to say, so it outranks the
 * name; the name outranks the base state. Pure, for the same reason
 * {@link formatElapsed} is.
 *
 * The base state is "Queued" while the turn is still waiting on the per-company
 * serial lock (issue #2021 — the receipt now rides the detached turn past its
 * 202 into the open-turn window, so the honest "waiting its turn" word must be
 * available to it), and "Sent" otherwise.
 */
export function receiptStateLine(
  receipt: ChatReceipt,
  steps: readonly TurnStep[] | undefined,
  agentNames: Record<string, string> | undefined,
  channel: Channel,
  queued?: boolean,
): string {
  const step = runningStepLabel(steps);
  const name = resolveReceiptAgentName(receipt, agentNames, channel);
  // Who outranks what, because the steps row beneath already names the call in
  // flight. A step used to replace the name outright, so a receipt that
  // reached "On step …" stopped saying who for the rest of the turn — and a
  // tool call is running for most of one.
  if (name) return `Picked up by ${name}`;
  if (step) return `On step ${step}`;
  return queued ? "Queued" : "Sent";
}

export function ChatLiveReceipt({
  channel,
  receipt,
  agentNames,
  steps,
  queued,
}: {
  channel: Channel;
  receipt: ChatReceipt;
  /** Roster agent id → display name, so the receipt never shows a raw id. */
  agentNames?: Record<string, string>;
  /** The turn's live steps, when any have arrived — folded below the line. */
  steps: TurnStep[];
  /**
   * The turn is accepted but has not taken the per-company serial lock (issue
   * #2021). Words the base line "Queued" instead of "Sent" and stills the pulse,
   * mirroring {@link WorkingIndicator}: a queued turn is not progressing, so the
   * mark says so.
   */
  queued?: boolean;
}) {
  const reduced = usePrefersReducedMotion();
  // Self-contained 1s clock, mounted only while this row is (the receipt is
  // present). `feed.now` is too coarse for a seconds readout, so this owns its
  // own interval and tears it down on unmount.
  const clock = useReceiptClock();
  const elapsed = Math.max(0, clock - receipt.startedAt);
  // Soft and reversible: a lull with no frame, seeded from `startedAt`, cleared
  // by the next frame that bumps `lastFrameAt`. Not an error state.
  const stalled = clock - receipt.lastFrameAt >= RECEIPT_STALL_AFTER_MS;
  const line = receiptStateLine(receipt, steps, agentNames, channel, queued);

  return (
    <div className="flex items-start gap-2.5 px-4 py-1">
      <TeammateAvatar
        name={channel.voice ?? channel.name}
        tone={channel.tone}
        avatar={channel.member?.avatar}
        company={channel.kind === "channel" && channel.id === GENERAL_CHANNEL_ID}
        className="size-9 shrink-0"
      />
      <div className="min-w-0 flex-1 space-y-1.5">
        <span
          data-testid="chat-live-receipt"
          data-stalled={stalled ? "true" : "false"}
          className="flex w-fit items-center gap-2 rounded-full bg-muted px-3 py-2 text-sm text-muted-foreground"
        >
          <span
            aria-hidden
            className={cn(
              "size-1.5 shrink-0 rounded-full",
              queued ? "bg-status-idle" : "bg-status-running",
              // The pulse is the "something is happening" signal; a reader who
              // asked for stillness keeps the mark without the motion, and a
              // queued turn keeps it without the motion either — nothing is
              // happening yet.
              !reduced && !queued && "animate-pulse",
            )}
          />
          {/* `aria-hidden`, because the stable assistive line below is what a
              screen reader should read — the visible line changes as the turn
              advances, and re-announcing every transition is noise. */}
          <span aria-hidden className="truncate">
            {line}
          </span>
          <span aria-hidden className="shrink-0 tabular-nums text-2xs text-muted-foreground/80">
            {formatElapsed(elapsed)}
          </span>
          <span className="sr-only">Waiting for a reply…</span>
        </span>
        {stalled && (
          <p role="status" className="px-1 text-2xs text-muted-foreground">
            No update for 30s… still waiting.
          </p>
        )}
        {/* What this turn has done so far, under the line that says who is
            doing it — the pairing this component's own `steps` doc has
            described since it was written, and did not render.

            Collapsed to "N steps", as a finished reply's timeline is, and
            auto-opening on a failed or parked step so a gated call is visible
            while it can still be acted on rather than after the fact (#411).
            These retire the instant the turn settles and the reply's durable
            steps take over, which is why they are not the "what the agent saw"
            claim Raw turns owns. */}
        {steps.length > 0 && <StepTimeline steps={steps} />}
      </div>
    </div>
  );
}

/** A 1-second clock that lives exactly as long as the row it drives. */
function useReceiptClock(): number {
  const [clock, setClock] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setClock(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);
  return clock;
}

/**
 * Whether the viewer asked for reduced motion, kept live. Mirrors
 * `WorkingIndicator`'s hook — reads `false` where `matchMedia` is unavailable,
 * and prefers the modern `addEventListener` spelling with the deprecated
 * `addListener` as the fallback older WebKitGTK builds still need.
 */
function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState(false);
  useEffect(() => {
    const mql = window.matchMedia?.("(prefers-reduced-motion: reduce)");
    if (!mql) return;
    setReduced(mql.matches);
    const onChange = () => setReduced(mql.matches);
    if (typeof mql.addEventListener === "function") {
      mql.addEventListener("change", onChange);
      return () => mql.removeEventListener("change", onChange);
    }
    mql.addListener(onChange);
    return () => mql.removeListener(onChange);
  }, []);
  return reduced;
}
