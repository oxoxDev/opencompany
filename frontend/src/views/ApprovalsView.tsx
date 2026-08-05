import { useEffect, useMemo, useState } from "react";
import {
  AtSign,
  Check,
  ChevronDown,
  ChevronUp,
  CreditCard,
  FileSignature,
  FileText,
  Globe,
  KeyRound,
  Loader2,
  Mail,
  MessageSquare,
  Repeat,
  RefreshCw,
  Rocket,
  ShieldCheck,
  SquareKanban,
  type LucideIcon,
  X,
} from "lucide-react";
import { toast } from "sonner";

import type { OpenCompanyClient } from "@/api/client";
import { ApiError, type ApprovalSummary, type Verdict } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import type { CompanyFeed } from "@/hooks/use-company";
import { approvalAction, approvalSummary, money, payloadLines, timeAgo } from "@/lib/language";
import { cn } from "@/lib/utils";

const KIND_ICONS: Record<string, LucideIcon> = {
  "payment.send": CreditCard,
  "subscription.start": Repeat,
  "email.send": Mail,
  "dm.external": MessageSquare,
  "filing.submit": FileText,
  "contract.accept": FileSignature,
  "external.publish": Globe,
  "website.deploy": Rocket,
  "handle.register": AtSign,
  "handle.renew": RefreshCw,
  "key.rotate": KeyRound,
};

/**
 * How much of a payload is shown before it is clamped. Past either bound the
 * block collapses behind a "Show everything" toggle — a queue of approvals has
 * to stay scannable, and a forty-line argument object buries the next card.
 */
const PREVIEW_LINES = 3;
const PREVIEW_VALUE_CHARS = 160;

interface Props {
  client: OpenCompanyClient;
  company: string | null;
  feed: CompanyFeed;
  onResolved: (systemLine: string) => void;
  onGoToConversation: () => void;
}

