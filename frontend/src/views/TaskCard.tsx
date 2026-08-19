// One card on a board, when the row behind it is a `Task`.
//
// # Why this is a file of its own
//
// It was the task board screen's card, and the screen is gone (issue #1140):
// the board an operator works is the `tasks` ledger's columns inside the
// Ledgers section, and meeting the same records twice — once on a Tasks page,
// once on a Ledgers page — was the duplication that retired the page.
//
// The card did not go with it. `LedgerBoard`'s `renderCard` slot exists exactly
// so one board can serve both a ledger a company declared this morning and the
// native board, and this is the second half of that pair: a priority, an
// assignee, a cost, a workflow chip, a plan badge, an output link and — the one
// that matters most — what a paused card is stopped behind and whether Resume
// is the right click. None of that is a ledger field; all of it comes off the
// `Task` record, which is why a role-driven renderer was tried here and was
// wrong. See `LedgerBoard`'s header for that argument in full.
//
// Nothing below changed on the way over. That was deliberate: the move is
// verified by `test/unit/task-blocked-card.test.ts` passing with only its
// import line touched.

import {
  AlertTriangle,
  CircleHelp,
  ClipboardList,
  FileText,
  Hourglass,
  ListTree,
  Paperclip,
  Play,
  ScrollText,
} from "lucide-react";

import type { Task, TaskPlan } from "@/api/tasks";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { PRIORITY_STYLES } from "@/lib/board-columns";
import { formatUsdCost } from "@/lib/cost";
import { approvalAction, timeAgo } from "@/lib/language";
import type { TaskApprovalBlock } from "@/lib/task-approvals";
import { extraOutputCount, primaryLink, type TaskLink } from "@/lib/task-output";
import { cn } from "@/lib/utils";
import { tallyPrerequisites } from "./TaskPlanBrief";

function priorityStyle(priority: string): string {
  return PRIORITY_STYLES[priority as keyof typeof PRIORITY_STYLES] ?? PRIORITY_STYLES.low;
}

/**
 * One card on the task board.
 *
 * It no longer carries the drag handlers: [`LedgerBoard`](./LedgerBoard) wraps
 * every card in the draggable element and owns the gesture, so this is purely
 * what a *task* looks like. That split is what lets one board serve both this
 * and a ledger a company declared — see that module's docs for why the card is
 * a slot rather than something built from field roles.
 *
 * Exported for `test/unit/task-blocked-card.test.ts` (issue #883). The
 * paused card's central claim — Resume is *disabled* while the card's own
 * approvals are undecided, because pressing it re-runs work that parks again —
 * exists only at the rendered button, so a pure test of the derivation cannot
 * reach it. Same exception `approval-batch-card.test.ts` earns, on the same
 * grounds: the thing under test is what reaches the operator's hand.
 */
