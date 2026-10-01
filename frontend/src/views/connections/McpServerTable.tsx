import { useState } from "react";
import {
  AlertTriangle,
  Check,
  Info,
  KeyRound,
  Loader2,
  LogIn,
  MoreHorizontal,
  Plug,
  Power,
  PowerOff,
  RefreshCw,
  Server,
  ShieldCheck,
  Trash2,
  Unplug,
  Wrench,
} from "lucide-react";

import type { McpHealth, McpServer } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import type { McpBridgeState } from "@/lib/mcp-bridge";
import { mcpHealthBadge } from "@/lib/mcp-bridge";
import { mcpRowControls, mcpSourceBadge } from "@/lib/mcp-registry";
import { McpReachCell } from "@/views/connections/mcp-reach-cell";

/** Everything a row's controls can do, owned by the section that holds the state. */
export interface McpRowActions {
  onOpen: (name: string) => void;
  onSignIn: (server: McpServer) => void;
  onAddToken: (server: McpServer) => void;
  onRotateEnv: (server: McpServer) => void;
  onLifecycle: (server: McpServer, direction: "connect" | "disconnect") => void;
  onTest: (server: McpServer) => void;
  onTools: (server: McpServer) => void;
  onPermissions: (name: string) => void;
  onToggle: (server: McpServer, enabled: boolean) => void;
  onRemove: (server: McpServer) => void;
}

/** Which one labelled action this server's state actually calls for. */
export type PrimaryAction =
  | { kind: "sign_in" }
  | { kind: "add_token" }
  | { kind: "rotate_env" }
  | { kind: "connect" }
  | null;

/** What a row or card shows for one installed server. */
export interface McpServerItemProps {
  server: McpServer;
  health?: McpHealth;
  bridge: McpBridgeState;
  canManage: boolean;
  /** The name of the row holding the mutation lock, or `null`. */
  busy: string | null;
  primary: PrimaryAction;
  /** Whether an OAuth sign-in for this row is still waiting on the other tab. */
  signingIn: boolean;
  actions: McpRowActions;
}

/** A server's mark, or a letter tile. Only an inline image is ever loaded. */
export function McpServerIcon({
  iconUrl,
  name,
  className = "size-7",
}: {
  iconUrl?: string;
  name: string;
  className?: string;
}) {
  const [failed, setFailed] = useState(false);
  if (!iconUrl?.startsWith("data:image/") || failed) {
    return (
      <span
        aria-hidden="true"
        className={`flex shrink-0 items-center justify-center rounded-md border border-border bg-muted/40 text-xs font-semibold text-muted-foreground ${className}`}
      >
        {name.charAt(0).toUpperCase() || <Server className="size-3.5" />}
      </span>
    );
  }
  return (
    <img
      src={iconUrl}
      alt=""
      aria-hidden="true"
      className={`shrink-0 rounded-md border border-border object-contain ${className}`}
      onError={() => setFailed(true)}
    />
  );
}

function HealthBadge({
  health,
  authConfigured,
  bridge,
}: {
  health?: McpHealth;
  authConfigured: boolean;
  bridge: McpBridgeState;
}) {
  const badge = mcpHealthBadge(health, authConfigured, bridge);
  if (!badge) return null;
  const tone =
    badge.tone === "delivering"
      ? { className: "text-status-done-text", Icon: Check }
      : badge.tone === "configured"
        ? { className: "text-muted-foreground", Icon: Info }
        : badge.tone === "warn"
          ? { className: "text-status-blocked-text", Icon: AlertTriangle }
          : { className: "text-destructive", Icon: AlertTriangle };
  return (
    <span
      className={`inline-flex items-center gap-1 text-xs whitespace-nowrap ${tone.className}`}
    >
      <tone.Icon className="size-3" /> {badge.label}
    </span>
  );
}

const ITEM_CONTROLS =
  "button, a, input, select, textarea, label, [role='menuitem'], [role='checkbox'], [role='menu']";

/** Opens the server unless the click landed on one of the item's own controls. */
export function openFromItem(
  event: React.MouseEvent<HTMLElement>,
  open: () => void,
) {
  if (event.detail > 1) return;
  if ((event.target as HTMLElement | null)?.closest(ITEM_CONTROLS)) return;
  open();
}