/** The approvals inbox: the few things the company parked for the operator. */
export function ApprovalsView({ client, company, feed, onResolved, onGoToConversation }: Props) {
  // Issue #373: in-flight state is per approval, not a single module-wide slot.
  //
  // Approving is not a quick write — the host mints a grant and re-dispatches
  // the agent, holding the POST open for a whole turn (#243) — so two decisions
  // being in flight at once is a legitimate state the operator can reach, and
  // one the host already handles by serialising them behind its per-company
  // lock. The old `string | null` could not represent it, which is why deciding
  // one card greyed out every other card on the screen until a hard reload.
  //
  // A map rather than a set of ids because the verdict has to survive the wait:
  // an approve and a decline are different promises to the operator ("the agent
  // is doing it" vs "recorded"), and the card says which one it is waiting on.
  const [inFlight, setInFlight] = useState<ReadonlyMap<string, Verdict>>(() => new Map());
  const { approvals, now } = feed;
  const askerNames = useAskerNames(client, company, approvals);

  const markInFlight = (id: string, verdict: Verdict | null) =>
    setInFlight((prev) => {
      const next = new Map(prev);
      if (verdict) next.set(id, verdict);
      else next.delete(id);
      return next;
    });

  async function decide(a: ApprovalSummary, verdict: Verdict) {
    // Per-row guard: only a double-press on THIS card is ignored. The global
    // early return that used to live here made every other card inert too.
    if (inFlight.has(a.id)) return;
    markInFlight(a.id, verdict);
    try {
      await client.resolveApproval(a.id, verdict, undefined, company);
      // Issue #243: approving no longer just records a verdict — it hands the
      // agent a single-use grant and re-dispatches it to make the call. The old
      // "Approved: …" read as "done", which was the exact lie that made the
      // missing re-dispatch invisible: the operator saw a success toast for work
      // that had silently dead-ended. Say what is actually happening instead.
      // Declining IS terminal, so its wording is unchanged.
      const line =
        verdict === "approve"
          ? `Approved — the agent is completing the action: ${approvalSummary(a)}`
          : `Declined: ${approvalSummary(a)}`;
      onResolved(line);
      toast.success(line);
      // The agent's reply arrives as a journaled `AgentReply` on its own thread,
      // so no extra plumbing is needed here — the existing feed refresh plus the
      // per-agent DM thread (#151) surface it.
      void feed.refresh();
    } catch (err) {
      const msg = err instanceof ApiError ? err.message : "something went wrong";
      onResolved(`Couldn't record your decision — ${msg}`);
      toast.error(`Couldn't record your decision — ${msg}`);
    } finally {
      // Unconditional, and keyed on the id rather than clearing a single slot:
      // the feed refreshes on its own schedule and routinely drops the decided
      // row while its request is still open, so the flag has to be removable
      // whether or not the row it belongs to still exists. Deleting a key that
      // is already gone is a no-op, which is the point.
      markInFlight(a.id, null);
    }
  }

  return (
    <div className="flex-1 overflow-y-auto">
      <div className="mx-auto w-full max-w-3xl px-4 py-6">
        {approvals.length === 0 ? (
          <EmptyApprovals onGoToConversation={onGoToConversation} />
        ) : (
          <>
            <div className="mb-4 flex items-baseline justify-between">
              <h2 className="text-sm font-medium text-muted-foreground">
                {approvals.length === 1
                  ? "1 thing needs your approval"
                  : `${approvals.length} things need your approval`}
              </h2>
            </div>
            <div className="flex flex-col gap-3">
              {approvals.map((a) => (
                <ApprovalCard
                  key={a.id}
                  approval={a}
                  now={now}
                  askerNames={askerNames}
                  deciding={inFlight.get(a.id) ?? null}
                  onDecide={(verdict) => void decide(a, verdict)}
                />
              ))}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

/**
 * One parked approval, told in full (#372).
 *
 * The card answers the four questions an operator needs before they can decide
 * — what will happen, who is asking, what it is for, and how long it has waited
 * — and it answers the first one *concretely*, with the tool call's own
 * arguments, because that is the thing being consented to. Before this it said
 * "Shell", which asks someone to authorise an action they cannot see.
 *
 * Laid out vertically rather than as one row: the payload block needs the full
 * width, and the stacked form leaves the slot #374's per-approval scope control
 * will occupy.
 *
 * **An old host degrades to the pre-#372 card by construction.** It omits
 * `payload` and `agent` from the wire, so the payload block and the "Asked by"
 * line simply do not render and what is left is the headline, the amount and
 * the relative time — exactly what shipped before.
 */
function ApprovalCard({
  approval: a,
  now,
  askerNames,
  deciding,
  onDecide,
}: {
  approval: ApprovalSummary;
  now: number;
  askerNames: Map<string, string>;
  /** The verdict this card is waiting on, or `null` when it is idle (#373). */
  deciding: Verdict | null;
  onDecide: (verdict: Verdict) => void;
}) {
  const Icon = KIND_ICONS[a.kind] ?? ShieldCheck;
  const lines = useMemo(() => payloadLines(a), [a]);
  const taskId = a.task?.link === "task" ? a.task.id : null;
  // An id the roster does not know still beats no attribution at all — the
  // operator can at least tell two askers apart.
  const asker = a.agent ? (askerNames.get(a.agent) ?? a.agent) : null;

  // No cross-card dimming: another card being decided is not this card's
  // business, and treating it as such is the visual half of the #373 bug.
  return (
    <Card>
      <CardContent className="flex flex-col gap-3 py-4">
        <div className="flex items-start gap-4">
          <div className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-muted text-foreground">
            <Icon className="size-5" />
          </div>
          <div className="min-w-0 flex-1">
            <p className="font-medium">{approvalAction(a)}</p>
            {a.amount_usd != null && (
              <p className="text-xs font-medium text-muted-foreground">{money(a.amount_usd)}</p>
            )}
          </div>
          {/* Disabled on THIS card's own state only — a decision in flight on
              another card leaves these live. That is the whole of #373's
              first cause. */}
          <div className="flex shrink-0 gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={deciding !== null}
              onClick={() => onDecide("deny")}
            >
              {deciding === "deny" ? (
                <Loader2 className="size-4 animate-spin" />
              ) : (
                <X className="size-4" />
              )}{" "}
              Decline
            </Button>
            <Button size="sm" disabled={deciding !== null} onClick={() => onDecide("approve")}>
              {deciding === "approve" ? (
                <Loader2 className="size-4 animate-spin" />
              ) : (
                <Check className="size-4" />
              )}{" "}
              Approve
            </Button>
          </div>
        </div>

        {lines.length > 0 && <PayloadBlock lines={lines} />}

        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
          {asker && (
            <>
              <span>
                Asked by <span className="font-medium text-foreground">{asker}</span>
              </span>
              <span aria-hidden>·</span>
            </>
          )}
          {taskId && (
            <>
              <a
                href={`#/tasks/${encodeURIComponent(taskId)}`}
                className="flex w-fit items-center gap-1 rounded-full bg-accent px-2 py-0.5 font-medium text-accent-foreground transition-opacity hover:opacity-80"
              >
                <SquareKanban className="size-3 shrink-0" />
                Open the card
              </a>
              <span aria-hidden>·</span>
            </>
          )}
          <span>{timeAgo(a.at_millis, now)}</span>
          {/* Honest copy for a request that spans an agent turn (#373): an
              approve is not done when the button stops spinning, it is handed
              to the agent. A decline IS terminal, so it only has to record. */}
          {deciding && (
            <>
              <span aria-hidden>·</span>
              <span className="text-foreground">
                {deciding === "approve" ? "Waiting for the agent…" : "Recording…"}
              </span>
            </>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

/**
 * The tool call's own arguments, verbatim (#372).
 *
 * Monospace and wrapping rather than truncating: a shell command cut off
 * mid-flag is exactly as un-decidable as no command at all, and `break-all` is
 * what keeps a long unbroken path or URL inside the card. Everything here was
 * redacted and bounded by the host, so `[redacted]` is a value the console
 * renders — never one it computes.
 */
function PayloadBlock({ lines }: { lines: { label: string; value: string }[] }) {
  const [expanded, setExpanded] = useState(false);
  const clampable =
    lines.length > PREVIEW_LINES || lines.some((l) => l.value.length > PREVIEW_VALUE_CHARS);
  const shown = expanded || !clampable ? lines : lines.slice(0, PREVIEW_LINES);

  return (
    <div className="rounded-lg border bg-muted/40 px-3 py-2">
      <div
        className={cn(
          "space-y-1 font-mono text-xs break-all whitespace-pre-wrap",
          clampable && !expanded && "max-h-24 overflow-hidden",
        )}
      >
        {shown.map((line) => (
          <div key={line.label}>
            <span className="text-muted-foreground">{line.label}: </span>
            <span className="text-foreground">{line.value}</span>
          </div>
        ))}
      </div>
      {clampable && (
        <button
          type="button"
          onClick={() => setExpanded((v) => !v)}
          className="mt-1.5 flex items-center gap-1 text-xs text-muted-foreground transition-colors hover:text-foreground"
        >
          {expanded ? <ChevronUp className="size-3" /> : <ChevronDown className="size-3" />}
          {expanded ? "Show less" : "Show everything"}
        </button>
      )}
    </div>
  );
}

/**
 * Agent id → display name, for the "Asked by" line.
 *
 * One roster read per company, not one per card: the ids on the queue are
 * roster ids, and the roster is small and stable. A host without the roster
 * route 404s, which is caught here — the card then shows the raw id rather than
 * dropping the attribution, because "which teammate asked" stays useful even
 * when we cannot pretty-print it.
 */
function useAskerNames(
  client: OpenCompanyClient,
  company: string | null,
  approvals: ApprovalSummary[],
): Map<string, string> {
  const [names, setNames] = useState<Map<string, string>>(new Map());
  // Keyed on the set of asker ids rather than on `approvals` itself: the feed
  // hands us a fresh array on every poll, and depending on the array would
  // refetch the roster every few seconds for a roster that rarely changes.
  const askerKey = useMemo(
    () =>
      Array.from(new Set(approvals.map((a) => a.agent).filter((id): id is string => !!id)))
        .sort()
        .join(","),
    [approvals],
  );

  useEffect(() => {
    if (!askerKey) return;
    let live = true;
    void (async () => {
      const roster = await client.listTeam(company).catch(() => []);
      if (!live) return;
      setNames(new Map(roster.map((m) => [m.id, m.name?.trim() || m.role])));
    })();
    return () => {
      live = false;
    };
  }, [client, company, askerKey]);

  return names;
}

function EmptyApprovals({ onGoToConversation }: { onGoToConversation: () => void }) {
  return (
    <div className="mt-16 flex flex-col items-center gap-3 text-center">
      <div className="flex size-12 items-center justify-center rounded-2xl bg-emerald-500/10 text-emerald-600 dark:text-emerald-400">
        <ShieldCheck className="size-6" />
      </div>
      <div className="space-y-1">
        <p className="font-medium">All clear</p>
        <p className="max-w-sm text-sm text-muted-foreground">
          Nothing is waiting on you. Your company will park anything that needs a sign-off here.
        </p>
      </div>
      <Button variant="outline" size="sm" onClick={onGoToConversation}>
        Back to the conversation
      </Button>
    </div>
  );
}
