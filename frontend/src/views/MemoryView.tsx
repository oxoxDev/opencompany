// Parked by issue #302, re-listed in `app-shell.tsx`'s `NAV` with the
// memory-engine work: an operator choosing between engines needs somewhere to
// see which one is bound, what it negotiated, and whether the boot probe
// reached it — that is the engine panel below the header.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Brain, Loader2, Plus, Search, Trash2 } from "lucide-react";
import { toast } from "sonner";

import {
  CONTEXT_ORIGINS,
  createMemory,
  deleteMemory,
  KIND_STYLES,
  listMemory,
  MEMORY_KINDS,
  memoryStats,
  ORIGIN_LABELS,
  ORIGIN_STYLES,
  type MemoryEntry,
  type MemoryKind,
  type MemoryStats,
} from "@/api/memory";
import type { OpenCompanyClient } from "@/api/client";
import type { MemorySpec } from "@/api/types";
import { Markdown } from "@/components/markdown";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";

interface Props {
  client: OpenCompanyClient;
  company: string | null;
}

const KIND_LABELS: Record<string, string> = {
  all: "All types",
  fact: "Facts",
  preference: "Preferences",
  person: "People",
  project: "Projects",
  reference: "References",
};

/**
 * The value the type filter matches a row on: a fact matches on its `kind`, a
 * read-only context row matches on its `origin` (agent-memory / task-outcome).
 * Keeps the original per-kind filtering while extending it to the new sources.
 */
function entryType(e: MemoryEntry): string {
  return e.origin === "fact" ? (e.kind ?? "fact") : e.origin;
}

/** The badge label + style for a row, from its kind (facts) or origin (context). */
function entryBadge(e: MemoryEntry): { label: string; style: string } {
  if (e.origin === "fact") {
    const kind = e.kind ?? "fact";
    return { label: kind, style: KIND_STYLES[kind] };
  }
  return { label: ORIGIN_LABELS[e.origin], style: ORIGIN_STYLES[e.origin] };
}

/** The type-filter options in display order: fact kinds, then context origins. */
const TYPE_FILTERS: string[] = [...MEMORY_KINDS, ...CONTEXT_ORIGINS];

/** Labels for every type-filter value (including `all`), for the Select. */
const TYPE_FILTER_LABELS: Record<string, string> = {
  all: "All types",
  ...Object.fromEntries(MEMORY_KINDS.map((k) => [k, KIND_LABELS[k]])),
  ...Object.fromEntries(CONTEXT_ORIGINS.map((o) => [o, ORIGIN_LABELS[o]])),
};

/** Formats an epoch-millis instant as a short absolute date, or a dash when 0. */
function formatUpdated(ms: number): string {
  if (!ms) return "—";
  return new Date(ms).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}

/**
 * The company's Brain: its durable memory, read live from the host (`…/memory`)
 * with a health strip proving the store is real (fact + agent-context counts).
 * Operators add and delete facts; a create is mirrored server-side into the
 * agents' recallable context so a note reaches an agent on its next turn.
 */
