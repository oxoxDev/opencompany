// Per-tool permissions for one MCP server (issue #2373).
//
// Opens under the server's row and is addressed by `?permissions=<name>`, so a
// link reaches the server whose permissions are in question rather than the
// page that lists it.
//
// Everything rendered here is the document the host echoed back. A control
// writes and then re-renders from the response, never from what was clicked:
// the host resolves a mode out of the override, the tier's bulk default and the
// declaration, so the answer to "what did that click do" is not derivable in the
// console without shipping a second copy of that ladder — the drift issue #414
// is about.

import { useCallback, useEffect, useState } from "react";
import { Loader2, RotateCcw, X } from "lucide-react";

import type { OpenCompanyClient } from "@/api/client";
import {
  type ApprovalMode,
  type ToolPolicyDocument,
  type ToolPolicyPatch,
  type ToolTier,
  policyTarget,
  readToolPolicy,
  resetToolPolicy,
  writeToolPolicy,
} from "@/api/mcp-tool-policy";
import { ApiError, type McpServer } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

/** What each mode does, in the words the operator is choosing between. */
const MODE_LABELS: Record<ApprovalMode, string> = {
  always_allow: "Runs",
  needs_approval: "Asks",
  blocked: "Blocked",
};

/**
 * The value the tier control carries when nothing is stored for that tier.
 *
 * A sentinel rather than an absent value: the control has to be able to say
 * "nothing is set here" and to be set back to it, and a `Select` with no value
 * can do neither.
 */
const UNSET = "unset";

/** The tier control's own vocabulary: the three modes, plus "nothing set". */
const TIER_DEFAULT_LABELS: Record<string, string> = {
  [UNSET]: "Not set",
  ...MODE_LABELS,
};

/** The tiers, in the order they escalate. */
const TIERS: readonly ToolTier[] = ["read_only", "interactive", "write_delete"];

const TIER_LABELS: Record<ToolTier, string> = {
  read_only: "Read-only",
  interactive: "Interactive",
  write_delete: "Write & delete",
};

/**
 * Why a tool the panel lists is unreachable regardless of what its row says.
 *
 * `allowedTools` / `disallowedTools` are a separate gate, enforced where the
 * server is attached to an agent rather than at the approval ladder — so a row
 * can read "Asks" while the transport refuses the call outright. Rendering the
 * row without saying so invites an operator to set a mode that will never be
 * consulted.
 */
function exclusion(server: McpServer, tool: string): string | null {
  if (server.disallowedTools.includes(tool)) return "Not sent — on the deny list";
  if (server.allowedTools.length > 0 && !server.allowedTools.includes(tool)) {
    return "Not sent — off the allow list";
  }
  return null;
}

/**
 * The patch a choice in the tier control means on the wire.
 *
 * The sentinel and the absence it stands for are two vocabularies, and the
 * translation between them is the whole of issue #2373's tier half: a tier
 * cleared back to unset has to arrive as `null`, because the host reads a
 * missing key as "leave it alone" and would keep the bulk allow standing.
 */
export function tierPatch(tier: ToolTier, value: string): ToolPolicyPatch {
  return { tierDefaults: { [tier]: value === UNSET ? null : (value as ApprovalMode) } };
}

interface Props {
  client: OpenCompanyClient;
  company: string | null;
  server: McpServer;
  /** Writes are an admin's. The host answers 403 whatever this says. */
  canManage: boolean;
  /**
   * Bumped by the page when a probe re-ran against this server.
   *
   * A probe rewrites the stored inventory, and the inventory is what a tier
   * default resolves against — so a panel that re-checked a server while open
   * goes on rendering the tool list from before the probe, empty state and all,
   * until it is closed and reopened.
   */
  reloadKey?: number;
  onClose: () => void;
}

