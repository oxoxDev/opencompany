// The gated calls one turn parked, raised inside the conversation that produced
// them (#379, consolidated by #842).
//
// The same content the Approvals page shows — headline, payload, asker, waiting
// time, and the grant-scope choice (#431), all from `@/components/approval-card`
// — laid out as a channel row so it reads as part of the thread rather than as a
// panel bolted beside it. Sharing the scope control rather than restating it is
// what keeps the two surfaces from offering different things for one approval.
//
// ## One card, however many calls (#842)
//
// A research turn that reaches three sites parks three approvals, and asking
// three times is the same fact told badly: it is one piece of work, and each
// interruption costs a re-dispatch cycle (#561) that can dead-end. So a batch
// renders as **one card with one Approve**, listing the hosts it covers.
//
// **All-or-nothing, deliberately.** Approve grants every call in the batch;
// Decline grants none. The operator is mid-conversation and wants one decision,
// not a form. Granularity is not missing — it lives on the Approvals page,
// which itemises the same parks one row at a time and is where an operator goes
// when they want precision or are cleaning up after the fact. Offering
// per-item control in both places would be redundant, and would double the
// state that has to stay in step between two surfaces.
//
// **What is not batched is the deciding.** One Approve resolves each item on
// its own id, so each approved call still mints its own host-scoped grant
// (#739) — three fetches produce three independently revocable standing
// permissions, exactly as they do today. Nothing about how grants are minted,
// stored or revoked changes here; only the asking is consolidated. There is no
// batch decision on the wire and no batch record on the host.
//
// A single-item batch renders exactly as this card did before #842: no list,
// no counts. The consolidation has to earn its extra furniture.
//
// The one thing it does differently from the page is how it resolves:
// **detached** (#391). The default resolve answers with the follow-up turn's
// replies, and rendering those here would put the continuation into the channel
// once from the POST body and again from its SSE echo. Detach has exactly one
// delivery path, so the duplicate-bubble race #391 deliberately left open
// outside chat POSTs cannot exist here.

import { Check, Loader2, ShieldCheck, TriangleAlert, X } from "lucide-react";
import { useMemo, useState } from "react";

import type { ApprovalSummary, GrantScope, Verdict } from "@/api/types";
import {
  ApprovalHeadline,
  ApprovalMeta,
  ApprovalPayload,
  ApprovalScopeControl,
  approvalIcon,
} from "@/components/approval-card";
import { Button } from "@/components/ui/button";
import { approvedLine } from "@/lib/approval-wording";
import { approvalAction, payloadLines } from "@/lib/language";
import { cn } from "@/lib/utils";

/** One count written with the noun it qualifies. */
function actionCount(count: number): string {
  return `${count} action${count === 1 ? "" : "s"}`;
}

/**
 * The one permanent receipt a fully settled turn leaves in the transcript
 * (#970).
 *
 * A batch's individual decisions are clicks; the turn only resumes once, when
 * the last one lands. By the time every item is in `decided`, that final
 * decision has happened, so this is the only point in chat that may honestly
 * say the agent is picking the work up. The toast and Approvals page remain
 * per-click feedback — this is deliberately the shared-channel summary.
 */
function settledReceipt(approvals: ApprovalSummary[], decided: Record<string, Verdict>): string {
  const approved = approvals.filter((a) => decided[a.id] === "approve").length;
  const declined = approvals.length - approved;

  // Preserve #561's established singular wording exactly. A one-action turn
  // earns no extra count or disclosure furniture. `undefined`, not `0`: this
  // card only knows its own item settled, not that the turn's stillAwaiting
  // count is zero — a sibling approval elsewhere in the same turn could still
  // be open, and claiming "picking it up now" here would be a guess.
  if (approvals.length === 1) {
    return approved === 1 ? approvedLine(undefined) : "Declined — recorded, and nothing will run";
  }
  if (approved === approvals.length) {
    return `Approved ${actionCount(approved)} — the teammate is picking it up now`;
  }
  if (declined === approvals.length) {
    return `Declined ${actionCount(declined)} — the teammate will not take them`;
  }
  return `Approved ${actionCount(approved)} and declined ${actionCount(declined)} — the teammate is picking it up now`;
}

/**
 * What the batch says while some of it is still undecided (#842).
 *
 * The sentence the two surfaces would otherwise drift apart on. An operator can
 * approve one row on the Approvals page while this card is on screen, and a
 * card that went on claiming three things were pending would be showing them a
 * queue that no longer exists. `decided` arrives from the shell's witnessed
 * map, which is fed by the `approval_resolved` stream frame, so this settles
 * without a reload wherever the decision was actually made.
 */
