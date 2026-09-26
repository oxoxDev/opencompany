import { useState } from "react";

import { useCrossingRunning } from "./referral-running";
// The one definition of "which step is in flight" — shared with the line
// above this row so the two can never disagree about it.
import { runningStepLabel } from "./WorkingIndicator";
import {
  AlertTriangle,
  Brain,
  ChevronDown,
  ChevronRight,
  CornerUpLeft,
  Hourglass,
  Loader2,
  Scissors,
  SquareKanban,
  Wrench,
  X,
  type LucideIcon,
} from "lucide-react";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";

import { teammateName } from "@/components/episode/teammate-name";
import { TeammateAvatar } from "@/components/teammate-avatar";

import type { AgentConversationDto, ReferralConversationDto } from "@/api/types";

import {
  AWAITING_APPROVAL_LABEL,
  STEP_FAILURE_LABEL,
  isFailedStep,
  type TurnStep,
  type TurnStepKind,
} from "@/api/types";
import { consoleHref } from "@/lib/console-paths";
import { cn } from "@/lib/utils";

/**
 * The scrubbed processing steps behind a company reply, rendered above its
 * bubble. Collapsed by default to a one-line "N steps · M failed" summary;
 * auto-expands when any step failed so a silent MCP failure is visible, not
 * buried. Renders nothing when there are no steps (a memory-served / tool-less
 * reply). Ported from the retired Conversation page (issue #246) so the chat
 * workspace keeps the same tool-call visibility it had.
 *
 * `defaultOpen` was written for the *live* timeline of a turn still running
 * (issue #367), on the reading that its rows are the content. Chat no longer
 * takes it, deliberately: the live pair pins a **line** to the foot of the
 * pane saying what is happening and who is doing it, and the timeline beneath
 * it is the detail behind that line — the same relationship a finished reply's
 * steps have to its text. An always-open list under every running turn also
 * grows the foot of the transcript by a row per tool call, pushing the very
 * line it supports off-screen on a long turn.
 *
 * What still opens by itself is what the operator can *act* on: a failed step,
 * or one parked on a sign-off. Those force the list open wherever it renders,
 * live or settled, because a silent MCP failure behind a count is the thing
 * #411 exists to prevent. The prop stays for callers outside chat, and the
 * operator's own toggle wins from the first click either way.
 */
