import { useState } from "react";
import { AlertTriangle, CheckCircle2, Loader2, Plus } from "lucide-react";

import type { OpenCompanyClient } from "@/api/client";
import { addMcpServer, updateMcpServer } from "@/api/mcp";
import { ApiError, type McpHealth, type McpMutationResponse } from "@/api/types";
import { Button } from "@/components/ui/button";
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
import type { McpBridgeState } from "@/lib/mcp-bridge";

type Phase =
  | { kind: "form" }
  | { kind: "saving" }
  | { kind: "connected"; result: McpMutationResponse };

/**
 * Adding a custom server: a name and its URL. A server that answers is
 * confirmed here; one that needs a sign-in or a credential continues in the
 * connect dialog.
 */
export function McpAddServerDialog({
  client,
  company,
  open,
  bridge,
  onOpenChange,
  onAdded,
  onConnect,
  onOpenServer,
}: {
  client: OpenCompanyClient;
  company: string | null;
  open: boolean;
  bridge: McpBridgeState;
  onOpenChange: (open: boolean) => void;
  /** Called once the list should re-read. */
  onAdded: () => void;
  /** The server was added but is not connected yet. */
  onConnect: (name: string, health: McpHealth | undefined) => void;
  onOpenServer: (name: string) => void;
}) {
  const [phase, setPhase] = useState<Phase>({ kind: "form" });
  const [name, setName] = useState("");
  const [endpoint, setEndpoint] = useState("");
  const [nameError, setNameError] = useState<string | null>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const [describing, setDescribing] = useState(false);

  function reset() {
    setPhase({ kind: "form" });
    setName("");
    setEndpoint("");
    setNameError(null);
    setFormError(null);
  }

  function close() {
    if (phase.kind === "saving") return;
    onOpenChange(false);
    reset();
  }

  async function submit() {
    if (phase.kind === "saving") return;
    setNameError(null);
    setFormError(null);
    if (!name.trim() || !endpoint.trim()) {
      setFormError("A server needs a name and an https URL.");
      return;
    }
    setPhase({ kind: "saving" });
    try {
      const result = await addMcpServer(client, company, {
        name: name.trim(),
        endpoint: endpoint.trim(),
      });
      onAdded();
      if (result.test?.status === "ok") {
        setPhase({ kind: "connected", result });
        return;
      }
      onOpenChange(false);
      reset();
      onConnect(result.server.name, result.test);
    } catch (err) {
      const sentence =
        err instanceof ApiError ? err.message : "Couldn't add the server.";
      if (/already exists|already configured/i.test(sentence)) {
        setNameError(sentence);
      } else {
        setFormError(sentence);
      }
      setPhase({ kind: "form" });
    }
  }

  async function useProbedDescription(server: string, probed: string) {
    setDescribing(true);
    try {
      await updateMcpServer(client, company, server, { description: probed });
      onAdded();
      close();
    } catch (err) {
      setFormError(
        err instanceof ApiError ? err.message : "Couldn't save that description.",
      );
    } finally {
      setDescribing(false);
    }
  }

  const saving = phase.kind === "saving";

  return (
    <Dialog open={open} onOpenChange={(next) => (next ? onOpenChange(true) : close())}>
      <DialogContent className="sm:max-w-md" data-testid="mcp-add-dialog">
        {phase.kind === "connected" ? (
          <Connected
            result={phase.result}
            describing={describing}
            error={formError}
            onUseProbed={useProbedDescription}
            onOpenServer={(server) => {
              close();
              onOpenServer(server);
            }}
            onDone={close}
          />
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>Add custom server</DialogTitle>
              <DialogDescription>
                Only connect servers you trust — every agent you grant it can
                call whatever tools it offers. Sign-in or a token comes next if
                the server asks for one.
              </DialogDescription>
            </DialogHeader>

            <form
              className="space-y-3"
              onSubmit={(event) => {
                event.preventDefault();
                void submit();
              }}
            >
              <div className="space-y-1">
                <Label htmlFor="mcp-name" className="text-xs">
                  Name
                </Label>
                <Input
                  id="mcp-name"
                  data-testid="mcp-add-name"
                  value={name}
                  placeholder="notion"
                  autoFocus
                  aria-invalid={nameError !== null}
                  onChange={(e) => {
                    setName(e.target.value);
                    setNameError(null);
                  }}
                />
                {nameError && (
                  <p className="text-xs text-destructive" data-testid="mcp-add-name-error">
                    {nameError} Open it instead, or pick another name.
                  </p>
                )}
              </div>
              <div className="space-y-1">
                <Label htmlFor="mcp-endpoint" className="text-xs">
                  MCP URL
                </Label>
                <Input
                  id="mcp-endpoint"
                  name="mcp-endpoint-url"
                  data-testid="mcp-add-endpoint"
                  value={endpoint}
                  placeholder="https://mcp.example.com/mcp"
                  autoComplete="url"
                  className="font-mono"
                  onChange={(e) => setEndpoint(e.target.value)}
                />
              </div>

              {bridge === "absent" && (
                <p
                  className="flex items-start gap-2 rounded-md border border-status-blocked-text/30 bg-status-blocked-text/10 px-2 py-1 text-xs text-status-blocked-text"
                  data-testid="mcp-add-no-bridge"
                >
                  <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
                  <span>
                    This deployment has no MCP bridge, so the server will be
                    stored but no agent will receive its tools.
                  </span>
                </p>
              )}

              {formError && (
                <p className="text-xs text-destructive" data-testid="mcp-add-error">
                  {formError}
                </p>
              )}

              <DialogFooter>
                <Button type="button" variant="ghost" disabled={saving} onClick={close}>
                  Cancel
                </Button>
                <Button
                  type="submit"
                  data-testid="mcp-add-submit"
                  disabled={saving || nameError !== null}
                >
                  {saving ? (
                    <Loader2 className="size-4 animate-spin" />
                  ) : (
                    <Plus className="size-4" />
                  )}
                  {bridge === "absent" ? "Save anyway" : "Add"}
                </Button>
              </DialogFooter>
            </form>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

function Connected({
  result,
  describing,
  error,
  onUseProbed,
  onOpenServer,
  onDone,
}: {
  result: McpMutationResponse;
  describing: boolean;
  error: string | null;
  onUseProbed: (server: string, probed: string) => void;
  onOpenServer: (server: string) => void;
  onDone: () => void;
}) {
  const server = result.server;
  const toolCount = result.test?.toolCount ?? 0;
  const probed = server.probedDescription?.trim();
  const offerProbed =
    probed !== undefined && probed !== "" && !server.description?.trim();

  return (
    <>
      <DialogHeader>
        <DialogTitle>Added {server.name}</DialogTitle>
      </DialogHeader>
      <div className="space-y-3">
        <div
          className="flex items-start gap-2 rounded-md border border-status-done-text/30 bg-status-done-text/10 p-3 text-sm"
          data-testid="mcp-add-outcome"
        >
          <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-status-done-text" />
          <div className="space-y-0.5">
            <p className="font-medium">
              Added and connected · {toolCount} tool{toolCount === 1 ? "" : "s"}
            </p>
            <p className="text-xs text-muted-foreground">
              Each tool starts on its tier&apos;s default permission until you
              change it.
            </p>
          </div>
        </div>
        {result.warning && (
          <p className="text-xs text-status-blocked-text">{result.warning}</p>
        )}
        {offerProbed && (
          <div
            className="space-y-2 rounded-md border border-border bg-muted/30 p-2"
            data-testid="mcp-add-probed-description"
          >
            <p className="text-xs text-muted-foreground">
              This server describes itself as:{" "}
              <span className="text-foreground">{probed}</span>
            </p>
            <Button
              size="sm"
              variant="outline"
              disabled={describing}
              data-testid="mcp-add-use-probed"
              onClick={() => onUseProbed(server.name, probed)}
            >
              {describing ? <Loader2 className="size-4 animate-spin" /> : "Use that description"}
            </Button>
          </div>
        )}
        {error && (
          <p className="text-xs text-destructive" data-testid="mcp-add-error">
            {error}
          </p>
        )}
      </div>
      <DialogFooter>
        <Button variant="ghost" onClick={onDone}>
          Done
        </Button>
        <Button data-testid="mcp-add-open-server" onClick={() => onOpenServer(server.name)}>
          Open server
        </Button>
      </DialogFooter>
    </>
  );
}
