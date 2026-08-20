import { useCallback, useEffect, useRef, useState } from "react";
import { AlertTriangle, Info, Loader2, Search } from "lucide-react";
import { toast } from "sonner";

import type { OpenCompanyClient } from "@/api/client";
import {
  getMcpRegistryEntry,
  installMcpRegistryEntry,
  searchMcpRegistry,
  type McpCatalogueDetail,
  type McpCatalogueEntry,
} from "@/api/mcp-registry";
import { ApiError } from "@/api/types";
import {
  missingEnvKeys,
  REGISTRY_UNWIRED_NOTICE,
  registryOutage,
  type McpRegistryOutage,
} from "@/lib/mcp-registry";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

/** How many directory rows one page asks for. */
const PAGE_SIZE = 10;

type Results =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "outage"; outage: McpRegistryOutage }
  | { kind: "ready"; page: number; totalPages: number; servers: McpCatalogueEntry[] };

/** The install dialog's state for the one entry an operator picked. */
type Picked =
  | { kind: "loading"; qualifiedName: string }
  | { kind: "failed"; qualifiedName: string; outage: McpRegistryOutage }
  | { kind: "ready"; detail: McpCatalogueDetail };

interface Props {
  client: OpenCompanyClient;
  company: string | null;
  /** Called after a successful install so the merged server list re-reads. */
  onInstalled: () => void;
}

/**
 * Browse the upstream MCP directories and install from them (issue #1270).
 *
 * The MCP tab could not discover anything: an operator had to arrive already
 * knowing a server's URL and type it in, so the tab stayed empty until someone
 * pasted something into it. This is the catalogue half — the host federates
 * Smithery.ai and `modelcontextprotocol/registry`, and an entry installed here
 * lands in the **same** list above with a `registry` badge, not in a second
 * section.
 *
 * ## It cannot take the server list down with it
 *
 * Everything here is fenced inside this component's own state. The directories
 * are two network hops away and either can be down; the host itself may be
 * built without the `mcp` feature, in which case these routes answer
 * `404 not_wired`. Both are rendered as a notice *inside this panel* — an empty
 * result with a reason — while the company's installed servers above go on
 * rendering. A dead directory is not a broken tab.
 *
 * ## Credentials
 *
 * An entry declares the env keys it needs, and those are the only fields the
 * install form has. Their values are write-only: they go out with the install
 * and no route ever sends one back, so nothing here reads or renders one.
 */
