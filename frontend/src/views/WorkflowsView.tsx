import { type ReactNode, useCallback, useEffect, useMemo, useState } from "react";
import {
  Background,
  BackgroundVariant,
  Controls,
  type Edge,
  MiniMap,
  type Node,
  ReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useTheme } from "next-themes";
import { History, Loader2, Pencil, Play, Plus, RotateCw, Trash2 } from "lucide-react";
import { toast } from "sonner";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import {
  deleteWorkflow,
  getWorkflow,
  listWorkflowRuns,
  listWorkflows,
  runWorkflow,
  type DeliveryReport,
  type DeliveryStatus,
  type WorkflowGraph,
  type WorkflowNode as WorkflowNodeModel,
  type WorkflowRunNode,
  type WorkflowRunOutcome,
  type WorkflowRunResult,
  type WorkflowSummary,
} from "@/api/workflows";
import type { CompanyStreamEvent } from "@/hooks/use-events";
import type { OpenCompanyClient } from "@/api/client";
import { ApiError } from "@/api/types";
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
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { WorkflowNode } from "@/components/workflow-node";
import { WorkflowCreateDialog } from "@/views/WorkflowCreateDialog";
import {
  nodeKindMeta,
  type NodeRunState,
  type WorkflowNodeData,
} from "@/lib/workflow-sample";

const NODE_TYPES = { oc: WorkflowNode };

/** Horizontal gap between layers and vertical gap between nodes in a layer. */
const COL_GAP = 300;
const ROW_GAP = 150;

/**
 * The live Workflows canvas. Reads the company's saved graphs from the host's
 * `…/workflows` routes, lets the operator pick one, renders its real nodes and
 * edges (auto-laid-out left→right by longest-path depth, since saved graphs
 * carry no coordinates), and runs it via `…/workflows/{wid}/run` — surfacing the
 * engine's final output and any nodes left pending approval.
 */
