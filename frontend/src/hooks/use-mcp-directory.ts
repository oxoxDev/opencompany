import { useCallback, useEffect, useRef, useState } from "react";

import type { OpenCompanyClient } from "@/api/client";
import { searchMcpRegistry, type McpCatalogueEntry } from "@/api/mcp-registry";
import { registryOutage, type McpRegistryOutage } from "@/lib/mcp-registry";

/** How many directory rows one page asks for. */
export const DIRECTORY_PAGE_SIZE = 20;

/** How long a keystroke waits before it costs a directory call. */
export const DIRECTORY_DEBOUNCE_MS = 350;

export type DirectoryState =
  | {
      kind: "loading";
      /** The rows from the previous answer, still shown while this one runs. */
      previous: McpCatalogueEntry[];
    }
  | { kind: "outage"; outage: McpRegistryOutage }
  | {
      kind: "ready";
      entries: McpCatalogueEntry[];
      page: number;
      totalPages: number;
      loadingMore: boolean;
      /** Why the last "Show more" failed, until the next attempt. */
      moreFailed: string | null;
    };

export interface McpDirectory {
  state: DirectoryState;
  loadMore: () => void;
  retry: () => void;
}

function isAbort(err: unknown): boolean {
  return err instanceof DOMException
    ? err.name === "AbortError"
    : err instanceof Error && err.name === "AbortError";
}

/** Rows of `next` not already in `known`, in order. */
export function appendPage(
  known: McpCatalogueEntry[],
  next: McpCatalogueEntry[],
): McpCatalogueEntry[] {
  const seen = new Set(known.map((e) => e.qualifiedName));
  const fresh: McpCatalogueEntry[] = [];
  for (const entry of next) {
    if (seen.has(entry.qualifiedName)) continue;
    seen.add(entry.qualifiedName);
    fresh.push(entry);
  }
  return [...known, ...fresh];
}

/**
 * The directory, browsed with no query and searched with one.
 *
 * A new term aborts the request it supersedes, so a slow earlier answer can
 * never land over a newer one, and the rows already on screen stay there until
 * the new answer replaces them. A failure is an outage with a reason, never an
 * exception.
 */
export function useMcpDirectory(
  client: OpenCompanyClient,
  company: string | null,
  query: string,
): McpDirectory {
  const [state, setState] = useState<DirectoryState>({
    kind: "loading",
    previous: [],
  });
  const [attempt, setAttempt] = useState(0);
  const current = useRef<AbortController | null>(null);
  const stateRef = useRef(state);
  stateRef.current = state;
  const term = query.trim();

  useEffect(() => {
    const controller = new AbortController();
    current.current?.abort();
    current.current = controller;
    setState((prev) => ({
      kind: "loading",
      previous:
        prev.kind === "ready"
          ? prev.entries
          : prev.kind === "loading"
            ? prev.previous
            : [],
    }));
    const timer = window.setTimeout(
      () => {
        void (async () => {
          try {
            const found = await searchMcpRegistry(
              client,
              company,
              { q: term || undefined, page: 1, pageSize: DIRECTORY_PAGE_SIZE },
              { signal: controller.signal },
            );
            if (controller.signal.aborted) return;
            setState({
              kind: "ready",
              entries: appendPage([], found.servers),
              page: found.page,
              totalPages: found.totalPages,
              loadingMore: false,
              moreFailed: null,
            });
          } catch (err) {
            if (controller.signal.aborted || isAbort(err)) return;
            setState({ kind: "outage", outage: registryOutage(err) });
          }
        })();
      },
      term === "" ? 0 : DIRECTORY_DEBOUNCE_MS,
    );
    return () => {
      window.clearTimeout(timer);
      controller.abort();
    };
  }, [client, company, term, attempt]);

  const loadMore = useCallback(() => {
    const now = stateRef.current;
    if (now.kind !== "ready" || now.loadingMore) return;
    if (now.page >= now.totalPages) return;
    const controller = current.current;
    if (!controller || controller.signal.aborted) return;
    const next = now.page + 1;
    setState({ ...now, loadingMore: true, moreFailed: null });
    void (async () => {
      try {
        const found = await searchMcpRegistry(
          client,
          company,
          { q: term || undefined, page: next, pageSize: DIRECTORY_PAGE_SIZE },
          { signal: controller.signal },
        );
        if (controller.signal.aborted) return;
        setState((prev) =>
          prev.kind === "ready"
            ? {
                kind: "ready",
                entries: appendPage(prev.entries, found.servers),
                page: found.page,
                totalPages: found.totalPages,
                loadingMore: false,
                moreFailed: null,
              }
            : prev,
        );
      } catch (err) {
        if (controller.signal.aborted || isAbort(err)) return;
        const outage = registryOutage(err);
        setState((prev) =>
          prev.kind === "ready"
            ? {
                ...prev,
                loadingMore: false,
                moreFailed:
                  outage.kind === "error"
                    ? outage.message
                    : "The directory didn't return more servers.",
              }
            : prev,
        );
      }
    })();
  }, [client, company, term]);

  const retry = useCallback(() => setAttempt((n) => n + 1), []);

  return { state, loadMore, retry };
}
