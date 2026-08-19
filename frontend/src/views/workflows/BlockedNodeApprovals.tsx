// The gated tool names and per-approval links a blocked run leaves behind
// (issues #881 / #1002, render half of #1014).
//
// The run drawer already NAMES the nodes a run blocked on (#881) and COUNTS the
// cards it parked (#880), but said nothing about *which* tools were gated and
// gave no way to reach the cards from here — the operator read "decide it in
// Approvals" with no link. This renders both, from data already on the wire:
// `WorkflowBlockedNode.tools` (the gated tool names — names only, never
// arguments) and `WorkflowBlockedNode.approvalIds` (the cards those calls
// opened).
//
// The link target is the whole Approvals queue (`#/approvals`), not a
// per-approval route: an approval id is deliberately never put in a URL — the
// console joins a run to its cards by `workflow_run_id` equality, not through a
// route (see `ApprovalSummary.workflow_run_id`), and the only id the queue
// route narrows on is a *board task* id (`#/approvals/<taskId>`, #883), which a
// blocked node does not carry. So each parked call links to the same queue the
// Approvals page and the sidebar badge already read — exactly the href that
// page's own "back to the whole queue" control uses (`ApprovalsView`).

import type {
  WorkflowBlockedNode,
  WorkflowRunApprovalRow,
} from "@/api/workflows";

/** The canonical Approvals queue route — the whole queue, no narrowing. */
const APPROVALS_ROUTE = "#/approvals";

/** approvalId → the tool whose gated call opened it, from the run's receipt (#880). */
function toolByApprovalId(
  rows: readonly WorkflowRunApprovalRow[] | undefined,
): Map<string, string> {
  const byId = new Map<string, string>();
  for (const row of rows ?? []) {
    if (row.approvalId && row.tool) byId.set(row.approvalId, row.tool);
  }
  return byId;
}

/**
 * Under a blocked run's sentence: one line per blocked node, naming the tools it
 * gated and linking each parked card to the Approvals queue.
 *
 * Renders nothing when no blocked node carries tools or approval ids — a host
 * predating #881 sends neither, and the sentence above already stands on its
 * own. Each `approvalId` becomes its own link, labelled with the tool that
 * opened it when the run's receipt names one and with a neutral fallback when it
 * does not (an old host that sent approval ids but no per-call rows).
 */
export function BlockedNodeApprovals({
  blockedNodes,
  approvalRows,
}: {
  blockedNodes: readonly WorkflowBlockedNode[];
  /** The run's per-call receipt (#880), used only to LABEL each link with its tool. */
  approvalRows?: readonly WorkflowRunApprovalRow[];
}) {
  const shown = blockedNodes.filter(
    (b) => b.tools.length > 0 || (b.approvalIds?.length ?? 0) > 0,
  );
  if (shown.length === 0) return null;
  const toolFor = toolByApprovalId(approvalRows);
  return (
    <ul
      className="mt-1 space-y-0.5 text-2xs text-[var(--status-blocked-text)]"
      data-testid="workflow-blocked-node-tools"
    >
      {shown.map((b) => {
        const approvalIds = b.approvalIds ?? [];
        const stranded = b.stranded ?? 0;
        return (
          <li key={b.nodeId}>
            <span className="font-medium">“{b.nodeId}”</span>
            {b.tools.length > 0 && <> gated {b.tools.join(", ")}</>}
            {approvalIds.length > 0 && stranded >= approvalIds.length ? (
              // Every card this node opened is gone from the queue (#1143), so
              // there is nothing to link to. Sending the operator to an empty
              // Approvals page and calling it an action is the defect itself —
              // the honest line says the run cannot be continued and why.
              <span data-testid="workflow-blocked-approval-stranded">
                {" — "}
                {approvalIds.length === 1
                  ? "its approval is no longer in the queue"
                  : "none of its approvals are in the queue any more"}
                , so this run cannot be continued. Re-run the workflow.
              </span>
            ) : (
              approvalIds.length > 0 && (
                <>
                  {" — decide in Approvals: "}
                  {approvalIds.map((id, i) => (
                    <span key={id}>
                      {i > 0 && ", "}
                      <a
                        href={APPROVALS_ROUTE}
                        className="underline underline-offset-2 hover:text-foreground"
                        data-testid="workflow-blocked-approval-link"
                      >
                        {toolFor.get(id) ?? "this call"}
                      </a>
                    </span>
                  ))}
                  {stranded > 0 && (
                    // Partly stranded: the surviving cards are still worth
                    // linking, but the count would otherwise overstate what the
                    // operator can actually act on.
                    <span data-testid="workflow-blocked-approval-partly-stranded">
                      {` (${stranded} more no longer in the queue)`}
                    </span>
                  )}
                </>
              )
            )}
          </li>
        );
      })}
    </ul>
  );
}
