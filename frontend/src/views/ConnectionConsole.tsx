// One connection's console: discovery, company selection, and the app shell.
//
// This is the phase machine `App` used to own. Moving it here is what makes a
// second host an *instance* rather than a second code path — and it is what
// makes failure local. `App`'s version set a single global phase, so any host
// being unreachable blanked the whole console and any expired session dropped
// the whole app to a login screen. With N connections that is the wrong shape:
// one host being down has to redden one row and leave the others working.

import { useCallback, useEffect, useMemo, useState } from "react";
import { Loader2 } from "lucide-react";

import { OpenCompanyClient } from "@/api/client";
import type { SignIn } from "@/api/auth";
import { ApiError, type CompanyStatus } from "@/api/types";
import { AppShell } from "@/components/app-shell";
import { CompanyPicker } from "@/components/company-picker";
import { ConsoleChrome } from "@/components/host-switcher";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { ConnectionScopeProvider } from "@/connections/ConnectionContext";
import { adoptSession, probe, useConnection } from "@/connections/registry";
import type { ConnectionId } from "@/connections/types";
import { Login } from "@/views/Login";
import { SetupWizard } from "@/views/setup/SetupWizard";

type Phase =
  | { kind: "loading" }
  | { kind: "error"; message: string; hint?: string }
  | { kind: "login"; company: string | null; notice?: string }
  // A host that has never been through the first-run setup flow. Entered ahead
  // of `login` on purpose: an unconfigured host may have no company and no
  // users at all, so a sign-in form there addresses nobody.
  | { kind: "setup" }
  // A configured host (setup has completed) that nonetheless has no
  // companies — an operator who ran setup for host settings alone, or who
  // deleted the only company afterward. Distinct from `error`: this is not a
  // connection problem, and the way out is back into setup, not a retry.
  | { kind: "no-company" }
  | { kind: "picker"; companies: CompanyStatus[] }
  | {
      kind: "console";
      company: string | null;
      status: CompanyStatus;
      companies: CompanyStatus[];
      canGoBack: boolean;
    };

interface Props {
  connectionId: ConnectionId;
  client: OpenCompanyClient;
  /** The company this console was configured to open, if any. */
  defaultCompany: string | null;
  /** A notice from a sign-in attempt that happened before this mounted. */
  notice?: string;
  /** Forces the sign-in view — a magic link that failed to redeem, say. */
  forceLogin?: boolean;
  /**
   * An address to offer the sign-in form, when the caller knows one.
   *
   * Only the embedded host has one: this client starts it, so it can be told
   * who that host admits (#632). Undefined everywhere else, and the form is
   * blank as before.
   */
  suggestedEmail?: string;
}

