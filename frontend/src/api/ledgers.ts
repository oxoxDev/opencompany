// The dynamic-ledger API: the company's own record — goals, decisions, and
// whatever axis this workspace declared — read and written through the host's
// `…/ledgers` routes.
//
// The console does not know what a ledger holds. It is told: every read carries
// the ledger's own `fields`, `statuses` and `sections`, and the UI renders a
// form and a table from that rather than from anything hard-coded here. That is
// the whole point — a ledger an agent declared this morning renders correctly
// this afternoon with no console release.

import type { OpenCompanyClient } from "./client";

/** Where a ledger's rows come from. */
export type LedgerSource =
  /** An append-only log this surface writes. */
  | "events"
  /**
   * A ledger the runtime already owns and renders — the task board. Readable
   * here and written where it lives, which `writtenBy` names.
   */
  | "native";

/** What a field is for, which is how the console renders and sorts it. */
export type FieldRole =
  | "id"
  | "title"
  | "status"
  | "prose"
  | "refs"
  | "owner"
  | "date"
  | "number";

export interface LedgerField {
  name: string;
  role: FieldRole;
  /** A line saying what belongs here, shown under the input. */
  description?: string;
  required?: boolean;
}

export interface LedgerStatus {
  name: string;
  /**
   * What a person reads, when it differs from the wire word.
   *
   * Absent for a company-declared ledger, whose statuses are already written to
   * be read; present on the board, whose columns are `in_progress` and
   * `in_review`. Carrying it on the wire is what let the console delete its own
   * copy of the board's label table.
   */
  label?: string;
  /** Whether this status ends a row's life. A closed row is kept, not deleted. */
  closed?: boolean;
  /** Whether closing into it demands a reason. The host refuses without one. */
  needsReason?: boolean;
}

export interface LedgerSection {
  heading: string;
  blurb?: string;
  statuses?: string[];
  cap: number;
  order: "recorded" | "recent";
}

/** One ledger, as the console lists and renders it. */
export interface LedgerSummary {
  slug: string;
  title: string;
  purpose: string;
  source: LedgerSource;
  /** Where its rendered file lives in the workspace, e.g. `derived/GOALS.md`. */
  derived: string;
  /**
   * How it is actually written, in a sentence. Rendered in place of a compose
   * box on a native ledger — the board is written by the board, and offering a
   * form here would produce a save the host refuses.
   */
  writtenBy: string;
  /** Whether the runtime ships it. A built-in cannot be retired. */
  builtin: boolean;
  fields: LedgerField[];
  statuses: LedgerStatus[];
  sections: LedgerSection[];
  writers?: string[];
  open: number;
  closed: number;
}

/** One row. `fields` is whatever the ledger declares — never a fixed shape. */
export interface LedgerEntry {
  id: string;
  fields: Record<string, string>;
  status: string;
  title: string;
  /** Whether the row's status is one the ledger calls closed. */
  closed: boolean;
  openedBy: LedgerAuthor;
  updatedBy: LedgerAuthor;
  openedAt: number;
  updatedAt: number;
  /** How many writes fold into this row. */
  events: number;
}

export interface LedgerAuthor {
  kind: "human" | "agent" | "system";
  id?: string;
  label?: string;
}

export interface LedgerList {
  ledgers: LedgerSummary[];
  /**
   * Declarations that could not be loaded, and why. Rendered rather than
   * dropped: a company whose ledger stopped appearing has no other way to find
   * out what happened to it.
   */
  faults?: string[];
  /** How many more this company may declare. */
  remaining: number;
}

export interface LedgerRead {
  ledger: LedgerSummary;
  entries: LedgerEntry[];
  /**
   * How many matched before the bound was applied. Shown whenever it exceeds
   * what came back, so a short list is never mistaken for all of them.
   */
  matched: number;
  faults?: string[];
}

export interface LedgerQuery {
  entry?: string;
  status?: string;
  q?: string;
  sort?: "recorded" | "recent";
  limit?: number;
}

/**
 * The body every write sends. There is one write: opening, amending and closing
 * a row all merge fields into it, so reusing an `id` updates that row.
 *
 * A field set to `null` clears it — the one thing a merge cannot otherwise say.
 */
export interface RecordEntryBody {
  id: string;
  fields?: Record<string, string | null>;
  status?: string;
  reason?: string;
}

