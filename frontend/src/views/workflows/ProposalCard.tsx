// Issue #415: what the operator reads before a copilot's change is applied.
//
// The card is the review, so it shows the DIFF and not the proposal: what would
// change about the graph on screen, step by step and connection by connection,
// computed from the candidate graph rather than from the model's op list. A
// change nobody proposed cannot appear in it, and an op that changes nothing
// shows as nothing.
//
// Nothing here mutates anything. Apply is the only path that writes, it goes
// through the same versioned `updateWorkflow` the editor uses, and Dismiss
// writes nothing at all: the card greys out and stays in the transcript so a
// later question can refer to what was turned down.

import { AlertTriangle, Check, Loader2, X } from "lucide-react";

import { type WorkflowProblem, workflowProblemLocator } from "@/api/types";
import type { GraphDiff } from "@/api/workflow-proposal";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { ProposalDiff } from "./ProposalDiff";

/** Where a proposal has got to. Only `pending` can write anything. */
export type ProposalState = "pending" | "applying" | "applied" | "dismissed";

interface Props {
  summary: string;
  diff: GraphDiff;
  state: ProposalState;
  /**
   * Why this proposal cannot be applied, when it cannot: the graph moved under
   * it, it was replayed from an earlier session, or it would change nothing.
   * Present means the Apply button is not offered at all rather than offered
   * and refused.
   */
  blocked?: string;
  /** The host's refusal, when an Apply was attempted and came back rejected. */
  error?: string;
  /**
   * The per-node breakdown behind {@link error}, when the host sent one
   * (issue #836).
   *
   * Rendered under the sentence rather than instead of it: the sentence is what
   * the host chose to say, and the list is where it happened. An operator whose
   * proposal touched one node reads the same thing twice and loses nothing; an
   * operator whose proposal broke three nodes gets the three node ids that
   * sentence had flattened together.
   */
  problems?: WorkflowProblem[];
  onApply: () => void;
  onDismiss: () => void;
}

export function ProposalCard({
  summary,
  diff,
  state,
  blocked,
  error,
  problems,
  onApply,
  onDismiss,
}: Props) {
  const settled = state === "applied" || state === "dismissed";
  return (
    <div
      data-testid="workflow-proposal"
      data-state={state}
      className={cn(
        "rounded-lg border bg-background p-2 text-2xs leading-snug",
        settled && "opacity-60",
      )}
    >
      <div className="flex items-start justify-between gap-2">
        <p className="font-medium text-foreground">{summary}</p>
        {state === "applied" && (
          <span className="inline-flex shrink-0 items-center gap-1 text-status-done-text">
            <Check className="size-3" /> Applied
          </span>
        )}
        {state === "dismissed" && <span className="shrink-0 text-muted-foreground">Dismissed</span>}
      </div>

      <ProposalDiff diff={diff} />

      {blocked && !settled && (
        <Alert className="mt-2 py-1.5">
          <AlertTriangle className="size-3" />
          <AlertDescription className="text-2xs leading-snug">{blocked}</AlertDescription>
        </Alert>
      )}

      {error && (
        <Alert variant="destructive" className="mt-2 py-1.5">
          <AlertTriangle className="size-3" />
          <AlertDescription className="text-2xs leading-snug">
            {error}
            {/* Issue #836: the node and field the host named, when it named
                any. Keyed by index because a problem carries no id of its own
                and the same node can legitimately raise two — the list is
                render-only and never reordered, so index is stable here. */}
            {problems && problems.length > 0 && (
              <ul className="mt-1 space-y-0.5" data-testid="workflow-proposal-problems">
                {problems.map((problem, i) => {
                  const locator = workflowProblemLocator(problem);
                  return (
                    <li key={i}>
                      {locator && <span className="font-medium">{locator} — </span>}
                      {problem.message}
                    </li>
                  );
                })}
              </ul>
            )}
          </AlertDescription>
        </Alert>
      )}

      {!settled && (
        <div className="mt-2 flex items-center gap-2">
          <Button
            size="sm"
            className="h-7 px-2 text-2xs"
            disabled={state === "applying" || Boolean(blocked)}
            onClick={onApply}
            data-testid="workflow-proposal-apply"
          >
            {state === "applying" ? (
              <Loader2 className="mr-1 size-3 animate-spin" />
            ) : (
              <Check className="mr-1 size-3" />
            )}
            Apply
          </Button>
          <Button
            size="sm"
            variant="ghost"
            className="h-7 px-2 text-2xs"
            disabled={state === "applying"}
            onClick={onDismiss}
            data-testid="workflow-proposal-dismiss"
          >
            <X className="mr-1 size-3" />
            Dismiss
          </Button>
        </div>
      )}
    </div>
  );
}