export function TaskItem({
  task,
  dragging,
  block,
  now,
  onOpen,
  onResume,
  onReview,
}: {
  task: Task;
  dragging: boolean;
  /** What this card is stopped behind, or `null` when nothing (issue #883). */
  block: TaskApprovalBlock | null;
  /** The clock `block` was derived against, for its relative label. */
  now: number;
  onOpen: () => void;
  onResume: () => void;
  /** Opens the Approvals page filtered to this card (issue #883). */
  onReview?: () => void;
}) {
  return (
    <div
      onClick={onOpen}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.target !== e.currentTarget) return;
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen();
        }
      }}
      className={cn(
        "cursor-grab rounded-lg border bg-card p-3 shadow-sm transition-shadow hover:shadow active:cursor-grabbing",
        dragging && "opacity-50",
      )}
    >
      <div className="flex items-start justify-between gap-2">
        <p className="text-sm font-medium leading-snug">{task.title}</p>
        <Badge variant="outline" className={cn("shrink-0 capitalize", priorityStyle(task.priority))}>
          {task.priority}
        </Badge>
      </div>
      {task.note && (
        <p className="mt-1 line-clamp-2 whitespace-pre-wrap text-xs text-muted-foreground">
          {task.note}
        </p>
      )}
      {task.assignee && (
        <div className="mt-3 flex items-center gap-2">
          <span
            className="flex size-6 items-center justify-center rounded-full bg-muted text-3xs font-semibold text-muted-foreground"
            aria-hidden
          >
            {initials(task.assignee)}
          </span>
          <span className="truncate text-xs text-muted-foreground">{task.assignee}</span>
        </div>
      )}
      {formatUsdCost(task.cost, "total") && (
        <div className="mt-2 text-2xs font-medium tabular-nums text-foreground">
          {formatUsdCost(task.cost, "total")}
        </div>
      )}
      {task.deliverable === "workflow" && (
        <div className="mt-2 inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-2xs text-muted-foreground">
          <ListTree className="size-3 shrink-0" />
          Workflow
        </div>
      )}
      {task.plan && <PlanBadgeRow plan={task.plan} />}
      {SHOWS_OUTPUT_LINK.has(task.column) && <OutputLinkRow task={task} />}
      {task.column === "paused" && (
        <>
          {block && <BlockedRow block={block} now={now} onReview={onReview} />}
          <Button
            variant="outline"
            size="sm"
            className={cn("h-7 w-full", block ? "mt-2" : "mt-3")}
            // Issue #883: the button is disabled rather than hidden while the
            // card is blocked. Hiding it would leave the card looking like it
            // had no next action at all, which is the ambiguity being fixed —
            // the operator has to be able to see that Resume is the wrong click
            // right now, not wonder where it went. `title` carries the reason
            // for a pointer; the row above carries it for everyone else.
            disabled={block !== null}
            title={
              block
                ? "Blocked — decide its approvals first; resuming re-runs the work from the start."
                : undefined
            }
            onClick={(e) => {
              // Don't let the click bubble to the card's open handler.
              e.stopPropagation();
              onResume();
            }}
          >
            <Play className="mr-1.5 size-3.5" />
            Resume
          </Button>
        </>
      )}
    </div>
  );
}

/**
 * Why a paused card is stopped, on the card itself (issue #883).
 *
 * The card used to carry a Resume button and nothing else, so "decided one of
 * five, still waiting on four" and "wedged" were the same pixels — and Resume
 * was the natural next click from both. It is the wrong click from the first:
 * the turn continues on its own when the last decision lands (#469), so
 * re-dispatching only re-runs the work and parks the same calls again.
 *
 * Names the calls, not the mechanism. One blocked call is quoted in full —
 * through {@link approvalAction}, the same function the Approvals page and the
 * chat card label their rows with, so all three say "Fetch a web page" rather
 * than three different things about one approval. Several are counted instead,
 * because five tool names is not something to read on a Kanban card; the count
 * plus the Review link is, and the page it links to lists them.
 */
function BlockedRow({
  block,
  now,
  onReview,
}: {
  block: TaskApprovalBlock;
  /** The same clock the block was derived against. */
  now: number;
  onReview?: () => void;
}) {
  const only = block.count === 1 ? block.approvals[0] : null;
  return (
    <div className="mt-2 rounded-md border border-status-blocked/30 bg-status-blocked-soft px-2 py-1.5">
      <div className="flex items-center gap-1.5 text-2xs font-medium text-status-blocked-text">
        <Hourglass className="size-3 shrink-0" />
        <span className="min-w-0 truncate">
          {only ? approvalAction(only) : `Blocked on ${block.count} approvals`}
        </span>
      </div>
      <div className="mt-0.5 flex items-center justify-between gap-2 text-2xs text-muted-foreground">
        <span className="truncate">
          Waiting for your approval · {timeAgo(block.since, now)}
        </span>
        {onReview && (
          <button
            type="button"
            className="shrink-0 font-medium text-status-blocked-text underline-offset-2 hover:underline"
            onClick={(e) => {
              // The card's own click handler opens task detail; this goes
              // somewhere else, so it must not also do that.
              e.stopPropagation();
              onReview();
            }}
          >
            Review
          </button>
        )}
      </div>
    </div>
  );
}

/**
 * The columns whose cards show what they produced (issue #339).
 *
 * Done **and In review**, which is a correction to how the epic is worded. A
 * clean success no longer lands in Done — it stops in In review, and Done is
 * reached only when a person accepts it. So a card that has produced something
 * spends most of its visible life in In review, and showing the link only in
 * Done would hide it during exactly the stretch where somebody is deciding
 * whether to accept the work and needs to read it.
 *
 * Not the earlier columns: a card in To-do or In progress either has no output
 * yet or has one from a superseded attempt, and advertising that mid-run would
 * suggest the work in flight is already finished.
 */
