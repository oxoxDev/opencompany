// The live memory API: the console reads and writes the company's real durable
// facts through the host's `…/memory` routes (REST, camelCase over the wire),
// and reads a `…/memory/stats` health snapshot. Replaces the client-side
// `lib/memory` localStorage stub, so a backend failure can never be masked by
// fake seeded data.

import type { OpenCompanyClient } from "./client";

/** The taxonomy of a durable fact — mirrors the host's `FactKind`. */
export type MemoryKind = "fact" | "preference" | "person" | "project" | "reference";

/**
 * Where a memory row came from — the host's `MemoryOrigin` discriminator.
 * `fact` rows are operator-authored (editable/deletable); `agent-memory` and
 * `task-outcome` rows are the agents' own runtime memory and are read-only.
 */
export type MemoryOrigin = "fact" | "agent-memory" | "task-outcome";

/** One memory row as the host returns it (an operator fact OR an agent chunk). */
export interface MemoryEntry {
  id: string;
  /** The fact taxonomy — present only on `fact` rows (omitted for context). */
  kind?: MemoryKind;
  /** Which backend the row came from; drives editable-vs-read-only rendering. */
  origin: MemoryOrigin;
  /** Whether the operator may delete this row (true only for `fact` rows). */
  editable: boolean;
  title: string;
  body: string;
  /** Which desk/teammate/agent captured it. */
  source: string;
  /**
   * Epoch-millis of the last update. For context rows this is when the chunk
   * was stored; `0` only when the backend has no stamp for it (chunks written
   * before store times were recorded), which still renders as `—`.
   */
  updatedAt: number;
}

/** The create-a-memory body; the host mints the id and timestamp. */
export interface CreateMemory {
  kind: MemoryKind;
  title: string;
  body: string;
  source?: string;
}

/**
 * The Brain health snapshot: durable facts plus the agents' runtime context
 * chunks. Lets the console prove the store is live at a glance.
 */
export interface MemoryStats {
  /** Number of durable operator facts. */
  facts: number;
  /** The newest fact's last-updated epoch-millis (`0` when there are none). */
  factsUpdatedAtMillis: number;
  /**
   * The newest epoch-millis across *every* memory source — operator facts and
   * the agents' context chunks alike (`0` when nothing is remembered yet).
   *
   * This, not `factsUpdatedAtMillis`, is what the "Last updated" stat renders:
   * agents only ever write context chunks, so the facts-only figure sits at `0`
   * for any company whose operator has not hand-authored a fact.
   */
  lastUpdatedAtMillis: number;
  /** Total agent-accessible context chunks (learned context + outcomes + mirrors). */
  agentChunks: number;
  /** Of those, how many are stored task outcomes. */
  taskOutcomes: number;
}

/** The kinds in display order, for filters and the add form. */
export const MEMORY_KINDS: MemoryKind[] = ["fact", "preference", "person", "project", "reference"];

/**
 * Per-kind badge styling — identity, not state.
 *
 * The identity palette (`--tone-*`): a memory's kind says what sort of thing
 * it is, never how it is doing. `reference` stays neutral on purpose, as the
 * kind with nothing to distinguish.
 */
export const KIND_STYLES: Record<MemoryKind, string> = {
  fact: "border-tone-2/30 bg-tone-2/10 text-tone-2-text",
  preference: "border-tone-1/30 bg-tone-1/10 text-tone-1-text",
  person: "border-tone-4/30 bg-tone-4/10 text-tone-4-text",
  project: "border-tone-3/30 bg-tone-3/10 text-tone-3-text",
  reference: "border-border bg-muted text-muted-foreground",
};

/** The read-only context origins, in display order (facts filter by kind). */
export const CONTEXT_ORIGINS: Exclude<MemoryOrigin, "fact">[] = ["agent-memory", "task-outcome"];

/** Human labels for each origin, for badges and the type filter. */
export const ORIGIN_LABELS: Record<MemoryOrigin, string> = {
  fact: "Fact",
  "agent-memory": "Teammate memory",
  "task-outcome": "Task outcome",
};

/** Per-origin badge styling for the read-only context rows. */
export const ORIGIN_STYLES: Record<Exclude<MemoryOrigin, "fact">, string> = {
  "agent-memory": "border-tone-3/30 bg-tone-3/10 text-tone-3-text",
  // Identity, not status. A task-outcome memory records what happened; it is
  // not itself a failure, which is what the rose it used to wear implied of
  // every one of them.
  "task-outcome": "border-tone-5/30 bg-tone-5/10 text-tone-5-text",
};

/** The company's durable facts, newest-first, optionally filtered server-side. */
export function listMemory(
  client: OpenCompanyClient,
  company: string | null,
  opts?: { query?: string; kind?: MemoryKind },
): Promise<MemoryEntry[]> {
  const params = new URLSearchParams();
  if (opts?.query) params.set("query", opts.query);
  if (opts?.kind) params.set("kind", opts.kind);
  const qs = params.toString();
  return client.get<MemoryEntry[]>(`${client.scopeFor(company)}/memory${qs ? `?${qs}` : ""}`);
}

/** Add a durable fact (also mirrored into the agents' recallable context). */
export function createMemory(
  client: OpenCompanyClient,
  company: string | null,
  body: CreateMemory,
): Promise<MemoryEntry> {
  return client.post<MemoryEntry>(`${client.scopeFor(company)}/memory`, body);
}

/** Delete a fact by id. */
export function deleteMemory(
  client: OpenCompanyClient,
  company: string | null,
  id: string,
): Promise<void> {
  return client.del<void>(`${client.scopeFor(company)}/memory/${encodeURIComponent(id)}`);
}

/** The Brain health snapshot. */
export function memoryStats(
  client: OpenCompanyClient,
  company: string | null,
): Promise<MemoryStats> {
  return client.get<MemoryStats>(`${client.scopeFor(company)}/memory/stats`);
}
