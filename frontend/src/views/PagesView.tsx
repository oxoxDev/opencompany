import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AppWindow } from "lucide-react";

import type { OpenCompanyClient } from "@/api/client";
import type { PageManifestDto } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

interface Props {
  client: OpenCompanyClient;
  company: string | null;
}

type Load = "loading" | "ready" | "error";

/**
 * A bridge request from a page: `{type: "oc:graphql", id, query, variables}`
 * (docs/spec/runtime/pages.md §6, plan §6). The page's own `client.query`
 * (`frontend/pages-sdk/client.ts`) sends exactly this shape.
 */
interface GraphQLBridgeMessage {
  type: "oc:graphql";
  id: string;
  query: string;
  /** The per-document capability minted for the currently loaded iframe. */
  capability: string;
  variables?: Record<string, unknown>;
}

function isGraphQLBridgeMessage(value: unknown): value is GraphQLBridgeMessage {
  return (
    typeof value === "object" &&
    value !== null &&
    (value as { type?: unknown }).type === "oc:graphql" &&
    typeof (value as { id?: unknown }).id === "string" &&
    typeof (value as { query?: unknown }).query === "string" &&
    typeof (value as { capability?: unknown }).capability === "string"
  );
}

/**
 * Agent-authored internal dashboard pages, rendered in a sandboxed iframe.
 *
 * Each page is real React, compiled server-side and served at
 * `client.pageUrl(slug, company)` — a fixed HTML shell (not agent content)
 * that mounts the page's own compiled bundle inside a
 * `sandbox="allow-scripts"` iframe with no `allow-same-origin`. That sandbox
 * is the actual security boundary (docs/spec/runtime/pages.md §5): the
 * iframe holds no session cookie and can make no credentialed request of its
 * own, so live data reaches it only through the postMessage bridge this view
 * owns — every `oc:graphql` request the page sends is executed here, with
 * this console's own authenticated `client.graphqlRequest`, and the result is
 * posted back. Both queries and mutations are forwarded verbatim: the sandbox
 * protects the operator's *session*, not what an authorized request can *do*
 * once it crosses the bridge (see the plan's §6 for why this is deliberate).
 */