export function WorkflowsView({
  client,
  company,
  runEventTick = 0,
  runEvent = null,
}: {
  client: OpenCompanyClient;
  company: string | null;
  /**
   * Bumped by the shell on every `workflow_run_finished` SSE event (issue
   * #228), so a run that finishes while this tab is open shows up without a
   * reload. Matters most for a scheduled run — nothing else would tell this
   * view a cron fired.
   */
  runEventTick?: number;
  /**
   * The latest workflow run frame off the SSE stream (issue #371) — a run
   * starting, a node finishing, or a run settling.
   *
   * Unlike `runEventTick` this carries the payload, because the canvas paints
   * per-node state and a counter cannot say which node of which run just
   * finished. The two coexist: the tick still drives the history refetch.
   */
  runEvent?: CompanyStreamEvent | null;
}) {
  const { resolvedTheme } = useTheme();
  const [workflows, setWorkflows] = useState<WorkflowSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [graph, setGraph] = useState<WorkflowGraph | null>(null);
  const [loadingList, setLoadingList] = useState(true);
  const [loadingGraph, setLoadingGraph] = useState(false);
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<WorkflowRunResult | null>(null);
  // Issue #154: what the operator is asking this run to work on. `ranWith` is
  // pinned when the run is dispatched so the result panel echoes the request the
  // shown output came from, not whatever has been typed since.
  const [request, setRequest] = useState("");
  const [ranWith, setRanWith] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  // Issue #259: the same dialog, hydrated from the selected graph. Separate
  // state from `createOpen` rather than a mode flag, so the create path keeps
  // working exactly as it did and neither can be half-open.
  const [editOpen, setEditOpen] = useState(false);
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  // Issue #228: what past runs did, read back from the host's journal. This is
  // the half that survives a reload — before it, a manual run's delivery rows
  // vanished when the drawer was dismissed and a scheduled run's never reached
  // the operator at all.
  const [runs, setRuns] = useState<WorkflowRunOutcome[]>([]);
  const [historyOpen, setHistoryOpen] = useState(false);
  // A host predating the runs route answers 404. That is not an error worth
  // showing — the rest of the view works — so it just means "no history here".
  const [historySupported, setHistorySupported] = useState(true);
  // Bumped after a manual run so the history picks it up without a reload.
  const [runsTick, setRunsTick] = useState(0);
  // Issue #259: a delete in flight, and the host's message when a write was
  // refused because the graph moved under us.
  const [deleting, setDeleting] = useState(false);
  // The confirm dialog is CONTROLLED on purpose. `AlertDialogAction` is a plain
  // `Button`, not an `AlertDialogPrimitive.Close` (only `AlertDialogCancel` is),
  // so confirming does not dismiss the dialog on its own — it stays up with its
  // backdrop swallowing every pointer event, over a view whose workflow has just
  // been deleted. Closing it explicitly in the confirm handler is the fix.
  const [confirmOpen, setConfirmOpen] = useState(false);
  // A version conflict is deliberately NOT a toast. It means the operator is
  // looking at a stale graph, so it has to persist next to the canvas with a way
  // out (Reload) — a toast that auto-dismisses would leave them staring at the
  // old graph believing the write landed.
  const [conflict, setConflict] = useState<string | null>(null);
  // Bumped by the conflict banner's Reload, to re-fetch the selected graph (and
  // with it a fresh `version`) without changing the selection.
  const [graphTick, setGraphTick] = useState(0);
  // Issue #371: what the run currently on screen has done, per node.
  //
  // `liveRunId` is the run these states belong to. It exists because a company
  // can have two runs in flight — an operator pressing Run while a cron fire is
  // mid-graph — and both stream onto the same SSE connection. Without it, the
  // cron run's node frames would repaint the canvas the operator is watching.
  const [liveRunId, setLiveRunId] = useState<string | null>(null);
  const [nodeRunState, setNodeRunState] = useState<Record<string, NodeRunState>>({});
  const [nodeElapsed, setNodeElapsed] = useState<Record<string, number>>({});
  // A past run selected from the history panel, overlaid on the canvas. When
  // set it WINS over the live state — the operator asked to look at that run.
  // This is what makes a scheduled run's failure point visible after the fact,
  // which is the half of the issue a live canvas alone cannot answer.
  const [overlayRun, setOverlayRun] = useState<WorkflowRunOutcome | null>(null);
  // The run this view just POSTed, held until its history row arrives.
  //
  // The fallback for a console with no live stream: if `/events` 404s or the
  // connection dropped, no progress frame ever lands and the canvas would stay
  // blank for a run the operator watched happen. Overlaying the row the host
  // journaled gets them the same per-node answer, just at the end instead of
  // during. Cleared as soon as it is used, or when live frames made it moot.
  const [awaitingRunId, setAwaitingRunId] = useState<string | null>(null);

  // Load the workflow list once, and auto-select the first entry.
  useEffect(() => {
    let live = true;
    (async () => {
      try {
        const rows = await listWorkflows(client, company);
        if (!live) return;
        setWorkflows(rows);
        // Keep the selection only if it still exists in the freshly loaded list
        // (this effect also reruns on company change) — otherwise a stale id
        // from the previous company would fetch the wrong/nonexistent graph.
        setSelectedId((prev) =>
          prev && rows.some((r) => r.id === prev) ? prev : (rows[0]?.id ?? null),
        );
        setError(null);
      } catch (e) {
        if (!live) return;
        setError(e instanceof Error ? e.message : "could not load workflows");
      } finally {
        if (live) setLoadingList(false);
      }
    })();
    return () => {
      live = false;
    };
  }, [client, company]);

  // Fetch the selected workflow's full graph.
  useEffect(() => {
    if (!selectedId) {
      setGraph(null);
      return;
    }
    let live = true;
    setLoadingGraph(true);
    setResult(null);
    setSelectedNodeId(null);
    (async () => {
      try {
        const g = await getWorkflow(client, company, selectedId);
        if (!live) return;
        setGraph(g);
        setError(null);
        // A successful re-read is exactly what clears a stale-graph warning:
        // whatever `version` we now hold is current.
        setConflict(null);
      } catch (e) {
        if (!live) return;
        setGraph(null);
        setError(e instanceof Error ? e.message : "could not load the workflow graph");
      } finally {
        if (live) setLoadingGraph(false);
      }
    })();
    return () => {
      live = false;
    };
  }, [client, company, selectedId, graphTick]);

  // Load the SELECTED workflow's run history. Re-runs when the selection or
  // company changes, after a manual run, and on every `workflow_run_finished`
  // the shell forwards — so a scheduled run that fires with this tab open lands
  // here on its own.
  //
  // The `workflow` filter is not an optimization, it is the correctness of this
  // read. The host applies `?workflow=` BEFORE the `limit` cut precisely so a
  // rarely-run workflow still returns its own most recent runs. Fetching the
  // company-wide page and filtering client-side would undo that: once other
  // workflows produce `limit` more-recent runs, this workflow's history falls
  // out of the page and the panel would claim it "hasn't finished a run yet"
  // while the run sits journaled on the host — which is issue #228's own
  // symptom, reappearing in the console.
  useEffect(() => {
    // No selection yet (first render, or a company with no workflows): there is
    // no history to ask for, and asking unfiltered is the bug described above.
    // `historySupported` is deliberately left alone — nothing was learned about
    // whether the host serves this route.
    if (!selectedId) {
      setRuns([]);
      return;
    }
    let live = true;
    (async () => {
      try {
        const rows = await listWorkflowRuns(client, company, {
          workflow: selectedId,
          limit: 50,
        });
        if (!live) return;
        setRuns(rows);
        setHistorySupported(true);
        // Issue #371, the no-live-stream fallback. If the run we just POSTed is
        // in this page and nothing was ever *reported* live (only the frontier
        // we derived ourselves), overlay the journaled row so the operator
        // still gets the per-node answer.
        if (awaitingRunId) {
          const mine = rows.find((r) => r.runId === awaitingRunId);
          if (mine) {
            setAwaitingRunId(null);
            setNodeRunState((prev) => {
              const reported = Object.values(prev).some((st) => st !== "running");
              if (!reported && (mine.nodes?.length ?? 0) > 0) setOverlayRun(mine);
              return prev;
            });
          }
        }
      } catch (e) {
        if (!live) return;
        // Degrade quietly: an older host simply has no history to show.
        console.debug("[WorkflowsView] run history unavailable", e);
        setRuns([]);
        setHistorySupported(false);
      }
    })();
    return () => {
      live = false;
    };
    // `awaitingRunId` is read but deliberately not a dependency: it is cleared
    // inside, and listing it would re-run this fetch on that clear.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, company, selectedId, runsTick, runEventTick]);

  const run = useCallback(async () => {
    if (!selectedId) return;
    setRunning(true);
    // Issue #371: clear the previous run's marks and paint the opening frontier
    // immediately, so the canvas responds to the click rather than waiting on
    // the first frame. The `workflow_run_started` frame re-sets the same thing
    // a moment later, which is idempotent.
    setOverlayRun(null);
    setNodeElapsed({});
    setNodeRunState(graph ? initialRunState(graph) : {});
    // Trimmed once here so the echoed request and the payload the host receives
    // can never disagree.
    const asked = request.trim();
    try {
      const res = await runWorkflow(
        client,
        company,
        selectedId,
        asked ? { request: asked } : {},
      );
      setRanWith(asked);
      setResult(res);
      setAwaitingRunId(res.runId ?? null);
      // The run is journaled host-side (#228); pull the history forward so the
      // chip and the panel reflect it immediately.
      setRunsTick((n) => n + 1);
      toast.success("Workflow ran.");
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "could not run the workflow");
      // A run that failed is journaled too (#228), and is the outcome most
      // worth finding again later — so refresh the history on this path as well.
      setRunsTick((n) => n + 1);
      // Nothing is executing any more. Drop the derived marks so a failed run
      // does not leave a node pulsing "running" forever; the reported ok/error
      // ones stay, because they are how far it got.
      setNodeRunState((prev) =>
        Object.fromEntries(Object.entries(prev).filter(([, st]) => st !== "running")),
      );
      setLiveRunId(null);
    } finally {
      setRunning(false);
    }
  }, [client, company, selectedId, request, graph]);

  // Issue #259: remove the selected workflow.
  //
  // The `version` from the graph we are looking at rides along, so this means
  // "delete the thing on my screen" rather than "delete whatever is there now".
  // If it changed underneath us the host refuses with a 409 and removes nothing,
  // and we surface that instead of quietly deleting a graph the operator never
  // saw.
  const remove = useCallback(async () => {
    if (!selectedId || !graph) return;
    const removedName = graph.name;
    setDeleting(true);
    try {
      await deleteWorkflow(client, company, selectedId, graph.version);
      // Drop it locally rather than re-listing: the host has confirmed, and a
      // re-list would flash an empty picker.
      const remaining = workflows.filter((w) => w.id !== selectedId);
      setWorkflows(remaining);
      setSelectedId(remaining[0]?.id ?? null);
      setGraph(null);
      setResult(null);
      setSelectedNodeId(null);
      setConflict(null);
      toast.success(`Deleted “${removedName}”.`);
    } catch (e) {
      // A 409 is the one failure the operator can actually act on, and acting
      // on it means reloading — so it gets the persistent banner, not a toast.
      if (e instanceof ApiError && e.status === 409) {
        setConflict(e.message);
      } else {
        toast.error(e instanceof Error ? e.message : "could not delete the workflow");
      }
    } finally {
      setDeleting(false);
    }
  }, [client, company, selectedId, graph, workflows]);

  // Issue #259: the edit dialog saved. The host answers with the stored graph
  // AND a fresh version token, so holding onto it is what lets the operator
  // save again without a re-read — dropping it and re-fetching would be a round
  // trip that can only return the same thing.
  const handleSaved = useCallback((saved: WorkflowGraph) => {
    setGraph(saved);
    // The name and description are editable, so the picker entry has to move
    // with them. The id cannot change, which is what makes this a rewrite of
    // one row rather than a re-list.
    setWorkflows((prev) =>
      prev
        .map((w) =>
          w.id === saved.id
            ? { ...w, name: saved.name, description: saved.description }
            : w,
        )
        .sort((a, b) => a.name.localeCompare(b.name)),
    );
    // The write landed, so whatever we hold is current — the same reasoning as
    // the graph-load effect's clear.
    setConflict(null);
    toast.success("Workflow saved.");
  }, []);

  // The creator posts the full graph back, so the new entry can be spliced
  // straight into the list and selected — no extra round trip to re-list.
  const handleCreated = useCallback((created: WorkflowGraph) => {
    setWorkflows((prev) => {
      const rest = prev.filter((w) => w.id !== created.id);
      return [...rest, { id: created.id, name: created.name, description: created.description }].sort(
        (a, b) => a.name.localeCompare(b.name),
      );
    });
    setSelectedId(created.id);
    toast.success("Workflow created.");
  }, []);

  // Issue #371: fold the live SSE progress frames into per-node canvas state.
  //
  // Everything here is guarded on the frame belonging to the workflow on screen
  // AND (after the start) to the run we adopted, because one SSE connection
  // carries every run of every workflow in the company.
  useEffect(() => {
    if (!runEvent || !selectedId) return;

    switch (runEvent.type) {
      case "workflow_run_started": {
        if (runEvent.workflowId !== selectedId) return;
        // Adopt this run and start from a clean canvas. A rerun must not leave
        // the previous run's marks behind — they would read as this run's.
        setLiveRunId(runEvent.runId);
        setOverlayRun(null);
        // The trigger fired by definition — the run exists — and the engine
        // reports no step for it, so nothing else would ever mark it. Its
        // successors are where execution is now.
        setNodeRunState(graph ? initialRunState(graph) : {});
        setNodeElapsed({});
        return;
      }
      case "workflow_node_finished": {
        if (runEvent.workflowId !== selectedId || runEvent.runId !== liveRunId) return;
        const finished = runEvent.nodeId;
        // Trust the host's word, but do not let an unknown one paint a node as
        // succeeded: anything that is not "ok" is treated as a failure.
        const state: NodeRunState = runEvent.status === "ok" ? "ok" : "error";
        setNodeRunState((prev) => {
          const next = { ...prev, [finished]: state };
          // Advance the frontier. Only a successful node hands execution on —
          // a failed one under the default `stop` policy ends the run, and
          // lighting up its successors would claim work that never happened.
          if (state === "ok" && graph) {
            for (const id of successorsOf(graph, finished)) {
              if (!next[id]) next[id] = "running";
            }
          }
          return next;
        });
        setNodeElapsed((prev) => ({ ...prev, [finished]: runEvent.elapsedMs }));
        return;
      }
      case "workflow_run_finished": {
        if (runEvent.workflowId !== selectedId) return;
        // A pre-#371 host sends no runId; treat that as "the run on screen"
        // rather than ignoring it, else the canvas would spin forever.
        if (runEvent.runId && liveRunId && runEvent.runId !== liveRunId) return;
        // Clear the derived "running" marks — nothing is executing any more —
        // but KEEP the reported ok/error ones. That residue is the answer to
        // "how far did it get?", and it stays until a reselect or a rerun.
        setNodeRunState((prev) =>
          Object.fromEntries(
            Object.entries(prev).filter(([, state]) => state !== "running"),
          ),
        );
        setLiveRunId(null);
        return;
      }
      default:
        return;
    }
    // `graph` is read inside, but is deliberately NOT a dependency: a graph
    // re-fetch mid-run would replay the last frame and could resurrect cleared
    // marks. The selection effect below is what resets state on a graph change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runEvent, selectedId, liveRunId]);

  // Switching workflow (or company) clears the canvas: another graph's node ids
  // are meaningless here, and a stale mark on a same-named node would be a lie.
  useEffect(() => {
    setNodeRunState({});
    setNodeElapsed({});
    setLiveRunId(null);
    setOverlayRun(null);
  }, [selectedId, company]);

  // What the canvas actually paints. An overlaid past run wins over the live
  // state — selecting a history row is an explicit "show me that one".
  const paintedStates = useMemo<Record<string, NodeRunState>>(
    () => (overlayRun ? statesFromRun(overlayRun) : nodeRunState),
    [overlayRun, nodeRunState],
  );
  const paintedElapsed = useMemo<Record<string, number>>(
    () => (overlayRun ? elapsedFromRun(overlayRun) : nodeElapsed),
    [overlayRun, nodeElapsed],
  );

  const { nodes, edges } = useMemo(
    () =>
      graph
        ? layout(graph, paintedStates, paintedElapsed)
        : { nodes: [], edges: [] },
    [graph, paintedStates, paintedElapsed],
  );

  const selected = workflows.find((w) => w.id === selectedId) ?? null;

  // The full node model (kind/name/summary/agent/config) for the clicked node,
  // looked up from the loaded graph so the inspector shows fields the laid-out
  // canvas node data doesn't carry (agent, config, …).
  const selectedNode = useMemo(
    () => graph?.nodes.find((n) => n.id === selectedNodeId) ?? null,
    [graph, selectedNodeId],
  );

  const onNodeClick = useCallback((_: unknown, node: Node) => {
    setSelectedNodeId(node.id);
  }, []);

  // `runs` already holds only the selected workflow's runs, newest first — the
  // host filters and orders them. Re-filtering here would be a second source of
  // truth that can only ever disagree with the first.
  const lastRun = runs[0] ?? null;

  // Issue #259. `editable === false` is the host saying "PUT/DELETE on this id
  // will 409" — a source-defined graph, or a name-only entry with no saved
  // graph. A host predating #259 sends no field at all, and `undefined` must NOT
  // read as a refusal, so only an explicit `false` disables the affordance.
  const notEditable = graph?.editable === false;
  const canDelete = !!graph && !notEditable && !deleting;
  // Same predicate as Delete, and deliberately the same explanation: the host
  // refuses both writes for the same reason, with one message.
  const canEdit = !!graph && !notEditable && !loadingGraph && !deleting;
  const notEditableReason = graph
    ? `“${graph.name}” is defined by a file in the company source tree, so it can't be changed or removed from the console. Edit workflows/${graph.id}.toml in the company repository instead.`
    : undefined;

  return (
    <div className="flex flex-1 flex-col overflow-hidden">
      <div className="flex flex-wrap items-center justify-between gap-2 border-b px-4 py-3">
        <div className="flex min-w-0 items-center gap-2">
          <h2 className="text-sm font-semibold">Workflows</h2>
          <Badge variant="secondary">{workflows.length}</Badge>
          {/* Issue #228: the last run's outcome at a glance — including for a
              scheduled run nobody watched, which is the case the issue is
              about. Absent until this workflow has run at least once. */}
          {lastRun && <LastRunChip run={lastRun} />}
          {selected?.description && (
            <p className="hidden truncate text-xs text-muted-foreground sm:block">
              {selected.description}
            </p>
          )}
        </div>
        <div className="flex items-center gap-2">
          <Select
            value={selectedId ?? undefined}
            onValueChange={(v) => setSelectedId(v)}
            disabled={loadingList || workflows.length === 0}
          >
            <SelectTrigger className="h-8 w-56">
              {/* The trigger renders the raw value unless told otherwise, and a
                  workflow's id is not its name — the closed picker showed
                  `e2e_conflict_1785687393855` where the popup showed "Conflict
                  probe". Resolve it back through the list the popup is built
                  from; fall back to the id if the list has not arrived yet. */}
              <SelectValue placeholder={loadingList ? "Loading…" : "Pick a workflow"}>
                {(selected) =>
                  workflows.find((w) => w.id === selected)?.name ?? String(selected ?? "")
                }
              </SelectValue>
            </SelectTrigger>
            <SelectContent>
              {workflows.map((w) => (
                <SelectItem key={w.id} value={w.id}>
                  {w.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Input
            value={request}
            onChange={(e) => setRequest(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && selectedId && !running && !loadingGraph) {
                void run();
              }
            }}
            disabled={!selectedId || running}
            placeholder="What should this run work on?"
            aria-label="Request for this run"
            className="h-8 w-64"
          />
          <Button size="sm" onClick={() => void run()} disabled={!selectedId || running || loadingGraph}>
            {running ? (
              <Loader2 className="mr-1.5 size-4 animate-spin" />
            ) : (
              <Play className="mr-1.5 size-4" />
            )}
            Run
          </Button>
          {historySupported && (
            <Button
              size="sm"
              variant="outline"
              onClick={() => setHistoryOpen((open) => !open)}
              aria-pressed={historyOpen}
              data-testid="workflow-history-toggle"
            >
              <History className="mr-1.5 size-4" />
              History
              {runs.length > 0 && (
                <Badge variant="secondary" className="ml-1.5 h-4 px-1.5 text-[10px] font-normal">
                  {runs.length}
                </Badge>
              )}
            </Button>
          )}
          {/* Issue #259. The wrapping span carries the explanation: a disabled
              button swallows pointer events in most browsers, so a `title` on
              the button itself would never show. */}
          <span title={notEditable ? notEditableReason : undefined}>
            <Button
              size="sm"
              variant="outline"
              onClick={() => setEditOpen(true)}
              disabled={!canEdit}
              aria-label="Edit workflow"
              data-testid="workflow-edit"
            >
              <Pencil className="mr-1.5 size-4" />
              Edit
            </Button>
          </span>
          <span title={notEditable ? notEditableReason : undefined}>
            <AlertDialog open={confirmOpen} onOpenChange={setConfirmOpen}>
              <AlertDialogTrigger
                render={
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={!canDelete}
                    aria-label="Delete workflow"
                    data-testid="workflow-delete"
                  >
                    {deleting ? (
                      <Loader2 className="mr-1.5 size-4 animate-spin" />
                    ) : (
                      <Trash2 className="mr-1.5 size-4" />
                    )}
                    Delete
                  </Button>
                }
              />
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>Delete “{graph?.name ?? selectedId}”?</AlertDialogTitle>
                  {/* Say exactly what goes and what stays. "Stops its schedule"
                      is the consequence an operator most needs spelled out, and
                      "past runs stay" stops them hesitating over losing history. */}
                  <AlertDialogDescription>
                    This removes the workflow and stops it running on its schedule. Past runs stay
                    in the run history. This can&apos;t be undone.
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>Keep it</AlertDialogCancel>
                  <AlertDialogAction
                    onClick={() => {
                      // Close FIRST: `remove()` clears the selection, so a
                      // dialog left mounted would re-render its title as
                      // `Delete “”?` over a backdrop nothing can click past.
                      setConfirmOpen(false);
                      void remove();
                    }}
                    className="bg-destructive text-white hover:bg-destructive/90"
                    data-testid="workflow-delete-confirm"
                  >
                    Delete workflow
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          </span>
          <Button size="sm" variant="outline" onClick={() => setCreateOpen(true)}>
            <Plus className="mr-1.5 size-4" />
            New workflow
          </Button>
        </div>
      </div>

      {/* Issue #259: a write refused because the graph moved under us. Distinct
          from `error` on purpose — this one is recoverable, and the recovery is
          right here, so it must not be mistaken for a generic load failure. */}
      {conflict && (
        <div className="px-4 pt-3">
          <Alert variant="destructive" data-testid="workflow-conflict">
            <AlertDescription className="flex flex-wrap items-center justify-between gap-2">
              <span>{conflict}</span>
              <Button
                size="sm"
                variant="outline"
                onClick={() => setGraphTick((n) => n + 1)}
                data-testid="workflow-conflict-reload"
              >
                <RotateCw className="mr-1.5 size-4" />
                Reload
              </Button>
            </AlertDescription>
          </Alert>
        </div>
      )}

      {error && (
        <div className="px-4 pt-3">
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        </div>
      )}

      {/* Issue #371: the canvas is showing a PAST run, not the live one. Said
          plainly, with the way out attached — an unexplained ring on a node
          would otherwise read as the current state of the workflow. */}
      {overlayRun && (
        <div className="px-4 pt-3">
          <Alert data-testid="workflow-overlay-banner">
            <AlertDescription className="flex flex-wrap items-center justify-between gap-2">
              <span className="text-xs">
                Showing the {overlayRun.scheduled ? "scheduled" : "manual"} run from{" "}
                {new Date(overlayRun.atMillis).toLocaleString()}
                {overlayRun.error
                  ? failedNodeOf(overlayRun)
                    ? ` — it failed at “${nodeName(graph, failedNodeOf(overlayRun)!)}”.`
                    : " — it failed before any node ran."
                  : "."}{" "}
                Unmarked nodes were never reached.
              </span>
              <Button size="sm" variant="outline" onClick={() => setOverlayRun(null)}>
                Clear
              </Button>
            </AlertDescription>
          </Alert>
        </div>
      )}

      <div className="relative flex-1">
        {loadingList || loadingGraph ? (
          <div className="absolute inset-0 p-4">
            <Skeleton className="h-full w-full rounded-xl" />
          </div>
        ) : !selectedId ? (
          <div className="flex h-full flex-col items-center justify-center gap-3 px-4 text-center text-sm text-muted-foreground">
            <p>This company has no saved workflows yet.</p>
            <Button size="sm" variant="outline" onClick={() => setCreateOpen(true)}>
              <Plus className="mr-1.5 size-4" />
              New workflow
            </Button>
          </div>
        ) : (
          <>
            <ReactFlow
              nodes={nodes}
              edges={edges}
              nodeTypes={NODE_TYPES}
              colorMode={resolvedTheme === "dark" ? "dark" : "light"}
              fitView
              fitViewOptions={{ padding: 0.2 }}
              nodesDraggable={false}
              nodesConnectable={false}
              elementsSelectable
              onNodeClick={onNodeClick}
              onPaneClick={() => setSelectedNodeId(null)}
              proOptions={{ hideAttribution: true }}
            >
              <Background variant={BackgroundVariant.Dots} gap={20} size={1} />
              <Controls showInteractive={false} />
              <MiniMap pannable zoomable className="!hidden sm:!block" />
            </ReactFlow>
            {selectedNode && (
              <NodeDetailPanel node={selectedNode} onClose={() => setSelectedNodeId(null)} />
            )}
          </>
        )}
      </div>

      {result && (
        <RunResultPanel
          result={result}
          graph={graph}
          request={ranWith}
          onClose={() => setResult(null)}
        />
      )}

      {historyOpen && historySupported && (
        <RunHistoryPanel
          runs={runs}
          workflowName={selected?.name ?? selectedId ?? ""}
          onClose={() => setHistoryOpen(false)}
          selectedRunSeq={overlayRun?.seq ?? null}
          onSelectRun={(picked) =>
            // Clicking the row already shown clears it, so the control is a
            // toggle rather than a one-way trip into overlay mode.
            setOverlayRun((prev) => (prev?.seq === picked.seq ? null : picked))
          }
        />
      )}

      <WorkflowCreateDialog
        client={client}
        company={company}
        open={createOpen}
        onOpenChange={setCreateOpen}
        onCreated={handleCreated}
      />

      {/* Issue #259: the same dialog in edit mode. `graph` carries the version
          token the save is conditional on, so it is passed as loaded rather
          than copied — and a 409 lands in the banner above, which outlives the
          dialog and holds the way out.

          `open` is gated on `graph` too: without it, a selection that goes away
          under an open dialog (a company switch, a failed re-read) would leave
          `workflow` null, which IS create mode — the operator would be looking
          at a blank New workflow form wearing the Edit title. */}
      <WorkflowCreateDialog
        client={client}
        company={company}
        open={editOpen && graph !== null}
        onOpenChange={setEditOpen}
        workflow={graph}
        onSaved={handleSaved}
        onConflict={setConflict}
      />
    </div>
  );
}

/** A read-only inspector for a single graph node, overlaid on the canvas when
 * the operator clicks a node. Surfaces the fields already on the wire from
 * `GET …/workflows/{wid}`: kind, name, summary, the assigned agent (agent
 * nodes), the trigger's cron schedule (trigger nodes, issue #169), and any
 * kind-specific config / error-handling policy. */
function NodeDetailPanel({
  node,
  onClose,
}: {
  node: WorkflowNodeModel;
  onClose: () => void;
}) {
  const meta = nodeKindMeta(node.kind);
  const hasConfig =
    node.config !== undefined && node.config !== null &&
    !(typeof node.config === "object" && Object.keys(node.config as object).length === 0);

  return (
    <div className="absolute right-3 top-3 bottom-3 z-10 flex w-72 flex-col overflow-hidden rounded-xl border bg-card/95 shadow-lg backdrop-blur sm:w-80">
      <div className="flex items-start justify-between gap-2 border-b px-3 py-2">
        <div className="flex min-w-0 items-center gap-2">
          <span className="text-base leading-none" aria-hidden>
            {meta.emoji}
          </span>
          <div className="min-w-0">
            <div className="truncate text-sm font-semibold">{node.name}</div>
            <div className="truncate text-[11px] text-muted-foreground">{node.id}</div>
          </div>
        </div>
        <Button variant="ghost" size="sm" className="-mr-1 h-7 px-2" onClick={onClose}>
          Close
        </Button>
      </div>

      <div className="min-h-0 flex-1 space-y-3 overflow-auto px-3 py-3 text-sm">
        <div className="flex flex-wrap items-center gap-1.5">
          <Badge variant="outline" className="font-normal">
            {node.kind}
          </Badge>
          {node.requiresApproval && (
            <Badge variant="outline" className="border-amber-500/40 bg-amber-500/10 font-normal">
              requires approval
            </Badge>
          )}
          {node.schedule && (
            <Badge variant="outline" className="border-sky-500/40 bg-sky-500/10 font-normal">
              scheduled
            </Badge>
          )}
        </div>

        {/* A saved schedule must be visible, not write-only — otherwise an
            operator cannot tell a self-running workflow from a manual one. */}
        {node.schedule && (
          <DetailField label="Schedule">
            <p className="font-mono text-xs">{node.schedule}</p>
            <p className="text-[10px] text-muted-foreground">5-field cron, UTC.</p>
          </DetailField>
        )}

        {node.summary && (
          <DetailField label="Summary">
            <p className="text-sm leading-snug">{node.summary}</p>
          </DetailField>
        )}

        {node.agent && (
          <DetailField label="Assigned agent">
            <p className="font-mono text-xs">{node.agent}</p>
          </DetailField>
        )}

        {hasConfig && (
          <DetailField label="Config">
            <pre className="overflow-auto rounded-lg border bg-muted/40 p-2 font-mono text-[11px] leading-snug">
              {JSON.stringify(node.config, null, 2)}
            </pre>
          </DetailField>
        )}

        {node.onError && (
          <DetailField label="On error">
            <p className="font-mono text-xs">{node.onError}</p>
          </DetailField>
        )}

        {node.retry && (
          <DetailField label="Retry">
            <pre className="overflow-auto rounded-lg border bg-muted/40 p-2 font-mono text-[11px] leading-snug">
              {JSON.stringify(node.retry, null, 2)}
            </pre>
          </DetailField>
        )}

        {node.destination && (
          <DetailField label="Destination">
            <p className="text-sm leading-snug">{describeDestination(node.destination)}</p>
          </DetailField>
        )}

        {!node.summary &&
          !node.agent &&
          !hasConfig &&
          !node.onError &&
          !node.retry &&
          !node.schedule &&
          !node.destination &&
          !node.requiresApproval && (
            <p className="text-xs text-muted-foreground">
              This node has no extra details beyond its kind and name.
            </p>
          )}
      </div>
    </div>
  );
}

/** Where an output node's report goes, in a sentence. `owner` deliberately has
 * no target to show — it resolves to the company's admins server-side, which is
 * exactly why an author can't point it at an outsider. */
function describeDestination(destination: NonNullable<WorkflowNodeModel["destination"]>): string {
  switch (destination.kind) {
    case "owner":
      return "Reports to the company's admins.";
    case "email":
      return `Emails ${destination.target ?? "(no address)"}.`;
    case "channel":
      return `Posts to the ${destination.target ?? "(unnamed)"} channel.`;
    default:
      return `${destination.kind}${destination.target ? ` → ${destination.target}` : ""}`;
  }
}

/** Badge styling per delivery outcome. A report that did NOT go out must not
 * look like one that did — `denied` and `failed` are the two an operator has to
 * act on, so they get the loud treatment. `pending` is neither: the report is
 * waiting in Approvals, so it reads as informational, not as a failure. */
const DELIVERY_TONE: Record<DeliveryStatus, string> = {
  sent: "border-emerald-500/40 bg-emerald-500/10",
  pending: "border-sky-500/40 bg-sky-500/10",
  skipped: "border-amber-500/40 bg-amber-500/10",
  denied: "border-red-500/40 bg-red-500/10",
  failed: "border-red-500/40 bg-red-500/10",
};

/** The delivery block of the run drawer: one line per attempt to route an
 * output node's report. This is the ONLY place an operator learns a report
 * didn't leave the building — a delivery failure never fails the run. */
function DeliveryRows({ deliveries }: { deliveries: DeliveryReport[] }) {
  // Two counters, not one. A parked report is waiting on the operator, not
  // broken — badging it red alongside a transport failure would send them
  // hunting for a bug when the fix is a click in Approvals.
  const pending = deliveries.filter((d) => d.status === "pending").length;
  const undelivered = deliveries.filter(
    (d) => d.status !== "sent" && d.status !== "pending",
  ).length;
  return (
    <div className="mb-3 space-y-1.5 rounded-lg border bg-background/40 p-2">
      <div className="flex items-center gap-2">
        <span className="text-xs font-medium">Report delivery</span>
        {pending > 0 && (
          <Badge variant="outline" className="h-4 px-1.5 text-[10px] font-normal border-sky-500/40 bg-sky-500/10">
            {pending} awaiting approval
          </Badge>
        )}
        {undelivered > 0 && (
          <Badge variant="outline" className="h-4 px-1.5 text-[10px] font-normal border-red-500/40 bg-red-500/10">
            {undelivered} not delivered
          </Badge>
        )}
      </div>
      {deliveries.map((d, i) => (
        <div key={`${d.node}-${d.target ?? ""}-${i}`} className="flex flex-wrap items-baseline gap-1.5">
          <Badge
            variant="outline"
            className={`h-4 px-1.5 text-[10px] font-normal ${DELIVERY_TONE[d.status] ?? ""}`}
          >
            {d.status}
          </Badge>
          <span className="font-mono text-[11px]">{d.node}</span>
          <span className="text-[11px] text-muted-foreground">
            → {d.kind}
            {d.target ? ` ${d.target}` : ""} — {d.detail}
          </span>
        </div>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Run history (issue #228)
// ---------------------------------------------------------------------------

/**
 * The `pending` delivery status — a report parked for an operator's approval —
 * is added to `DeliveryStatus` by issue #227. It is typed `string` rather than
 * written as a literal so these comparisons compile both before and after that
 * lands: against today's union TypeScript would reject the literal as a
 * no-overlap comparison, and once the union widens this keeps behaving
 * identically. The runtime check is what matters — the host can already send a
 * status this console's type doesn't name yet.
 */
const PENDING_STATUS: string = "pending";

/** Reports that did NOT reach their destination **and will not without a
 * change** — the number worth acting on. `pending` is excluded on purpose: it
 * is a report parked for an operator's approval, so counting it here would
 * badge a working approvals queue as a failure. */
function undeliveredCount(deliveries: DeliveryReport[]): number {
  return deliveries.filter((d) => d.status !== "sent" && d.status !== PENDING_STATUS).length;
}

/** Reports waiting on an operator's verdict rather than on a fix. */
function pendingCount(deliveries: DeliveryReport[]): number {
  return deliveries.filter((d) => d.status === PENDING_STATUS).length;
}

/** A compact "N minutes ago" for a run timestamp — enough to tell last night's
 * scheduled run from the one just clicked, without a date library. */
function relativeTime(atMillis: number): string {
  const seconds = Math.max(0, Math.round((Date.now() - atMillis) / 1000));
  if (seconds < 60) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.round(hours / 24)}d ago`;
}

/** The status dot for a whole run: red when it failed or lost a report, sky
 * when something is parked for approval, green when everything landed. */
function runTone(run: WorkflowRunOutcome): { dot: string; label: string } {
  if (run.error) return { dot: "bg-red-500", label: "failed" };
  if (undeliveredCount(run.deliveries) > 0) return { dot: "bg-red-500", label: "not delivered" };
  if (pendingCount(run.deliveries) > 0) return { dot: "bg-sky-500", label: "awaiting approval" };
  return { dot: "bg-emerald-500", label: "ok" };
}

/** The last-run chip beside the workflow title: a status dot, the undelivered
 * count when there is one, and how long ago it ran. This is the at-a-glance
 * answer to "did last night's scheduled run actually deliver?" — the question
 * that had no answer at all before issue #228. */
function LastRunChip({ run }: { run: WorkflowRunOutcome }) {
  const tone = runTone(run);
  const undelivered = undeliveredCount(run.deliveries);
  const pending = pendingCount(run.deliveries);
  return (
    <Badge
      variant="outline"
      className="h-5 gap-1.5 px-2 text-[10px] font-normal"
      data-testid="workflow-last-run-chip"
      title={
        run.error
          ? `Last run failed: ${run.error}`
          : `Last ${run.scheduled ? "scheduled" : "manual"} run — ${tone.label}`
      }
    >
      <span className={`size-1.5 rounded-full ${tone.dot}`} />
      {run.scheduled ? "Scheduled" : "Manual"} run
      {run.error
        ? " failed"
        : undelivered > 0
          ? ` · ${undelivered} not delivered`
          : pending > 0
            ? ` · ${pending} awaiting approval`
            : ""}
      <span className="text-muted-foreground">· {relativeTime(run.atMillis)}</span>
    </Badge>
  );
}

/** The run-history drawer: one row per finished run of the selected workflow,
 * newest first, each expanding to the very same {@link DeliveryRows} block the
 * live run drawer shows.
 *
 * This is the durable half of issue #228. A manual run's delivery rows used to
 * live only in the run drawer until it was dismissed, and a scheduled run's only
 * on the host's stdout — which on a hosted tenant is the platform team, not the
 * operator. These rows come back from the company's journal, so they survive a
 * console reload and a run nobody was watching. */
function RunHistoryPanel({
  runs,
  workflowName,
  onClose,
  selectedRunSeq,
  onSelectRun,
}: {
  runs: WorkflowRunOutcome[];
  workflowName: string;
  onClose: () => void;
  /** The run currently overlaid on the canvas, if any (issue #371). */
  selectedRunSeq: number | null;
  /** Overlay this run's per-node states on the canvas (issue #371). */
  onSelectRun: (run: WorkflowRunOutcome) => void;
}) {
  return (
    <div className="border-t bg-card/60" data-testid="workflow-run-history">
      <div className="flex items-center justify-between px-4 py-2">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">Run history</span>
          {workflowName && (
            <span className="truncate text-xs text-muted-foreground">{workflowName}</span>
          )}
          <Badge variant="secondary">{runs.length}</Badge>
        </div>
        <Button variant="ghost" size="sm" onClick={onClose}>
          Dismiss
        </Button>
      </div>
      <div className="max-h-72 overflow-auto px-4 pb-3">
        {runs.length === 0 ? (
          <p className="text-xs text-muted-foreground">
            This workflow hasn't finished a run yet. Runs appear here once they
            do — including scheduled ones that run while you're away.
          </p>
        ) : (
          <div className="space-y-2">
            {runs.map((run) => (
              <RunHistoryRow
                key={run.seq}
                run={run}
                selected={run.seq === selectedRunSeq}
                onSelect={() => onSelectRun(run)}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

/** One finished run: a summary line, its per-node trail, and its delivery rows.
 *
 * Clicking it overlays that run's node states on the canvas (issue #371) —
 * which is what makes a scheduled run's failure point visible, the case the
 * live canvas by definition cannot cover because nobody was watching. */
function RunHistoryRow({
  run,
  selected,
  onSelect,
}: {
  run: WorkflowRunOutcome;
  selected: boolean;
  onSelect: () => void;
}) {
  const tone = runTone(run);
  const nodes = run.nodes ?? [];
  const failedNode = failedNodeOf(run);
  return (
    <div
      className={`rounded-lg border bg-background/40 p-2 ${
        selected ? "ring-2 ring-primary/40" : ""
      }`}
      data-testid="workflow-run-row"
    >
      <div className="mb-1 flex flex-wrap items-center gap-2">
        <span className={`size-1.5 rounded-full ${tone.dot}`} />
        <Badge variant="outline" className="h-4 px-1.5 text-[10px] font-normal">
          {run.scheduled ? "scheduled" : "manual"}
        </Badge>
        <span className="text-[11px] text-muted-foreground">
          {new Date(run.atMillis).toLocaleString()} · {relativeTime(run.atMillis)}
        </span>
        {run.pendingApprovals.length > 0 && (
          <Badge
            variant="outline"
            className="h-4 px-1.5 text-[10px] font-normal border-amber-500/40 bg-amber-500/10"
          >
            {run.pendingApprovals.length} pending approval
            {run.pendingApprovals.length === 1 ? "" : "s"}
          </Badge>
        )}
        {run.running && (
          <Badge
            variant="outline"
            className="h-4 px-1.5 text-[10px] font-normal border-sky-500/40 bg-sky-500/10"
          >
            running
          </Badge>
        )}
        {nodes.length > 0 && (
          <Button
            size="sm"
            variant="ghost"
            className="ml-auto h-5 px-2 text-[10px]"
            onClick={onSelect}
            aria-pressed={selected}
            data-testid="workflow-run-overlay-toggle"
          >
            {selected ? "Hide on canvas" : "Show on canvas"}
          </Button>
        )}
      </div>

      {/* Issue #371: the per-node trail, which is what turns "it failed" into
          "it failed HERE". Absent for a run journaled before #371 — those rows
          render exactly as they always did. */}
      {nodes.length > 0 && (
        <div className="mb-1 flex flex-wrap gap-1" data-testid="workflow-run-nodes">
          {nodes.map((node) => (
            <RunNodeChip key={`${node.nodeId}-${node.elapsedMs}`} node={node} />
          ))}
        </div>
      )}
      {run.error ? (
        // The outcome that used to be quietest of all: a run that died left one
        // host-stdout warning and nothing an operator could ever find.
        <Alert variant="destructive" className="py-2">
          <AlertDescription className="text-[11px]">
            {/* Name the node when the trail names one — the engine reports a
                failing node as an errored step, so this is exact. When it does
                not (a graph that would not compile, a capability that could not
                be built), say nothing about nodes rather than guessing. */}
            {failedNode ? `This run failed at “${failedNode}”: ` : "This run failed: "}
            {run.error}
          </AlertDescription>
        </Alert>
      ) : run.deliveries.length > 0 ? (
        // Deliberately the SAME component the live run drawer uses, so a report
        // reads identically whether it's on screen now or a week old.
        <DeliveryRows deliveries={run.deliveries} />
      ) : (
        <p className="text-[11px] text-muted-foreground">
          Finished — this run routed no reports.
        </p>
      )}
    </div>
  );
}

/** One node's outcome in a history row: its id, how it went, how long it took. */
function RunNodeChip({ node }: { node: WorkflowRunNode }) {
  const ok = node.status === "ok";
  return (
    <span
      className={`inline-flex items-center gap-1 rounded border px-1.5 py-0.5 text-[10px] ${
        ok
          ? "border-emerald-500/40 bg-emerald-500/10"
          : "border-red-500/50 bg-red-500/10"
      }`}
    >
      <span className={`size-1.5 rounded-full ${ok ? "bg-emerald-500" : "bg-red-500"}`} />
      <span className="font-medium">{node.nodeId}</span>
      <span className="font-mono opacity-70">
        {node.elapsedMs < 1000 ? `${node.elapsedMs}ms` : `${(node.elapsedMs / 1000).toFixed(1)}s`}
      </span>
    </span>
  );
}

/** A labelled block inside the node inspector. */
function DetailField({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="space-y-1">
      <p className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</p>
      {children}
    </div>
  );
}

/** The run-output drawer: one readable card per executed node (the producing
 * agent and its reply, markdown-rendered) plus the branch each condition node
 * took, any nodes left pending approval, and the raw engine state collapsed
 * behind a toggle. Falls back to a raw JSON dump when the output doesn't match
 * the expected per-node shape. */
function RunResultPanel({
  result,
  graph,
  request,
  onClose,
}: {
  result: WorkflowRunResult;
  graph: WorkflowGraph | null;
  /** What the operator asked this run for (issue #154); "" when they asked for
   * nothing, in which case the line is omitted rather than showing a bare dash. */
  request: string;
  onClose: () => void;
}) {
  const nodeResults = useMemo(
    () => parseRunNodes(result.output, graph),
    [result.output, graph],
  );
  const deliveries = result.deliveries ?? [];
  const pendingDeliveryCount = deliveries.filter((d) => d.status === "pending").length;
  const undeliveredCount = deliveries.filter(
    (d) => d.status !== "sent" && d.status !== "pending",
  ).length;

  return (
    <div className="border-t bg-card/60">
      <div className="flex items-center justify-between px-4 py-2">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">Run result</span>
          {pendingDeliveryCount > 0 && (
            <Badge variant="outline" className="border-sky-500/40 bg-sky-500/10">
              {pendingDeliveryCount} report{pendingDeliveryCount === 1 ? "" : "s"} awaiting approval
            </Badge>
          )}
          {undeliveredCount > 0 && (
            <Badge variant="outline" className="border-red-500/40 bg-red-500/10">
              {undeliveredCount} report{undeliveredCount === 1 ? "" : "s"} not delivered
            </Badge>
          )}
          {result.pendingApprovals.length > 0 && (
            <Badge variant="outline" className="border-amber-500/40 bg-amber-500/10">
              {result.pendingApprovals.length} pending approval
              {result.pendingApprovals.length === 1 ? "" : "s"}
            </Badge>
          )}
        </div>
        <Button variant="ghost" size="sm" onClick={onClose}>
          Dismiss
        </Button>
      </div>
      <div className="max-h-72 overflow-auto px-4 pb-3">
        {request && (
          <p className="mb-2 text-xs text-muted-foreground">
            <span className="font-medium text-foreground">Requested:</span> {request}
          </p>
        )}
        {result.pendingApprovals.length > 0 && (
          <p className="mb-2 text-xs text-muted-foreground">
            Waiting on: {result.pendingApprovals.join(", ")}
          </p>
        )}

        {deliveries.length > 0 && <DeliveryRows deliveries={deliveries} />}

        {nodeResults && nodeResults.length > 0 ? (
          <div className="mb-2 space-y-2">
            {nodeResults.map((n) => (
              <NodeResultCard key={n.id} node={n} />
            ))}
          </div>
        ) : (
          <p className="mb-2 text-xs text-muted-foreground">
            The run finished, but its output didn't match the expected node
            shape — see the raw output below.
          </p>
        )}

        <details open={!nodeResults || nodeResults.length === 0}>
          <summary className="cursor-pointer text-xs text-muted-foreground">
            Show raw engine output
          </summary>
          <pre className="mt-1 rounded-lg border bg-muted/40 p-2 font-mono text-[11px] leading-snug">
            {JSON.stringify(result.output, null, 2)}
          </pre>
        </details>
      </div>
    </div>
  );
}

/** One node's readable result: its name, the producing agent, and its reply
 * (markdown-rendered). Falls back to a subtle placeholder / the branch it took
 * when it produced no text (e.g. a trigger or a condition node). */
function NodeResultCard({ node }: { node: NodeResult }) {
  return (
    <div className="rounded-lg border bg-background/40 p-2">
      <div className="mb-1 flex items-center gap-2">
        <span className="truncate text-xs font-medium">{node.name}</span>
        {node.port !== null && (
          <Badge variant="outline" className="h-4 px-1.5 text-[10px] font-normal">
            branch: {node.port}
          </Badge>
        )}
      </div>
      {node.messages.map((m, i) => (
        <div key={i} className={i > 0 ? "mt-2 border-t pt-2" : undefined}>
          {m.agentRef && (
            <p className="mb-1 text-[10px] uppercase tracking-wide text-muted-foreground">
              {m.agentRef}
            </p>
          )}
          {m.text ? (
            <div className="prose prose-sm max-w-none dark:prose-invert">
              <ReactMarkdown remarkPlugins={[remarkGfm]}>{m.text}</ReactMarkdown>
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">—</p>
          )}
        </div>
      ))}
      {node.messages.length === 0 && (
        <p className="text-sm text-muted-foreground">—</p>
      )}
    </div>
  );
}

/** A node's display name, falling back to its id when the graph is not loaded
 * (a company switch mid-overlay) — never a blank quote. */
function nodeName(graph: WorkflowGraph | null, nodeId: string): string {
  return graph?.nodes.find((n) => n.id === nodeId)?.name ?? nodeId;
}

/** The node ids `from` hands execution to. */
function successorsOf(graph: WorkflowGraph, from: string): string[] {
  return graph.edges.filter((e) => e.from === from).map((e) => e.to);
}

/** The canvas state a run starts in (issue #371): every trigger marked done,
 * its successors marked running.
 *
 * Both halves are derived rather than reported. The engine emits no step for a
 * trigger node — it is the thing that fired, not a thing that ran — and it has
 * no `on_step_start` hook at all, so "where is it now" has to come from the
 * graph. After a branch point this briefly marks more than one arm; that
 * corrects itself as the real finishes arrive. */
function initialRunState(graph: WorkflowGraph): Record<string, NodeRunState> {
  const state: Record<string, NodeRunState> = {};
  for (const node of graph.nodes) {
    if (node.kind !== "trigger") continue;
    state[node.id] = "ok";
    for (const id of successorsOf(graph, node.id)) state[id] = "running";
  }
  return state;
}

/** The per-node states of a PAST run, for overlaying it on the canvas.
 *
 * No `running` ever comes out of this: the run is over. A node the run never
 * reached simply has no entry, so it renders unmarked — "not reached" and not
 * "still to come", which for a failed run is the honest reading. */
function statesFromRun(run: WorkflowRunOutcome): Record<string, NodeRunState> {
  const state: Record<string, NodeRunState> = {};
  for (const node of run.nodes ?? []) {
    state[node.nodeId] = node.status === "ok" ? "ok" : "error";
  }
  return state;
}

/** Per-node durations of a past run, keyed for the canvas. */
function elapsedFromRun(run: WorkflowRunOutcome): Record<string, number> {
  const out: Record<string, number> = {};
  for (const node of run.nodes ?? []) out[node.nodeId] = node.elapsedMs;
  return out;
}

/** The node a run failed at, when its trail names one (issue #371).
 *
 * The engine reports a failing node as an `error` step before the run ends, so
 * this is exact rather than inferred. `null` when the run failed with no
 * errored node — a graph that would not compile, a capability that could not be
 * built — where naming a node would be a fabrication. */
function failedNodeOf(run: WorkflowRunOutcome): string | null {
  return (run.nodes ?? []).find((n) => n.status !== "ok")?.nodeId ?? null;
}

/** Lays a saved graph out left→right by longest-path depth, stacking siblings
 * vertically within each layer. Cycles are bounded by an iteration cap, so a
 * back edge never loops forever.
 *
 * `runStates` / `elapsed` (issue #371) tint each node with what the run on
 * screen did. Both default to empty, which is the resting canvas — identical to
 * how it rendered before #371. */
function layout(
  graph: WorkflowGraph,
  runStates: Record<string, NodeRunState> = {},
  elapsed: Record<string, number> = {},
): { nodes: Node<WorkflowNodeData>[]; edges: Edge[] } {
  const depth = new Map<string, number>(graph.nodes.map((n) => [n.id, 0]));
  for (let i = 0; i < graph.nodes.length; i++) {
    let changed = false;
    for (const e of graph.edges) {
      const d = (depth.get(e.from) ?? 0) + 1;
      if (d > (depth.get(e.to) ?? 0)) {
        depth.set(e.to, d);
        changed = true;
      }
    }
    if (!changed) break;
  }

  const rowInLayer = new Map<number, number>();
  const nodes: Node<WorkflowNodeData>[] = graph.nodes.map((n) => {
    const layer = depth.get(n.id) ?? 0;
    const row = rowInLayer.get(layer) ?? 0;
    rowInLayer.set(layer, row + 1);
    const meta = nodeKindMeta(n.kind);
    return {
      id: n.id,
      type: "oc",
      position: { x: layer * COL_GAP, y: row * ROW_GAP },
      data: {
        kind: n.kind,
        name: n.name,
        // Agent nodes surface their roster id; otherwise the node's summary.
        summary: n.summary ?? (n.agent ? `Agent: ${n.agent}` : ""),
        emoji: meta.emoji,
        color: meta.color,
        runState: runStates[n.id],
        elapsedMs: elapsed[n.id],
      },
    };
  });

  const edges: Edge[] = graph.edges.map((e, i) => ({
    id: `${e.from}-${e.to}-${i}`,
    source: e.from,
    target: e.to,
    label: e.label,
    animated: true,
  }));

  return { nodes, edges };
}

/** A single agent reply extracted from a node's `items[].json`. */
interface NodeMessage {
  text: string | null;
  agentRef: string | null;
}

/** One node's readable, shape-checked result, ready to render. */
interface NodeResult {
  id: string;
  name: string;
  /** The condition branch taken (`null` when the node isn't a branch point). */
  port: string | null;
  messages: NodeMessage[];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

/** A non-empty trimmed string, else `null` (defensive against non-strings). */
function nonEmptyString(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value : null;
}

/** Pull a field from an item's `json`, preferring the OUTERMOST value and
 * falling back to the NESTED `json.json.<key>` the engine sometimes emits.
 * Handles the observed shape where `json` carries both a top-level `text` and
 * a nested `json.json.text` — the outer one wins. */
function readNested(json: unknown, key: string): string | null {
  if (!isRecord(json)) return null;
  const outer = nonEmptyString(json[key]);
  if (outer) return outer;
  const inner = json.json;
  if (isRecord(inner)) return nonEmptyString(inner[key]);
  return null;
}

/** Safely parse the engine's run output into per-node results, ordered by the
 * loaded graph when available (falling back to the map's insertion order).
 * Returns `null` when `output` doesn't match the expected `{ nodes: {…} }`
 * shape, signalling the caller to fall back to the raw JSON dump. Every access
 * is guarded — `output` is typed `unknown` and older/edge runs may be a plain
 * string, missing `nodes`, or carry malformed node values. */
function parseRunNodes(
  output: unknown,
  graph: WorkflowGraph | null,
): NodeResult[] | null {
  if (!isRecord(output) || !isRecord(output.nodes)) {
    console.debug(
      "[WorkflowsView] run output missing a `nodes` map; showing raw JSON",
      output,
    );
    return null;
  }
  const nodes = output.nodes;

  // Order by the graph's node order when we have it, then append any node ids
  // present in the output but not in the graph (in the map's insertion order).
  const graphOrder = graph?.nodes.map((n) => n.id) ?? [];
  const orderedIds = [
    ...graphOrder.filter((id) => id in nodes),
    ...Object.keys(nodes).filter((id) => !graphOrder.includes(id)),
  ];

  const nameById = new Map(graph?.nodes.map((n) => [n.id, n.name]) ?? []);

  const results: NodeResult[] = orderedIds.map((id) => {
    const raw = nodes[id];
    const items = isRecord(raw) && Array.isArray(raw.items) ? raw.items : [];
    const messages: NodeMessage[] = items
      .map((item) => {
        const json = isRecord(item) ? item.json : undefined;
        return {
          text: readNested(json, "text"),
          agentRef: readNested(json, "agent_ref"),
        };
      })
      .filter((m) => m.text || m.agentRef);
    const port = isRecord(raw) ? nonEmptyString(raw.port) : null;
    return { id, name: nameById.get(id) ?? id, port, messages };
  });

  return results;
}