export function StepTimeline({
  steps,
  defaultOpen = false,
}: {
  steps: TurnStep[];
  defaultOpen?: boolean;
}) {
  const failed = steps.filter((s) => isFailedStep(s.status)).length;
  // A parked step is counted and surfaced separately — it is not a failure, and
  // it is the most actionable thing in the list, so it must not hide behind a
  // collapsed summary either (#411).
  const parked = steps.filter((s) => s.status === "awaiting_approval").length;
  const hasError = failed > 0;
  // The call in flight, named in the collapsed summary.
  //
  // This row is where "what is happening" lives — the line above it names the
  // teammate and stops. Collapsed, the summary was a bare count, so between
  // them the two rows said who was working and how many things had happened
  // and never what was happening now. Naming it here keeps that visible at a
  // glance without opening a list that grows by a row per tool call.
  const running = runningStepLabel(steps);
  const [open, setOpen] = useState(defaultOpen || hasError || parked > 0);

  if (steps.length === 0) return null;

  return (
    <div className="mt-1 w-full max-w-[85%] sm:max-w-[75%]">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className={cn(
          "flex items-center gap-1 rounded-md px-1.5 py-0.5 text-2xs font-medium transition-colors hover:bg-accent/60",
          hasError
            ? "text-destructive"
            : parked > 0
              ? "text-status-blocked-text"
              : "text-muted-foreground",
        )}
      >
        {open ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
        <span>
          {steps.length} step{steps.length === 1 ? "" : "s"}
          {failed > 0 && ` · ${failed} failed`}
          {parked > 0 && ` · ${parked} awaiting approval`}
          {!open && running && ` · ${running}`}
        </span>
      </button>
      {open && (
        <ol className="mt-0.5 flex flex-col gap-1 rounded-lg border bg-card/60 px-2.5 py-1.5">
          {steps.map((step, i) => (
            <StepRow key={i} step={step} />
          ))}
        </ol>
      )}
    </div>
  );
}

/**
 * A crossing with somebody outside this desk, as one collapsed line.
 *
 * Same idiom as {@link StepTimeline} and for the same reason: the exchange is
 * detail behind a report, not part of the desk's own conversation. The relayed
 * rows are dropped host-side — an agent who does not work here did not speak
 * here — so this is the only place an operator can read what was actually asked
 * and answered rather than the asker's paraphrase of it.
 *
 * Closed by default. The count is the whole point of the collapsed state: it
 * says how much was said without saying it.
 */
export function ReferralConversation({
  crossing,
  rowId,
  agentNames,
}: {
  crossing: ReferralConversationDto;
  /**
   * The row this crossing folds onto, so the chip can tell a crossing still
   * being had from one that is over. Optional: a surface that does not know it
   * renders the finished wording, which is what every surface did before.
   */
  rowId?: string;
  /** Roster id to display name, for the asker and the other side. */
  agentNames?: Readonly<Record<string, string>>;
}) {
  const [open, setOpen] = useState(false);
  const running = useCrossingRunning(rowId);
  const count = crossing.lines.length;
  if (count === 0) return null;

  return (
    <div className="mt-1 w-full max-w-[85%] sm:max-w-[75%]">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-2xs font-medium text-muted-foreground transition-colors hover:bg-accent/60"
      >
        {open ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
        <span>
          {/* Who was asked, in the form they were asked in: `@name` went to a
              person, `#desk` was put to a room. Naming the answerer's desk
              alongside their name read as though the desk had been asked, which
              for a `@name` crossing is the one thing that did not happen.

              On the desk that WAS asked the same exchange runs the other way,
              and every field is named from the asker's side — so the unswapped
              label read "asked #order_ops" over a row where `order_ops` was
              the desk doing the asking. `inbound` names the teammate who
              raised it, which is the one thing this side does not already
              know: the chip above says the desk, the fold says who. */}
          {/* **Present tense while it is still happening.**

              "asked @amendments · 1 message" describes a crossing that finished
              after one reply. Shown the moment the question is journaled, it
              described a conversation that was still going — and the count was
              simply however much had landed so far, which is why it read as a
              finished exchange that had produced one line. Past tense is a
              claim about something being over, so it waits until it is. */}
          {running
            ? crossing.inbound
              ? `answering @${teammateName(crossing.otherId, agentNames)}`
              : `${teammateName(crossing.askerId, agentNames)} is talking to ${
                  crossing.direct
                    ? `@${teammateName(crossing.otherId, agentNames)}`
                    : `#${crossing.otherDeskId}`
                }`
            : crossing.inbound
              ? `asked by @${teammateName(crossing.otherId, agentNames)}`
              : `asked ${
                  crossing.direct
                    ? `@${teammateName(crossing.otherId, agentNames)}`
                    : `#${crossing.otherDeskId}`
                }`}{" "}
          {/* "so far" while it runs, because the number is not the total yet. */}
          · {count} message{count === 1 ? "" : "s"}
          {running ? " so far" : ""}
        </span>
      </button>
      {open && (
        // Rendered as a conversation, because that is what it is. The same
        // gutter-avatar-then-author-then-body shape a message uses in the
        // transcript above, one size down: an operator reading this is reading
        // a chat between two desks, and a label-over-paragraph list made them
        // translate it back into one.
        <ol className="mt-0.5 flex flex-col gap-2 rounded-lg border bg-card/60 px-2.5 py-2">
          {crossing.lines.map((line, i) => {
            const who = line.outbound
              ? teammateName(crossing.askerId, agentNames)
              : line.authorLabel || teammateName(line.authorId, agentNames);
            // The desk each side is speaking from — the asker's is this one, so
            // it goes unsaid; the answer comes from somewhere the reader may not
            // have open.
            return (
              <li key={i} className="flex gap-2">
                <TeammateAvatar name={who} className="mt-0.5 size-5 shrink-0" />
                <div className="flex min-w-0 flex-col gap-0.5">
                  {/* The name alone. The header already says whether this was
                      a person or a desk, and repeating the answerer's desk on
                      their line was what made a `@name` crossing read as though
                      the desk had been asked. */}
                  <span className="text-2xs leading-none font-semibold">{who}</span>
                  <span className="text-2xs leading-relaxed whitespace-pre-wrap text-muted-foreground">
                    {line.text}
                  </span>
                </div>
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}

/**
 * One agent-to-agent exchange, as one collapsed line on the row that opened it.
 *
 * Same idiom as {@link ReferralConversation} above, and deliberately so: to a
 * reader these are the same act — somebody on this desk stepped aside to ask
 * somebody else, and the desk's own transcript cannot show it. What differs is
 * only where the rows live. A crossing's relayed rows are dropped host-side;
 * these are kept, in the pair channel the two seats wrote to, and the host
 * folds them here because a desk reads its own channel and they are not in it.
 *
 * Closed by default, for the reason a crossing is: the count says how much was
 * said without saying it, and the desk still reads as its own conversation.
 *
 * `concluded` comes from the host rather than from a running-turn lookup — the
 * conclusion is journaled, so there is no need to infer it from whether a turn
 * happens to be open.
 */
export function AgentConversation({
  exchange,
  agentNames,
}: {
  exchange: AgentConversationDto;
  /** Roster id to display name, for the asker and the askee. */
  agentNames?: Readonly<Record<string, string>>;
}) {
  const [open, setOpen] = useState(false);
  const count = exchange.lines.length;
  if (count === 0) return null;
  const running = !exchange.concluded;

  return (
    <div className="mt-1 w-full max-w-[85%] sm:max-w-[75%]" data-testid="agent-conversation">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        data-conversation-state={running ? "live" : exchange.forced ? "unanswered" : "answered"}
        className="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-2xs font-medium text-muted-foreground transition-colors hover:bg-accent/60"
      >
        {open ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
        <span>
          {/* Present tense while it is still happening, for the reason the
              crossing above words it that way: past tense is a claim that
              something is over, and this one says so from the journal. */}
          {running
            ? `${teammateName(exchange.askerId, agentNames)} is talking to @${teammateName(exchange.askeeId, agentNames)}`
            : exchange.forced
              ? `asked @${teammateName(exchange.askeeId, agentNames)}, unanswered`
              : `asked @${teammateName(exchange.askeeId, agentNames)}`}{" "}
          · {count} message{count === 1 ? "" : "s"}
          {running ? " so far" : ""}
        </span>
      </button>
      {open && (
        <ol className="mt-0.5 flex flex-col gap-2 rounded-lg border bg-card/60 px-2.5 py-2">
          {exchange.lines.map((line, i) => {
            const who = line.outbound
              ? teammateName(exchange.askerId, agentNames)
              : line.authorLabel || teammateName(line.authorId, agentNames);
            return (
              <li key={i} className="flex gap-2">
                <TeammateAvatar name={who} className="mt-0.5 size-5 shrink-0" />
                <div className="flex min-w-0 flex-col gap-0.5">
                  <span className="text-2xs leading-none font-semibold">{who}</span>
                  <span className="text-2xs leading-relaxed whitespace-pre-wrap text-muted-foreground">
                    {line.text}
                  </span>
                </div>
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}

function StepRow({ step }: { step: TurnStep }) {
  const error = isFailedStep(step.status);
  const parked = step.status === "awaiting_approval";
  const Icon = parked ? Hourglass : stepIcon(step.kind);
  return (
    <li
      className={cn(
        "flex flex-col gap-0.5 text-2xs leading-relaxed",
        error
          ? "text-destructive"
          : parked
            ? "text-status-blocked-text"
            : "text-muted-foreground",
      )}
    >
      <div className="flex items-center gap-1.5">
        <Icon className={cn("size-3 shrink-0", step.status === "running" && "animate-pulse")} />
        <span className={cn("font-medium", !error && !parked && "text-foreground/80")}>
          {step.label}
        </span>
        {/* The typed state, rendered by lookup — never by reading `result`. */}
        {parked && <StepChip tone="amber">{AWAITING_APPROVAL_LABEL}</StepChip>}
        {step.failure && <StepChip tone="rose">{STEP_FAILURE_LABEL[step.failure]}</StepChip>}
        {step.truncated && (
          <StepChip tone="amber">
            <Scissors className="size-2.5 shrink-0" aria-hidden />
            Result cut
          </StepChip>
        )}
        {step.detail && <span className="min-w-0 truncate">— {step.detail}</span>}
        <span className="ml-auto shrink-0 tabular-nums opacity-70">
          {formatElapsed(step.elapsedMs, step.status)}
        </span>
      </div>
      {/* What came back, on its own line: it is the answer to "how far did we
          get", and inlining it would push the arguments off the row. */}
      {step.result && (
        <span className="min-w-0 truncate pl-[18px] opacity-80">{step.result}</span>
      )}
    </li>
  );
}

function StepChip({
  tone,
  children,
}: {
  tone: "amber" | "rose";
  children: React.ReactNode;
}) {
  return (
    <span
      className={cn(
        "flex shrink-0 items-center gap-1 rounded px-1 py-px text-3xs font-medium",
        tone === "amber"
          ? "bg-status-blocked-soft text-status-blocked-text"
          : "bg-status-failed-soft text-status-failed-text",
      )}
    >
      {children}
    </span>
  );
}

function stepIcon(kind: TurnStepKind): LucideIcon {
  switch (kind) {
    case "tool_call":
      return Wrench;
    case "thinking":
      return Brain;
    case "note":
      return AlertTriangle;
    default:
      return Wrench;
  }
}

/**
 * A step's duration.
 *
 * A gated call never ran, so it reports `0ms` — which read identically to a
 * fast success and was one of the things #411 called out. Say "didn't run"
 * instead: a duration of zero on a step that never left the process is not a
 * measurement, it is the absence of one.
 */
function formatElapsed(ms: number | undefined, status: TurnStep["status"]): string {
  if (status === "awaiting_approval") return "didn't run";
  if (typeof ms !== "number") return "";
  return ms < 1000 ? `${ms}ms` : `${(ms / 1000).toFixed(1)}s`;
}

/**
 * The "a card opened from this reply" chip (issue #246) — links straight to
 * the board card a turn opened, or the one it dispatched to, and dismisses it
 * (issue #984).
 *
 * The dismissal is the half #442 promised and never shipped on this surface:
 * it allowed a turn to open a card from an ordinary message on the grounds
 * that *"a spurious card can be dismissed in one click"*, while this chip was
 * a bare link to the card's detail screen. `onDismiss` is optional so a caller
 * that has no card-delete route — the thread panel, a future read-only
 * transcript — still renders the link half rather than a control that throws.
 */
/**
 * Where a crossing referral came from, or went (tinyhivemind P15).
 *
 * Modelled on {@link CardChip} deliberately: both are provenance on the bubble
 * — "this message is connected to something not on this screen" — and a reader
 * should not have to learn two shapes for one idea. It is NOT a centred system
 * pill; a pill is a line the runtime wrote *about* the conversation, and this
 * is a fact about the message under it.
 *
 * Two directions, two words, because the reader's question differs. In the
 * desk that was asked, the question is "why is this here?" — `Asked by
 * Engineering`. In the desk that asked, an answer has arrived from a teammate
 * who is not on this desk, and without saying so it reads as local work —
 * `Answered by Product & Design`.
 *
 * The label is whatever the host captured with the row; this never resolves a
 * desk id at render, and never shows one — an id in operator copy is the thing
 * `DiscussionMessage.author` refuses for the same reason.
 */
export function ReferralChip({
  deskId,
  deskName,
  askerId,
  sequence,
  direction,
  direct = false,
  agentNames,
}: {
  deskId: string;
  deskName: string;
  askerId: string;
  sequence: number;
  direction: "asked" | "answered";
  direct?: boolean;
  agentNames?: Readonly<Record<string, string>>;
}) {
  // Whoever was actually addressed. A crossing put to a PERSON never reached
  // their desk — that desk holds none of the exchange and its other members had
  // no part in it — so naming the desk here credited a room that was never asked.
  const who = direct ? `@${teammateName(askerId, agentNames)}` : deskName;
  const label = direction === "asked" ? `Asked by ${who}` : `Answered by ${who}`;
  const body = (
    <>
      <CornerUpLeft className="size-3 shrink-0" />
      {label}
    </>
  );
  return (
    <span className="mt-1.5 flex w-fit items-center rounded-full bg-accent text-accent-foreground">
      {/* A DESK crossing ran on that desk, so its transcript is where the
          question at `sequence` is and the link reaches it.

          A DIRECT one did not: both sides are held in the pair's own thread and
          the target's desk holds none of it, so this link would open an
          unrelated conversation and land on a sequence that is not there.
          Nothing to link to until a pair thread is a surface an operator can
          open — and the exchange itself is already one click away, on the
          message this chip sits under. So it reads as a label rather than
          offering a way somewhere wrong. */}
      {direct ? (
        <span className="flex items-center gap-1 px-2 py-0.5 text-2xs font-medium">{body}</span>
      ) : (
        <a
          href={`#/chat?desk=${encodeURIComponent(deskId)}&at=${sequence}`}
          className="flex items-center gap-1 px-2 py-0.5 text-2xs font-medium transition-opacity hover:opacity-80"
          title={`Open the conversation that ${direction === "asked" ? "asked" : "answered"}`}
        >
          {body}
        </a>
      )}
    </span>
  );
}

