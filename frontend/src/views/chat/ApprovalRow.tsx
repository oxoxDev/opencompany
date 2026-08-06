// A parked approval, raised inside the conversation that produced it (#379).
//
// The same content the Approvals page shows — headline, payload, asker, waiting
// time, and the grant-scope choice (#431), all from `@/components/approval-card`
// — laid out as a channel row so it reads as part of the thread rather than as a
// panel bolted beside it. Sharing the scope control rather than restating it is
// what keeps the two surfaces from offering different things for one approval.
//
// The one thing it does differently is how it resolves: **detached** (#391).
// The default resolve answers with the follow-up turn's replies, and rendering
// those here would put the continuation into the channel once from the POST
// body and again from its SSE echo. Detach has exactly one delivery path, so
// the duplicate-bubble race #391 deliberately left open outside chat POSTs
// cannot exist here.

import { Check, Loader2, X } from "lucide-react";
import { useState } from "react";

import type { ApprovalSummary, GrantScope, Verdict } from "@/api/types";
import {
  ApprovalHeadline,
  ApprovalMeta,
  ApprovalPayload,
  ApprovalScopeControl,
} from "@/components/approval-card";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/** What the card says once a verdict has been witnessed. */
function settledLabel(verdict: Verdict): string {
  // Mirrors the wording the Approvals page files into the transcript, and for
  // the same reason: approving is not "done", it hands the agent a single-use
  // grant and re-dispatches it. A decline IS terminal.
  return verdict === "approve"
    ? "Approved — the agent is completing the action"
    : "Declined — recorded, and nothing will run";
}

export function ApprovalRow({
  approval,
  now,
  askerNames,
  deciding,
  decided,
  onDecide,
}: {
  approval: ApprovalSummary;
  now: number;
  askerNames: Map<string, string>;
  /** The verdict this card is waiting on, or `null` when idle. */
  deciding: Verdict | null;
  /** A verdict already witnessed — from this console or from the page. */
  decided: Verdict | null;
  onDecide: (verdict: Verdict, scope: GrantScope) => void;
}) {
  // Per-card, exactly as on the page: two approvals can be parked in one
  // channel and each carries its own decision. Defaults to `once`, so a card
  // decided without touching the control behaves as it did before #431 — the
  // scope is opt-in here too.
  const [scope, setScope] = useState<GrantScope>({ kind: "once" });

  return (
    <div className="px-4 py-2">
      <div
        role="group"
        aria-label="Approval request"
        data-approval-id={approval.id}
        className={cn(
          "rounded-xl border bg-card px-4 py-3 shadow-sm",
          // A settled card steps back rather than disappearing: the operator
          // has to be able to see their own decision land.
          decided && "opacity-70",
        )}
      >
        <div className="flex flex-col gap-3">
          <ApprovalHeadline
            approval={approval}
            actions={
              decided ? undefined : (
                <>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={deciding !== null}
                    /* A decline never carries a scope — there is nothing to
                       grant, and the host refuses the pairing anyway. */
                    onClick={() => onDecide("deny", { kind: "once" })}
                  >
                    {deciding === "deny" ? (
                      <Loader2 className="size-4 animate-spin" />
                    ) : (
                      <X className="size-4" />
                    )}{" "}
                    Decline
                  </Button>
                  <Button
                    size="sm"
                    disabled={deciding !== null}
                    onClick={() => onDecide("approve", scope)}
                  >
                    {deciding === "approve" ? (
                      <Loader2 className="size-4 animate-spin" />
                    ) : (
                      <Check className="size-4" />
                    )}{" "}
                    Approve
                  </Button>
                </>
              )
            }
          />

          <ApprovalPayload approval={approval} />

          {/*
           * The same control the page renders, from the same module — it
           * self-gates on `broadly_grantable`, so an approval that may not be
           * granted broadly shows nothing here for exactly the reason it shows
           * nothing there. A settled card drops it: there is no decision left
           * to scope.
           */}
          {!decided && (
            <ApprovalScopeControl
              approval={approval}
              askerNames={askerNames}
              scope={scope}
              onChange={setScope}
              disabled={deciding !== null}
            />
          )}

          <ApprovalMeta
            approval={approval}
            now={now}
            askerNames={askerNames}
            status={
              decided
                ? settledLabel(decided)
                : deciding
                  ? deciding === "approve"
                    ? "Waiting for the agent…"
                    : "Recording…"
                  : undefined
            }
          />
        </div>
      </div>
    </div>
  );
}