export function MemoryView({ client, company }: Props) {
  const [entries, setEntries] = useState<MemoryEntry[]>([]);
  const [stats, setStats] = useState<MemoryStats | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<string>("all");
  const [addOpen, setAddOpen] = useState(false);
  // A generation token so a response from a previous company scope (or after
  // unmount) can't overwrite the current one.
  const gen = useRef(0);

  const load = useCallback(
    async (opts?: { silent?: boolean }) => {
      const mine = ++gen.current;
      if (!opts?.silent) setLoading(true);
      try {
        const [rows, s] = await Promise.all([
          listMemory(client, company),
          memoryStats(client, company),
        ]);
        if (mine !== gen.current) return;
        setEntries(rows);
        setStats(s);
        setError(null);
      } catch (e) {
        if (mine !== gen.current) return;
        setError(e instanceof Error ? e.message : "could not load memory");
      } finally {
        if (mine === gen.current && !opts?.silent) setLoading(false);
      }
    },
    [client, company],
  );

  useEffect(() => {
    setEntries([]);
    setStats(null);
    void load();
    return () => {
      gen.current++;
    };
  }, [load]);

  // The bound memory engine, from the unauthenticated /spec handshake. Shown
  // so an operator can see which engine is live and — for a remote driver
  // serving only the mandatory families — what it does NOT support, before a
  // cycle discovers it. undefined = host predates the field; render nothing.
  const [engine, setEngine] = useState<MemorySpec | undefined>(undefined);
  useEffect(() => {
    let live = true;
    // Clear first: on a client swap the old badge would otherwise survive a
    // failed /spec and name the wrong host.
    setEngine(undefined);
    client
      .spec()
      .then((spec) => {
        if (live) setEngine(spec.memory);
      })
      .catch(() => {
        /* spec is best-effort here; the Brain view works without it */
      });
    return () => {
      live = false;
    };
  }, [client]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return entries
      .filter((e) => kind === "all" || entryType(e) === kind)
      .filter((e) => !q || e.title.toLowerCase().includes(q) || e.body.toLowerCase().includes(q));
  }, [entries, query, kind]);

  // Per-type counts for the health badges, keyed by the same value the filter
  // matches on (fact kind or context origin) so every source shows a count.
  const perType = useMemo(() => {
    const counts: Record<string, number> = {};
    for (const e of entries) {
      const t = entryType(e);
      counts[t] = (counts[t] ?? 0) + 1;
    }
    return counts;
  }, [entries]);

  async function add(fields: { kind: MemoryKind; title: string; body: string }) {
    await createMemory(client, company, fields);
    await load({ silent: true });
    setAddOpen(false);
  }

  async function remove(entry: MemoryEntry) {
    // Optimistic: drop the card immediately, then reconcile counts from the host.
    setEntries((all) => all.filter((x) => x.id !== entry.id));
    try {
      await deleteMemory(client, company, entry.id);
      await load({ silent: true });
    } catch (e) {
      // Re-insert only this entry on failure (no whole-list rollback).
      setEntries((all) => (all.some((x) => x.id === entry.id) ? all : [entry, ...all]));
      toast.error(e instanceof Error ? e.message : "could not delete the memory");
    }
  }

  return (
    <div className="flex-1 overflow-y-auto">
      <div className="mx-auto w-full max-w-5xl space-y-5 px-4 py-6">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="space-y-1">
            <h2 className="text-2xl font-semibold tracking-tight">Brain</h2>
            <p className="text-sm text-muted-foreground">
              What your company remembers — facts, people, projects, and preferences your
              teammates can recall.
            </p>
          </div>
          <div className="flex items-center gap-3">
            {engine && engine.backend !== "store" && (
              <span
                className="rounded-full border px-3 py-1 text-xs text-muted-foreground"
                title={
                  engine.capabilities.length
                    ? `Capability families: ${engine.capabilities.join(", ")}`
                    : "Capabilities not negotiated"
                }
                data-testid="memory-engine-badge"
              >
                engine: {engine.driver_id ?? engine.backend}
                {engine.capabilities.length > 0 && (
                  <> · {engine.capabilities.length} families</>
                )}
              </span>
            )}
            <Button onClick={() => setAddOpen(true)} data-testid="memory-add">
              <Plus className="size-4" /> New memory
            </Button>
          </div>
        </div>

        <EnginePanel engine={engine} />

        {error && (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        <HealthStrip loading={loading} stats={stats} total={entries.length} perType={perType} />

        <div className="flex flex-wrap items-center gap-2">
          <div className="relative flex-1 sm:max-w-xs">
            <Search className="absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search memory…"
              className="pl-8"
            />
          </div>
          <Select value={kind} onValueChange={(v) => v && setKind(v)} items={TYPE_FILTER_LABELS}>
            <SelectTrigger className="w-40">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All types</SelectItem>
              {TYPE_FILTERS.map((t) => (
                <SelectItem key={t} value={t}>
                  {TYPE_FILTER_LABELS[t]}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        {loading ? (
          <div className="grid gap-3 sm:grid-cols-2">
            <Skeleton className="h-28 rounded-xl" />
            <Skeleton className="h-28 rounded-xl" />
          </div>
        ) : filtered.length === 0 ? (
          <EmptyMemory hasEntries={entries.length > 0} />
        ) : (
          <div className="grid gap-3 sm:grid-cols-2">
            {filtered.map((e) => (
              <MemoryCard key={e.id} entry={e} onDelete={() => void remove(e)} />
            ))}
          </div>
        )}
      </div>

      <AddMemoryDialog open={addOpen} onOpenChange={setAddOpen} onAdd={add} />
    </div>
  );
}

function HealthStrip({
  loading,
  stats,
  total,
  perType,
}: {
  loading: boolean;
  stats: MemoryStats | null;
  total: number;
  perType: Record<string, number>;
}) {
  if (loading && !stats) {
    return <Skeleton className="h-16 rounded-xl" />;
  }
  const tiles: { label: string; value: string }[] = [
    { label: "Total items", value: String(total) },
    { label: "Teammate memory", value: String(stats?.agentChunks ?? 0) },
    { label: "Task outcomes", value: String(stats?.taskOutcomes ?? 0) },
    // Across every memory source, not just operator facts — teammates write only
    // context chunks, so a facts-only figure left this stat at "—" forever.
    { label: "Last updated", value: formatUpdated(stats?.lastUpdatedAtMillis ?? 0) },
  ];
  return (
    <Card data-testid="memory-health">
      <CardContent className="flex flex-wrap items-center gap-x-8 gap-y-3 py-4">
        {tiles.map((t) => (
          <div key={t.label} className="space-y-0.5">
            <p className="text-xs text-muted-foreground">{t.label}</p>
            <p className="text-lg font-semibold tabular-nums">{t.value}</p>
          </div>
        ))}
        <div className="flex flex-wrap items-center gap-1.5">
          {MEMORY_KINDS.filter((k) => perType[k]).map((k) => (
            <Badge key={k} variant="outline" className={cn("capitalize", KIND_STYLES[k])}>
              {k} · {perType[k]}
            </Badge>
          ))}
          {CONTEXT_ORIGINS.filter((o) => perType[o]).map((o) => (
            <Badge key={o} variant="outline" className={ORIGIN_STYLES[o]}>
              {ORIGIN_LABELS[o]} · {perType[o]}
            </Badge>
          ))}
        </div>
      </CardContent>
    </Card>
  );
}

function MemoryCard({ entry, onDelete }: { entry: MemoryEntry; onDelete: () => void }) {
  const badge = entryBadge(entry);
  return (
    <Card className="group" data-testid="memory-card">
      <CardContent className="space-y-2 py-4">
        <div className="flex items-start justify-between gap-2">
          <p className="font-medium leading-snug">{entry.title}</p>
          <Badge variant="outline" className={cn("shrink-0 capitalize", badge.style)}>
            {badge.label}
          </Badge>
        </div>
        {entry.body && (
          // Render markdown so **bold**/lists in memory bodies format instead
          // of showing raw markup. Force muted-foreground on every descendant so
          // prose's own palette doesn't override the card's muted body styling.
          <Markdown className="text-muted-foreground [&_*]:text-muted-foreground [&>:first-child]:mt-0 [&>:last-child]:mb-0">
            {entry.body}
          </Markdown>
        )}
        <div className="flex items-center justify-between pt-1">
          <span className="text-xs text-muted-foreground">via {entry.source}</span>
          {/* Delete is only offered on operator facts; agent memory and task
              outcomes are read-only, so they show no delete affordance. */}
          {entry.editable && (
            <Button
              variant="ghost"
              size="icon"
              className="size-7 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 hover:text-destructive"
              onClick={onDelete}
              aria-label="Delete memory"
            >
              <Trash2 className="size-4" />
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

function EmptyMemory({ hasEntries }: { hasEntries: boolean }) {
  return (
    <div className="mt-16 flex flex-col items-center gap-2 text-center text-muted-foreground">
      <Brain className="size-8" />
      <p className="text-sm">{hasEntries ? "No memories match your search." : "No memories yet."}</p>
    </div>
  );
}

function AddMemoryDialog({
  open,
  onOpenChange,
  onAdd,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  onAdd: (fields: { kind: MemoryKind; title: string; body: string }) => Promise<void>;
}) {
  const [kind, setKind] = useState<MemoryKind>("fact");
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState(false);

  function reset() {
    setKind("fact");
    setTitle("");
    setBody("");
  }

  async function submit() {
    if (!title.trim()) return;
    setBusy(true);
    try {
      await onAdd({ kind, title: title.trim(), body: body.trim() });
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "could not save the memory");
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        onOpenChange(o);
        if (!o) reset();
      }}
    >
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>New memory</DialogTitle>
          <DialogDescription>Capture something your company should remember.</DialogDescription>
        </DialogHeader>
        <div className="grid gap-2">
          <Label htmlFor="mem-kind">Type</Label>
          <Select
            value={kind}
            onValueChange={(v) => v && setKind(v as MemoryKind)}
            items={Object.fromEntries(MEMORY_KINDS.map((k) => [k, KIND_LABELS[k]]))}
          >
            <SelectTrigger id="mem-kind" className="w-full">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {MEMORY_KINDS.map((k) => (
                <SelectItem key={k} value={k}>
                  {KIND_LABELS[k]}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="grid gap-2">
          <Label htmlFor="mem-title">Title</Label>
          <Input
            id="mem-title"
            data-testid="memory-title"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder="e.g. Client prefers Friday reviews"
          />
        </div>
        <div className="grid gap-2">
          <Label htmlFor="mem-body">Details</Label>
          <Textarea
            id="mem-body"
            data-testid="memory-body"
            rows={3}
            value={body}
            onChange={(e) => setBody(e.target.value)}
            placeholder="The detail your company should recall."
          />
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
            Cancel
          </Button>
          <Button
            disabled={!title.trim() || busy}
            onClick={() => void submit()}
            data-testid="memory-save"
          >
            {busy && <Loader2 className="mr-1.5 size-4 animate-spin" />}
            Save memory
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** The three families every provider-backed engine must serve. */
/**
 * Frontend copy of the backend's mandatory-family contract. There is no
 * generated contract to import, so this can drift silently if the backend
 * renames a family — the authoritative assertions live in
 * `src/server/routes.rs` (spec_memory_capability_names) and
 * `src/store/memory/test.rs`; a rename must touch all three or the
 * mandatory-floor note below silently stops rendering.
 */
export const MANDATORY_FAMILIES = ["core", "recall", "portability"];

/**
 * What the engine panel does for a given `/spec` answer, as a pure decision
 * so it is unit-testable without a DOM:
 * - `hidden`: no `/spec` yet or an old host (`engine` undefined), or the base
 *   `store` backend — memory served by the host's own stores, nothing to say.
 * - `discard`: the `null` engine — bound, probed, and **throwing every write
 *   away**; the one case that must render as a warning, not a healthy row.
 * - `normal`: a real engine (embedded or remote).
 */
export function enginePanelMode(
  engine: MemorySpec | undefined,
): "hidden" | "normal" | "discard" {
  if (!engine || engine.backend === "store") return "hidden";
  if (engine.backend === "null") return "discard";
  return "normal";
}

/**
 * The boot-probe line, total over the field's whole domain: `true`, `false`,
 * and anything else (absent field, old host, or a literal `null` from a
 * future serializer change) — the render must never be an empty label.
 */
export function probeLabel(healthy: boolean | undefined | null): string {
  if (healthy === true) return "reachable at boot";
  if (healthy === false) return "unreachable at boot — check the endpoint and credential";
  return "not probed";
}

/** Whether the negotiated families are exactly the mandatory floor. */
export function isMandatoryOnly(capabilities: string[]): boolean {
  return (
    capabilities.length === MANDATORY_FAMILIES.length &&
    MANDATORY_FAMILIES.every((f) => capabilities.includes(f))
  );
}

/**
 * The read-only memory-engine panel: which engine is bound, what it
 * negotiated, and whether the boot probe reached it.
 *
 * Read-only by design — engine selection is instance-wide and belongs to the
 * infra operator (the `OPENCOMPANY_MEMORY*` variables, read once at boot), so
 * there is deliberately no setter here: a console admin must never be able to
 * repoint a deployment's storage. `docs/spec/runtime/memory-engine.md` carries
 * the switch runbook this panel points at.
 *
 * Renders nothing for the `store` default (no separate engine to describe) and
 * for a host predating the `/spec` memory field.
 */
export function EnginePanel({ engine }: { engine: MemorySpec | undefined }) {
  const mode = enginePanelMode(engine);
  if (mode === "hidden" || !engine) return null;

  // Exactly the mandatory three means a hosted engine serving the contract
  // floor: worth saying out loud, because the spec's operator rights assume
  // richer families that live only engine-side.
  const mandatoryOnly = isMandatoryOnly(engine.capabilities);

  return (
    <Card data-testid="memory-engine-panel">
      <CardHeader className="pb-3">
        <CardTitle className="text-base">Memory engine</CardTitle>
        <CardDescription>
          Selected by the infra operator (<code className="text-xs">OPENCOMPANY_MEMORY*</code>,
          read at boot) — instance-wide, no setter here by design.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-2 text-sm">
        <div className="flex flex-wrap items-center gap-x-6 gap-y-1">
          <span>
            <span className="text-muted-foreground">Engine </span>
            <span className="font-mono text-xs">{engine.driver_id ?? engine.backend}</span>
          </span>
          <span>
            <span className="text-muted-foreground">Mode </span>
            <span className="font-mono text-xs">{engine.backend}</span>
          </span>
          <span className="inline-flex items-center gap-1.5" data-testid="memory-engine-health">
            <span
              className={cn(
                "size-2 rounded-full",
                // The null engine's green would be a lie beside a write
                // button: reachable, yes — retaining, no. Amber, always.
                mode === "discard"
                  ? "bg-status-blocked"
                  : engine.healthy === true
                    ? "bg-status-done"
                    : engine.healthy === false
                      ? "bg-status-failed"
                      : "bg-muted-foreground/40",
              )}
            />
            {probeLabel(engine.healthy)}
          </span>
        </div>
        {mode === "discard" && (
          <Alert variant="destructive" data-testid="memory-engine-discard">
            <AlertDescription>
              This engine accepts and discards every write — nothing this company is told will
              be remembered, and every read comes back empty, indistinguishable from a company
              that simply hasn't learned anything yet. Adding memories here does nothing until
              the infra operator unsets <code className="text-xs">OPENCOMPANY_MEMORY=null</code>.
            </AlertDescription>
          </Alert>
        )}
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-muted-foreground">Capabilities</span>
          {engine.capabilities.length === 0 ? (
            <span className="text-muted-foreground">
              not negotiated — the in-pod engine is driven directly
            </span>
          ) : (
            engine.capabilities.map((family) => (
              <Badge key={family} variant="outline" className="text-xs">
                {family}
              </Badge>
            ))
          )}
        </div>
        {mandatoryOnly && (
          <p className="text-xs text-muted-foreground">
            The mandatory contract families. Richer families (summary tree, graph, taint) live
            engine-side; this engine serves only the floor.
          </p>
        )}
      </CardContent>
    </Card>
  );
}