export function PagesView({ client, company }: Props) {
  const [load, setLoad] = useState<Load>("loading");
  const [error, setError] = useState<string | null>(null);
  const [pages, setPages] = useState<PageManifestDto[]>([]);
  const [activeSlug, setActiveSlug] = useState("");
  const iframeRef = useRef<HTMLIFrameElement>(null);
  // The per-document bridge capability. Rotated on every iframe `load`, so a
  // document the page navigated itself to — which shares the same
  // `contentWindow` and could not have received the current token — is rejected.
  const capabilityRef = useRef<string>("");

  // Only nav-visible pages appear in the sidebar (`nav_visible = false` in
  // `page.toml` deliberately keeps one off the nav, reachable only by direct
  // URL). Alphabetical within, so the list order doesn't jump around as
  // pages are added.
  const visible = useMemo(
    () =>
      pages
        .filter((p) => p.navVisible !== false)
        .slice()
        .sort((a, b) => a.title.localeCompare(b.title)),
    [pages],
  );
  const active = visible.find((p) => p.slug === activeSlug) ?? visible[0];

  // Mint a fresh capability for the newly loaded iframe document and hand it
  // to that document via postMessage. Because `sandbox="allow-scripts"` makes
  // the frame opaque-origin, we cannot target it by origin — but any document
  // the page later navigates itself to has no way to learn this token, so only
  // the exact document we just minted it for can speak through the bridge.
  const handleLoad = useCallback(() => {
    const frame = iframeRef.current;
    const cap =
      typeof globalThis.crypto?.randomUUID === "function"
        ? globalThis.crypto.randomUUID()
        : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
    capabilityRef.current = cap;
    frame?.contentWindow?.postMessage({ type: "oc:init", capability: cap }, "*");
  }, []);

  const loadRun = useRef(0);
  const loadPages = useCallback(async () => {
    const run = ++loadRun.current;
    try {
      const rows = await client.listPages(company);
      if (run !== loadRun.current) return;
      setPages(rows);
      setActiveSlug((current) => (rows.some((p) => p.slug === current) ? current : (rows[0]?.slug ?? "")));
      setError(null);
      setLoad("ready");
    } catch (cause) {
      if (run !== loadRun.current) return;
      // No fixture fallback: a host that can't serve pages says so rather
      // than render an invented list.
      setPages([]);
      setError(cause instanceof Error ? cause.message : "Couldn't load pages.");
      setLoad("error");
    }
  }, [client, company]);

  useEffect(() => {
    setLoad("loading");
    void loadPages();
    return () => {
      loadRun.current += 1;
    };
  }, [loadPages]);

  // The bridge: forwards a page's GraphQL request to the console's own
  // authenticated endpoint and posts the answer back. Scoped to the active
  // iframe element and torn down on unmount or whenever the selected page
  // changes, so a stale iframe (the previous page, already unmounted) can
  // never be mistaken for the source of a later request.
  useEffect(() => {
    function onMessage(event: MessageEvent) {
      // The actual authentication of "did this really come from my own
      // embedded page":
      //   * `source` identity — only this console's own iframe element.
      //   * `event.origin === "null"` — only an opaque-origin sandboxed iframe
      //     reports the literal `"null"` origin; any other frame or tab has a
      //     real origin.
      //   * the per-document `capability` — rotated on every `load`, so a
      //     document the page navigated itself to cannot replay it. This is
      //     what closes the post-navigation exfiltration window that
      //     `event.source` alone (which survives navigation) would leave open.
      if (event.source !== iframeRef.current?.contentWindow) return;
      if (event.origin !== "null") return;
      if (!isGraphQLBridgeMessage(event.data)) return;
      if (event.data.capability !== capabilityRef.current) return;
      const { id, query, variables } = event.data;
      const replyTo = event.source as Window;
      void client
        .graphqlRequest(query, variables)
        .then((result) => {
          replyTo.postMessage({ type: "oc:graphql:result", id, data: result.data, errors: result.errors }, "*");
        })
        .catch((cause: unknown) => {
          replyTo.postMessage(
            {
              type: "oc:graphql:result",
              id,
              errors: [{ message: cause instanceof Error ? cause.message : "request failed" }],
            },
            "*",
          );
        });
    }

    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [client, active?.slug]);

  if (load === "loading") {
    return (
      <div className="flex flex-1 gap-2 p-4">
        <div className="w-64 shrink-0 space-y-2">
          {Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} className="h-9 rounded-lg" />
          ))}
        </div>
        <Skeleton className="flex-1 rounded-lg" />
      </div>
    );
  }

  if (load === "error") {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-3 text-center text-muted-foreground">
        <AppWindow className="size-8" />
        <div className="space-y-1">
          <p className="font-medium text-foreground">Pages unavailable</p>
          <p className="max-w-sm text-sm">{error}</p>
        </div>
        <Button variant="outline" size="sm" onClick={() => void loadPages()}>
          Try again
        </Button>
      </div>
    );
  }

  if (visible.length === 0) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-3 text-center text-muted-foreground">
        <AppWindow className="size-8" />
        <div className="space-y-1">
          <p className="font-medium text-foreground">No pages yet</p>
          <p className="max-w-sm text-sm">
            Ask the <span className="font-medium">Page Builder</span> to design one — a metrics
            view, a pipeline board, a status page — and it shows up here.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-1 overflow-hidden">
      <section className="hidden w-64 shrink-0 flex-col overflow-y-auto border-r py-2 md:flex" data-testid="pages-list">
        {visible.map((page) => (
          <button
            key={page.slug}
            onClick={() => setActiveSlug(page.slug)}
            data-testid="pages-list-item"
            className={cn(
              "flex flex-col items-start gap-0.5 px-3 py-2 text-left text-sm transition-colors",
              page.slug === active?.slug ? "bg-accent text-accent-foreground" : "hover:bg-accent/50",
            )}
          >
            <span className="truncate font-medium">{page.title || page.slug}</span>
            {page.description && (
              <span className="truncate text-xs text-muted-foreground">{page.description}</span>
            )}
          </button>
        ))}
      </section>
      <section className="flex flex-1 flex-col overflow-hidden">
        {active ? (
          <iframe
            key={active.slug}
            ref={iframeRef}
            onLoad={handleLoad}
            sandbox="allow-scripts"
            src={client.pageUrl(active.slug, company)}
            title={active.title || active.slug}
            style={{ width: "100%", height: "100%", border: "none" }}
          />
        ) : (
          <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
            Select a page.
          </div>
        )}
      </section>
    </div>
  );
}
