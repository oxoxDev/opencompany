import { useCallback, useEffect, useRef, useState } from "react";
import { AlertTriangle, BadgeCheck, Check, Loader2, Plus } from "lucide-react";

import type { OpenCompanyClient } from "@/api/client";
import {
  getMcpRegistryEntry,
  searchMcpRegistry,
  type McpCatalogueDetail,
  type McpCatalogueEntry,
} from "@/api/mcp-registry";
import type { McpServer } from "@/api/types";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import {
  catalogPublisher,
  REGISTRY_UNWIRED_NOTICE,
  registryOutage,
  type McpRegistryOutage,
} from "@/lib/mcp-registry";
import {
  McpServerIcon,
  openFromItem,
} from "@/views/connections/McpServerTable";

/** How many directory rows one page asks for. */
const PAGE_SIZE = 20;

/** How long a keystroke waits before it costs a directory call. */
const DEBOUNCE_MS = 350;

export type DirectoryState =
  | { kind: "loading" }
  | { kind: "outage"; outage: McpRegistryOutage }
  | {
      kind: "ready";
      entries: McpCatalogueEntry[];
      page: number;
      totalPages: number;
      loadingMore: boolean;
    };

/**
 * The directory, browsed with no query and searched with one. A failure is an
 * outage with a reason, never an exception.
 */
export function useMcpDirectory(
  client: OpenCompanyClient,
  company: string | null,
  query: string,
): { state: DirectoryState; loadMore: () => void } {
  const [state, setState] = useState<DirectoryState>({ kind: "loading" });
  const generation = useRef(0);
  const term = query.trim();

  useEffect(() => {
    generation.current += 1;
    const mine = generation.current;
    setState({ kind: "loading" });
    const timer = window.setTimeout(
      () => {
        void (async () => {
          try {
            const found = await searchMcpRegistry(client, company, {
              q: term || undefined,
              page: 1,
              pageSize: PAGE_SIZE,
            });
            if (generation.current !== mine) return;
            setState({
              kind: "ready",
              entries: found.servers,
              page: found.page,
              totalPages: found.totalPages,
              loadingMore: false,
            });
          } catch (err) {
            if (generation.current !== mine) return;
            setState({ kind: "outage", outage: registryOutage(err) });
          }
        })();
      },
      term === "" ? 0 : DEBOUNCE_MS,
    );
    return () => window.clearTimeout(timer);
  }, [client, company, term]);

  const loadMore = useCallback(() => {
    if (state.kind !== "ready" || state.loadingMore) return;
    if (state.page >= state.totalPages) return;
    const mine = generation.current;
    const next = state.page + 1;
    setState({ ...state, loadingMore: true });
    void (async () => {
      try {
        const found = await searchMcpRegistry(client, company, {
          q: term || undefined,
          page: next,
          pageSize: PAGE_SIZE,
        });
        if (generation.current !== mine) return;
        setState((prev) => {
          if (prev.kind !== "ready") return prev;
          const known = new Set(prev.entries.map((e) => e.qualifiedName));
          return {
            kind: "ready",
            entries: [
              ...prev.entries,
              ...found.servers.filter((e) => !known.has(e.qualifiedName)),
            ],
            page: found.page,
            totalPages: found.totalPages,
            loadingMore: false,
          };
        });
      } catch {
        if (generation.current !== mine) return;
        setState((prev) =>
          prev.kind === "ready" ? { ...prev, loadingMore: false } : prev,
        );
      }
    })();
  }, [client, company, state, term]);

  return { state, loadMore };
}

/** The name this company already holds a directory entry under, if it does. */
export function installedAs(
  servers: McpServer[],
  entry: McpCatalogueEntry,
): string | null {
  const byQualified = servers.find(
    (s) => s.qualifiedName === entry.qualifiedName,
  );
  if (byQualified) return byQualified.name;
  const slug = entry.displayName.trim().toLowerCase();
  const byName = servers.find((s) => s.name.trim().toLowerCase() === slug);
  return byName?.name ?? null;
}

