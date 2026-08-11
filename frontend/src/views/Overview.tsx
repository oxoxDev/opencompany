import { lazy, Suspense, useEffect, useMemo, useState } from "react";

import { listPeople, type Person } from "@/api/auth";
import type { OpenCompanyClient } from "@/api/client";
import { listMemory, type MemoryEntry } from "@/api/memory";
import { listTasks, type Task } from "@/api/tasks";
import type { DeskDto } from "@/api/types";
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

/** Everything the graph is drawn from, fetched once per company. */
interface Sources {
  tasks: Task[];
  team: TeamMember[];
  /**
   * The company's desks — ring 1 (issue #486).
   *
   * Best-effort like every other source here. A host that cannot serve them
   * draws a graph with no pillars, which is the same picture as a company that
   * declares no desks. The org chart treats a failed `/desks` as a hard error
   * because desks *are* that page; here they are one ring of five, and failing
   * the whole graph over them would take the real rings down with them.
   */
  desks: DeskDto[];
  people: Person[];
  memories: MemoryEntry[];
}

const EMPTY: Sources = {
  tasks: [],
  team: starterTeam(),
  desks: [],
  people: [],
  memories: [],
};

/**
 * The command centre: the company's knowledge graph, and nothing else.
 *
 * The page is the graph — no header, no strip, no top bar (the shell hides its
 * own for this view). The company sits at the core, its desks are the pillars,
 * the jobs hang off each pillar, the teammate who does each job sits above it,
 * and their tools are the outer ring.
 *
 * The pillars are **declared**: they are the company's own desks (issue #486),
 * not the keyword-matched guess that used to stand there. So is the outer ring:
 * the tools are the grants the host resolved for each teammate (issue #601),
 * not a deal from the company's catalogue. What is still derived is the
 * workflow templates — there is no flow API. See `DERIVED_NOTICE` in
 * `kg/adapter.ts`; it is the standing caveat on what remains.
 */
export function Overview({ client, company }: Props) {
  const [sources, setSources] = useState<Sources>(EMPTY);

  useEffect(() => {
    let live = true;
    void (async () => {
      const [tasks, roster, desks, people, memories] = await Promise.all([
        listTasks(client, company).catch(() => [] as Task[]),
        client.listTeam(company).catch(() => null),
        // Ring 1 (issue #486). Best-effort: see `Sources.desks`.
        client.listDesks(company).catch(() => [] as DeskDto[]),
        // Only an admin may list people; a member just gets no humans on the
        // graph, which is the right amount of information for them to have.
        listPeople(client, company).catch(() => [] as Person[]),
        // The company's real durable memory (issue #36). A host without the
        // surface draws no constellation rather than a seeded one — the graph
        // must never claim the company remembers something it doesn't.
        listMemory(client, company).catch(() => [] as MemoryEntry[]),
      ]);
      if (!live) return;

      setSources({
        tasks,
        team: roster?.length ? roster.map(fromDto) : starterTeam(),
        desks,
        people,
        memories,
      });
    })();
    return () => {
      live = false;
    };
  }, [client, company]);

  const adapted = useMemo(
    () =>
      adapt({
        members: sources.team,
        desks: sources.desks,
        tasks: sources.tasks,
        people: sources.people,
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