function partialLabel(settled: number, total: number): string {
  return `${settled} of ${total} decided — ${total - settled} still waiting on you`;
}

/**
 * What the card says when a decision did not land (#842 review).
 *
 * The failure consolidation makes worse, said out loud. Deciding three cards
 * separately, a failure belongs to the one card just clicked. Deciding one card
 * covering three, a failure on the third leaves two effects authorised and one
 * not — and a toast is both the wrong home for that (it does not say *which*)
 * and a temporary one. So the count is stated on the card, the failed rows name
 * themselves, and the buttons stay live because a retry is the way out.
 *
 * Never "nothing was recorded" unless that is true: on a batch, saying so about
 * a click that authorised two of three would be a fresh lie in place of the
 * silence it replaces.
 */
function failureLabel(failedCount: number, total: number): string {
  if (total === 1) return "Not recorded — try again";
  return failedCount === total
    ? `None of the ${total} were recorded — try again`
    : `${failedCount} of ${total} weren't recorded — try again`;
}

/**
 * One item's line in a batch: what this particular call will do.
 *
 * The **first payload line**, which is the tool's leading argument — the URL for
 * a fetch, the command for a shell call — because `PAYLOAD_KEY_ORDER` already
 * promotes the argument that is the thing being consented to. Falls back to the
 * action's own words when the host sent no payload (an old host) or withheld it
 * (#618), so a row never renders blank and never invents a value it does not
 * have.
 */
function itemLabel(a: ApprovalSummary): string {
  if (a.contents_hidden) return `${approvalAction(a)} — details hidden by your role`;
  return payloadLines(a)[0]?.value ?? approvalAction(a);
}

