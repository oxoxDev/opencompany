import { useCallback, useEffect, useRef, useState } from "react";
import { Check, KeyRound, Loader2, LogIn, Mail, Plug, Save, Trash2 } from "lucide-react";
import { toast } from "sonner";

import type { OpenCompanyClient } from "@/api/client";
import {
  getComposioStatus,
  listComposioConnections,
  setComposioToken,
  startComposioAuthorize,
  type ComposioStatus,
} from "@/api/composio";
import { ApiError } from "@/api/types";
import { beginComposioSignin, COMPOSIO_TOKEN_UPDATED_EVENT } from "@/lib/composio-signin";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";

interface Props {
  client: OpenCompanyClient;
  company: string | null;
}

/** Friendly display labels for the common toolkits; slug-cased fallback otherwise. */
const TOOLKIT_LABELS: Record<string, string> = {
  gmail: "Gmail",
  slack: "Slack",
  github: "GitHub",
  googlecalendar: "Google Calendar",
  notion: "Notion",
  linear: "Linear",
};

function toolkitLabel(slug: string): string {
  return TOOLKIT_LABELS[slug.toLowerCase()] ?? slug.charAt(0).toUpperCase() + slug.slice(1);
}

/**
 * Per-tenant Composio connection management (issue #110, Cell D). Two ways to
 * connect: a per-provider OAuth "Sign in" list driven by the granted toolkits
 * (Composio runs the hosted OAuth; we open it in a tab and poll until the
 * toolkit reports connected), and — as a fallback — a WRITE-ONLY token input
 * (the per-tenant OAuth bearer is the entire isolation lever, so it is stored
 * and never shown again). A set/clear takes effect on the agents' next turn, no
 * restart. Hidden entirely when the feature is not in the build.
 */