export function ConnectionConsole({
  connectionId,
  client,
  defaultCompany,
  notice,
  forceLogin,
  suggestedEmail,
}: Props) {
  const [phase, setPhase] = useState<Phase>(
    forceLogin ? { kind: "login", company: defaultCompany, notice } : { kind: "loading" },
  );
  // Incremented to re-run discovery on demand. The boot effect's dependencies
  // (`client`, `defaultCompany`, `forceLogin`) never change when a sign-in
  // completes, so without an explicit epoch a successful re-login would either
  // hang on "Connecting…" (nothing re-ran the boot) or reload the document
  // (every other connection's stream died with it).
  const [bootEpoch, setBootEpoch] = useState(0);
  const connection = useConnection(connectionId);

  // The registry marks this connection `unauthenticated` when its client sees a
  // 401 — that is the *only* 401 handler, so one host refusing a credential
  // cannot reach into another host's view. Deriving the sign-in screen from
  // that status rather than from a second callback is what keeps it that way.
  const refused = connection?.status === "unauthenticated";

  // Re-probe this connection and re-run the boot effect. A reload would tear
  // down every *other* connection's stream to recover this one; bumping the
  // epoch keeps the recovery local.
  const reBoot = useCallback(
    (result?: SignIn) => {
      // Store the session *before* probing. A cross-origin sign-in's token is
      // the only proof this connection has — no cookie was set — and adopting
      // it replaces the client, so a probe that ran first would authenticate
      // with the pre-sign-in client and conclude the host still refuses us.
      if (result?.session) adoptSession(connectionId, result.session);
      void probe(connectionId).then(() => {
        setPhase({ kind: "loading" });
        setBootEpoch((n) => n + 1);
      });
    },
    [connectionId],
  );

  useEffect(() => {
    // `forceLogin` forces the sign-in view until the *first* boot; a sign-in
    // bumps the epoch, so the forced-login recover cannot re-enter the boot.
    if (forceLogin && bootEpoch === 0) return;
    let cancelled = false;
    const set = (p: Phase) => !cancelled && setPhase(p);

    async function boot() {
      // Ask the host whether it has ever been configured. `/spec` is the
      // unauthenticated handshake, which is what makes this answerable before
      // sign-in — and it has to be, because an unconfigured host has no users
      // to sign in as. A host too old to carry the field omits it, and
      // `!== false` leaves those on the existing path unchanged.
      try {
        const spec = await client.spec();
        if (spec.setup_complete === false) {
          set({ kind: "setup" });
          return;
        }
      } catch {
        // A host that cannot answer `/spec` is a connection problem, not an
        // unconfigured one. Fall through to discovery, which reports it
        // properly.
      }

      // Explicit company wins: go straight to its console.
      if (defaultCompany) {
        try {
          const status = await client.status(defaultCompany);
          set({
            kind: "console",
            company: defaultCompany,
            status,
            companies: [status],
            canGoBack: false,
          });
        } catch (err) {
          set(connectionError(client, err, defaultCompany));
        }
        return;
      }

      try {
        const companies = await client.listCompanies();
        if (companies.length === 1) {
          const c = companies[0];
          set({ kind: "console", company: c.id, status: c, companies, canGoBack: false });
        } else if (companies.length > 1) {
          set({ kind: "picker", companies });
        } else {
          set({ kind: "no-company" });
        }
      } catch (listErr) {
        // Fall back to the single-company alias (prosumer serve).
        try {
          const status = await client.status(null);
          set({ kind: "console", company: null, status, companies: [], canGoBack: false });
        } catch {
          set(connectionError(client, listErr, defaultCompany));
        }
      }
    }

    void boot();
    return () => {
      cancelled = true;
    };
  }, [client, defaultCompany, forceLogin, bootEpoch]);

  const switchCompany = useCallback(
    async (id: string, companies: CompanyStatus[]) => {
      try {
        const status = await client.status(id);
        if (phase.kind === "console" && phase.company !== id) {
          clearEntityHash();
        }
        setPhase({ kind: "console", company: id, status, companies, canGoBack: true });
      } catch (err) {
        setPhase(connectionError(client, err, id));
      }
    },
    [client, phase],
  );

  const backToPicker = useCallback(() => {
    void client
      .listCompanies()
      .then((companies) => setPhase({ kind: "picker", companies }))
      .catch((err: unknown) => setPhase(connectionError(client, err, null)));
  }, [client]);

  const consoleCompany = phase.kind === "console" ? phase.company : null;
  const scope = useMemo(
    () => ({ connection: connectionId, company: consoleCompany }),
    [connectionId, consoleCompany],
  );

  // A 401 must not preempt setup. An instance that has never been configured
  // can have no companies and no users at all, so every authenticated route on
  // it answers 401 — and letting that swap the wizard for a sign-in form would
  // put the operator back at the dead end this flow exists to remove, asking
  // them to authenticate against a roster that does not exist yet.
  if (refused && phase.kind !== "login" && phase.kind !== "setup" && phase.kind !== "no-company") {
    return (
      <ConsoleChrome>
        <Login
          client={client}
          company={defaultCompany}
          notice={notice}
          suggestedEmail={suggestedEmail}
          onSignedIn={reBoot}
        />
      </ConsoleChrome>
    );
  }

  // Every phase but `console` is a full-screen state with no app shell, and so
  // no sidebar header for the host switcher to live in. `ConsoleChrome` puts it
  // back — otherwise a host that cannot be reached is a screen with no way off
  // it, which is exactly the dead end the rail's permanent presence prevented.
  switch (phase.kind) {
    case "loading":
      return (
        <FullScreen>
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" /> Connecting…
          </div>
        </FullScreen>
      );

    case "setup":
      return (
        <ConsoleChrome>
          <SetupWizard client={client} onDone={reBoot} />
        </ConsoleChrome>
      );

    case "login":
      return (
        <ConsoleChrome>
          <Login
            client={client}
            company={phase.company}
            notice={phase.notice}
            suggestedEmail={suggestedEmail}
            onSignedIn={reBoot}
          />
        </ConsoleChrome>
      );

    case "error":
      return (
        <FullScreen>
          <div className="w-full max-w-md space-y-4" data-testid="connection-error">
            <Alert variant="destructive">
              <AlertTitle>Can&apos;t connect</AlertTitle>
              <AlertDescription>
                {phase.message}
                {phase.hint && (
                  <span className="mt-1 block font-mono text-xs opacity-80">{phase.hint}</span>
                )}
              </AlertDescription>
            </Alert>
            <Button className="w-full" onClick={() => location.reload()}>
              Retry
            </Button>
          </div>
        </FullScreen>
      );

    case "no-company":
      return (
        <FullScreen>
          <div className="w-full max-w-md space-y-4" data-testid="no-company">
            <Alert variant="destructive">
              <AlertTitle>No companies are running on this host</AlertTitle>
              <AlertDescription>
                Start one from setup, or with{" "}
                <span className="font-mono text-xs">opencompany serve --company &lt;dir&gt;</span>.
              </AlertDescription>
            </Alert>
            <Button className="w-full" onClick={() => setPhase({ kind: "setup" })}>
              Open setup
            </Button>
          </div>
        </FullScreen>
      );

    case "picker":
      return (
        <ConsoleChrome>
          <CompanyPicker
            companies={phase.companies}
            onPick={(id) => void switchCompany(id, phase.companies)}
          />
        </ConsoleChrome>
      );

    case "console":
      return (
        // The remount key carries the connection as well as the company. It was
        // already the isolation primitive — a company switch throws away every
        // piece of in-flight view state rather than reconciling it — and the
        // comments in `app-shell.tsx` about "another company's channel ids are
        // another namespace" are only true again once the host is in the key
        // too. Two hosts each serving an `acme` are two namespaces.
        <ConnectionScopeProvider scope={scope}>
          <AppShell
            key={`${connectionId}:${phase.company ?? "single"}`}
            client={client}
            company={phase.company}
            initialStatus={phase.status}
            companies={phase.companies}
            onSwitchCompany={(id) => void switchCompany(id, phase.companies)}
            onBackToPicker={phase.canGoBack ? backToPicker : undefined}
          />
        </ConnectionScopeProvider>
      );
  }
}

