// A board over any ledger: columns from the host, cards from the caller.
//
// # What this owns, and what it deliberately does not
//
// It owns the **board**: the columns and their order, the counts, the drag
// mechanics, and the three fixes behind issue #334 that a re-implementation
// loses every time (see below). It does not own the **card**. `renderCard` is a
// slot, and that is the whole reason one component can serve both the task board
// and a ledger a company declared this morning.
//
// The alternative — a card built from field roles — was tried and is wrong. A
// task card carries a priority badge, an assignee, a plan badge, an output link,
// a workflow chip and a Resume button, none of which is a ledger field and all
// of which come off the `Task` record rather than the ledger's projection of it.
// A generic renderer would have had to either drop them or grow a special case
// per ledger, and the second is just this slot with more steps.
//
// # The three things a re-implementation loses
//
// Issue #334 was reported as *"a card cannot be moved out of In review"*, and
// three separate things produced it. They live here, once, so that every board
// in the console has them:
//
// 1. **The board scrolls itself while a drag sits on its edge.** HTML5
//    drag-and-drop does not scroll a nested scroll container, so a column parked
//    off-screen cannot be reached by the gesture at all.
// 2. **A drop that misses every column says so.** The pixels between and around
//    the columns used to swallow the whole gesture without a word, which is
//    indistinguishable from a frozen app.
// 3. **The trailing gutter exists.** A flex scroll container drops its
//    `padding-inline-end`, so without the spacer the last column hugs the edge
//    and a near miss becomes the common case.
//
// # Empty columns collapse to a rail (issue #1101)
//
// A healthy company's board reads as dead. To-do, Planning and In progress are
// the three columns that fit an ordinary window, and they are exactly the three
// that empty out first — so an operator opens Tasks, gets three confident
// zeros, and never learns that the 101 cards they came for are two columns off
// the right edge. The board that has moved the most work is the board that
// looks the most finished.
//
// So a column holding nothing shrinks to a ~40px rail — its label set vertically
// and its zero underneath — and the columns holding the work take the room back.
// Three properties keep that from breaking the board's main gesture, and each
// one is a line somebody will be tempted to delete:
//
// * **A rail is still a whole drop target.** Empty columns are precisely the
//   ones you drag *into*: To-do is where returned work lands, In progress is
//   where a card is handed to its assignee. The drop handlers sit on the column
//   wrapper, which is the element that shrinks — never on the expanded body.
// * **A rail opens under the drag.** `over` already tracks the column the
//   pointer is on, so the same state that draws the drop highlight suppresses
//   the collapse. Dropping into an empty column looks the way it always did.
// * **Nothing collapses unless something else is populated.** A board that is
//   empty everywhere would otherwise render as six rails and no board, which is
//   a worse answer to "show me the work" than the one this fixes.
//
// Clicking a rail pins it open, and a pinned column stays open until the
// operator collapses it again. Nothing else may re-collapse it: a column that
// folded itself back up while somebody was reading it would be this bug's
// mirror image.

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { ChevronsLeft } from "lucide-react";

import type { TaskColumn } from "@/lib/board-columns";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

/**
 * The least a board needs to know about a row: which one it is.
 *
 * Which *column* it is in is asked for separately
 * ({@link BoardProps.statusOf}) rather than required as a property, because the
 * two things this board serves spell it differently — a `Task` calls it
 * `column`, a `LedgerEntry` calls it `status` — and making either side rename
 * its own field to satisfy a board would be the board deciding what its callers'
 * data is called. Everything else is the caller's, and reaches the screen
 * through {@link BoardProps.renderCard}.
 */
export interface BoardRow {
  id: string;
}

/**
 * How near the board's left or right edge a drag has to come before the board
 * starts scrolling itself, and how fast it goes once hard against that edge.
 *
 * The board is a horizontal scroller and, at six columns, wider than an
 * ordinary window: the last column sits off the right edge. Without this, the
 * far column cannot be reached by the very gesture the board recommends.
 */
const EDGE_BAND_PX = 72;
const EDGE_SPEED_PX = 16;

/**
 * What a drag carries.
 *
 * Every read and write of it is optional-chained. A real drag always carries a
 * `dataTransfer`; a synthesized `DragEvent` — which is how a test may drive
 * these handlers — does not, and the ref fallback covers that case.
 */
