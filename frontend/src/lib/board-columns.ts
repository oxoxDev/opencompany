// The board's columns, read off the `tasks` ledger rather than kept here.
//
// # Why this replaced a literal list
//
// `TASK_COLUMNS` was a hand-maintained copy of the host's `BOARD_COLUMNS`, and
// its own comment admitted what that cost: *"a Rust test cannot see the TS
// list, so a column added on one side and not the other keeps this green."* A
// column that existed only here was one the host's write boundary refused; a
// column that existed only there was one the board silently never rendered, so
// its cards vanished with no error — the exact disappearance the column
// vocabulary was introduced to prevent.
//
// The host now declares columns once (`src/ledger/board.rs`), builds the
// `tasks` ledger's statuses from that table, and sends each one's `label` on the
// wire. So the console asks. A column added on the host appears here on the next
// read, correctly labelled, with no console release and nothing to keep in step.

import type { OpenCompanyClient } from "@/api/client";
import { listLedgers, type LedgerStatus, type LedgerSummary } from "@/api/ledgers";

/** The `tasks` ledger's slug — the board, as the ledger surface names it. */
export const BOARD_LEDGER = "tasks";

export interface TaskColumn {
  id: string;
  label: string;
  /** Whether a card here is finished. Only `done` is. */
  closed: boolean;
}

/**
 * A readable label for a status the host sent no label for.
 *
 * Used for the ledgers a company declares, whose statuses are already written
 * to be read (`open`, `at_risk`, `kept`), and as the last resort for a stored
 * card carrying a column this build has never heard of. It is deliberately not
 * used for the board: `in_progress` humanises to "In progress" by luck and
 * `todo` becomes "Todo", which is why the host sends the real labels.
 */
export function humanizeStatus(id: string): string {
  const words = id.trim().replace(/[_-]+/g, " ").trim();
  if (!words) return id;
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** A ledger status as a column, preferring the host's label. */
export function columnOf(status: LedgerStatus): TaskColumn {
  return {
    id: status.name,
    label: status.label?.trim() || humanizeStatus(status.name),
    closed: status.closed === true,
  };
}

/**
 * Every column a ledger declares, in declaration order.
 *
 * Declaration order is board order: the host's table is written left to right,
 * and a console that sorted these itself would put Done next to To-do the first
 * time somebody added a column.
 */
export function columnsOf(ledger: LedgerSummary): TaskColumn[] {
  return ledger.statuses.map(columnOf);
}

/**
 * The label for one column id, given the columns currently known.
 *
 * Falls back to humanising rather than to the raw id, so a card whose column
 * this build does not know still reads as words. Never invents a mapping: the
 * host's label wins whenever there is one.
 */
export function labelFor(columns: TaskColumn[], id: string): string {
  return columns.find((column) => column.id === id)?.label ?? humanizeStatus(id);
}

/**
 * The board's columns, fetched once per company.
 *
 * Returns `[]` until the read lands. Callers render labels through
 * {@link labelFor}, which humanises in the meantime — so a board that has not
 * yet heard from the host shows "In progress" rather than an empty header or a
 * flash of `in_progress`.
 */
export async function fetchBoardColumns(
  client: OpenCompanyClient,
  company: string,
): Promise<TaskColumn[]> {
  const list = await listLedgers(client, company);
  const board = list.ledgers.find((held) => held.slug === BOARD_LEDGER);
  return board ? columnsOf(board) : [];
}

// ---------------------------------------------------------------------------
// The rest of the board's presentation vocabulary
// ---------------------------------------------------------------------------
//
// These two arrived here when `tasks-sample.ts` was deleted with the board
// screen. That file was named for illustrative data the console outgrew years
// ago — its header still claimed "the console has no live task API yet" — and
// what actually survived in it were these: one board concern each. They live
// beside the columns because that is what they are about.

export type TaskPriority = "low" | "medium" | "high";

/**
 * The one column that offers the "+" add-task button (issue #206).
 *
 * New work enters the board in exactly one place. Offering `+` on every column
 * — as the board used to — let an operator create a card straight into
 * `in_progress`, `in_review`, or `done`, which either skips the dispatch edge
 * or fabricates a terminal state for work that never ran.
 */
export const ADD_TASK_COLUMN = "todo";

/**
 * Priority badges.
 *
 * These deliberately *do* use the status hues, unlike the category and kind
 * palettes elsewhere. Priority and status share one axis — how much this
 * wants your attention — so red-for-high and amber-for-medium reinforce the
 * vocabulary rather than competing with it, and `low` stays neutral for the
 * same reason `idle` does: nothing is being asked of anyone.
 */
export const PRIORITY_STYLES: Record<TaskPriority, string> = {
  high: "border-status-failed/30 bg-status-failed-soft text-status-failed-text",
  medium: "border-status-blocked/30 bg-status-blocked-soft text-status-blocked-text",
  low: "border-border bg-muted text-muted-foreground",
};

/**
 * The board columns that mean *a teammate is on this right now*.
 *
 * These are the two ids the host files under its "In flight" section
 * (`src/ledger/board.rs`): `planning` is a planning pass turning a card into a
 * brief, `in_progress` is an open attempt. `paused` and `in_review` are
 * deliberately not here — that module calls them "stopped, not finished", and
 * they are waiting on a person rather than on the teammate.
 *
 * Named ids rather than a flag read off the wire, because the section is the
 * one part of the column table the host does not send: `LedgerStatus` carries
 * `name`, `label` and `closed`, and nothing else.
 *
 * This is *not* the drift the header of this file warns about. That warning is
 * about a console-side copy of a table a company can extend; this table is
 * explicitly the one ledger a company may **not** declare — entering a column
 * here spends money, so `board.rs` fixes the set at six and says so. A host
 * that renames one of these reports every teammate idle, which is the honest
 * failure: it under-claims rather than inventing activity.
 */
export const IN_FLIGHT_COLUMNS = ["planning", "in_progress"];
