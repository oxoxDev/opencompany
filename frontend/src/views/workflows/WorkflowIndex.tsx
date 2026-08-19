// Issue #303: the company's workflows as CARDS or as a LIST.
//
// Issue #1110 promoted it from a panel that opened over the canvas to the body
// of `#/workflows` itself, which is why it no longer draws a header: the tab's
// own toolbar carries the title, the count, the Cards/List toggle and New
// workflow, and this renders the list under it.
//
// Why this exists at all: before #303 the only way to see the company's
// workflows was the `<Select>` in the toolbar — a combobox shows one workflow at
// a time and carries nothing but a name, so "which of these is failing?" had no
// answer short of selecting each in turn and reading its chip.
//
// WHAT A CARD IS ALLOWED TO SAY is the whole design constraint here. Two
// requests back this surface — `GET …/workflows` (id, name, description,
// editable) and `GET …/workflows/runs` (the company-wide run journal) — and a
// card renders strictly what those two carry. In particular it does NOT show a
// schedule or a node count: both live only in the full graph, which is a
// per-workflow `GET …/workflows/{wid}`, and fetching one per card would be an
// N+1 on every render of this panel. Showing "no schedule" without having
// looked would be a lie, so the card says nothing at all about schedules.

import type { WorkflowRunOutcome, WorkflowSummary } from "@/api/workflows";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";

import { failedNodeOf } from "./graph";
import { pendingCount, relativeTime, runTone, undeliveredCount } from "./run-health";

/** Which rendering the index is showing. Persisted by the caller. */
export type IndexMode = "cards" | "list";

/** How many recent runs the health strip shows per workflow. */
const STRIP_RUNS = 5;

export function WorkflowIndex({
  workflows,
  runsByWorkflow,
  onSelect,
  mode,
  loading,
  runsLoaded,
}: {
  workflows: WorkflowSummary[];
  /** The recent runs of each workflow, newest first, from the company-wide page. */
  runsByWorkflow: Map<string, WorkflowRunOutcome[]>;
  /**
   * Open one workflow. Issue #1110: there is no `selectedId` counterpart, and
   * that is the point — selecting IS leaving this surface, so nothing here is
   * ever "the selected one". The cards used to carry `aria-pressed`, which
   * announced every one of them to a screen reader as a toggle that was off;
   * they are links to a page, and they read as buttons that do something now.
   */
  onSelect: (id: string) => void;
  /**
   * Cards or list. Owned and rendered by the caller (issue #1110): the index is
   * the whole body of `#/workflows` now, so its heading and the tab's heading
   * were the same bar drawn twice — "Workflows 7" over "All workflows 7", with
   * the toggle stranded under the duplicate. The control moved up into the one
   * toolbar; this component reads the answer and renders it.
   */
  mode: IndexMode;
  loading: boolean;
  /**
   * Whether the run page has come back yet.
   *
   * Load-bearing for honesty: until it has, every workflow legitimately has no
   * runs to show, and rendering "No recent runs" would state as fact something
   * we have not asked about yet. The health line stays blank instead.
   */
  runsLoaded: boolean;
}) {
  return (
    <div className="h-full overflow-auto p-4" data-testid="workflow-index">
      {loading ? (
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
          {[0, 1, 2].map((i) => (
            <Skeleton key={i} className="h-28 rounded-xl" />
          ))}
        </div>
      ) : workflows.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          This company has no saved workflows yet.
        </p>
      ) : mode === "cards" ? (
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
          {workflows.map((w) => (
            <WorkflowCard
              key={w.id}
              workflow={w}
              runs={runsByWorkflow.get(w.id) ?? []}
              runsLoaded={runsLoaded}
              onSelect={() => onSelect(w.id)}
            />
          ))}
        </div>
      ) : (
        <div className="divide-y rounded-xl border">
          {workflows.map((w) => (
            <WorkflowRow
              key={w.id}
              workflow={w}
              runs={runsByWorkflow.get(w.id) ?? []}
              runsLoaded={runsLoaded}
              onSelect={() => onSelect(w.id)}
            />
          ))}
        </div>
      )}
    </div>
  );
}

/** One workflow as a card: what it is, whether the console can change it, and
 * how its recent runs went. */
function WorkflowCard({
  workflow,
  runs,
  runsLoaded,
  onSelect,
}: {
  workflow: WorkflowSummary;
  runs: WorkflowRunOutcome[];
  runsLoaded: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      data-testid="workflow-card"
      className="flex flex-col gap-2 rounded-xl border bg-card/60 p-3 text-left transition hover:bg-accent/40"
    >
      <div className="flex items-start justify-between gap-2">
        <span className="min-w-0 truncate text-sm font-semibold">{workflow.name}</span>
        {/* Issue #259's rule, repeated exactly: only an explicit `false` is a
            refusal — a host predating it sends no field, and `undefined` must
            not render as "you can't edit this". */}
        {workflow.editable === false && (
          <Badge
            variant="outline"
            className="h-4 shrink-0 px-1.5 text-3xs font-normal"
            title="Defined by a file in the company source tree, so it can't be changed or removed from the console."
          >
            in source
          </Badge>
        )}
      </div>

      {workflow.description ? (
        <p className="line-clamp-2 text-xs text-muted-foreground">{workflow.description}</p>
      ) : (
        <p className="text-xs italic text-muted-foreground/70">No description.</p>
      )}

      <div className="mt-auto space-y-1.5 pt-1">
        <HealthLine runs={runs} runsLoaded={runsLoaded} />
        <RunStrip runs={runs} />
      </div>
    </button>
  );
}