function primaryLabel(server: McpServer, primary: PrimaryAction) {
  if (primary === null) return null;
  if (primary.kind === "sign_in") {
    return {
      short: "Sign in",
      long: `Sign in to ${server.name}`,
      Icon: LogIn,
      testId: "mcp-sign-in",
    };
  }
  if (primary.kind === "connect") {
    return {
      short: "Connect",
      long: `Connect ${server.name}`,
      Icon: Plug,
      testId: "mcp-lifecycle",
    };
  }
  return {
    short: server.authConfigured ? "Replace credential" : "Add credential",
    long: server.authConfigured
      ? `Replace ${server.name}'s API token`
      : `Add an API token for ${server.name}`,
    Icon: KeyRound,
    testId: primary.kind === "rotate_env" ? "mcp-rotate-env" : "mcp-add-token",
  };
}

function runPrimary(
  server: McpServer,
  primary: PrimaryAction,
  actions: McpRowActions,
) {
  if (primary === null) return;
  if (primary.kind === "sign_in") actions.onSignIn(server);
  else if (primary.kind === "add_token") actions.onAddToken(server);
  else if (primary.kind === "rotate_env") actions.onRotateEnv(server);
  else actions.onLifecycle(server, "connect");
}

function PrimaryButton({
  server,
  primary,
  canManage,
  busy,
  actions,
  compact = false,
}: Pick<McpServerItemProps, "server" | "primary" | "canManage" | "busy" | "actions"> & {
  compact?: boolean;
}) {
  const label = primaryLabel(server, primary);
  if (!label || !canManage) return null;
  return (
    <Button
      size="sm"
      disabled={busy !== null}
      aria-label={label.long}
      data-mcp-primary="true"
      data-testid={label.testId}
      onClick={() => runPrimary(server, primary, actions)}
    >
      {busy === server.name ? (
        <Loader2 className="size-3.5 animate-spin" />
      ) : (
        <label.Icon className="size-3.5" />
      )}
      <span className={compact ? "hidden sm:inline" : undefined}>
        {label.short}
      </span>
    </Button>
  );
}

