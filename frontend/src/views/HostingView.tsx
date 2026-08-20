import { useCallback, useEffect, useState } from "react";
import { Check, Globe, Loader2, TriangleAlert } from "lucide-react";
import { toast } from "sonner";

import {
  clearHosting,
  getHosting,
  saveHosting,
  type HostingStatus,
} from "@/api/hosting";
import type { OpenCompanyClient } from "@/api/client";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

interface Props {
  client: OpenCompanyClient;
  company: string | null;
}

/** The message out of a rejected request, whatever it was rejected with. */
function reason(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Settings → Hosting: the company's hosting provider connection.
 *
 * With a key stored here and the `hosting` grant in the manifest, a teammate can
 * put a site in this company's workspace on the public internet — with a managed
 * database wired into it, environment variables set before the build, and a
 * custom domain attached.
 *
 * # Why three states and not a "Connected ✓" badge
 *
 * Three separate things can each be missing, and two of them are invisible from
 * this form's own fields:
 *
 *   - no key       — teammates have no hosting tools at all
 *   - not granted  — the key is stored and STILL nothing reaches a teammate,
 *                    because the manifest does not grant `hosting`. The fix is
 *                    `company.toml`, not this page.
 *   - not in build — the running host was compiled without the harness, so no
 *                    amount of configuring will do anything.
 *
 * A single "Connected" badge would be green for the last two and send an
 * operator hunting through this form for a problem that is not in it.
 *
 * # The key is write-only
 *
 * It is never returned by the host, so it is never rendered. A stored key shows
 * as "Configured" and the input stays empty with a placeholder saying that
 * typing replaces it — an input pre-filled with dots invites an operator to
 * "correct" a value they cannot see, and submitting the dots would store the
 * dots.
 */
export function HostingView({ client, company }: Props) {
  const [status, setStatus] = useState<HostingStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [apiKey, setApiKey] = useState("");
  const [team, setTeam] = useState("");

  // Every piece of state here belongs to ONE company — including the
  // typed-but-unsaved API key. `SettingsSection` renders this with
  // `key={company}` so a company switch remounts rather than carrying a key
  // typed for one company into another company's Save. See the same note in
  // BillingView.
  const load = useCallback(async () => {
    try {
      const next = await getHosting(client, company);
      setStatus(next);
      setLoadError(null);
      // Seed the team box with what is stored — it is the one non-secret field,
      // and an operator correcting a typo should not have to retype it.
      setTeam(next.team ?? "");
    } catch (err) {
      setLoadError(reason(err));
    }
  }, [client, company]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onSave() {
    // Send only what was actually entered. The host treats this as a patch, so
    // an untouched key keeps its stored value rather than being cleared.
    const body: Record<string, string> = {};
    if (apiKey.trim()) body.apiKey = apiKey.trim();
    if (team.trim() && team.trim() !== (status?.team ?? "")) body.team = team.trim();

    if (Object.keys(body).length === 0) {
      toast.info("Nothing to save — fill in a field first.");
      return;
    }

    setBusy(true);
    try {
      const next = await saveHosting(client, company, body);
      setStatus(next);
      // Clear the secret input on success: leaving a key sitting in a form field
      // after it has been stored is one stray screen-share from a leak.
      setApiKey("");
      toast.success("Hosting settings saved.");
    } catch (err) {
      toast.error(reason(err));
    } finally {
      setBusy(false);
    }
  }

  async function onClear() {
    setBusy(true);
    try {
      const next = await clearHosting(client, company);
      setStatus(next);
      setApiKey("");
      setTeam("");
      toast.success("Hosting credentials cleared.");
    } catch (err) {
      toast.error(reason(err));
    } finally {
      setBusy(false);
    }
  }

  if (loadError) {
    return (
      <div className="mx-auto w-full max-w-5xl px-4 py-6">
        <Alert variant="destructive" data-testid="hosting-load-error">
          <TriangleAlert className="size-4" />
          <AlertDescription>Could not load hosting settings: {loadError}</AlertDescription>
        </Alert>
      </div>
    );
  }

  if (!status) {
    return (
      <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
        <Loader2 className="mr-2 size-4 animate-spin" /> Loading hosting…
      </div>
    );
  }

  const connected = status.inBuild && status.granted && status.apiKeyConfigured;

  return (
    <div className="flex-1 overflow-y-auto" data-testid="hosting-view">
      <div className="mx-auto w-full max-w-5xl space-y-6 px-4 py-6">
        <div>
          <h1 className="flex items-center gap-2 text-lg font-medium">
            <Globe className="size-5" /> Hosting
          </h1>
          <p className="text-sm text-muted-foreground">
            Connect a hosting provider so your teammates can put a site from this
            company&rsquo;s workspace on the internet — with a managed database
            behind it, and a custom domain in front.
          </p>
        </div>

        {/* The two problems this form cannot fix, said before the form so an
          operator does not fill it in and wonder why nothing happened. */}
        {!status.inBuild ? (
          <Alert data-testid="hosting-not-in-build">
            <TriangleAlert className="size-4" />
            <AlertDescription>
              This host was built without the hosting tools, so these settings
              will be stored and have no effect. Rebuild with the{" "}
              <code>openhuman</code> feature.
            </AlertDescription>
          </Alert>
        ) : null}

        {status.inBuild && !status.granted ? (
          <Alert data-testid="hosting-not-granted">
            <TriangleAlert className="size-4" />
            <AlertDescription>
              This company does not grant <code>hosting</code>, so no teammate
              will get the deployment tools even once a key is saved. Add{" "}
              <code>hosting</code> to <code>[tools].allow</code> in the
              company&rsquo;s manifest — a catch-all <code>*</code> deliberately
              does not confer it, and it cannot be fixed from this page.
            </AlertDescription>
          </Alert>
        ) : null}

        <Card>
          <CardContent className="space-y-4 pt-6">
            <div className="flex items-center justify-between">
              <div className="space-y-1">
                <p className="text-sm font-medium capitalize">{status.provider}</p>
                <p className="text-xs text-muted-foreground">
                  {connected
                    ? status.team
                      ? `Connected, deploying under ${status.team}.`
                      : "Connected, deploying under the personal account."
                    : "Not connected yet."}
                </p>
              </div>
              {connected ? (
                <Badge variant="secondary" data-testid="hosting-connected">
                  <Check className="mr-1 size-3" /> Connected
                </Badge>
              ) : null}
            </div>

            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="hosting-key">API token</Label>
                <Input
                  id="hosting-key"
                  data-testid="hosting-api-key"
                  type="password"
                  autoComplete="off"
                  placeholder={
                    status.apiKeyConfigured ? "Configured — type to replace" : "vercel_…"
                  }
                  value={apiKey}
                  onChange={(e) => setApiKey(e.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  Create one at vercel.com → Account Settings → Tokens. Stored
                  write-only: it is never shown again, here or anywhere else.
                </p>
              </div>

              <div className="space-y-2">
                <Label htmlFor="hosting-team">Team (optional)</Label>
                <Input
                  id="hosting-team"
                  data-testid="hosting-team"
                  placeholder="team_…"
                  value={team}
                  onChange={(e) => setTeam(e.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  The team or organization to deploy under. Leave empty for a
                  personal account.
                </p>
              </div>
            </div>

            <div className="flex items-center gap-2">
              <Button onClick={() => void onSave()} disabled={busy} data-testid="hosting-save">
                {busy ? <Loader2 className="mr-2 size-4 animate-spin" /> : null}
                Save
              </Button>
              {status.apiKeyConfigured ? (
                <Button
                  variant="outline"
                  onClick={() => void onClear()}
                  disabled={busy}
                  data-testid="hosting-clear"
                >
                  Disconnect
                </Button>
              ) : null}
            </div>
          </CardContent>
        </Card>

        <p className="text-xs text-muted-foreground">
          A deployment publishes the files in a teammate&rsquo;s workspace to the
          public internet under this account, and provisioning a database is a
          bill this account pays. Nothing else in this app can read the
          connection string a database produces — the provider injects it into
          the site&rsquo;s own environment.
        </p>
      </div>
    </div>
  );
}