export function CardChip({
  taskId,
  busy = false,
  disabled = false,
  onDismiss,
}: {
  taskId: string;
  busy?: boolean;
  disabled?: boolean;
  onDismiss?: (taskId: string) => void;
}) {
  const link = (
    <a
      href={consoleHref("tasks", taskId)}
      className={cn(
        "flex items-center gap-1 py-0.5 text-2xs font-medium transition-opacity hover:opacity-80",
        onDismiss ? "pl-2 pr-1" : "px-2",
      )}
    >
      <SquareKanban className="size-3 shrink-0" />
      Card opened
    </a>
  );
  return (
    <span className="mt-1.5 flex w-fit items-center rounded-full bg-accent text-accent-foreground">
      {link}
      {onDismiss && (
        <AlertDialog>
          <AlertDialogTrigger
            render={
              <button
                type="button"
                // Always in the DOM and focusable rather than hover-revealed:
                // this chip is the only place the card can be dismissed from
                // the channel, and a hover-only control is unreachable by
                // keyboard and on touch.
                className="flex items-center rounded-full py-0.5 pl-0.5 pr-1.5 transition-opacity hover:opacity-80 disabled:opacity-50"
                disabled={busy || disabled}
                title="Dismiss this card"
                aria-label="Dismiss this card"
              >
                {busy ? (
                  <Loader2 className="size-3 shrink-0 animate-spin" />
                ) : (
                  <X className="size-3 shrink-0" />
                )}
              </button>
            }
          />
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Dismiss this card?</AlertDialogTitle>
              <AlertDialogDescription>
                This deletes the card from the board and can’t be undone. The
                message stays in the channel.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Keep card</AlertDialogCancel>
              <AlertDialogAction
                onClick={() => onDismiss(taskId)}
                className="bg-destructive text-white hover:bg-destructive/90"
              >
                Dismiss card
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      )}
    </span>
  );
}