interface EntryProps {
  entry: McpCatalogueEntry;
  installedAs: string | null;
  installing: boolean;
  canManage: boolean;
  onInstall: (entry: McpCatalogueEntry) => void;
  onOpen: (entry: McpCatalogueEntry) => void;
}

function Verified({ official }: { official: boolean }) {
  if (!official) return null;
  return (
    <BadgeCheck
      className="size-4 shrink-0 text-muted-foreground"
      aria-label="Verified publisher"
      data-testid="mcp-discover-verified"
    />
  );
}

function InstallControl({
  entry,
  installedAs,
  installing,
  canManage,
  onInstall,
}: Omit<EntryProps, "onOpen">) {
  if (installedAs !== null) {
    return (
      <span
        className="flex size-8 items-center justify-center rounded-md border border-status-done-text/30 bg-status-done-text/10 text-status-done-text"
        aria-label={`${entry.displayName} is installed`}
        data-testid="mcp-discover-installed"
      >
        <Check className="size-4" />
      </span>
    );
  }
  if (!canManage) return null;
  return (
    <Button
      size="icon-sm"
      variant="outline"
      disabled={installing}
      aria-label={`Install ${entry.displayName}`}
      data-testid="mcp-discover-install"
      onClick={() => onInstall(entry)}
    >
      {installing ? (
        <Loader2 className="size-4 animate-spin" />
      ) : (
        <Plus className="size-4" />
      )}
    </Button>
  );
}

export function McpDirectoryCard(props: EntryProps) {
  const { entry, onOpen } = props;
  const publisher = catalogPublisher(entry);
  return (
    <div
      data-testid="mcp-discover-card"
      onClick={(event) => openFromItem(event, () => onOpen(entry))}
      className="flex cursor-pointer gap-3 rounded-xl border border-border p-4 transition-colors select-none hover:bg-muted/40"
    >
      <McpServerIcon
        iconUrl={entry.iconUrl}
        name={entry.displayName}
        className="size-10"
      />
      <div className="min-w-0 flex-1 space-y-0.5">
        <div className="flex min-w-0 items-center gap-1.5">
          <span className="truncate text-sm font-medium">
            {entry.displayName}
          </span>
          <Verified official={entry.official} />
        </div>
        {entry.description && (
          <p className="line-clamp-2 text-xs text-muted-foreground">
            {entry.description}
          </p>
        )}
        {publisher && (
          <p className="truncate text-xs text-muted-foreground/80">
            by {publisher}
          </p>
        )}
      </div>
      <div className="shrink-0">
        <InstallControl {...props} />
      </div>
    </div>
  );
}

export function McpDirectoryRow(props: EntryProps) {
  const { entry, onOpen } = props;
  const publisher = catalogPublisher(entry);
  return (
    <tr
      data-testid="mcp-discover-row"
      onClick={(event) => openFromItem(event, () => onOpen(entry))}
      className="cursor-pointer transition-colors select-none hover:bg-muted/40"
    >
      <td className="border-b border-border py-3 pr-3 pl-4 align-middle">
        <div className="flex min-w-0 items-center gap-2">
          <McpServerIcon iconUrl={entry.iconUrl} name={entry.displayName} />
          <span className="truncate text-sm font-medium">
            {entry.displayName}
          </span>
          <Verified official={entry.official} />
        </div>
      </td>
      <td className="hidden border-b border-border px-3 py-3 align-middle text-xs text-muted-foreground md:table-cell">
        <span className="block truncate">{publisher ?? "—"}</span>
      </td>
      <td className="border-b border-border py-3 pr-4 pl-3 align-middle">
        <div className="flex justify-end">
          <InstallControl {...props} />
        </div>
      </td>
    </tr>
  );
}