/** One workflow as a list row — the same facts, laid out for scanning down a
 * column rather than across a grid. */
function WorkflowRow({
  workflow,
  runs,
  runsLoaded,
  onSelect,
}: {
  workflow: WorkflowSummary;
  runs: WorkflowRunOutcome[];
  runsLoaded: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      data-testid="workflow-list-row"
      className="flex w-full flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-left transition hover:bg-accent/40"
    >
      <span className="min-w-0 flex-1 truncate text-sm font-medium">{workflow.name}</span>
      {workflow.editable === false && (
        <Badge
          variant="outline"
          className="h-4 shrink-0 px-1.5 text-3xs font-normal"
          title="Defined by a file in the company source tree, so it can't be changed or removed from the console."
        >
          in source
        </Badge>
      )}
      <span className="hidden min-w-0 flex-1 truncate text-xs text-muted-foreground sm:block">
        {workflow.description ?? ""}
      </span>
      <span className="shrink-0">
        <HealthLine runs={runs} runsLoaded={runsLoaded} />
      </span>
    </button>
  );
}

/** How the most recent run went, in one line.
 *
 * The three terminal readings issue #383 separated are all distinguishable
 * here — failed, stopped by an operator, and finished — because {@link runTone}
 * is the same function the history rows use. */
function HealthLine({ runs, runsLoaded }: { runs: WorkflowRunOutcome[]; runsLoaded: boolean }) {
  const last = runs[0];

  if (!last) {
    // Nothing yet, and the two reasons for that are NOT the same thing.
    if (!runsLoaded) {
      return <span className="text-2xs text-muted-foreground/60">Loading runs…</span>;
    }
    // "No recent runs", never "never run". The company-wide run page is cut by
    // a limit, so a workflow whose last run has scrolled off it is
    // indistinguishable from one that has never run — and claiming the stronger
    // of the two would be exactly the false negative issue #228 was about.
    // Selecting the workflow re-reads its history scoped server-side, which
    // does answer the stronger question.
    return (
      <span
        className="text-2xs text-muted-foreground"
        title="No runs in the recent company-wide page. Open the workflow to read its own run history."
      >
        No recent runs
      </span>
    );
  }

  const tone = runTone(last);
  const undelivered = undeliveredCount(last.deliveries);
  const pending = pendingCount(last.deliveries);
  const failedNode = last.error ? failedNodeOf(last) : null;

  return (
    <span className="flex flex-wrap items-center gap-1.5 text-2xs">
      {/* `runTone` owns the running reading too, so this reads the same way as
          the last-run chip and the history rows rather than being a second
          opinion that can drift from them. */}
      <span className={`size-1.5 rounded-full ${tone.dot}`} />
      <span className="text-muted-foreground">
        {last.scheduled ? "Scheduled" : "Manual"} run {tone.label}
        {failedNode ? ` at “${failedNode}”` : ""}
      </span>
      <span className="text-muted-foreground/70">· {relativeTime(last.atMillis)}</span>
      {undelivered > 0 && (
        <Badge
          variant="outline"
          className="h-4 px-1.5 text-3xs font-normal border-status-failed/40 bg-status-failed-soft"
        >
          {undelivered} not delivered
        </Badge>
      )}
      {pending > 0 && (
        <Badge
          variant="outline"
          className="h-4 px-1.5 text-3xs font-normal border-status-blocked/40 bg-status-blocked-soft"
        >
          {pending} awaiting approval
        </Badge>
      )}
    </span>
  );
}

/** The last few runs as dots, newest first — the "is this flaky or is this
 * broken?" reading a single last-run status cannot give.
 *
 * Renders nothing when there are no runs: an empty row of placeholder dots
 * would imply runs we do not have. */
function RunStrip({ runs }: { runs: WorkflowRunOutcome[] }) {
  const recent = runs.slice(0, STRIP_RUNS);
  if (recent.length === 0) return null;
  return (
    <span className="flex items-center gap-1" data-testid="workflow-card-strip">
      {recent.map((run) => {
        const tone = runTone(run);
        return (
          <span
            key={run.seq}
            className={`size-1.5 rounded-full ${tone.dot}`}
            title={`${run.scheduled ? "Scheduled" : "Manual"} run — ${tone.label} · ${relativeTime(
              run.atMillis,
            )}`}
          />
        );
      })}
      <span className="ml-0.5 text-3xs text-muted-foreground/70">
        last {recent.length}
      </span>
    </span>
  );
}
