import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Loader2 } from "lucide-react";

import { redeemHandoff, verifyCode } from "@/api/auth";
import { OpenCompanyClient } from "@/api/client";
import { ApiError, type CompanyStatus } from "@/api/types";
import { AppShell } from "@/components/app-shell";
import { CompanyPicker } from "@/components/company-picker";
import { Login } from "@/views/Login";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { resolveConfig } from "@/config";

type Phase =
  | { kind: "loading" }
  | { kind: "error"; message: string; hint?: string }
  | { kind: "login"; company: string | null; notice?: string }
  | { kind: "picker"; companies: CompanyStatus[] }
  | {
      kind: "console";
      company: string | null;
      status: CompanyStatus;
      companies: CompanyStatus[];
      canGoBack: boolean;
    };

/**
 * Reads `?company=&code=` off a magic-link landing.
 *
 * **Pure.** It must stay that way: this runs in a `useMemo`, and StrictMode
 * double-invokes those. Stripping the URL here — as this once did — meant the
 * second invocation read an already-cleaned URL and returned nothing, silently
 * dropping the code and the company. Clearing is a side effect, so it lives in
 * an effect: see `clearMagicLinkFromUrl`.
 */
function readMagicLink(): { company: string | null; code: string } | null {
  const params = new URLSearchParams(window.location.search);
  const code = params.get("code");
  if (!code) return null;
  return { company: params.get("company"), code };
}

/**
 * Reads `?oc_handoff=` off an ecosystem landing.
 *
 * The TinyHumans dashboard mints this when someone clicks into a company, so it
 * arrives on a plain top-level navigation from another origin. It is a
 * single-use credential exactly like a magic-link code, so it obeys the same
 * three rules that path already learned: read it purely (StrictMode
 * double-invokes the `useMemo`), strip it in an effect, and redeem it once
 * through the shared single-flight ref.
 */
function readHandoff(): { company: string | null; code: string } | null {
  const params = new URLSearchParams(window.location.search);
  const code = params.get("oc_handoff");
  if (!code) return null;
  return { company: params.get("company"), code };
}

/**
 * Strips the magic link out of the address bar.
 *
 * The code is a single-use credential, so it must not linger in the URL, the
 * history, or a `Referer` header once we hold it.
 */
function clearMagicLinkFromUrl(): void {
  clearCredentialFromUrl("code");
}

/**
 * Strips a single-use sign-in credential out of the address bar.
 *
 * `replaceState` rather than a push, so the credential is gone from the entry
 * as well as from the bar — a back button that restored it would hand a spent
 * code to a reload, and a `Referer` carrying it would hand it to whatever the
 * console links out to next.
 */
function clearCredentialFromUrl(param: "code" | "oc_handoff"): void {
  const params = new URLSearchParams(window.location.search);
  if (!params.has(param)) return;
  params.delete(param);
  params.delete("company");
  const query = params.toString();
  window.history.replaceState({}, "", window.location.pathname + (query ? `?${query}` : ""));
}