export function McpRegistryBrowser({ client, company, onInstalled }: Props) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<Results>({ kind: "idle" });
  const [picked, setPicked] = useState<Picked | null>(null);
  const [env, setEnv] = useState<Record<string, string>>({});
  const [installing, setInstalling] = useState(false);
  const [installError, setInstallError] = useState<string | null>(null);
  // Only the newest search may write its answer. Two searches issued quickly —
  // a fresh query while a page turn is in flight — can land out of order, and
  // the older answer arriving last would paint results for a query the operator
  // has already replaced. Bumped on every scope change too, so an answer for
  // the company just left cannot repaint this panel.
  const generation = useRef(0);

  // Switching company re-keys everything this panel shows. Without the reset,
  // one company's directory page and a half-filled install form stay on screen
  // under another company's heading.
  useEffect(() => {
    generation.current += 1;
    setResults({ kind: "idle" });
    setPicked(null);
    setEnv({});
    setInstallError(null);
  }, [client, company]);

  const runSearch = useCallback(
    async (page: number) => {
      generation.current += 1;
      const mine = generation.current;
      setResults({ kind: "loading" });
      try {
        const found = await searchMcpRegistry(client, company, {
          q: query,
          page,
          pageSize: PAGE_SIZE,
        });
        if (generation.current !== mine) return;
        setResults({
          kind: "ready",
          page: found.page,
          totalPages: found.totalPages,
          servers: found.servers,
        });
      } catch (err) {
        if (generation.current !== mine) return;
        // Classified, never rethrown: see the component doc.
        setResults({ kind: "outage", outage: registryOutage(err) });
      }
    },
    [client, company, query],
  );

  async function pick(entry: McpCatalogueEntry) {
    setInstallError(null);
    setEnv({});
    setPicked({ kind: "loading", qualifiedName: entry.qualifiedName });
    try {
      const detail = await getMcpRegistryEntry(client, company, entry.qualifiedName);
      setPicked({ kind: "ready", detail });
    } catch (err) {
      setPicked({
        kind: "failed",
        qualifiedName: entry.qualifiedName,
        outage: registryOutage(err),
      });
    }
  }

  async function install(detail: McpCatalogueDetail) {
    if (installing) return;
    const missing = missingEnvKeys(detail.requiredEnvKeys, env);
    if (missing.length > 0) {
      setInstallError(
        `This server needs a value for ${missing.join(", ")} before it can be installed.`,
      );
      return;
    }
    setInstalling(true);
    setInstallError(null);
    try {
      const res = await installMcpRegistryEntry(client, company, {
        qualifiedName: detail.qualifiedName,
        env,
      });
      // An install that lands "needs a credential" is NOT a rollback — the host
      // says so explicitly — so it is reported where the operator can act on it
      // rather than dressed up as a failed install.
      if (res.test && res.test.status !== "ok") {
        setInstallError(res.test.message);
      } else {
        toast.success(`Installed ${detail.displayName}. ${res.note}`);
        setPicked(null);
      }
      setEnv({});
      onInstalled();
    } catch (err) {
      setInstallError(
        err instanceof ApiError ? err.message : "Couldn't install that server.",
      );
    } finally {
      setInstalling(false);
    }
  }

  return (
    <div className="space-y-3 border-t border-border pt-3" data-testid="mcp-registry-browser">
      <div className="space-y-1">
        <h4 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          Add from the MCP directory
        </h4>
        <p className="text-xs text-muted-foreground">
          Search Smithery and the official MCP registry. An installed server joins the list above
          and every teammate can call it.
        </p>
      </div>

      <form
        className="flex items-end gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          void runSearch(1);
        }}
      >
        <div className="flex-1 space-y-1">
          <Label htmlFor="mcp-registry-q" className="text-xs">
            Search the directory
          </Label>
          <Input
            id="mcp-registry-q"
            data-testid="mcp-registry-search"
            value={query}
            placeholder="github, linear, postgres…"
            autoComplete="off"
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <Button type="submit" variant="secondary" disabled={results.kind === "loading"}>
          {results.kind === "loading" ? (
            <Loader2 className="size-4 animate-spin" />
          ) : (
            <Search className="size-4" />
          )}{" "}
          Search
        </Button>
      </form>

      {results.kind === "outage" && <OutageNotice outage={results.outage} />}

      {results.kind === "ready" &&
        (results.servers.length === 0 ? (
          <p className="text-sm text-muted-foreground" data-testid="mcp-registry-empty">
            No hosted servers matched that. The directory only offers servers this host can dial
            over HTTP — one that runs as a local subprocess is not listed.
          </p>
        ) : (
          <>
            <ul className="divide-y divide-border" data-testid="mcp-registry-results">
              {results.servers.map((entry) => (
                <li key={entry.qualifiedName} className="space-y-2 py-2 first:pt-0">
                  <div className="flex items-start gap-2">
                    <CatalogueIcon entry={entry} />
                    <div className="min-w-0 flex-1 space-y-0.5">
                      <div className="flex flex-wrap items-center gap-1.5">
                        <span className="text-sm font-medium">{entry.displayName}</span>
                        {entry.official && <Badge variant="secondary">official</Badge>}
                        <Badge variant="outline">{entry.source}</Badge>
                      </div>
                      <p className="truncate font-mono text-xs text-muted-foreground">
                        {entry.qualifiedName}
                      </p>
                      {entry.description && (
                        <p className="text-xs text-muted-foreground">{entry.description}</p>
                      )}
                    </div>
                    <Button
                      size="sm"
                      variant="outline"
                      data-testid="mcp-registry-pick"
                      disabled={installing}
                      onClick={() => void pick(entry)}
                    >
                      Install
                    </Button>
                  </div>
                  {isPicked(picked, entry.qualifiedName) && (
                    <InstallForm
                      picked={picked}
                      env={env}
                      setEnv={setEnv}
                      installing={installing}
                      error={installError}
                      onCancel={() => setPicked(null)}
                      onInstall={install}
                    />
                  )}
                </li>
              ))}
            </ul>
            {results.totalPages > 1 && (
              <div className="flex items-center gap-2 text-xs text-muted-foreground">
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={results.page <= 1}
                  onClick={() => void runSearch(results.page - 1)}
                >
                  Previous
                </Button>
                <span>
                  Page {results.page} of {results.totalPages}
                </span>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={results.page >= results.totalPages}
                  onClick={() => void runSearch(results.page + 1)}
                >
                  Next
                </Button>
              </div>
            )}
          </>
        ))}
    </div>
  );
}

/** Whether the open install dialog belongs to this row. */
function isPicked(picked: Picked | null, qualifiedName: string): picked is Picked {
  if (picked === null) return false;
  const of = picked.kind === "ready" ? picked.detail.qualifiedName : picked.qualifiedName;
  return of === qualifiedName;
}