export async function listLedgers(
  client: OpenCompanyClient,
  company: string,
): Promise<LedgerList> {
  return client.get<LedgerList>(`${client.scopeFor(company)}/ledgers`);
}

export async function readLedger(
  client: OpenCompanyClient,
  company: string,
  slug: string,
  query: LedgerQuery = {},
): Promise<LedgerRead> {
  const params = new URLSearchParams();
  if (query.entry) params.set("entry", query.entry);
  if (query.status) params.set("status", query.status);
  if (query.q) params.set("q", query.q);
  if (query.sort) params.set("sort", query.sort);
  if (query.limit != null) params.set("limit", String(query.limit));
  const suffix = params.toString() ? `?${params.toString()}` : "";
  return client.get<LedgerRead>(
    `${client.scopeFor(company)}/ledgers/${encodeURIComponent(slug)}${suffix}`,
  );
}

/**
 * The exact Markdown `derived/<NAME>.md` holds.
 *
 * Served from the derivation rather than by reading the file back, so a
 * workspace write that failed can never show a stale page.
 */
export async function renderedLedger(
  client: OpenCompanyClient,
  company: string,
  slug: string,
): Promise<string> {
  const { text } = await client.getDocument(
    `${client.scopeFor(company)}/ledgers/${encodeURIComponent(slug)}/rendered`,
  );
  return text;
}

export async function recordEntry(
  client: OpenCompanyClient,
  company: string,
  slug: string,
  body: RecordEntryBody,
): Promise<LedgerEntry> {
  return client.post<LedgerEntry>(
    `${client.scopeFor(company)}/ledgers/${encodeURIComponent(slug)}/entries`,
    body,
  );
}

/**
 * Deletes a row and everything recorded against it.
 *
 * The one irreversible operation on a ledger, and the one an agent cannot
 * reach: teammates close rows, which keeps the reason. So this is only ever
 * called from a person's click.
 */
export async function deleteEntry(
  client: OpenCompanyClient,
  company: string,
  slug: string,
  id: string,
): Promise<void> {
  await client.del<void>(
    `${client.scopeFor(company)}/ledgers/${encodeURIComponent(slug)}/entries/${encodeURIComponent(id)}`,
  );
}

export async function defineLedger(
  client: OpenCompanyClient,
  company: string,
  declaration: unknown,
): Promise<LedgerSummary> {
  return client.post<LedgerSummary>(
    `${client.scopeFor(company)}/ledgers`,
    declaration,
  );
}

/**
 * Retires a declared ledger.
 *
 * `purge` deletes the rows too, and defaults off: retiring a ledger nobody
 * reads is worth doing, and deleting what it recorded is a separate decision
 * that should be made on purpose.
 */
export async function retireLedger(
  client: OpenCompanyClient,
  company: string,
  slug: string,
  purge = false,
): Promise<void> {
  const suffix = purge ? "?purge=true" : "";
  await client.del<void>(
    `${client.scopeFor(company)}/ledgers/${encodeURIComponent(slug)}${suffix}`,
  );
}

/** The field carrying a row's identity, if the ledger declares one. */
export function idField(ledger: LedgerSummary): LedgerField | undefined {
  return ledger.fields.find((field) => field.role === "id");
}

/** The field carrying a row's one-line summary. */
export function titleField(ledger: LedgerSummary): LedgerField | undefined {
  return ledger.fields.find((field) => field.role === "title");
}

/** The field carrying a row's status. */
export function statusField(ledger: LedgerSummary): LedgerField | undefined {
  return ledger.fields.find((field) => field.role === "status");
}

/**
 * The fields a compose form should offer: everything but the id and the status,
 * which the form asks for separately.
 */
export function composableFields(ledger: LedgerSummary): LedgerField[] {
  return ledger.fields.filter(
    (field) => field.role !== "id" && field.role !== "status",
  );
}

/**
 * The sentinel the status filter stores for "do not filter".
 *
 * A `Select` item cannot carry an empty string, so the no-filter choice needs a
 * value of its own — the same constraint the workflow destination picker met
 * with `__none__` in issue #813.
 */
export const EVERY_STATUS = "all";

