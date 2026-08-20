// What each teammate is carrying, derived from the board rather than reported.
//
// The Company page's cards say two things about a teammate that no host field
// answers (issue #1141): whether they are working, and how much is on them. The
// roster read carries neither — `TeamMemberDto` is identity, budget, tier,
// tools and desks — and there is no presence plane to ask.
//
// So both are **derived from the board the console already reads**, and derived
// narrowly enough to stay true:
//
//   * open work is a card in a column the *host* says is not `closed`, so
//     "open" means whatever the host's column table means by it;
//   * working is a card in one of [`IN_FLIGHT_COLUMNS`], which is the only
//     state that says an attempt or a planning pass is actually running.
//
// # What this deliberately does not do
//
// **A card assigned to a desk is not counted against that desk's members.** A
// task's `assignee` is a canonical id that is either a desk id or a teammate id
// (issue #263), and the host's own `AssigneeResolution` refuses to rewrite a
// desk assignment to that desk's lead — `links_working_agent` is true only for
// `Unassigned | Agent(_)`, precisely so the card stays the desk's. Attributing
// it here would undo that on screen and put work on a person nobody gave it to.
//
// **Nothing is invented for a host that cannot answer.** No columns means an
// empty map, and a caller with no entry for a teammate draws no status and no
// count — not a zero, which would claim the teammate is free.

import type { Task } from "@/api/tasks";
import { IN_FLIGHT_COLUMNS, type TaskColumn } from "@/lib/board-columns";

/** Whether a teammate has something running right now. */
export type TeammateStatus = "idle" | "working";

/** One teammate's load, as the cards render it. */
export interface Workload {
  /** Open cards assigned to this teammate by id — never a desk's. */
  open: number;
  status: TeammateStatus;
}

/**
 * Open cards and running state per assignee id.
 *
 * Keyed by the **raw** `assignee` value, which is the roster teammate id for a
 * teammate's card and the desk id for a desk's — so a caller looking a teammate
 * up by their roster id gets only their own work, and a desk's cards are simply
 * never found under it.
 *
 * Returns an empty map when the column vocabulary is unknown. A card's column
 * is meaningless without it: every id would look neither closed nor in flight,
 * so every teammate would read as idle-with-work, which is a claim rather than
 * an absence.
 *
 * A column the host did not declare — a card stored under an id this build has
 * never heard of — counts as **open but not in flight**. It is outstanding
 * work by any reading, and nothing about an unrecognised id says an attempt is
 * running.
 */
export function workloadByAssignee(
  tasks: Task[],
  columns: TaskColumn[],
): Map<string, Workload> {
  const loads = new Map<string, Workload>();
  if (columns.length === 0) return loads;

  const closed = new Set(columns.filter((column) => column.closed).map((column) => column.id));

  for (const task of tasks) {
    // `""` is the unassigned wire value — a real choice that hands the card to
    // the orchestrator, and nobody's personal load.
    const assignee = task.assignee?.trim();
    if (!assignee) continue;
    if (closed.has(task.column)) continue;

    const held = loads.get(assignee) ?? { open: 0, status: "idle" as TeammateStatus };
    held.open += 1;
    if (IN_FLIGHT_COLUMNS.includes(task.column)) held.status = "working";
    loads.set(assignee, held);
  }

  return loads;
}
