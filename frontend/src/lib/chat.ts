import type { ChatHistoryMessageDto, TurnStep } from "@/api/types";

/**
 * The company's main line, by thread id.
 *
 * Mirrors the host's `MAIN_THREAD_ID` (`src/server/chat_history.rs`), whose
 * `is_general_chat` folds this id, `"General"` and the **empty string** into a
 * single conversation. Named rather than spelled inline because
 * {@link dispatchMarkerPlacement} has to resolve an empty origin to *this*
 * thread rather than to nothing — see the rules on that function.
 */
export const MAIN_THREAD_ID = "main";

/** One person's reaction on one line. Mirrors `ChatReactionDto` on the host. */
export interface Reaction {
  emoji: string;
  /** Who reacted, as a display label — never a raw user id. */
  by: string;
  /** Whether the reader is the one who reacted. */
  mine: boolean;
}

/** One line in the conversation with the company. */
export interface ChatMessage {
  id: string;
  from: "you" | "company" | "system";
  text: string;
  /** Wall-clock the line was added, for timestamps and grouping. */
  at: number;
  /**
   * The reply's originating channel (e.g. "operator"). Threads the company
   * side by sender: a distinct channel reads as its own agent in the chat.
   */
  channel?: string;
  /**
   * The message this one replies to. A line with a parent is a thread reply:
   * it stays out of the channel timeline and renders inside the thread panel.
   *
   * Always another line's `id`, so it moves with {@link reconcileIds} when an
   * optimistic id is replaced by the durable one the host assigned.
   */
  parentId?: string;
  /**
   * Who reacted to this line with what — one row per person per emoji, not a
   * count (issue #364).
   *
   * A count could not say who reacted, and could not tell the reader whether
   * one of the reactions was their own, which is what makes a chip a toggle.
   * The renderer groups these into chips; nothing groups a count back into
   * people. Absent until someone reacts.
   */
  reactions?: Reaction[];
  /**
   * The scrubbed processing steps behind a company reply (tool calls, thinking,
   * surfaced failures), rendered as a timeline above the bubble. Absent/empty
   * on your own messages and on tool-less replies.
   */
  steps?: TurnStep[];
  /**
   * The board card this line is about (issue #246): one the turn opened, or one
   * created from this message by "Add to board". Renders as a chip linking to
   * `#/tasks/<id>`.
   */
  taskId?: string;
}

/**
 * How long a card title derived from a chat message may be before it is
 * elided. Long enough to keep a normal one-line ask intact, short enough that a
 * pasted paragraph does not become a board card nobody can scan.
 */
const TITLE_CAP = 80;

/**
 * A board-card title derived from a chat message (issue #246).
 *
 * Takes the first non-blank line — a multi-line ask reads as "headline, then
 * detail", and the detail belongs in the card's note, which is where the full
 * text goes. Returns an empty string for a message with nothing in it, so the
 * caller can refuse rather than opening a blank card.
 *
 * Elides on **code points**, not UTF-16 units, so a title cut mid-emoji cannot
 * produce a lone surrogate; the cap includes the ellipsis rather than
 * overshooting by one.
 */
export function titleFromMessage(text: string): string {
  const line = text
    .split("\n")
    .map((l) => l.trim())
    .find((l) => l.length > 0);
  if (!line) return "";
  const points = Array.from(line);
  return points.length <= TITLE_CAP
    ? line
    : `${points.slice(0, TITLE_CAP - 1).join("")}…`;
}

let seq = 0;
const nextId = () => `m${seq++}`;

/**
 * The identity a live-streamed company reply must be built with (issue #483).
 *
 * A reply arriving over the stream and the same reply coming back from
 * `chat/history` are one message, and the console has to be able to tell. Both
 * sides carry the host's `StoredEvent` sequence — the stream frame as `seq`,
 * the history entry as `id` — so stamping the live line with it makes the two
 * resolve to the same console id, and `hydrateChannel`'s id dedupe recognises
 * the line it already has.
 *
 * Without this the live line was born under an ephemeral `m<counter>` id that
 * hydration could never match, so a reply that arrived while its channel was
 * closed was rendered twice on opening it.
 *
 * This exists as its own function so the identity decision has somewhere a test
 * can reach. Inlined at the two call sites, nothing could assert it was still
 * being made.
 */
export function liveReplyIdentity(event: { seq: number }): { messageId: string } {
  return { messageId: String(event.seq) };
}