export function App() {
  const config = useMemo(() => resolveConfig(), []);
  const client = useMemo(() => new OpenCompanyClient(config), [config]);
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });
  // A pure read, so StrictMode's double render is harmless.
  const magicLink = useMemo(() => readMagicLink(), []);
  const handoff = useMemo(() => readHandoff(), []);
  /**
   * The in-flight redemption, so a link is redeemed exactly once.
   *
   * StrictMode double-invokes effects, and a login code is single-use: the
   * second call would spend nothing and 401, bouncing a perfectly good sign-in
   * to the login screen. Both runs await this one promise instead. A ref rather
   * than a "done" flag, because the second run must *wait for* the first — not
   * skip ahead and query with a session that does not exist yet.
   */
  const redemption = useRef<Promise<unknown> | null>(null);

  // Now that the code is captured in state, take it out of the URL.
  useEffect(() => {
    if (magicLink) clearMagicLinkFromUrl();
    if (handoff) clearCredentialFromUrl("oc_handoff");
  }, [magicLink, handoff]);

  // An expired or revoked session anywhere in the console drops to sign-in
  // rather than showing a broken page.
  useEffect(() => {
    client.onUnauthorized = () => setPhase({ kind: "login", company: config.company });
    return () => {
      client.onUnauthorized = null;
    };
  }, [client, config.company]);

  useEffect(() => {
    let cancelled = false;
    const set = (p: Phase) => !cancelled && setPhase(p);

    async function boot() {
      // An ecosystem landing: someone clicked this company on their TinyHumans
      // dashboard. Redeem before anything else, for the same reason a magic
      // link does — the session has to exist before the console asks for data.
      if (handoff) {
        const company = handoff.company ?? config.company;
        try {
          redemption.current ??= redeemHandoff(client, company, handoff.code);
          await redemption.current;
        } catch (err) {
          // A dead or misaddressed handoff falls back to this company's own
          // sign-in rather than stranding them: magic link still works, and it
          // is the only thing that works on a self-hosted host anyway.
          set({ kind: "login", company, notice: handoffNotice(err) });
          return;
        }
      }

      // A magic-link landing: redeem it before anything else, so the session
      // exists by the time the console asks for data.
      if (magicLink) {
        const company = magicLink.company ?? config.company;
        try {
          redemption.current ??= verifyCode(client, company, magicLink.code);
          await redemption.current;
        } catch {
          // A dead link is not fatal — fall through to sign-in and let them
          // ask for another. The reason stays vague on purpose.
          set({ kind: "login", company });
          return;
        }
      }

      // Explicit company wins: go straight to its console.
      if (config.company) {
        try {
          const status = await client.status(config.company);
          set({ kind: "console", company: config.company, status, companies: [status], canGoBack: false });
        } catch (err) {
          set(connectionError(client, err, config.company));
        }
        return;
      }

      // Otherwise discover companies from the host.
      try {
        const companies = await client.listCompanies();
        if (companies.length === 1) {
          const c = companies[0];
          set({ kind: "console", company: c.id, status: c, companies, canGoBack: false });
        } else if (companies.length > 1) {
          set({ kind: "picker", companies });
        } else {
          set({
            kind: "error",
            message: "No companies are running on this host.",
            hint: "Start one with `opencompany serve --company <dir>`.",
          });
        }
      } catch (listErr) {
        // Fall back to the single-company alias (prosumer serve).
        try {
          const status = await client.status(null);
          set({ kind: "console", company: null, status, companies: [], canGoBack: false });
        } catch {
          set(connectionError(client, listErr, config.company));
        }
      }
    }

    void boot();
    return () => {
      cancelled = true;
    };
  }, [client, config.company, handoff, magicLink]);

  const switchCompany = useCallback(
    async (id: string, companies: CompanyStatus[]) => {
      try {
        const status = await client.status(id);
        setPhase({ kind: "console", company: id, status, companies, canGoBack: true });
      } catch (err) {
        setPhase(connectionError(client, err, id));
      }
    },
    [client],
  );

  const backToPicker = useCallback(() => {
    void client.listCompanies().then((companies) => setPhase({ kind: "picker", companies }));
  }, [client]);

  switch (phase.kind) {
    case "loading":
      return (
        <FullScreen>
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" /> Connecting…
          </div>
        </FullScreen>
      );

    case "login":
      return (
        <Login
          client={client}
          company={phase.company}
          notice={phase.notice}
          onSignedIn={() => window.location.reload()}
        />
      );

    case "error":
      return (
        <FullScreen>
          <div className="w-full max-w-md space-y-4">
            <Alert variant="destructive">
              <AlertTitle>Can&apos;t connect</AlertTitle>
              <AlertDescription>
                {phase.message}
                {phase.hint && <span className="mt-1 block font-mono text-xs opacity-80">{phase.hint}</span>}
              </AlertDescription>
            </Alert>
            <Button className="w-full" onClick={() => location.reload()}>
              Retry
            </Button>
          </div>
        </FullScreen>
      );

    case "picker":
      return (
        <CompanyPicker
          companies={phase.companies}
          onPick={(id) => void switchCompany(id, phase.companies)}
        />
      );

    case "console":
      return (
        <AppShell
          key={phase.company ?? "single"}
          client={client}
          company={phase.company}
          initialStatus={phase.status}
          companies={phase.companies}
          onSwitchCompany={(id) => void switchCompany(id, phase.companies)}
          onBackToPicker={phase.canGoBack ? backToPicker : undefined}
        />
      );
  }
}

function FullScreen({ children }: { children: React.ReactNode }) {
  return (
    <div className="grid min-h-svh place-items-center bg-background p-6 text-center">{children}</div>
  );
}

/**
 * What to tell someone whose ecosystem handoff did not work.
 *
 * Each line is about the *code*, never about the person: "expired", "for a
 * different company", "no access". None of them confirms or denies that any
 * address has an account here, which is the rule the whole sign-in surface is
 * built around.
 */
function handoffNotice(err: unknown): string {
  const code = err instanceof ApiError ? err.code : "";
  switch (code) {
    case "handoff_invalid":
      return "That sign-in expired. Open this company again from your dashboard, or use a link below.";
    case "handoff_wrong_company":
      return "That sign-in was for a different company. Open this one from your dashboard.";
    case "not_a_member":
      return "You're signed in to TinyHumans, but this company hasn't given you access yet. Ask an admin to invite you.";
    case "handoff_unavailable":
      return "This host isn't connected to a TinyHumans account. Sign in with a link instead.";
    default:
      return "We couldn't complete that sign-in. Try a link below.";
  }
}

function connectionError(client: OpenCompanyClient, err: unknown, company: string | null): Phase {
  const where = client.baseUrl || "this origin";
  if (err instanceof ApiError && err.status === 401) {
    // A 401 now usually means "no session", not "no operator token" — humans
    // sign in. Offering the login view is right for a user and harmless for an
    // operator, who can still pass ?token=. Returning the error phase here
    // would also race the client's onUnauthorized hook and win, stranding a
    // signed-out user on a dead end.
    return { kind: "login", company };
  }
  return {
    kind: "error",
    message: `Couldn't reach a company host at ${where}.`,
    hint: "Set the host with ?api=<url>, or run `opencompany serve`.",
  };
}