function OverflowMenu({
  server,
  health,
  canManage,
  busy,
  primary,
  actions,
}: Omit<McpServerItemProps, "bridge" | "signingIn">) {
  const controls = mcpRowControls(server, health);
  const dial = controls.lifecycle === "none" ? null : controls.lifecycle;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={`More actions for ${server.name}`}
            data-testid="mcp-row-overflow"
            disabled={busy !== null}
          />
        }
      >
        <MoreHorizontal className="size-4" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem
          data-testid="mcp-permissions"
          onClick={() => actions.onPermissions(server.name)}
        >
          <ShieldCheck className="mr-2 size-4" />
          Tool permissions
        </DropdownMenuItem>
        {controls.probe && (
          <>
            <DropdownMenuItem
              data-testid="mcp-test"
              onClick={() => actions.onTest(server)}
            >
              <RefreshCw className="mr-2 size-4" />
              Re-check
            </DropdownMenuItem>
            <DropdownMenuItem
              data-testid="mcp-tools"
              onClick={() => actions.onTools(server)}
            >
              <Wrench className="mr-2 size-4" />
              List its tools
            </DropdownMenuItem>
          </>
        )}
        {dial !== null && canManage && primary?.kind !== "connect" && (
          <DropdownMenuItem
            data-testid="mcp-lifecycle"
            onClick={() => actions.onLifecycle(server, dial)}
          >
            {dial === "connect" ? (
              <Plug className="mr-2 size-4" />
            ) : (
              <Unplug className="mr-2 size-4" />
            )}
            {dial === "connect" ? "Connect" : "Disconnect"}
          </DropdownMenuItem>
        )}
        {controls.toggle && canManage && (
          <DropdownMenuItem
            data-testid="mcp-toggle"
            onClick={() => actions.onToggle(server, !server.enabled)}
          >
            {server.enabled ? (
              <PowerOff className="mr-2 size-4" />
            ) : (
              <Power className="mr-2 size-4" />
            )}
            {server.enabled ? "Turn off" : "Turn on"}
          </DropdownMenuItem>
        )}
        {controls.removal.kind !== "none" && canManage && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              variant="destructive"
              data-testid="mcp-remove"
              onClick={() => actions.onRemove(server)}
            >
              <Trash2 className="mr-2 size-4" />
              Remove
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function StatusCell({
  server,
  health,
  bridge,
  signingIn,
}: Pick<McpServerItemProps, "server" | "health" | "bridge" | "signingIn">) {
  const controls = mcpRowControls(server, health);
  return (
    <div className="flex flex-col gap-0.5">
      <HealthBadge
        health={health}
        authConfigured={server.authConfigured}
        bridge={bridge}
      />
      {controls.toggle && !server.enabled && (
        <span
          className="text-3xs text-muted-foreground"
          data-testid="mcp-disabled-badge"
        >
          off
        </span>
      )}
      {signingIn && (
        <span
          className="text-3xs text-status-blocked-text"
          data-testid="mcp-signing-in"
        >
          waiting for sign-in
        </span>
      )}
    </div>
  );
}

function NameButton({
  server,
  actions,
}: Pick<McpServerItemProps, "server" | "actions">) {
  return (
    <button
      type="button"
      data-testid="mcp-server-open"
      className="block max-w-full truncate rounded-sm text-left text-sm font-medium hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
      onClick={() => actions.onOpen(server.name)}
      aria-label={`Open ${server.name}`}
    >
      {server.name}
    </button>
  );
}

const HEADERS: { label: string; className: string }[] = [
  { label: "Server", className: "w-full" },
  { label: "Source", className: "hidden md:table-cell" },
  { label: "Status", className: "" },
  { label: "Reach", className: "hidden lg:table-cell" },
  { label: "", className: "" },
];

export function McpServerTable({ children }: { children: React.ReactNode }) {
  return (
    <div className="overflow-hidden rounded-lg border border-border">
      <table className="w-full border-collapse text-sm">
        <thead>
          <tr>
            {HEADERS.map((head, i) => (
              <th
                key={head.label || `spacer-${i}`}
                className={`border-b border-border px-3 py-2.5 text-left text-3xs font-medium tracking-wide whitespace-nowrap text-muted-foreground uppercase first:pl-4 last:pr-4 ${head.className}`}
              >
                {head.label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
    </div>
  );
}

export function McpServerRow(props: McpServerItemProps) {
  const { server, bridge, actions } = props;
  const badge = mcpSourceBadge(server.source);
  const reach = server.reachableBy;
  const open = () => actions.onOpen(server.name);
  return (
    <tr
      data-testid="mcp-server-row"
      onClick={(event) => openFromItem(event, open)}
      className="cursor-pointer transition-colors select-none hover:bg-muted/40"
    >
      <td className="w-full max-w-0 border-b border-border py-3 pr-3 pl-4 align-middle">
        <div className="flex min-w-0 items-center gap-2">
          <McpServerIcon iconUrl={server.iconUrl} name={server.name} />
          <div className="min-w-0">
            <NameButton server={server} actions={actions} />
          </div>
        </div>
      </td>
      <td className="hidden border-b border-border px-3 py-3 align-middle whitespace-nowrap md:table-cell">
        <Badge variant={badge.variant} data-testid="mcp-source-badge">
          {badge.label}
        </Badge>
      </td>
      <td className="border-b border-border px-3 py-3 align-middle whitespace-nowrap">
        <StatusCell {...props} />
      </td>
      <td className="hidden max-w-48 border-b border-border px-3 py-3 align-middle lg:table-cell">
        {bridge === "absent" || reach === undefined || !server.enabled ? (
          <span className="text-xs text-muted-foreground">—</span>
        ) : (
          <McpReachCell
            agents={reach}
            serverName={server.name}
            onOverflow={open}
          />
        )}
      </td>
      <td className="border-b border-border py-3 pr-4 pl-3 align-middle">
        <div className="flex items-center justify-end gap-1">
          <PrimaryButton {...props} compact />
          <OverflowMenu {...props} />
        </div>
      </td>
    </tr>
  );
}

/** The installed servers as cards. */
export function McpServerGrid({ children }: { children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2" data-testid="mcp-server-grid">
      {children}
    </div>
  );
}

export function McpServerCard(props: McpServerItemProps) {
  const { server, actions } = props;
  const badge = mcpSourceBadge(server.source);
  const blurb = server.description?.trim() || server.probedDescription?.trim();
  return (
    <div
      data-testid="mcp-server-card"
      onClick={(event) => openFromItem(event, () => actions.onOpen(server.name))}
      className="flex cursor-pointer gap-3 rounded-xl border border-border p-4 transition-colors select-none hover:bg-muted/40"
    >
      <McpServerIcon
        iconUrl={server.iconUrl}
        name={server.name}
        className="size-10"
      />
      <div className="min-w-0 flex-1 space-y-1">
        <div className="flex min-w-0 items-center gap-2">
          <NameButton server={server} actions={actions} />
          <Badge
            variant={badge.variant}
            className="shrink-0"
            data-testid="mcp-source-badge"
          >
            {badge.label}
          </Badge>
        </div>
        {blurb && (
          <p className="line-clamp-2 text-xs text-muted-foreground">{blurb}</p>
        )}
        <StatusCell {...props} />
      </div>
      <div className="flex shrink-0 flex-col items-end gap-1">
        <OverflowMenu {...props} />
        <PrimaryButton {...props} />
      </div>
    </div>
  );
}