export function ApprovalRow({
  approvals,
  now,
  askerNames,
  deciding,
  decided,
  failed,
  onDecide,
}: {
  /** The gated calls this turn parked. Never empty — see `TimelineItem`. */
  approvals: ApprovalSummary[];
  now: number;
  askerNames: Map<string, string>;
  /** The verdict an item is waiting on, keyed by approval id; empty when idle. */
  deciding: ReadonlyMap<string, Verdict>;
  /** Verdicts already witnessed — from this console or from the page. */
  decided: Record<string, Verdict>;
  /**
   * Decisions that did not land, keyed by approval id — the message to show.
   *
   * Separate from {@link decided} because a failed decision is neither: the
   * item is not settled, and it is not simply still pending either. One click
   * covering three calls can leave two authorised and one not, and an item that
   * dropped back to its pending look would read as "still working" rather than
   * "this one did not take" — the operator would believe they got all three.
   */
  failed: Record<string, string>;
  onDecide: (approval: ApprovalSummary, verdict: Verdict, scope: GrantScope) => void;
}) {
  // Per-card, exactly as on the page: two batches can be parked in one channel
  // and each carries its own decision. Defaults to `once`, so a card decided
  // without touching the control behaves as it did before #431 — the scope is
  // opt-in here too.
  const [scope, setScope] = useState<GrantScope>({ kind: "once" });

  const lead = approvals[0];
  const pending = useMemo(() => approvals.filter((a) => !decided[a.id]), [approvals, decided]);
  const settledCount = approvals.length - pending.length;
  const failedCount = pending.filter((a) => failed[a.id]).length;
  const busy = deciding.size > 0;
  // Everything decided: the card has nothing left to ask and steps back.
  const done = pending.length === 0;
  /** Whether any item in this card is waiting on `verdict` right now. */
  const awaiting = (verdict: Verdict) => [...deciding.values()].includes(verdict);

  /**
   * The decision, applied to every item the card is still asking about.
   *
   * **Every** item, and that is what makes the card honest: the turn is blocked
   * until each call it parked has an answer (#469), so a decision that left one
   * undecided would hold the turn open while looking like it had resolved the
   * card. One click answers all of them, and the runtime continues the turn
   * once, when the last of them lands.
   *
   * `pending` and not `approvals`: an item already decided on the Approvals
   * page has been dropped by the host, and resolving it again would be a second
   * decision on an approval that no longer exists.
   *
   * A decline carries no scope — there is nothing to grant, and the host
   * refuses the pairing anyway.
   */
  const decideAll = (verdict: Verdict) => {
    for (const a of pending) {
      onDecide(a, verdict, verdict === "approve" ? scope : { kind: "once" });
    }
  };

  const actions = done ? undefined : (
    <>
      <Button variant="outline" size="sm" disabled={busy} onClick={() => decideAll("deny")}>
        {awaiting("deny") ? (
          <Loader2 className="size-4 animate-spin" />
        ) : (
          <X className="size-4" />
        )}{" "}
        Decline
      </Button>
      <Button size="sm" disabled={busy} onClick={() => decideAll("approve")}>
        {awaiting("approve") ? (
          <Loader2 className="size-4 animate-spin" />
        ) : (
          <Check className="size-4" />
        )}{" "}
        Approve
      </Button>
    </>
  );

  // Keep a completed turn visible, but compact it into one receipt instead of
  // leaving a full approval form permanently embedded in every shared channel.
  // The disclosure preserves the individual verdicts without making them the
  // transcript's primary story.
  if (done) {
    return (
      <SettledApprovalReceipt approvals={approvals} decided={decided} />
    );
  }

  return (
    <div className="px-4 py-2">
      <div
        role="group"
        aria-label={approvals.length > 1 ? "Approval request for several actions" : "Approval request"}
        data-approval-id={lead.id}
        data-approval-count={approvals.length}
        className="rounded-xl border bg-card px-4 py-3 shadow-sm"
      >
        <div className="flex flex-col gap-3">
          {approvals.length > 1 ? (
            <BatchHeadline
              approvals={approvals}
              askerNames={askerNames}
              actions={actions}
            />
          ) : (
            <ApprovalHeadline approval={lead} actions={actions} />
          )}

          {approvals.length > 1 ? (
            // What the one decision covers, spelled out. Read-only: the card is
            // all-or-nothing, so a control here would offer a choice the
            // buttons below do not honour. An item settled elsewhere still says
            // so, which is how the card stops claiming three things are pending
            // when one has been decided on the page.
            <ul className="flex flex-col gap-1.5">
              {approvals.map((a) => (
                <BatchItem
                  key={a.id}
                  approval={a}
                  verdict={decided[a.id] ?? null}
                  deciding={deciding.get(a.id) ?? null}
                  failure={failed[a.id] ?? null}
                />
              ))}
            </ul>
          ) : (
            <ApprovalPayload approval={lead} />
          )}

          {/*
           * The same control the page renders, from the same module — it
           * self-gates on `broadly_grantable`, so an approval that may not be
           * granted broadly shows nothing here for exactly the reason it shows
           * nothing there. A settled card drops it: there is no decision left
           * to scope.
           *
           * Rendered against the first still-undecided item, and it means the
           * same thing for every item it covers: each approved call mints its
           * own grant, scoped to its own arguments (#739). One choice, one
           * grant per item — never one grant spanning them.
           */}
          {!done && (
            <ApprovalScopeControl
              approval={pending[0]}
              askerNames={askerNames}
              scope={scope}
              onChange={setScope}
              disabled={busy}
            />
          )}

          <ApprovalMeta
            approval={lead}
            now={now}
            askerNames={askerNames}
            status={
              busy
                  ? awaiting("approve")
                    ? "Waiting for the teammate…"
                    : "Recording…"
                  : // A failure outranks the partial count, because it is the
                    // one thing here the operator has to act on. It also has to
                    // reach a single-item card, which renders no item list to
                    // carry the per-row form.
                    failedCount > 0
                    ? failureLabel(failedCount, approvals.length)
                    : settledCount > 0
                      ? partialLabel(settledCount, approvals.length)
                      : undefined
            }
          />
        </div>
      </div>
    </div>
  );
}

/** The compact, inspectable receipt a settled turn leaves in chat (#970). */
function SettledApprovalReceipt({
  approvals,
  decided,
}: {
  approvals: ApprovalSummary[];
  decided: Record<string, Verdict>;
}) {
  const lead = approvals[0];
  const multiple = approvals.length > 1;

  return (
    <div className="px-4 py-2">
      <section
        aria-label={multiple ? "Approval receipt for several actions" : "Approval receipt"}
        data-approval-receipt="true"
        data-approval-id={lead.id}
        data-approval-count={approvals.length}
        className="rounded-xl border bg-card px-4 py-3 text-sm shadow-sm"
      >
        <p className="font-medium">{settledReceipt(approvals, decided)}</p>
        {multiple && (
          <details className="mt-2">
            <summary className="cursor-pointer text-xs text-muted-foreground">
              Show individual decisions
            </summary>
            <ul className="mt-2 flex flex-col gap-1.5">
              {approvals.map((approval) => (
                <BatchItem
                  key={approval.id}
                  approval={approval}
                  verdict={decided[approval.id] ?? null}
                  deciding={null}
                  failure={null}
                />
              ))}
            </ul>
          </details>
        )}
      </section>
    </div>
  );
}

/**
 * The batch's headline: what the turn is asking for, and how many of them.
 *
 * Named by the shared action when every call is the same tool — the reported
 * case, three fetches from one research turn — and by a neutral count when they
 * are not, because "Fetch web pages" over a batch that also sends mail would be
 * a card that understates what approving it does.
 */