/**
 * How the console labels each board column in a marker.
 *
 * **The one label copy that survives**, and it is deliberate. Everywhere else
 * the console now reads labels off the `tasks` ledger
 * (`lib/board-columns.ts`), which the host builds from its single column table.
 * A marker cannot: it is rendered synchronously from a **thin** SSE frame
 * carrying a raw column id, in a transcript that may have no ledger read behind
 * it at all, and awaiting one would leave a marker line blank or flickering
 * mid-conversation.
 *
 * What makes that acceptable is the blast radius. Drift here can only *reword*
 * one marker; it cannot lose a card, refuse a write, or leave a column
 * unrendered, which is what the board's own copy could do and why that one is
 * gone. It stays in step with the host's `dispatch_marker_text`
 * (`src/server/chat_history.rs`), and unit tests on both sides pin the same
 * literals.
 */
const COLUMN_LABELS: Record<string, string> = {
  todo: "To-do",
  planning: "Planning",
  in_progress: "In progress",
  paused: "Paused",
  in_review: "In review",
  done: "Done",
};

/**
 * The channel line a settled dispatch leaves behind (issue #377) —
 * `finished → In review`.
 *
 * The console needs its own copy because the live SSE frame is **thin**: it
 * carries the raw column id, not prose, exactly as `task_card_changed` and
 * `approval_parked` do. The host holds the same sentence for the rehydrated
 * half (`dispatch_marker_text`), and unit tests on both sides pin the identical
 * literals. See `COLUMN_LABELS` above for why this copy is the one exception
 * to the ledger-driven rule.
 *
 * Drift between the two can only *reword* a marker across a reload; it can
 * never double one, because the dedupe is on identity (`h<seq>`) and not on
 * content. That was #483's lesson and it is what makes two spellings a
 * tolerable cost rather than a returning bug.
 *
 * "finished" means the run **stopped**, not that it succeeded — a cancelled or
 * failed dispatch says `finished → To-do`, a parked one `finished → Paused`.
 * The misleading case this exists for is precisely the run that stopped without
 * finishing the work. An unrecognised column id passes through verbatim rather
 * than rendering blank.
 */
export function dispatchMarkerText(column: string): string {
  return `finished → ${COLUMN_LABELS[column] ?? column}`;
}

/**
 * Build a stamped message. `at` is injected so callers stay pure/testable.
 *
 * `messageId` is the host's own id for the line, when the caller already has
 * one — a reply that came back from the chat POST does. Passing it means the
 * bubble is born durable and can be replied to or reacted on immediately,
 * rather than waiting for a reconciliation it does not need.
 */
export function makeMessage(
  from: ChatMessage["from"],
  text: string,
  opts: {
    channel?: string;
    at?: number;
    parentId?: string;
    steps?: TurnStep[];
    taskId?: string;
    messageId?: string;
  } = {},
): ChatMessage {
  return {
    id: opts.messageId ? hostMessageId(opts.messageId) : nextId(),
    from,
    text,
    at: opts.at ?? Date.now(),
    channel: opts.channel,
    parentId: opts.parentId,
    steps: opts.steps,
    taskId: opts.taskId,
  };
}

/** The fields {@link dispatchMarkerPlacement} reads off a `desk_task_completed` frame. */
export interface DispatchTerminalFrame {
  taskId: string;
  column: string;
  /** The channel the card was raised in; absent for a board-created card. */
  chatId?: string;
  /** The host's `StoredEvent` sequence — the marker's durable identity. */
  seq: number;
  atMillis: number;
}

/** Where a settled dispatch's marker goes, and the line to put there. */
export interface DispatchMarkerPlacement {
  /** The chat thread the card was raised in. */
  threadId: string;
  /** The channel rendering that thread, or `null` when this company has none. */
  channelId: string | null;
  message: ChatMessage;
}

