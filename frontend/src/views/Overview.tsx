import { lazy, Suspense, useCallback, useEffect, useMemo, useState } from "react";
import { RefreshCw } from "lucide-react";

import { listPeople, type Person } from "@/api/auth";
import type { OpenCompanyClient } from "@/api/client";
import { listMemory, type MemoryEntry } from "@/api/memory";
import { listTasks, type Task } from "@/api/tasks";
import type { DeskDto } from "@/api/types";
import { getWorkflow, listWorkflows, type WorkflowGraph } from "@/api/workflows";
import { fromDto, starterTeam, type TeamMember } from "@/lib/team";
import { adapt, buildMemoryGraph } from "./overview/kg/adapter";
import { buildKnowledgeGraph } from "./overview/kg/model";
import { ownedBy } from "./overview/pulse";

// The graph carries the force simulation and every detail card with it. Its own
// chunk means a cold load paints the frame before the physics arrives.
const KnowledgeGraph = lazy(() =>
  import("./overview/kg/KnowledgeGraph").then((m) => ({ default: m.KnowledgeGraph })),
);

interface Props {
  client: OpenCompanyClient;
  company: string | null;
}

/** Everything the graph is drawn from — a snapshot, taken on demand. */
interface Sources {
  tasks: Task[];
  team: TeamMember[];
  people: Person[];
  memories: MemoryEntry[];
  /** The company's desks: the graph's department pillars. */
  desks: DeskDto[];
  /** Its saved workflow graphs, whole — nodes and edges, not just names. */
  workflows: WorkflowGraph[];
  /** When this snapshot was taken (epoch millis); `null` before the first read. */
  fetchedAt: number | null;
}

const EMPTY: Sources = {
  tasks: [],
  team: starterTeam(),
  people: [],
  memories: [],
  desks: [],
  workflows: [],
  fetchedAt: null,
};

/**
 * The command centre: the company's knowledge graph, and nothing else.
 *
 * The page is the graph — no header, no strip, no top bar (the shell hides its
 * own for this view). The company sits at the core, its desks are the pillars,
 * the jobs hang off each pillar, the teammate who does each job sits above it,
 * and the tools that teammate actually holds are the outer ring.
 *
 * Every one of those is a value the host answers for (issue #601). The one
 * thing this console decides for itself is which pillar a workflow hangs off,
 * because the company scopes a flow to itself rather than to a desk — see
 * `DERIVED_NOTICE` in `kg/adapter.ts`.
 *
 * ## Why this is a snapshot with a button rather than a live view
 *
 * One paint of this page is six reads — the board, the roster, the people, the
 * memory, the desks, the workflow list — plus one more per saved workflow. On a
 * timer that is a standing cost per open tab, for a picture that changes when
 * an operator does something, not on the clock. So the page fetches once and
 * **says** it fetched once, with a control that re-reads on demand: the
 * question issue #601 raises is answered out loud rather than by omission, and
 * the operator is never looking at stale data they had no way to notice.
 */