function BatchHeadline({
  approvals,
  askerNames,
  actions,
}: {
  approvals: ApprovalSummary[];
  askerNames: Map<string, string>;
  actions?: React.ReactNode;
}) {
  const lead = approvals[0];
  const sameKind = approvals.every((a) => a.kind === lead.kind);
  // A mixed batch has no one glyph that is true of it, so it wears the neutral
  // one rather than the first item's — an envelope over a card that also
  // deploys a website would be the icon quietly making a claim.
  const Icon = sameKind ? approvalIcon(lead.kind) : ShieldCheck;
  const asker = lead.agent ? (askerNames.get(lead.agent) ?? lead.agent) : null;
  const title = sameKind ? approvalAction(lead) : `${approvals.length} actions need your sign-off`;

  return (
    <div className="flex items-start gap-4">
      <div className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-muted text-foreground">
        <Icon className="size-5" />
      </div>
      <div className="min-w-0 flex-1">
        <p className="font-medium">{title}</p>
        <p className="text-xs text-muted-foreground">
          {asker ? `${asker} needs` : "This turn needs"} {approvals.length} sign-offs before it
          can carry on
        </p>
      </div>
      {actions && <div className="flex shrink-0 gap-2">{actions}</div>}
    </div>
  );
}

/**
 * One call inside a batch: what it will do, and whether it is still waiting.
 *
 * Read-only. The card decides all-or-nothing, so a control on a line would
 * offer a choice the buttons do not honour — and the granular path already
 * exists on the Approvals page, which lists these same parks one row at a time.
 *
 * An item already decided — here, or on the page while this card was open —
 * states its verdict instead. That is the half of #842 that keeps the two
 * surfaces honest: a card cannot keep listing as pending something that has
 * already been answered somewhere else.
 */
function BatchItem({
  approval: a,
  verdict,
  deciding,
  failure,
}: {
  approval: ApprovalSummary;
  /** A verdict already witnessed for this item, or `null` while it is pending. */
  verdict: Verdict | null;
  /** The verdict this item is waiting on, or `null` when idle. */
  deciding: Verdict | null;
  /** Why this item's decision did not land, or `null` when none has failed. */
  failure: string | null;
}) {
  const label = itemLabel(a);

  // A failed decision outranks the pending look, and says which item and why.
  // Silence here is the failure mode worth designing against: the operator
  // clicked once for three calls, two were authorised, and a third that merely
  // looks unstarted reads as "still working". Stated plainly, with the way back
  // — the card's own buttons are still live, so a retry is one press.
  //
  // **A settled verdict outranks it in turn**, which is why `verdict` is tested
  // first. A failure describes one *attempt*; a verdict describes the approval.
  // An item that failed here and was then resolved on the Approvals page or in
  // another tab has both, and showing "not recorded" over an approval the host
  // has already acted on would be the card contradicting the queue — the exact
  // drift this work exists to remove. The shell also clears the failure when
  // that frame arrives; this ordering is what makes the render correct
  // regardless of which state reaches it first.
  if (failure && !deciding && !verdict) {
    return (
      <li
        data-approval-item={a.id}
        data-approval-failed="true"
        className="flex items-center gap-2 text-sm text-status-blocked-text"
      >
        <TriangleAlert className="size-3.5 shrink-0" aria-hidden />
        <span className="min-w-0 flex-1 truncate font-mono text-xs">{label}</span>
        <span className="shrink-0 text-xs">Not recorded — {failure}</span>
      </li>
    );
  }

  return (
    // Addressable per item, because the card is no longer one approval. The
    // group keeps `data-approval-id` for the first — an existing selector still
    // finds the card the request was raised in — and this is how a caller
    // reaches any of the others.
    <li
      data-approval-item={a.id}
      className="flex items-center gap-2 text-sm text-muted-foreground"
    >
      {verdict === "approve" ? (
        <Check className="size-3.5 shrink-0" />
      ) : verdict === "deny" ? (
        <X className="size-3.5 shrink-0" />
      ) : (
        <span aria-hidden className="shrink-0 text-xs">
          ·
        </span>
      )}
      <span
        className={cn(
          "min-w-0 flex-1 truncate font-mono text-xs",
          verdict ? "line-through" : "text-foreground",
        )}
      >
        {label}
      </span>
      {deciding ? (
        <Loader2 className="size-3.5 shrink-0 animate-spin" />
      ) : (
        verdict && (
          <span className="shrink-0 text-xs">
            {verdict === "approve" ? "Approved" : "Declined"}
          </span>
        )
      )}
    </li>
  );
}