/**
 * Decide where a settled dispatch's marker belongs (issue #377), or that it
 * belongs nowhere.
 *
 * A named function rather than three lines inside the shell's injector, for the
 * reason {@link liveReplyIdentity} is one: every rule below is a decision that
 * silently stops being made if it is inlined, and each of them has a specific
 * bug on the other side of it.
 *
 * - **No `chatId` → `null`.** Nothing raised that card from a conversation (it
 *   was opened on the board, or by a scheduler), so no channel is the right
 *   one. The host already declines to file such a card into any desk's history;
 *   this is the live half of the same rule, and the two must agree or a marker
 *   would appear live and vanish on reload.
 * - **An empty `chatId` is the General thread, not an absent one.** The host
 *   folds `""` into General (`is_general_chat` treats an empty id, `"main"` and
 *   `"General"` as one conversation), and the chat route takes `chat` straight
 *   off the request body without normalising it — so a client posting
 *   `chat: ""` stores `origin_chat_id: Some("")` and the projection emits
 *   `chatId: ""`. Treating that as absent would drop the live marker while
 *   `chat/history` still served the rehydrated twin: the marker would appear
 *   only after a reload, which is the live-vs-history split the whole
 *   identity-dedupe exists to prevent. Absent means `undefined`, and only
 *   `undefined`. (The REST create path already normalises a blank field away
 *   for the same reason — issue #246.)
 * - **`channelId: null` when the thread matches no channel** — never a fall
 *   back to whatever channel the operator has open. The shell's `noteInChannel`
 *   does fall back, deliberately, because an approval decision has to be seen;
 *   a marker does not, and falling back would file one conversation's settle
 *   into another. That is issue #368's bug, and this is the shape that makes
 *   re-introducing it impossible rather than merely discouraged.
 * - **The line is born under the host's id** (`h<seq>`), which is exactly what
 *   `chat/history` mints for the same event — so {@link fromHistory}'s twin
 *   dedupes against it on the next reload. Identity, not content: #483 was a
 *   content check hydration could never satisfy.
 */
export function dispatchMarkerPlacement(
  event: DispatchTerminalFrame,
  chatChannelByThread: Record<string, string>,
): DispatchMarkerPlacement | null {
  if (event.chatId === undefined) return null;
  // `""` is the General thread spelled empty, not a missing origin — see above.
  const threadId = event.chatId === "" ? MAIN_THREAD_ID : event.chatId;
  return {
    threadId,
    channelId: chatChannelByThread[threadId] ?? null,
    message: makeMessage("system", dispatchMarkerText(event.column), {
      taskId: event.taskId,
      messageId: String(event.seq),
      at: event.atMillis,
    }),
  };
}

/**
 * Maps a desk's persisted transcript (`GET .../chat/history`, issue #65) to
 * the console's chat lines, preserving `mine`/author/text and ordering — the
 * backend already returns messages oldest-first. `id`s are namespaced with an
 * `h` prefix so a rehydrated line can never collide with one built locally by
 * {@link makeMessage} (`m<seq>`).
 */
export function fromHistory(entries: ChatHistoryMessageDto[]): ChatMessage[] {
  return entries.map((entry) => {
    // A host-authored system line (issue #377's dispatch marker) is neither
    // yours nor the company's voice: it renders as a centred pill, not a
    // bubble. Read off the author the host projected — `mine` is false for it,
    // so without this check a rehydrated marker came back as a company message
    // and a settle read like something an agent had said.
    const from: ChatMessage["from"] =
      entry.author === "system" ? "system" : entry.mine ? "you" : "company";
    return {
      id: hostMessageId(entry.id),
      from,
      text: entry.text,
      at: entry.atMillis,
      // The host names the parent by its own id, which lives in the same
      // namespace as `entry.id` — so it takes the same prefix, or the reply
      // would point at a line no console id matches (issue #364).
      parentId: entry.parentId ? hostMessageId(entry.parentId) : undefined,
      // Reactions come through whoever the host said reacted; nothing is
      // inferred here, `mine` included.
      reactions: entry.reactions?.length ? entry.reactions : undefined,
      // A sent message never carries a channel; only attribute one when the
      // line came from someone/something else, mirroring `ChatPane.send`.
      channel: from === "company" ? entry.channel : undefined,
      // Rehydrate the persisted tool-call timeline so it survives a thread
      // switch / reload — the render already draws `m.steps` (Conversation.tsx).
      steps: from === "company" ? entry.steps : undefined,
      // Rehydrate the "card opened" chip (issue #246) for the same reason as
      // the timeline above: a chip that lives only on the live POST response
      // vanishes on the first thread switch.
      //
      // System rows carry it too (issue #377): a dispatch marker's whole
      // usefulness is the link to the card that settled, and dropping the id
      // here would leave a rehydrated marker as a sentence with nowhere to go.
      // Only your own lines never have one — you did not open a card by
      // speaking.
      taskId: from === "you" ? undefined : entry.taskId,
    };
  });
}