const SHOWS_OUTPUT_LINK = new Set(["in_review", "done"]);

/**
 * What a planned card carries, in one line on the board (issue #337).
 *
 * Shown on **every** column a plan survives into rather than a chosen set, and
 * that is the difference from {@link SHOWS_OUTPUT_LINK} above. An output is
 * only meaningful once there is one, so it earns a column filter; a plan is
 * only ever present because a person deliberately asked for one, so hiding it
 * anywhere would be second-guessing that request.
 *
 * The blocked case is the one that has to be loud. A pass that could not clear
 * a card returns it to To-do, where it sits looking exactly like work nobody
 * has picked up — and the difference between "not started" and "cannot start"
 * is the whole point of having planned it. So blockers get the destructive
 * treatment and a count; a clear plan gets a quiet step count and stays out of
 * the way.
 *
 * `needsApproval` and `unknown` are deliberately not counted here. Neither
 * stops the card host-side, and a badge that counted them would tell an
 * operator to go fix something that is not blocking anything.
 */
function PlanBadgeRow({ plan }: { plan: TaskPlan }) {
  const { blocking, approval, unchecked } = tallyPrerequisites(plan);
  if (blocking > 0) {
    return (
      <div className="mt-2 flex items-center gap-1.5 text-2xs font-medium text-destructive">
        <AlertTriangle className="size-3 shrink-0" />
        <span>
          Planned — needs {blocking} thing{blocking === 1 ? "" : "s"}
        </span>
      </div>
    );
  }
  // Nothing blocking, but not necessarily all-clear either — the same three-way
  // distinction the brief's headline makes, kept in step with it so the board
  // and the card can never disagree about whether a plan is settled. A count
  // here is a prompt to open the card, where the rows say which is which.
  const unresolved = approval + unchecked;
  if (unresolved > 0) {
    return (
      <div className="mt-2 flex items-center gap-1.5 text-2xs text-status-blocked-text">
        <CircleHelp className="size-3 shrink-0" />
        <span>
          Planned — {unresolved} to be aware of
        </span>
      </div>
    );
  }
  return (
    <div className="mt-2 flex items-center gap-1.5 text-2xs text-muted-foreground">
      <ClipboardList className="size-3 shrink-0" />
      <span>
        Planned
        {plan.steps.length > 0 && ` · ${plan.steps.length} step${plan.steps.length === 1 ? "" : "s"}`}
      </span>
    </div>
  );
}

function LinkIcon({ kind }: { kind: TaskLink["kind"] }) {
  const className = "size-3.5 shrink-0";
  if (kind === "artifact") return <Paperclip className={className} />;
  if (kind === "workflow") return <ListTree className={className} />;
  if (kind === "trace") return <ScrollText className={className} />;
  return <FileText className={className} />;
}

/**
 * One line on a finished card: *here is the thing this task produced*.
 *
 * Every card in these columns gets one, including the ones that produced no
 * file — for those the link opens the attempt's trace, which is the deliverable
 * when there is no document. A card that recorded no attempt at all links to
 * itself, which is honest rather than absent.
 *
 * The anchor stops its own click from bubbling: the whole card is a button that
 * opens the detail screen, and without this a click on the link would both
 * follow the href and fire the card's `onOpen`, racing two navigations.
 */
function OutputLinkRow({ task }: { task: Task }) {
  const link = primaryLink(task);
  const extra = extraOutputCount(task);
  return (
    <div className="mt-3 flex items-center gap-2 border-t pt-2 text-xs">
      <a
        href={link.href}
        title={link.hint}
        onClick={(e) => e.stopPropagation()}
        className="flex min-w-0 items-center gap-1.5 text-muted-foreground hover:text-foreground hover:underline"
      >
        <LinkIcon kind={link.kind} />
        <span className="truncate">{link.label}</span>
      </a>
      {extra > 0 && (
        <a
          href={`#/tasks/${encodeURIComponent(task.id)}`}
          title="Open the task to see everything it produced."
          onClick={(e) => e.stopPropagation()}
          className="shrink-0 text-muted-foreground hover:text-foreground hover:underline"
        >
          +{extra} more
        </a>
      )}
    </div>
  );
}


function initials(name: string): string {
  return name
    .trim()
    .split(/\s+/)
    .slice(0, 2)
    .map((p) => p.charAt(0).toUpperCase())
    .join("");
}
