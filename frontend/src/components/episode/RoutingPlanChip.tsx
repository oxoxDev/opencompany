/**
 * How a message was routed to seats — the plan an episode opened with, or
 * the plan a broadcast resolved to.
 *
 * One chip for both, because they are the same shape on the wire
 * (`RoutingPlanDto`) and mean the same thing: who the driver decided should
 * speak next. The router badge says who decided — the System One router,
 * the lead/mention fallback, or an explicit `@mention` — since a company
 * whose every plan reads `fallback` is one whose router is not reachable,
 * and that is worth seeing on the band rather than in a log.
 */

import { GitBranch, HelpCircle, LifeBuoy, Route, User } from "lucide-react";

import type { RoutingPlanDto, RoutingRouter } from "@/api/types";
import { cn } from "@/lib/utils";

interface Props {
  plan: RoutingPlanDto;
  /** Who chose the plan, when the host said. */
  router?: RoutingRouter;
  /** Display names by agent id; a missing name falls back to the id. */
  agentNames?: Readonly<Record<string, string>>;
  className?: string;
}

/** The chip's sentence, without the router — exported for the band's title. */
export function describePlan(
  plan: RoutingPlanDto,
  agentNames?: Readonly<Record<string, string>>,
): string {
  const name = (id: string) => agentNames?.[id] ?? id;
  switch (plan.kind) {
    case "one":
      return `→ ${name(plan.primaryId)}`;
    case "hive": {
      const invited = plan.invitedIds.filter((id) => id !== plan.primaryId).map(name);
      return invited.length ? `→ ${name(plan.primaryId)} + ${invited.join(", ")}` : `→ ${name(plan.primaryId)}`;
    }
    case "clarify":
      return plan.question ? `clarify: ${plan.question}` : "clarify";
    case "fallback":
      // The seat the host fell back to is the useful half; the reason is
      // why nothing better chose it.
      return plan.primaryId ? `→ ${name(plan.primaryId)} (fallback: ${plan.reason})` : `fallback: ${plan.reason}`;
  }
}

const ICON = {
  one: User,
  hive: GitBranch,
  clarify: HelpCircle,
  fallback: LifeBuoy,
} as const;

export function RoutingPlanChip({ plan, router, agentNames, className }: Props) {
  const Icon = ICON[plan.kind] ?? Route;
  return (
    <span
      data-testid="routing-plan-chip"
      data-plan-kind={plan.kind}
      data-router={router}
      title={router ? `routed by ${router}` : undefined}
      className={cn(
        "inline-flex max-w-full items-center gap-1 rounded-full border px-2 py-0.5 text-2xs font-medium text-muted-foreground",
        plan.kind === "fallback" && "border-dashed",
        className,
      )}
    >
      <Icon className="size-3 shrink-0" aria-hidden />
      <span className="truncate">{describePlan(plan, agentNames)}</span>
      {router && (
        <span className="rounded-sm bg-muted px-1 font-mono text-3xs uppercase tracking-wide">
          {router}
        </span>
      )}
    </span>
  );
}