export function Overview({ client, company }: Props) {
  const [sources, setSources] = useState<Sources>(EMPTY);
  // Bumped by the refresh control; re-runs the read below and nothing else.
  const [reload, setReload] = useState(0);
  const [loading, setLoading] = useState(true);
  const refresh = useCallback(() => setReload((n) => n + 1), []);

  useEffect(() => {
    let live = true;
    setLoading(true);
    void (async () => {
      const [tasks, roster, people, memories, desks, flowList] = await Promise.all([
        listTasks(client, company).catch(() => [] as Task[]),
        client.listTeam(company).catch(() => null),
        // Only an admin may list people; a member just gets no humans on the
        // graph, which is the right amount of information for them to have.
        listPeople(client, company).catch(() => [] as Person[]),
        // The company's real durable memory (issue #36). A host without the
        // surface draws no constellation rather than a seeded one — the graph
        // must never claim the company remembers something it doesn't.
        listMemory(client, company).catch(() => [] as MemoryEntry[]),
        // Desks are the pillars. A host that cannot answer draws no pillars
        // rather than invented ones; the notice below says which it is.
        client.listDesks(company).catch(() => [] as DeskDto[]),
        listWorkflows(client, company).catch(() => []),
      ]);
      if (!live) return;

      // The list read names the flows; only the graph read carries their nodes
      // and edges, which is what the stages ring is drawn from.
      const workflows = await Promise.all(
        flowList.map((summary) =>
          getWorkflow(client, company, summary.id).catch(
            // A name-only entry has no saved graph to fetch (and a read can
            // simply fail). The company still declares the flow, so it is drawn
            // with no stages rather than dropped — a flow missing from the
            // wheel would be a quieter lie than one drawn empty.
            (): WorkflowGraph => ({
              id: summary.id,
              name: summary.name,
              description: summary.description,
              nodes: [],
              edges: [],
            }),
          ),
        ),
      );
      if (!live) return;

      setSources({
        tasks,
        team: roster?.length ? roster.map(fromDto) : starterTeam(),
        people,
        memories,
        desks,
        workflows,
        fetchedAt: Date.now(),
      });
      setLoading(false);
    })();
    return () => {
      live = false;
    };
  }, [client, company, reload]);

  const adapted = useMemo(
    () =>
      adapt({
        members: sources.team,
        tasks: sources.tasks,
        people: sources.people,
        desks: sources.desks,
        workflows: sources.workflows,
        ownedBy,
      }),
    [sources],
  );

  const graph = useMemo(
    () =>
      buildKnowledgeGraph(
        adapted.agents,
        adapted.departments,
        adapted.people,
        adapted.tasks,
        adapted.workflows,
      ),
    [adapted],
  );

  const memoryGraph = useMemo(() => buildMemoryGraph(sources.memories), [sources.memories]);

  // A company that has declared no desks has no pillars to draw, and everybody
  // lands on the one "No desk" group. Saying so is the difference between an
  // empty state and a broken-looking wheel — and it is the state to say out
  // loud rather than fill with five invented departments (issue #601).
  const noDesks = sources.fetchedAt !== null && sources.desks.length === 0;

  return (
    // The whole viewport: the shell hides its top bar for this view, so there
    // is nothing above to subtract.
    <div
      className="oc-kg relative h-svh min-h-0 w-full min-w-0 overflow-hidden"
      // The guided tour's Overview stop anchors here. It used to spotlight the
      // quick-action row this page had before it became the graph; the graph is
      // the page now, so the graph is what gets spotlighted.
      data-tour="overview-graph"
    >
      {noDesks && (
        <p
          role="status"
          className="pointer-events-none absolute left-1/2 top-3 z-10 flex max-w-[calc(100%-1.5rem)] -translate-x-1/2 items-center gap-1.5 rounded-md border bg-background/90 px-2.5 py-1.5 text-xs text-muted-foreground shadow-sm backdrop-blur"
        >
          <span className="min-w-0 truncate">
            This company hasn&apos;t set up any desks, so everyone is drawn on one group.
          </span>
        </p>
      )}
      {/* The snapshot line: what the page is, when it was taken, and the way to
          take another. The graph is not live and says so, rather than leaving an
          operator to assume a stale wheel is the current company. */}
      <div className="absolute right-3 top-3 z-10 flex items-center gap-1.5 rounded-md border bg-background/90 px-2 py-1 text-[11px] text-muted-foreground shadow-sm backdrop-blur">
        <span className="truncate">
          {sources.fetchedAt === null
            ? "Loading…"
            : `Snapshot ${new Date(sources.fetchedAt).toLocaleTimeString([], {
                hour: "2-digit",
                minute: "2-digit",
              })}`}
        </span>
        <button
          type="button"
          onClick={refresh}
          disabled={loading}
          title="This page is a snapshot, not a live view. Re-read the company."
          aria-label="Refresh the graph"
          className="inline-flex items-center gap-1 rounded px-1 py-0.5 hover:bg-muted disabled:opacity-50"
        >
          <RefreshCw className={`size-3 ${loading ? "animate-spin" : ""}`} aria-hidden />
          Refresh
        </button>
      </div>
      <Suspense
        fallback={
          <div className="grid h-full place-items-center text-sm text-muted-foreground">
            Drawing the graph…
          </div>
        }
      >
        <KnowledgeGraph
          graph={graph}
          agents={adapted.agents}
          departments={adapted.departments}
          people={adapted.people}
          tasks={adapted.tasks}
          memory={memoryGraph}
          toolLabels={adapted.toolLabels}
        />
      </Suspense>
    </div>
  );
}