const CARD_MIME = "application/x-opencompany-task";

/**
 * The collapsed column's width.
 *
 * Wide enough for the vertical label and the count badge under it, narrow
 * enough that three of them cost less than half of one open column — which is
 * the whole point: the room has to go somewhere the operator can use it.
 */
const RAIL_WIDTH = "w-10";

export interface BoardProps<T extends BoardRow> {
  /** The columns, in board order. The host declares them; nothing here does. */
  columns: TaskColumn[];
  rows: T[];
  /** Which column a row sits in. See {@link BoardRow}. */
  statusOf: (row: T) => string;
  /** One card. `dragging` is this row's own drag state, for the lifted look. */
  renderCard: (row: T, dragging: boolean) => ReactNode;
  /** A row was dropped on a column. Already filtered: never fires for a no-op. */
  onMove: (row: T, status: string) => void;
  /** A drop landed on no column at all. */
  onMiss: () => void;
  /** Show a placeholder in each column until the first read lands. */
  loading?: boolean;
  /** What an empty column says. */
  emptyHint?: string;
  /** Rendered at the top of the column whose id this names — the intake slot. */
  columnHeader?: (column: TaskColumn) => ReactNode;
}

export function LedgerBoard<T extends BoardRow>({
  columns,
  rows,
  statusOf,
  renderCard,
  onMove,
  onMiss,
  loading = false,
  emptyHint = "Nothing here",
  columnHeader,
}: BoardProps<T>) {
  const [dragId, setDragId] = useState<string | null>(null);
  const [over, setOver] = useState<string | null>(null);
  // The columns the operator has clicked open. Only a click puts an id in here
  // and only a click takes it out again — see the header note on why nothing
  // else is allowed to.
  const [pinned, setPinned] = useState<ReadonlySet<string>>(() => new Set());
  const boardRef = useRef<HTMLDivElement | null>(null);
  // Refs rather than state: this is driven by `dragover` at pointer rate and
  // must not re-render the board on every move.
  const edgeSpeed = useRef(0);
  const edgeFrame = useRef<number | null>(null);

  const stopEdgeScroll = useCallback(() => {
    edgeSpeed.current = 0;
    if (edgeFrame.current !== null) {
      cancelAnimationFrame(edgeFrame.current);
      edgeFrame.current = null;
    }
  }, []);

  /**
   * Aims the board's self-scroll at wherever the drag currently is: full speed
   * hard against an edge, easing to nothing at the band's inner lip, and off
   * entirely across the middle of the board.
   */
  const edgeScrollTo = useCallback(
    (clientX: number) => {
      const board = boardRef.current;
      if (!board) return;
      const { left, right } = board.getBoundingClientRect();
      const ramp = (depth: number) =>
        Math.min(1, Math.max(0, 1 - depth / EDGE_BAND_PX));
      let speed = 0;
      if (clientX < left + EDGE_BAND_PX)
        speed = -EDGE_SPEED_PX * ramp(clientX - left);
      else if (clientX > right - EDGE_BAND_PX)
        speed = EDGE_SPEED_PX * ramp(right - clientX);
      if (speed === 0) {
        stopEdgeScroll();
        return;
      }
      edgeSpeed.current = speed;
      if (edgeFrame.current !== null) return;
      const step = () => {
        const el = boardRef.current;
        if (!el || edgeSpeed.current === 0) {
          edgeFrame.current = null;
          return;
        }
        el.scrollLeft += edgeSpeed.current;
        edgeFrame.current = requestAnimationFrame(step);
      };
      edgeFrame.current = requestAnimationFrame(step);
    },
    [stopEdgeScroll],
  );

  // A drag interrupted by anything that unmounts the board — switching ledgers,
  // opening a card — must not leave a frame running against a detached node.
  useEffect(() => stopEdgeScroll, [stopEdgeScroll]);

  const togglePin = useCallback((id: string) => {
    setPinned((current) => {
      const next = new Set(current);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  }, []);

  /**
   * Each column's rows, bucketed once.
   *
   * A `filter` per column is one pass over every row per column, which at the
   * hundred-card boards this issue was reported on is six passes on every
   * `dragover`. Bucketing keeps declaration order within a column, which is the
   * order the caller handed the rows in.
   */
  const held = useMemo(() => {
    const buckets = new Map<string, T[]>(
      columns.map((column) => [column.id, []]),
    );
    for (const row of rows) buckets.get(statusOf(row))?.push(row);
    return buckets;
  }, [columns, rows, statusOf]);

  /**
   * Whether an empty column is allowed to collapse at all.
   *
   * Only once some *other* column has work in it. A board that is empty
   * everywhere has nothing to make room for, and six rails would answer "show
   * me the work" worse than the three honest zeros this issue is about.
   */
  const collapsible =
    !loading &&
    columns.some((column) => (held.get(column.id)?.length ?? 0) > 0);

  /** Reads the dragged row out of the event, falling back to our own state. */
  const dragged = (event: React.DragEvent): T | undefined => {
    const id = event.dataTransfer?.getData(CARD_MIME) || dragId;
    return rows.find((row) => row.id === id);
  };

  if (columns.length === 0 && !loading) {
    return (
      <p className="text-sm text-muted-foreground">
        This ledger declares no statuses, so it has no columns.
      </p>
    );
  }

  return (
    <div
      ref={boardRef}
      data-testid="ledger-board"
      onDragOver={(event) => {
        // The columns preventDefault as well; this handler also covers the
        // pixels between and around them, so the board keeps scrolling while a
        // drag crosses a gap on its way to a far column.
        event.preventDefault();
        edgeScrollTo(event.clientX);
      }}
      onDragLeave={(event) => {
        // Only when the pointer has genuinely left the board, not while it
        // moves between two of the board's own children.
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
          stopEdgeScroll();
        }
      }}
      onDrop={(event) => {
        // Columns claim their own drops and stop them here, so anything that
        // reaches this handler landed on dead board pixels.
        event.preventDefault();
        stopEdgeScroll();
        const id = event.dataTransfer?.getData(CARD_MIME) || dragId;
        setDragId(null);
        setOver(null);
        if (id) onMiss();
      }}
      className="flex min-h-0 flex-1 gap-4 overflow-x-auto py-1"
    >
      {columns.map((column) => {
        const cards = held.get(column.id) ?? [];
        const head = columnHeader?.(column);
        // A column whose header slot is filled carries an affordance — the
        // intake `+` is the one this board was built for — and a rail has
        // nowhere to put it. Collapsing it would hide a control rather than
        // some whitespace, so it stays open. `false` counts as empty: a caller
        // writing `column.id === ADD_TASK_COLUMN && <Plus/>` returns it for
        // every other column.
        const hasHead = head != null && head !== false && head !== "";
        const collapsed =
          collapsible &&
          cards.length === 0 &&
          !hasHead &&
          !pinned.has(column.id) &&
          over !== column.id;
        // Offered only where it undoes something: on a column the operator
        // pinned open that would otherwise have collapsed itself.
        const canCollapse =
          collapsible && cards.length === 0 && pinned.has(column.id);
        return (
          <div
            key={column.id}
            data-testid="board-column"
            data-column={column.id}
            data-collapsed={collapsed ? "true" : "false"}
            onDragOver={(event) => {
              event.preventDefault();
              // Runs before the board's own dragover (this is the inner
              // handler), and the board leaves it alone — so the cursor says
              // "move" over a column and nothing over the dead pixels.
              if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
              setOver(column.id);
            }}
            onDragLeave={(event) => {
              // Only when the pointer has genuinely left the column. A rail
              // that opens under the drag replaces its own contents, and an
              // element removed from under a pointer can fire `dragleave` —
              // which without this guard would collapse the column again on
              // the frame it opened, forever.
              if (
                event.currentTarget.contains(event.relatedTarget as Node | null)
              )
                return;
              setOver((current) => (current === column.id ? null : current));
            }}
            onDrop={(event) => {
              event.preventDefault();
              // Claim the drop, so anything still reaching the board's own
              // handler is known to have missed every column.
              event.stopPropagation();
              stopEdgeScroll();
              const row = dragged(event);
              setDragId(null);
              setOver(null);
              // A row dropped back on its own column is a no-op, not a move:
              // the one deliberate silence on a board where every other exit
              // says something.
              if (row && statusOf(row) !== column.id) onMove(row, column.id);
            }}
            className={cn(
              "flex min-h-0 shrink-0 flex-col rounded-xl border bg-card/40 transition-colors",
              collapsed ? RAIL_WIDTH : "w-72",
              over === column.id && "border-primary/40 bg-accent/40",
            )}
          >
            {collapsed ? (
              // The whole rail is the control, so the target is the thing the
              // eye reads rather than a chevron tucked inside it. The label and
              // count are `aria-hidden` and restated in the button's own name:
              // read as text they run together ("To-do0"), and a vertical
              // writing mode is a paint instruction that a screen reader cannot
              // see at all.
              <button
                type="button"
                onClick={() => togglePin(column.id)}
                aria-label={`Expand ${column.label}, ${cards.length} cards`}
                title={`Expand ${column.label}`}
                className="flex min-h-0 flex-1 cursor-pointer flex-col items-center gap-2 rounded-xl py-3 outline-none transition-colors hover:bg-accent/60 focus-visible:ring-3 focus-visible:ring-ring/50"
              >
                <span
                  aria-hidden
                  data-testid="column-label"
                  className="min-h-0 [writing-mode:vertical-rl] truncate text-sm font-medium text-muted-foreground"
                >
                  {column.label}
                </span>
                <span
                  aria-hidden
                  className="mt-auto rounded-full bg-muted px-1.5 text-2xs font-medium tabular-nums text-muted-foreground"
                >
                  {cards.length}
                </span>
              </button>
            ) : (
              <>
                <div className="mx-3 flex shrink-0 items-center gap-2 border-b py-2.5">
                  <span
                    data-testid="column-label"
                    className="text-sm font-medium"
                  >
                    {column.label}
                  </span>
                  <span className="rounded-full bg-muted px-1.5 text-2xs font-medium tabular-nums text-muted-foreground">
                    {cards.length}
                  </span>
                  <div className="ml-auto flex items-center gap-1">
                    {head}
                    {canCollapse && (
                      <button
                        type="button"
                        onClick={() => togglePin(column.id)}
                        aria-label={`Collapse ${column.label}`}
                        title={`Collapse ${column.label}`}
                        className="flex size-5 cursor-pointer items-center justify-center rounded-md text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50"
                      >
                        <ChevronsLeft className="size-3.5" />
                      </button>
                    )}
                  </div>
                </div>
                <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto px-2 pb-2">
                  {loading && cards.length === 0 ? (
                    <Skeleton className="h-20 rounded-lg" />
                  ) : (
                    cards.map((row) => (
                      <div
                        key={row.id}
                        draggable
                        onDragStart={(event) => {
                          setDragId(row.id);
                          if (event.dataTransfer) {
                            event.dataTransfer.effectAllowed = "move";
                            // The id is what a drop reads back. The `text/plain`
                            // copy is what makes the drag well-formed for browsers
                            // that abort one carrying no data at all.
                            event.dataTransfer.setData(CARD_MIME, row.id);
                            event.dataTransfer.setData("text/plain", row.id);
                          }
                        }}
                        onDragEnd={() => {
                          setDragId(null);
                          setOver(null);
                          stopEdgeScroll();
                        }}
                      >
                        {renderCard(row, dragId === row.id)}
                      </div>
                    ))
                  )}
                  {!loading && cards.length === 0 && (
                    <p
                      className={cn(
                        "mt-2 flex h-38 items-center justify-center rounded-lg px-1 text-center text-xs",
                        over === column.id && dragId
                          ? // A column that opened to receive a card says so. The
                            // rail is a small target and the operator is holding
                            // something: the empty column has to read as a landing
                            // spot, not as the same "nothing here" it shows at rest.
                            "border border-dashed border-primary/50 bg-accent/60 font-medium text-foreground"
                          : "bg-muted/50 text-muted-foreground",
                      )}
                    >
                      {over === column.id && dragId
                        ? "Drop it here"
                        : emptyHint}
                    </p>
                  )}
                </div>
              </>
            )}
          </div>
        );
      })}
      {/* Trailing gutter: flex scroll containers drop their padding-inline-end,
          so this spacer keeps ~16px of breathing room past the last column. */}
      <div aria-hidden className="w-4 shrink-0" />
    </div>
  );
}