function DirectoryTable({ children }: { children: React.ReactNode }) {
  return (
    <div className="overflow-hidden rounded-lg border border-border">
      <table className="w-full table-fixed border-collapse text-sm">
        <thead>
          <tr>
            <th className="border-b border-border py-2.5 pr-3 pl-4 text-left text-3xs font-medium tracking-wide text-muted-foreground uppercase">
              Connector
            </th>
            <th className="hidden w-48 border-b border-border px-3 py-2.5 text-left text-3xs font-medium tracking-wide text-muted-foreground uppercase md:table-cell">
              Publisher
            </th>
            <th className="w-20 border-b border-border py-2.5 pr-4 pl-3" />
          </tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
    </div>
  );
}

/**
 * One directory entry, before it is installed: what it is, who publishes it,
 * and whether this host can install it.
 */
function McpDirectoryDialog({
  client,
  company,
  entry,
  installedAs,
  installing,
  canManage,
  onInstall,
  onClose,
}: {
  client: OpenCompanyClient;
  company: string | null;
  entry: McpCatalogueEntry | null;
  installedAs: string | null;
  installing: boolean;
  canManage: boolean;
  onInstall: (entry: McpCatalogueEntry) => void;
  onClose: () => void;
}) {
  const [detail, setDetail] = useState<
    | { kind: "loading" }
    | { kind: "ready"; detail: McpCatalogueDetail }
    | { kind: "failed"; message: string }
  >({ kind: "loading" });

  useEffect(() => {
    if (!entry) return;
    let live = true;
    setDetail({ kind: "loading" });
    getMcpRegistryEntry(client, company, entry.qualifiedName)
      .then((found) => live && setDetail({ kind: "ready", detail: found }))
      .catch((err) => {
        if (!live) return;
        const outage = registryOutage(err);
        setDetail({
          kind: "failed",
          message:
            outage.kind === "unwired" ? REGISTRY_UNWIRED_NOTICE : outage.message,
        });
      });
    return () => {
      live = false;
    };
  }, [client, company, entry]);

  if (!entry) return null;
  const publisher = catalogPublisher(entry);
  const refusal =
    detail.kind === "ready" && !detail.detail.installable
      ? detail.detail.refusal
      : undefined;

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-lg" data-testid="mcp-discover-detail">
        <DialogHeader>
          <div className="flex items-center gap-3">
            <McpServerIcon
              iconUrl={entry.iconUrl}
              name={entry.displayName}
              className="size-12"
            />
            <div className="min-w-0">
              <DialogTitle className="flex items-center gap-1.5">
                <span className="truncate">{entry.displayName}</span>
                <Verified official={entry.official} />
              </DialogTitle>
              {publisher && (
                <DialogDescription>by {publisher}</DialogDescription>
              )}
            </div>
          </div>
        </DialogHeader>
        <div className="space-y-3 text-sm">
          <p className="text-muted-foreground">
            {entry.description ?? "The directory listing carries no description."}
          </p>
          {detail.kind === "loading" ? (
            <Skeleton className="h-8 rounded-md" />
          ) : detail.kind === "failed" ? (
            <p className="text-xs text-destructive">{detail.message}</p>
          ) : detail.detail.endpoint ? (
            <code className="block truncate rounded-md border border-border bg-muted/40 px-2 py-1 font-mono text-xs select-text">
              {detail.detail.endpoint}
            </code>
          ) : null}
          {refusal && (
            <p className="text-xs text-status-blocked-text" data-testid="mcp-discover-refusal">
              {refusal}
            </p>
          )}
          {!entry.official && (
            <p
              className="flex items-start gap-2 rounded-md border border-status-blocked-text/30 bg-status-blocked-text/10 px-2 py-1 text-xs text-status-blocked-text"
              data-testid="mcp-directory-unverified"
            >
              <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
              <span>
                Not a verified publisher. Every tool it exposes still starts
                un-granted until somebody sets it.
              </span>
            </p>
          )}
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={onClose}>
            Close
          </Button>
          {installedAs !== null ? (
            <Button disabled data-testid="mcp-discover-detail-installed">
              <Check className="size-4" /> Installed
            </Button>
          ) : (
            canManage && (
              <Button
                data-testid="mcp-discover-detail-install"
                disabled={installing || refusal !== undefined}
                onClick={() => onInstall(entry)}
              >
                {installing ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <Plus className="size-4" />
                )}
                Install
              </Button>
            )
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** Discover: the directory as cards or a list, browsed before anything is typed. */
export function McpDiscover({
  client,
  company,
  query,
  layout,
  servers,
  installing,
  canManage,
  onInstall,
}: {
  client: OpenCompanyClient;
  company: string | null;
  query: string;
  layout: "cards" | "list";
  servers: McpServer[];
  installing: string | null;
  canManage: boolean;
  onInstall: (entry: McpCatalogueEntry) => void;
}) {
  const { state, loadMore } = useMcpDirectory(client, company, query);
  const [previewing, setPreviewing] = useState<McpCatalogueEntry | null>(null);
  const searching = query.trim() !== "";

  if (state.kind === "outage") {
    return state.outage.kind === "unwired" ? (
      <p className="text-xs text-muted-foreground" data-testid="mcp-registry-unwired">
        {REGISTRY_UNWIRED_NOTICE}
      </p>
    ) : (
      <p className="text-xs text-status-blocked-text" data-testid="mcp-registry-error">
        <strong className="font-medium">The directory isn&apos;t answering.</strong>{" "}
        {state.outage.message} Your own servers are unaffected.
      </p>
    );
  }

  const itemProps = (entry: McpCatalogueEntry): EntryProps => ({
    entry,
    installedAs: installedAs(servers, entry),
    installing: installing === entry.qualifiedName,
    canManage,
    onInstall,
    onOpen: setPreviewing,
  });

  return (
    <section className="space-y-3" data-testid="mcp-discover">
      <div className="flex items-center gap-2">
        <h3 className="text-sm font-medium">
          {searching ? "Results" : "Top connectors"}
        </h3>
        {state.kind === "loading" && (
          <Loader2 className="size-3.5 animate-spin text-muted-foreground" />
        )}
      </div>

      {state.kind === "loading" ? (
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          {Array.from({ length: 4 }, (_, i) => (
            <Skeleton key={i} className="h-24 rounded-xl" />
          ))}
        </div>
      ) : state.entries.length === 0 ? (
        <p className="text-sm text-muted-foreground" data-testid="mcp-search-nothing">
          {searching
            ? "The directory has no listing for that. A server running inside your own network is added as a custom server."
            : "The directory returned nothing to show."}
        </p>
      ) : layout === "cards" ? (
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          {state.entries.map((entry) => (
            <McpDirectoryCard key={entry.qualifiedName} {...itemProps(entry)} />
          ))}
        </div>
      ) : (
        <DirectoryTable>
          {state.entries.map((entry) => (
            <McpDirectoryRow key={entry.qualifiedName} {...itemProps(entry)} />
          ))}
        </DirectoryTable>
      )}

      {state.kind === "ready" && state.page < state.totalPages && (
        <div className="flex justify-center">
          <Button
            variant="outline"
            size="sm"
            disabled={state.loadingMore}
            data-testid="mcp-discover-more"
            onClick={loadMore}
          >
            {state.loadingMore && <Loader2 className="size-4 animate-spin" />}
            Show more
          </Button>
        </div>
      )}

      <McpDirectoryDialog
        client={client}
        company={company}
        entry={previewing}
        installedAs={previewing ? installedAs(servers, previewing) : null}
        installing={previewing !== null && installing === previewing.qualifiedName}
        canManage={canManage}
        onInstall={onInstall}
        onClose={() => setPreviewing(null)}
      />
    </section>
  );
}
