import { createContext, useContext, useMemo, type ReactNode } from "react";

import type { ReferralWorking } from "./channels";

/**
 * Which crossings are still being had, by the id of the row they fold onto.
 *
 * ## Why this exists
 *
 * The chip reads "asked @amendments · 1 message" from the moment the question
 * is journaled, which describes a crossing that finished after one reply. It
 * had not finished; the pair were still talking, and the count was whatever had
 * landed so far. A running exchange presented as a completed one is the fault —
 * the stale count was only how it showed.
 *
 * ## Why a context rather than a prop
 *
 * The chip is a leaf rendered from three places — the timeline, the thread
 * panel, and an agent's session — and none of them knows which desk it is
 * drawing, which is the key this state is held under. Threading it would mean
 * new props through three chains to reach one label.
 *
 * ## Why the row id, and not the two names
 *
 * `pair_conversation` sorts the pair, so two seats share ONE thread for every
 * crossing they ever have, whichever of them asked (`dm:amendments+
 * cancellations` held fourteen crossings in both directions in one live run).
 * Matching on the names would therefore mark every past crossing between those
 * two as running. The row a crossing folds onto is unique to it.
 */
type Running = {
  /** Ids of the rows whose crossing is still running. */
  rows: ReadonlySet<string>;
  /** The crossing each desk is waiting on, for a surface that draws the desk. */
  byDesk: Readonly<Record<string, ReferralWorking>>;
};

const RunningCrossings = createContext<Running>({ rows: new Set(), byDesk: {} });

export function ReferralRunningProvider({
  rows,
  byDesk,
  children,
}: {
  /** Ids of the rows whose crossing is still running. */
  rows: readonly string[];
  /** The same crossings keyed by the desk that asked. */
  byDesk: Record<string, ReferralWorking>;
  children: ReactNode;
}) {
  // Keyed on content, not array identity: the shell rebuilds this list whenever
  // its state changes, and a fresh Set each time would re-render every chip on
  // screen for a crossing that did not change.
  const key = rows.join(" ");
  const deskKey = Object.keys(byDesk).sort().join(" ");
  const value = useMemo(
    () => ({ rows: new Set(rows), byDesk }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [key, deskKey],
  );
  return <RunningCrossings.Provider value={value}>{children}</RunningCrossings.Provider>;
}

/** Whether the crossing folded onto `rowId` is still being had. */
export function useCrossingRunning(rowId: string | undefined): boolean {
  const { rows } = useContext(RunningCrossings);
  return rowId !== undefined && rows.has(rowId);
}

/**
 * The crossing a desk is waiting on, for the surface that draws the desk.
 *
 * A desk answering as a room shows its round band rather than the timeline's
 * working row, so the band is where a crossing has to say it is happening —
 * otherwise a room shows its lanes and nothing else while a seat spends a
 * model turn asking another desk (#2341 live report).
 */
export function useDeskCrossing(deskId: string | undefined): ReferralWorking | undefined {
  const { byDesk } = useContext(RunningCrossings);
  return deskId ? byDesk[deskId] : undefined;
}