/**
 * What the status filter should *show* for a stored value (issue #1217).
 *
 * base-ui renders the raw stored value in a collapsed `Select` unless it is
 * given explicit text, so without this the trigger read `all` while its own open
 * list read "Every status", and a chosen board column read `in_progress` while
 * the columns beside it read "In progress". Exactly the leak `destinationLabel`
 * exists to stop on the workflows side.
 *
 * Prefers the declared `label` over the wire `name`, which is what that field is
 * for: the board's statuses carry both, and the console deleted its own copy of
 * that table when the host started sending it. An unrecognized value falls back
 * to itself rather than to the sentinel — a filter we cannot name is still a
 * filter, and rendering it as "Every status" would claim the opposite.
 */
export function statusFilterLabel(
  ledger: LedgerSummary | null | undefined,
  value: string,
): string {
  if (value === EVERY_STATUS || value === "") return "Every status";
  const declared = ledger?.statuses.find((status) => status.name === value);
  if (!declared) return value;
  return `${declared.label ?? declared.name}${declared.closed ? " (closed)" : ""}`;
}

/**
 * Why this ledger is showing nothing — the filtered reading, or `null` when no
 * filter is on and "nothing recorded yet" is the honest answer (issue #1217).
 *
 * A ledger read is filtered **server-side**, so zero rows back means "no
 * matches" and "no rows" indistinguishably. The view knows which it is, because
 * the query and the status are its own state — this is that knowledge written
 * down once, so the empty state stops telling an operator with 14 rows that
 * their ledger is empty while the nav beside it counts them.
 *
 * Pure, and separate from the component, for the reason `workflowSavedToast` is:
 * the branch is the whole bug and it should be assertable without a render.
 */
export function filteredEmptyNotice(
  ledger: LedgerSummary | null | undefined,
  query: string,
  statusFilter: string,
): string | null {
  const searching = query.trim() !== "";
  const narrowed = statusFilter !== EVERY_STATUS && statusFilter !== "";
  if (!searching && !narrowed) return null;
  if (searching && narrowed) {
    return `No rows match “${query.trim()}” with status ${statusFilterLabel(ledger, statusFilter)}.`;
  }
  if (searching) return `No rows match “${query.trim()}”.`;
  return `No rows with status ${statusFilterLabel(ledger, statusFilter)}.`;
}

/**
 * The compose dialog's title (issue #1264).
 *
 * The id field is editable in every non-closing dialog — "Record" opens it
 * empty, "Amend" opens it prefilled but still editable — so whether this says
 * "Amend" cannot be "is `composing.id` non-empty" (that flips true on the
 * first keystroke of a row that has never been saved, before it exists). It
 * has to ask whether the *current* typed id names a row this ledger already
 * has, checked against whatever is currently loaded (`existingIds`) the same
 * way `save()` treats the id — trimmed.
 */
export function composeDialogTitle(
  ledger: LedgerSummary,
  composing: { id: string; closing: boolean },
  existingIds: ReadonlySet<string>,
): string {
  if (composing.closing) return `Close ${composing.id}`;
  return existingIds.has(composing.id.trim())
    ? `Amend ${composing.id}`
    : `New row on ${ledger.title}`;
}

/** The compose dialog's description — the same "does this id exist" read as the title. */
export function composeDialogDescription(
  composing: { id: string },
  existingIds: ReadonlySet<string>,
): string {
  return existingIds.has(composing.id.trim())
    ? "Only what you change is written; everything else on the row is left alone."
    : "Give it a short, readable id — it is how anybody names this row later.";
}

/** Whether this status ends a row's life on this ledger. */
export function isClosingStatus(ledger: LedgerSummary, status: string): boolean {
  return ledger.statuses.some(
    (declared) =>
      declared.name.toLowerCase() === status.trim().toLowerCase() &&
      declared.closed === true,
  );
}

/**
 * Whether closing into `status` demands a reason.
 *
 * Read here so the form can *ask* for one, rather than letting the save fail
 * and reporting the host's refusal — the same rule, met before it bites.
 */
export function statusNeedsReason(
  ledger: LedgerSummary,
  status: string,
): boolean {
  return ledger.statuses.some(
    (declared) =>
      declared.name.toLowerCase() === status.trim().toLowerCase() &&
      declared.needsReason === true,
  );
}

/** Whether this ledger takes writes from this surface at all. */
export function isWritable(ledger: LedgerSummary): boolean {
  return ledger.source === "events";
}

/** A row's byline, for the "recorded by" column. */
export function byline(author: LedgerAuthor): string {
  const name = author.label?.trim() || author.id?.trim();
  return name ? `${name} (${author.kind})` : author.kind;
}