type State =
  | { kind: "loading" }
  | { kind: "ready"; doc: ToolPolicyDocument }
  /** The stored document cannot be parsed. Clearing it is the way back. */
  | { kind: "unreadable"; message: string }
  | { kind: "failed"; message: string };

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function McpToolPermissions({
  client,
  company,
  server,
  canManage,
  reloadKey = 0,
  onClose,
}: Props) {
  const [state, setState] = useState<State>({ kind: "loading" });
  const [busy, setBusy] = useState(false);
  const [writeError, setWriteError] = useState<string | null>(null);

  const target = policyTarget(server);
  const targetKey = target
    ? target.kind === "registry"
      ? `registry:${target.serverId}`
      : `declared:${target.name}`
    : null;

  useEffect(() => {
    if (!target) {
      setState({
        kind: "failed",
        message: "This row has no install behind it, so it carries no permissions.",
      });
      return;
    }
    let live = true;
    setState({ kind: "loading" });
    void (async () => {
      try {
        const doc = await readToolPolicy(client, company, target);
        if (live) setState({ kind: "ready", doc });
      } catch (err) {
        if (!live) return;
        setState(
          err instanceof ApiError && err.code === "policy_unreadable"
            ? { kind: "unreadable", message: err.message }
            : { kind: "failed", message: message(err) },
        );
      }
    })();
    return () => {
      live = false;
    };
    // `target` is rebuilt every render from the row; `targetKey` is the value
    // this effect actually depends on.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, company, targetKey, reloadKey]);

  const apply = useCallback(
    async (patch: ToolPolicyPatch) => {
      if (!target) return;
      setBusy(true);
      setWriteError(null);
      try {
        const doc = await writeToolPolicy(client, company, target, patch);
        setState({ kind: "ready", doc });
      } catch (err) {
        setWriteError(message(err));
      } finally {
        setBusy(false);
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [client, company, targetKey],
  );

  const reset = useCallback(async () => {
    if (!target) return;
    setBusy(true);
    setWriteError(null);
    try {
      const doc = await resetToolPolicy(client, company, target);
      setState({ kind: "ready", doc });
    } catch (err) {
      setWriteError(message(err));
    } finally {
      setBusy(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, company, targetKey]);

  return (
    <div
      className="space-y-3 rounded-md bg-muted/40 p-3"
      data-testid="mcp-tool-permissions"
    >
      <div className="flex items-start justify-between gap-2">
        <div className="space-y-0.5">
          <p className="text-sm font-medium">Tool permissions</p>
          <p className="text-xs text-muted-foreground">
            What happens when a teammate calls one of {server.name}&apos;s tools.
            Blocked refuses the call outright — no approver can wave it through.
          </p>
        </div>
        <Button
          size="sm"
          variant="ghost"
          onClick={onClose}
          aria-label="Close tool permissions"
          data-testid="mcp-permissions-close"
        >
          <X className="size-4" />
        </Button>
      </div>

      {state.kind === "loading" && (
        <p className="flex items-center gap-1 text-xs text-muted-foreground">
          <Loader2 className="size-3 animate-spin" /> Reading this server&apos;s permissions…
        </p>
      )}

      {state.kind === "failed" && (
        <p className="text-xs text-destructive" data-testid="mcp-permissions-failed">
          {state.message}
        </p>
      )}

      {state.kind === "unreadable" && (
        <div className="space-y-2" data-testid="mcp-permissions-unreadable">
          <p className="text-xs text-destructive">{state.message}</p>
          {canManage && (
            <Button
              size="sm"
              variant="outline"
              disabled={busy}
              onClick={() => void reset()}
              data-testid="mcp-permissions-clear"
            >
              {busy ? <Loader2 className="size-4 animate-spin" /> : "Clear stored permissions"}
            </Button>
          )}
        </div>
      )}

      {state.kind === "ready" && (
        <>
          <div className="space-y-1">
            <p className="text-xs font-medium">Per tier</p>
            <div className="flex flex-wrap gap-3">
              {TIERS.map((tier) => (
                <div key={tier} className="space-y-1">
                  <Label htmlFor={`tier-${tier}`} className="text-xs text-muted-foreground">
                    {TIER_LABELS[tier]}
                  </Label>
                  <Select
                    value={
                      state.doc.tierDefaults[tier].stored
                        ? state.doc.tierDefaults[tier].mode
                        : UNSET
                    }
                    onValueChange={(v) => v && void apply(tierPatch(tier, v))}
                    items={TIER_DEFAULT_LABELS}
                    disabled={!canManage || busy}
                  >
                    <SelectTrigger id={`tier-${tier}`} className="w-36">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value={UNSET}>Not set</SelectItem>
                      {(Object.keys(MODE_LABELS) as ApprovalMode[]).map((mode) => (
                        <SelectItem key={mode} value={mode}>
                          {MODE_LABELS[mode]}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
              ))}
            </div>
          </div>

          {state.doc.tools.length === 0 ? (
            <p className="text-xs text-muted-foreground" data-testid="mcp-permissions-empty">
              No tools are listed for this server yet. A tier above still applies to
              every tool in it; re-check the server to list what it actually has.
            </p>
          ) : (
            <ul className="space-y-2">
              {state.doc.tools.map((row) => (
                <li
                  key={row.tool}
                  className="flex flex-wrap items-center gap-2"
                  data-testid="mcp-permission-row"
                >
                  <span className="min-w-40 flex-1 font-mono text-xs">{row.tool}</span>
                  {exclusion(server, row.tool) && (
                    <Badge variant="outline" className="text-3xs text-muted-foreground">
                      {exclusion(server, row.tool)}
                    </Badge>
                  )}
                  {row.suggestedTier && row.suggestedTier !== row.effectiveTier && (
                    <Badge variant="outline" className="text-3xs">
                      discovery said {TIER_LABELS[row.suggestedTier]}
                    </Badge>
                  )}
                  <Select
                    value={row.effectiveTier}
                    onValueChange={(v) =>
                      v && void apply({ tools: [{ tool: row.tool, tier: v as ToolTier }] })
                    }
                    items={TIER_LABELS}
                    disabled={!canManage || busy}
                  >
                    <SelectTrigger
                      aria-label={`Tier for ${row.tool}`}
                      className="w-36"
                    >
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {TIERS.map((tier) => (
                        <SelectItem key={tier} value={tier}>
                          {TIER_LABELS[tier]}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <Select
                    value={row.mode}
                    onValueChange={(v) =>
                      v && void apply({ tools: [{ tool: row.tool, mode: v as ApprovalMode }] })
                    }
                    items={MODE_LABELS}
                    disabled={!canManage || busy}
                  >
                    <SelectTrigger
                      aria-label={`What happens when ${row.tool} is called`}
                      className="w-32"
                    >
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {(Object.keys(MODE_LABELS) as ApprovalMode[]).map((mode) => (
                        <SelectItem key={mode} value={mode}>
                          {MODE_LABELS[mode]}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  {row.isOverride && canManage && (
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={busy}
                      aria-label={`Clear the decision on ${row.tool}`}
                      data-testid="mcp-permission-clear-row"
                      onClick={() => void apply({ tools: [{ tool: row.tool }] })}
                    >
                      <RotateCcw className="size-3.5" />
                    </Button>
                  )}
                </li>
              ))}
            </ul>
          )}

          {writeError && (
            <p className="text-xs text-destructive" data-testid="mcp-permissions-write-error">
              {writeError}
            </p>
          )}
        </>
      )}
    </div>
  );
}