function clearEntityHash() {
  const [path] = window.location.hash.replace(/^#\/?/, "").split("?");
  const [view, sub] = path.split("/").filter(Boolean);
  if (!view || !sub) return;

  // A hash sub-route names an entity within the current company. The shell
  // remounts for a company switch, so remove that stale identity before the
  // new shell reads the route as an intentional deep link.
  window.history.replaceState(null, "", `#/${view}`);
}

/**
 * A full-screen state, with the host switcher over it.
 *
 * Every caller here is a phase with no app shell, and every one of them is a
 * state an operator may need to leave by choosing a different host — so the
 * chrome is part of the wrapper rather than something each case remembers.
 */
function FullScreen({ children }: { children: React.ReactNode }) {
  return (
    <ConsoleChrome>
      <div className="grid min-h-svh place-items-center bg-background p-6 text-center">
        {children}
      </div>
    </ConsoleChrome>
  );
}

function connectionError(
  client: OpenCompanyClient,
  err: unknown,
  company: string | null,
): Phase {
  const where = client.baseUrl || "this origin";
  if (err instanceof ApiError && err.status === 401) {
    // A 401 now usually means "no session", not "no operator token" — humans
    // sign in. Offering the login view is right for a user and harmless for an
    // operator, who can still pass ?token=.
    return { kind: "login", company };
  }
  return {
    kind: "error",
    message: `Couldn't reach a company host at ${where}.`,
    hint: "Set the host with ?api=<url>, or run `opencompany serve`.",
  };
}