export function ComposioSection({ client, company }: Props) {
  const [load, setLoad] = useState<"loading" | "ready" | "unavailable">("loading");
  const [status, setStatus] = useState<ComposioStatus | null>(null);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState<"save" | "clear" | null>(null);
  // "Sign in with tinyhumans" (Layer A): the email to send the sign-in link to,
  // and whether that request is in flight.
  const [signinEmail, setSigninEmail] = useState("");
  const [signinBusy, setSigninBusy] = useState(false);
  // toolkit slug -> connected. Absent = unknown / not yet fetched.
  const [connected, setConnected] = useState<Record<string, boolean>>({});
  // toolkit slug currently mid-sign-in (open tab + poll).
  const [signingIn, setSigningIn] = useState<string | null>(null);

  const requestGeneration = useRef(0);
  const pollTimers = useRef<Record<string, number>>({});

  // Clear any in-flight poll timers on unmount / company switch.
  useEffect(() => {
    const timers = pollTimers.current;
    return () => {
      Object.values(timers).forEach((id) => window.clearTimeout(id));
      pollTimers.current = {};
    };
  }, [company]);

  const refreshConnections = useCallback(
    async (s: ComposioStatus) => {
      // Connections need a configured token; skip the call otherwise (it 409s).
      if (!s.tokenConfigured || s.toolkits.length === 0) {
        setConnected({});
        return;
      }
      try {
        const rows = await listComposioConnections(client, company);
        setConnected(Object.fromEntries(rows.map((r) => [r.toolkit.toLowerCase(), r.connected])));
      } catch {
        // Non-fatal: leave the provider rows in their "sign in" state.
        setConnected({});
      }
    },
    [client, company],
  );

  const refresh = useCallback(async () => {
    const generation = ++requestGeneration.current;
    try {
      const s = await getComposioStatus(client, company);
      if (generation !== requestGeneration.current) return;
      setStatus(s);
      // Hide the whole section when the feature is not compiled into this build.
      setLoad(s.inBuild ? "ready" : "unavailable");
      if (s.inBuild) void refreshConnections(s);
    } catch {
      if (generation !== requestGeneration.current) return;
      setLoad("unavailable");
    }
  }, [client, company, refreshConnections]);

  useEffect(() => {
    setStatus(null);
    setConnected({});
    setLoad("loading");
    void refresh();
  }, [refresh]);

  // When a returning "Sign in with tinyhumans" stores the bearer (captured at the
  // app root, since the backend redirects there), refresh so `tokenConfigured`
  // flips true and the per-provider list activates without a manual reload.
  useEffect(() => {
    const onUpdated = () => void refresh();
    window.addEventListener(COMPOSIO_TOKEN_UPDATED_EVENT, onUpdated);
    return () => window.removeEventListener(COMPOSIO_TOKEN_UPDATED_EVENT, onUpdated);
  }, [refresh]);

  // Kick off tinyhumans web sign-in: the backend emails a link that returns to
  // this console and hands back a platform JWT, which we then store as the
  // company's Composio bearer (no paste needed). Live completion needs the
  // console origin allowlisted on the backend (OAUTH_LOGIN_ALLOWED_DOMAINS).
  async function signInWithTinyhumans() {
    if (signinBusy || !status) return;
    const email = signinEmail.trim();
    if (!email) return;
    setSigninBusy(true);
    try {
      await beginComposioSignin(status.backendUrl, company, email);
      setSigninEmail("");
      toast.success(
        "Check your email for a tinyhumans sign-in link. Open it in this browser to finish connecting Composio.",
      );
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "Couldn't start tinyhumans sign-in.");
    } finally {
      setSigninBusy(false);
    }
  }

  async function save() {
    if (!token.trim()) return;
    setBusy("save");
    try {
      const res = await setComposioToken(client, company, token.trim());
      setStatus(res.status);
      setToken("");
      toast.success(res.note);
      void refreshConnections(res.status);
    } catch (err) {
      toast.error(err instanceof ApiError ? err.message : "Could not save the token.");
    } finally {
      setBusy(null);
    }
  }

  async function clear() {
    setBusy("clear");
    try {
      const res = await setComposioToken(client, company, "");
      setStatus(res.status);
      setToken("");
      setConnected({});
      toast.success("Composio token cleared.");
    } catch (err) {
      toast.error(err instanceof ApiError ? err.message : "Could not clear the token.");
    } finally {
      setBusy(null);
    }
  }

  // Per-provider OAuth: open Composio's hosted connect URL in a new tab, then
  // poll the connection list every 2s up to ~2 minutes until this toolkit flips
  // to connected (Composio completes the OAuth on its side — no local callback).
  async function signIn(toolkit: string) {
    // Guard the shared flag AND a poll already in flight for this toolkit.
    if (signingIn || pollTimers.current[toolkit] !== undefined) return;
    setSigningIn(toolkit);
    const label = toolkitLabel(toolkit);
    try {
      const { connectUrl } = await startComposioAuthorize(client, company, toolkit);
      window.open(connectUrl, "_blank", "noopener,noreferrer");
      toast.message(`Complete ${label} sign-in in the new tab.`);
      const deadline = Date.now() + 120_000;
      const poll = async () => {
        delete pollTimers.current[toolkit];
        if (Date.now() > deadline) {
          setSigningIn((t) => (t === toolkit ? null : t));
          toast.message(`${label} sign-in timed out. Try again if it didn't complete.`);
          return;
        }
        try {
          const rows = await listComposioConnections(client, company);
          const map = Object.fromEntries(rows.map((r) => [r.toolkit.toLowerCase(), r.connected]));
          setConnected(map);
          if (map[toolkit.toLowerCase()]) {
            setSigningIn((t) => (t === toolkit ? null : t));
            toast.success(`Connected ${label}.`);
            return;
          }
        } catch {
          // Ignore transient probe errors while the operator finishes sign-in.
        }
        pollTimers.current[toolkit] = window.setTimeout(() => void poll(), 2_000);
      };
      pollTimers.current[toolkit] = window.setTimeout(() => void poll(), 2_000);
    } catch (err) {
      setSigningIn((t) => (t === toolkit ? null : t));
      toast.error(err instanceof ApiError ? err.message : `Couldn't start ${label} sign-in.`);
    }
  }

  if (load === "unavailable") return null;

  const showProviders = load === "ready" && status !== null && status.toolkits.length > 0;

  return (
    <section className="space-y-3">
      <div className="flex items-center gap-2">
        <Plug className="size-4 text-muted-foreground" />
        <h3 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          Composio (Gmail / Slack / GitHub)
        </h3>
      </div>
      <p className="text-sm text-muted-foreground">
        Give your agents Gmail, Slack &amp; GitHub via Composio. Sign in with tinyhumans to set this
        company&apos;s Composio identity, then connect each provider — or paste a Composio OAuth token
        directly as a fallback. Either way, the connection takes effect on the next turn, no restart.
      </p>

      {load === "loading" ? (
        <Skeleton className="h-32 rounded-xl" />
      ) : (
        <>
          {status && (
            <div className="flex flex-wrap items-center gap-2">
              <Badge variant={status.granted ? "secondary" : "outline"}>
                {status.granted ? "granted" : "not granted"}
              </Badge>
              {status.tokenConfigured ? (
                <span className="inline-flex items-center gap-1 text-xs text-emerald-600 dark:text-emerald-400">
                  <Check className="size-3" /> token set
                </span>
              ) : (
                <span className="text-xs text-muted-foreground">no token yet</span>
              )}
            </div>
          )}

          {status && !status.granted && (
            <p className="rounded-md bg-muted/40 p-2 text-xs text-muted-foreground">
              This company does not grant the <span className="font-mono">composio</span> tool
              namespace, so agents will not receive Composio tools even once connected. Add
              <span className="font-mono"> composio</span> to the company&apos;s tool grants first.
            </p>
          )}

          {status && (
            <Card>
              <CardContent className="space-y-3 py-4">
                <div className="flex items-center gap-2">
                  <LogIn className="size-4 text-muted-foreground" />
                  <p className="text-sm font-medium">Sign in with tinyhumans</p>
                  {status.tokenConfigured && (
                    <span className="inline-flex items-center gap-1 text-xs text-emerald-600 dark:text-emerald-400">
                      <Check className="size-3" /> connected
                    </span>
                  )}
                </div>
                <p className="text-xs text-muted-foreground">
                  The recommended way. We email you a sign-in link; open it in this browser and your
                  tinyhumans account becomes this company&apos;s Composio identity — no token to
                  paste.{" "}
                  {status.tokenConfigured && "Sign in again to rotate the current identity."}
                </p>
                <div className="flex flex-wrap items-end gap-2">
                  <div className="min-w-0 flex-1 space-y-1">
                    <Label htmlFor="composio-signin-email" className="text-xs">
                      tinyhumans email
                    </Label>
                    <Input
                      id="composio-signin-email"
                      type="email"
                      autoComplete="email"
                      placeholder="you@company.com"
                      value={signinEmail}
                      disabled={signinBusy}
                      onChange={(e) => setSigninEmail(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") void signInWithTinyhumans();
                      }}
                    />
                  </div>
                  <Button
                    disabled={signinBusy || !signinEmail.trim()}
                    onClick={() => void signInWithTinyhumans()}
                  >
                    {signinBusy ? (
                      <Loader2 className="size-4 animate-spin" />
                    ) : (
                      <Mail className="size-4" />
                    )}
                    Send sign-in link
                  </Button>
                </div>
              </CardContent>
            </Card>
          )}

          {showProviders && status && (
            <Card>
              <CardContent className="space-y-2 py-4">
                <p className="text-xs font-medium text-muted-foreground">Sign in per provider</p>
                {!status.tokenConfigured && (
                  <p className="rounded-md bg-muted/40 p-2 text-xs text-muted-foreground">
                    Sign in with tinyhumans above (or paste a token below) first — it is the
                    per-tenant identity the provider sign-in authorizes against.
                  </p>
                )}
                <ul className="divide-y divide-border">
                  {status.toolkits.map((toolkit) => {
                    const isConnected = connected[toolkit.toLowerCase()] === true;
                    const isSigningIn = signingIn === toolkit;
                    return (
                      <li key={toolkit} className="flex items-center justify-between py-2">
                        <span className="text-sm">{toolkitLabel(toolkit)}</span>
                        {isConnected ? (
                          <span className="inline-flex items-center gap-1 text-xs text-emerald-600 dark:text-emerald-400">
                            <Check className="size-3" /> connected
                          </span>
                        ) : (
                          <Button
                            size="sm"
                            variant="outline"
                            disabled={
                              signingIn !== null || !status.tokenConfigured
                            }
                            onClick={() => void signIn(toolkit)}
                          >
                            {isSigningIn ? (
                              <Loader2 className="size-4 animate-spin" />
                            ) : (
                              <LogIn className="size-4" />
                            )}
                            Sign in
                          </Button>
                        )}
                      </li>
                    );
                  })}
                </ul>
              </CardContent>
            </Card>
          )}

          <Card>
            <CardContent className="space-y-4 py-4">
              <div className="space-y-1">
                <Label htmlFor="composio-token" className="text-xs">
                  Composio token (fallback){" "}
                  {status?.tokenConfigured ? "— set, leave blank to keep" : ""}
                </Label>
                <p className="text-xs text-muted-foreground">
                  Paste this company&apos;s Composio OAuth token directly if you already have one. It
                  is stored securely and never shown again.
                </p>
                <Input
                  id="composio-token"
                  type="password"
                  autoComplete="off"
                  placeholder="paste the company's Composio OAuth token"
                  value={token}
                  onChange={(e) => setToken(e.target.value)}
                />
                {status && (
                  <p className="truncate text-xs text-muted-foreground">{status.backendUrl}</p>
                )}
              </div>

              <div className="flex items-center gap-2">
                <Button disabled={busy !== null || !token.trim()} onClick={() => void save()}>
                  {busy === "save" ? (
                    <Loader2 className="size-4 animate-spin" />
                  ) : (
                    <Save className="size-4" />
                  )}
                  Save token
                </Button>
                {status?.tokenConfigured && (
                  <Button variant="outline" disabled={busy !== null} onClick={() => void clear()}>
                    {busy === "clear" ? (
                      <Loader2 className="size-4 animate-spin" />
                    ) : (
                      <Trash2 className="size-4" />
                    )}
                    Clear
                  </Button>
                )}
                <KeyRound className="ml-auto size-4 text-muted-foreground" />
              </div>
            </CardContent>
          </Card>
        </>
      )}
    </section>
  );
}