/**
 * The console id for a message the host has journaled (issue #364).
 *
 * Two id namespaces meet in a transcript, and keeping them apart is the whole
 * job of this prefix: `m<n>` is a browser-minted counter that means nothing
 * outside this tab, and `h<seq>` is the host's own sequence position, which any
 * reader — a reload, a second operator — resolves to the same message. The `h`
 * is what lets {@link isHostMessageId} tell "saved" from "not saved yet"
 * without asking the server.
 */
export function hostMessageId(seq: string): string {
  return `h${seq}`;
}

/** Whether an id names a message the host has journaled. */
export function isHostMessageId(id: string | undefined): boolean {
  return !!id && id.startsWith("h");
}

/**
 * The host-side id an `h`-prefixed console id names, or `null` for a local one.
 *
 * The inverse of {@link hostMessageId}, used when a durable id has to go back
 * over the wire — a thread reply's parent, a reaction's target.
 */
export function toHostMessageId(id: string | undefined): string | null {
  return isHostMessageId(id) ? id!.slice(1) : null;
}

/**
 * Replace an optimistic message id with the durable one the host assigned, and
 * re-point anything that was already replying to it (issue #364).
 *
 * The subtle half is the re-parenting, and it is why this is a named function
 * with its own tests rather than three lines inside a `setState`. A send is
 * rendered before the POST resolves, so a fast operator can open a thread on
 * that bubble and reply to it while its id is still the local counter. Swapping
 * only the parent's own id would leave those replies pointing at an id that no
 * longer exists — they would silently drop out of the thread and reappear in
 * the channel, which reads as the console losing them.
 *
 * Pure and total: unknown ids pass through, and a list with nothing to change
 * is returned as-is so React sees no new array.
 */
export function reconcileIds(
  messages: ChatMessage[],
  localId: string,
  hostSeq: string,
): ChatMessage[] {
  const nextId = hostMessageId(hostSeq);
  if (nextId === localId) return messages;
  let changed = false;
  const next = messages.map((m) => {
    const isTarget = m.id === localId;
    const isChild = m.parentId === localId;
    if (!isTarget && !isChild) return m;
    changed = true;
    return {
      ...m,
      ...(isTarget ? { id: nextId } : {}),
      ...(isChild ? { parentId: nextId } : {}),
    };
  });
  return changed ? next : messages;
}

/**
 * Forget the board card `taskId` on every line that carries it (issue #984).
 *
 * The dismissal half of "Add to board": #442 justified opening cards from chat
 * on the grounds that *"a spurious card can be dismissed in one click"*, and
 * the chat surfaces offered no such click — the chip was a bare link to the
 * card's detail screen. Deleting the card on the host is only half of it; this
 * is what stops the console still drawing a chip for a card that is gone.
 *
 * Keyed on the **card**, not on the message the operator clicked, and that is
 * the reason this is a named function rather than two lines inside a
 * `setState`. One card can be named by more than one line — a turn journals the
 * id onto its reply, and "Add to board" writes it onto the operator's own
 * message — so clearing only the clicked bubble would leave the other chips
 * pointing at a card the host no longer has, i.e. a link to a 404. A dismissal
 * that leaves a stale chip on screen reads as the delete having failed.
 *
 * Pure and total: an unknown id passes through, and a list with nothing to
 * change is returned as-is so React sees no new array.
 *
 * # This is the client half only
 *
 * It clears the chip from state that is never serialised, so on its own it
 * survives a thread switch and **not** a reload: the console rehydrates from
 * `GET …/chat/history` (see {@link fromHistory}) and merges by message id, so an
 * empty transcript takes every row back. The host is what stops the chip
 * returning — its history projection blanks `task_id` for a card the board no
 * longer has, whoever deleted it (`server::chat_history::drop_dead_cards`,
 * issue #984). Neither half is sufficient alone.
 */
export function clearTaskCard(messages: ChatMessage[], taskId: string): ChatMessage[] {
  let changed = false;
  const next = messages.map((m) => {
    if (m.taskId !== taskId) return m;
    changed = true;
    // Drop the key rather than setting it to `undefined`: `taskId` is an
    // optional field and every render site tests it for truthiness, but the two
    // are not interchangeable to `Object.keys`, a spread, or a deep-equality
    // assertion, and an absent key is what "this line has no card" means
    // everywhere else in this module.
    //
    // NOT because these rows are persisted — they are not. `transcripts` is
    // React state and is never serialised. The rows that persist are the
    // host's, and they are why this helper is only half the dismissal: see the
    // note on the function above.
    const { taskId: _dropped, ...rest } = m;
    return rest;
  });
  return changed ? next : messages;
}
