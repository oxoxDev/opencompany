import { useEffect, useMemo, useState } from "react";
import { Loader2 } from "lucide-react";

import type { OpenCompanyClient } from "@/api/client";
import { PageHeader } from "@/components/page-header";
import { listTasks } from "@/api/tasks";
import { isGeneralDesk } from "@/lib/desks";
import { startVisiblePolling } from "@/lib/visible-poll";
import { CommsGraphView } from "@/views/comms/CommsGraphView";
import {
  applyObservations,
  boardObservations,
  neighbourhood,
  structuralGraph,
  type CommsAgent,
  type CommsDesk,
  type CommsObservation,
} from "@/views/comms/model";

/**
 * Who talks to whom, who may, and who made whom.
 *
 * # Read-only, and a snapshot is the authority
 *
 * The same discipline the Observatory documents: the fetched roster and desk
 * list are the truth, and a live frame **never merges into them** — it only adds
 * an observation alongside. Two frames collapsing inside one React batch still
 * mean "re-read" exactly once, whereas two payloads collapsing loses one.
 *
 * # What this can and cannot say today
 *
 * The structural half is exact: `delegates_to` and desk membership come straight
 * off the manifest. The observed half is derived — the host emits no event for a
 * hand-off or a spawn, so the console joins tool-call frames with dispatch
 * frames. That means a spawn whose arguments were redacted shows as an agent
 * appearing with no edge to its creator, and the graph says so rather than
 * guessing. Closing that gap is a host change (`WorkHandedOff`, `TeammateAdded`),
 * not a console one.
 */
export function CommsView({
  client,
  company,
  observations = [],
}: {
  client: OpenCompanyClient;
  company: string | null;
  /**
   * What the live stream has said so far, folded by the shell.
   *
   * Passed in rather than subscribed here, so this view stays a pure function of
   * a snapshot plus a list — which is what makes it renderable from a fixture.
   */
  observations?: CommsObservation[];
}) {
  const [agents, setAgents] = useState<CommsAgent[] | null>(null);
  const [desks, setDesks] = useState<CommsDesk[] | null>(null);
  const [board, setBoard] = useState<CommsObservation[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);

  const [tick, setTick] = useState(0);

  /*
   * A visible poll rather than an SSE subscription, and deliberately.
   *
   * The host now journals every structural change — `TeammateAdded`,
   * `DeskCreated`, `DeskMembersChanged` — so this view *could* subscribe. But
   * the Observatory's rule applies with more force here: a frame never merges
   * into the snapshot, it only means "re-read". Once that is true, a poll and a
   * subscription do the same job, and the poll needs no state threaded down from
   * the shell — which is the coupling the room store exists to undo, and not one
   * worth adding a second instance of.
   *
   * `startVisiblePolling` stops while the tab is hidden and re-reads once on the
   * way back, so a console left open costs nothing.
   */
  useEffect(() => startVisiblePolling(() => setTick((n) => n + 1), 15_000), []);

  useEffect(() => {
    let live = true;
    setError(null);
    Promise.all([
      client.listTeam(company),
      client.listDesks(company),
      // The board is where a hand-off is durably recorded, so it is read here
      // rather than reconstructed from the live stream — which arrives redacted
      // and does not survive a reload. A host without the route simply
      // contributes no observed edges.
      listTasks(client, company).catch(() => []),
    ])
      .then(([team, deskList, cards]) => {
        if (!live) return;
        setBoard(boardObservations(cards));
        if (!live) return;
        setAgents(
          team.map((m) => ({
            id: m.id,
            name: m.name ?? m.id,
            role: m.role,
            isOrchestrator: m.isOrchestrator === true,
            delegatesTo: m.delegatesTo,
          })),
        );
        setDesks(
          deskList
            .filter((d) => !isGeneralDesk(d))
            .map((d) => ({ id: d.id, name: d.name, members: d.members })),
        );
      })
      .catch((e: unknown) => {
        if (!live) return;
        setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      live = false;
    };
  }, [client, company, tick]);

  const graph = useMemo(() => {
    if (!agents || !desks) return null;
    return applyObservations(structuralGraph(agents, desks), [
      ...board,
      ...observations,
    ]);
  }, [agents, desks, board, observations]);

  const shown = useMemo(
    () => (graph && selected ? neighbourhood(graph, selected) : graph),
    [graph, selected],
  );

  return (
    <div className="p-4">
      <PageHeader
        title="Activity"
        description="Who may reach whom, who has, and who created whom."
      />
      {error ? (
        <p className="text-sm text-muted-foreground">{error}</p>
      ) : !shown ? (
        <Loader2 aria-hidden className="size-4 animate-spin text-muted-foreground" />
      ) : (
        <>
          {selected && (
            <button
              type="button"
              className="mb-2 text-xs text-muted-foreground underline decoration-dotted"
              onClick={() => setSelected(null)}
            >
              Showing one neighbourhood — show everything
            </button>
          )}
          <CommsGraphView graph={shown} selected={selected} onSelect={setSelected} />
        </>
      )}
    </div>
  );
}