/**
 * The directory is not answering.
 *
 * A missing build feature is said as a missing build feature — an operator can
 * do nothing about it and it is not a fault of theirs or of the network — while
 * a real failure carries the host's own sentence. Neither is a toast: both are
 * facts that stay true while the panel is open.
 */
function OutageNotice({ outage }: { outage: McpRegistryOutage }) {
  if (outage.kind === "unwired") {
    return (
      <Alert data-testid="mcp-registry-unwired">
        <Info className="size-4" />
        <AlertTitle>The MCP directory isn&apos;t in this build</AlertTitle>
        <AlertDescription>{REGISTRY_UNWIRED_NOTICE}</AlertDescription>
      </Alert>
    );
  }
  return (
    <Alert variant="destructive" data-testid="mcp-registry-error">
      <AlertTriangle className="size-4" />
      <AlertTitle>Couldn&apos;t search the MCP directory</AlertTitle>
      <AlertDescription>
        {outage.message} The servers above are unaffected — this is the catalogue, not your
        installed set.
      </AlertDescription>
    </Alert>
  );
}

/**
 * The install form for one picked entry.
 *
 * Its fields are exactly the env keys the entry declared, and they are password
 * inputs with no value ever read back: an install credential is write-only in
 * both directions, the same rule List A's token field follows.
 */
function InstallForm({
  picked,
  env,
  setEnv,
  installing,
  error,
  onCancel,
  onInstall,
}: {
  picked: Picked;
  env: Record<string, string>;
  setEnv: (next: Record<string, string>) => void;
  installing: boolean;
  error: string | null;
  onCancel: () => void;
  onInstall: (detail: McpCatalogueDetail) => void;
}) {
  if (picked.kind === "loading") {
    return (
      <p className="flex items-center gap-1 text-xs text-muted-foreground">
        <Loader2 className="size-3 animate-spin" /> Reading what this server needs…
      </p>
    );
  }
  if (picked.kind === "failed") {
    return (
      <p className="text-xs text-destructive" data-testid="mcp-registry-entry-error">
        {picked.outage.kind === "unwired"
          ? REGISTRY_UNWIRED_NOTICE
          : `${picked.outage.message} Nothing was installed.`}
      </p>
    );
  }

  const detail = picked.detail;
  if (!detail.installable) {
    return (
      <p className="text-xs text-destructive" data-testid="mcp-registry-refusal">
        {detail.refusal ?? "This host can't install that server."}
      </p>
    );
  }

  return (
    <div className="space-y-2 rounded-md bg-muted/40 p-2" data-testid="mcp-registry-install-form">
      {detail.endpoint && (
        <p className="font-mono text-xs break-all text-muted-foreground">{detail.endpoint}</p>
      )}
      {detail.requiredEnvKeys.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          This server asks for no credentials — installing connects it straight away.
        </p>
      ) : (
        detail.requiredEnvKeys.map((key) => (
          <div key={key} className="space-y-1">
            <Label htmlFor={`mcp-env-${key}`} className="font-mono text-xs">
              {key}
            </Label>
            <Input
              id={`mcp-env-${key}`}
              data-testid="mcp-registry-env-field"
              type="password"
              autoComplete="new-password"
              placeholder="write-only"
              value={env[key] ?? ""}
              onChange={(e) => setEnv({ ...env, [key]: e.target.value })}
            />
          </div>
        ))
      )}
      {error && (
        <p className="text-xs text-destructive" data-testid="mcp-registry-install-error">
          {error}
        </p>
      )}
      <div className="flex items-center gap-2">
        <Button
          size="sm"
          data-testid="mcp-registry-install"
          disabled={installing}
          onClick={() => onInstall(detail)}
        >
          {installing ? <Loader2 className="size-4 animate-spin" /> : null} Install
        </Button>
        <Button size="sm" variant="ghost" disabled={installing} onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </div>
  );
}

/** A catalogue icon, falling back to an initial when the remote image dies. */
function CatalogueIcon({ entry }: { entry: McpCatalogueEntry }) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [entry.iconUrl]);

  if (!entry.iconUrl || failed) {
    return (
      <span
        aria-hidden="true"
        className="flex size-6 shrink-0 items-center justify-center rounded-md bg-muted text-xs font-semibold text-muted-foreground"
      >
        {entry.displayName.charAt(0).toUpperCase()}
      </span>
    );
  }
  return (
    <img
      src={entry.iconUrl}
      alt=""
      aria-hidden="true"
      loading="lazy"
      className="size-6 shrink-0 rounded-md object-contain"
      onError={() => setFailed(true)}
    />
  );
}
