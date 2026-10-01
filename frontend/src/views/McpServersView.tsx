import { useEffect, useState } from "react";
import { Info } from "lucide-react";

import { me as fetchMe } from "@/api/auth";
import type { OpenCompanyClient } from "@/api/client";
import type { RosterAgent } from "@/api/types";
import { PageHeader } from "@/components/page-header";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { McpServersSection } from "@/views/connections/McpServersSection";

interface Props {
  client: OpenCompanyClient;
  company: string | null;
}

/**
 * Connections, MCP Servers: the company's tool servers and the directory they
 * come from. `#/connections/mcp?tab=json` opens the mcp.json editor.
 */
export function McpServersView({ client, company }: Props) {
  // Adding or removing a server changes what tools the company's agents can
  // call, so it is an admin's (issue #403). Courtesy only: the host answers 403
  // whatever this says. Reading the installed set stays open.
  const [canManage, setCanManage] = useState(false);
  // The roster, for the per-teammate lens on a server's tool permissions. A host
  // with no team plane 404s, and the lens then does not render at all.
  const [agents, setAgents] = useState<RosterAgent[]>([]);

  useEffect(() => {
    let live = true;
    void (async () => {
      let admin = false;
      try {
        admin = (await fetchMe(client, company)).role === "admin";
      } catch {
        // No user plane on this host, or not signed in — treat as non-admin.
      }
      if (live) setCanManage(admin);
    })();
    return () => {
      live = false;
    };
  }, [client, company]);

  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const roster = await client.listTeam(company);
        if (live) {
          setAgents(
            roster.map((m) => ({ id: m.id, name: m.name?.trim() || m.role })),
          );
        }
      } catch {
        // No team plane on this host. The lens does not render.
        if (live) setAgents([]);
      }
    })();
    return () => {
      live = false;
    };
  }, [client, company]);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <PageHeader
        title="MCP Servers"
        width="full"
        description={
          <>
            The tool servers this company&apos;s agents can call, from its manifest and the
            ones you add here.
          </>
        }
      />
      <div className="min-h-0 w-full flex-1 space-y-6 overflow-y-auto px-4 py-6">
        {!canManage && (
          <Alert data-testid="mcp-read-only">
            <Info className="size-4" />
            <AlertTitle>Only an admin can change this company&apos;s tool servers</AlertTitle>
            <AlertDescription>
              A server here hands every agent a new set of tools, so an admin adds and removes
              them. You can see what is installed.
            </AlertDescription>
          </Alert>
        )}

        <McpServersSection
          client={client}
          company={company}
          canManage={canManage}
          chrome="standalone"
          agents={agents}
        />
      </div>
    </div>
  );
}
