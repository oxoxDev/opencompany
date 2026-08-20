import { useCallback, useEffect, useState } from "react";
import { Check, Copy, CreditCard, Loader2, TriangleAlert } from "lucide-react";
import { toast } from "sonner";

import {
  clearBilling,
  clearPaypal,
  getBilling,
  getPaypal,
  saveBilling,
  savePaypal,
  type BillingStatus,
  type PaypalStatus,
} from "@/api/billing";
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
 * Settings → Billing: the company's Chargebee connection (issue #788, #527).
 *
 * # Why four states and not a "Connected ✓" badge
 *
 * Four separate things can each be missing, and three of them are invisible
 * from this form's own fields:
 *
 *   - no key or site  — the agent has no billing tools at all
 *   - no webhook      — the tools work, but nobody is told when a customer pays
 *   - not granted     — both credentials stored and STILL nothing reaches an
 *                       agent, because the manifest does not grant `chargebee`.
 *                       The fix is `company.toml`, not this page.
 *   - not in build    — the running host was compiled without the feature, so
 *                       no amount of configuring will do anything.
 *
 * A single "Connected" badge would be green for the last three and send an
 * operator hunting through this form for a problem that is not in it. So each
 * is reported on its own terms, and the two that this page cannot fix say where
 * the fix actually is.
 *
 * # Credentials are write-only
 *
 * The API key and the webhook credential are never returned by the host, so
 * they are never rendered. A stored key shows as "Configured", and the input
 * stays empty with a placeholder saying that typing replaces it — an input
 * pre-filled with dots invites an operator to "correct" a value they cannot
 * see, and submitting the dots would store the dots.
 */
export function BillingView({ client, company }: Props) {
  const [status, setStatus] = useState<BillingStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [apiKey, setApiKey] = useState("");
  const [site, setSite] = useState("");
  const [webhookSecret, setWebhookSecret] = useState("");

  const [paypal, setPaypal] = useState<PaypalStatus | null>(null);
  const [paypalError, setPaypalError] = useState<string | null>(null);
  const [clientId, setClientId] = useState("");
  const [clientSecret, setClientSecret] = useState("");
  const [environment, setEnvironment] = useState("sandbox");

  // Every piece of state here belongs to ONE company: the status badges, the
  // site box, and — the dangerous ones — the typed-but-unsaved API key, webhook
  // secret and PayPal credentials. `SettingsSection` renders this with
  // `key={company}`, so a company switch remounts rather than re-running this
  // against carried-over state.
  //
  // That key is load-bearing, not cosmetic. Clearing fields by hand here fixed
  // only the ones somebody remembered, and left an operator who typed a key for
  // one company, switched, and pressed Save writing that credential into the
  // other company's secret store. It also makes `company` constant for this
  // instance's lifetime, so a slow response from a previous company cannot land
  // on a later one's view — there is no later one to land on.
  // Chargebee and PayPal are unrelated integrations, and they are loaded
  // independently for that reason. Under `Promise.all` a single rejection took
  // the whole page to an error card, so a PayPal read that failed — a host built
  // without the feature answering oddly, a slow store, one bad request — left an
  // operator unable to reach the Chargebee form at all, for a problem that had
  // nothing to do with Chargebee. Each side now reports its own failure and the
  // other still renders.
  const load = useCallback(async () => {
    const [billing, pp] = await Promise.allSettled([
      getBilling(client, company),
      getPaypal(client, company),
    ]);

    if (billing.status === "fulfilled") {
      setStatus(billing.value);
      setLoadError(null);
      // Seed the site box with what is stored — it is the one non-secret field,
      // and an operator correcting a typo should not have to retype it.
      setSite(billing.value.site ?? "");
    } else {
      setLoadError(reason(billing.reason));
    }

    if (pp.status === "fulfilled") {
      setPaypal(pp.value);
      setPaypalError(null);
      setEnvironment(pp.value.environment || "sandbox");
    } else {
      // Not fatal to the page: the PayPal card says so where the PayPal card is,
      // and the Chargebee half above is untouched.
      setPaypal(null);
      setPaypalError(reason(pp.reason));
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
    if (site.trim() && site.trim() !== (status?.site ?? ""))
      body.site = site.trim();
    if (webhookSecret.trim()) body.webhookSecret = webhookSecret.trim();

    if (Object.keys(body).length === 0) {
      toast.info("Nothing to save — fill in a field first.");
      return;
    }

    setBusy(true);
    try {
      const next = await saveBilling(client, company, body);
      setStatus(next);
      // Clear the secret inputs on success: leaving a key sitting in a form
      // field after it has been stored is one stray screen-share from a leak.
      setApiKey("");
      setWebhookSecret("");
      toast.success("Chargebee settings saved.");
    } catch (err) {
      toast.error(
        err instanceof Error
          ? err.message
          : "Could not save Chargebee settings.",
      );
    } finally {
      setBusy(false);
    }
  }

  async function onClear() {
    setBusy(true);
    try {
      const next = await clearBilling(client, company);
      setStatus(next);
      setApiKey("");
      setWebhookSecret("");
      setSite("");
      toast.success("Chargebee credentials cleared.");
    } catch (err) {
      toast.error(
        err instanceof Error ? err.message : "Could not clear the credentials.",
      );
    } finally {
      setBusy(false);
    }
  }

  async function onSavePaypal() {
    const body: Record<string, string> = {};
    if (clientId.trim()) body.clientId = clientId.trim();
    if (clientSecret.trim()) body.clientSecret = clientSecret.trim();
    if (environment !== (paypal?.environment ?? "sandbox"))
      body.environment = environment;

    if (Object.keys(body).length === 0) {
      toast.info("Nothing to save — fill in a field first.");
      return;
    }
    setBusy(true);
    try {
      const next = await savePaypal(client, company, body);
      setPaypal(next);
      setClientId("");
      setClientSecret("");
      toast.success("PayPal settings saved.");
    } catch (err) {
      toast.error(
        err instanceof Error ? err.message : "Could not save PayPal settings.",
      );
    } finally {
      setBusy(false);
    }
  }

  async function onClearPaypal() {
    setBusy(true);
    try {
      const next = await clearPaypal(client, company);
      setPaypal(next);
      setClientId("");
      setClientSecret("");
      setEnvironment(next.environment || "sandbox");
      toast.success("PayPal disconnected.");
    } catch (err) {
      toast.error(
        err instanceof Error ? err.message : "Could not disconnect PayPal.",
      );
    } finally {
      setBusy(false);
    }
  }

  if (loadError) {
    return (
      <div className="mx-auto w-full max-w-5xl px-4 py-6">
        <Alert variant="destructive" data-testid="billing-load-error">
          <TriangleAlert className="size-4" />
          <AlertDescription>
            Could not load billing settings: {loadError}
          </AlertDescription>
        </Alert>
      </div>
    );
  }

  if (!status) {
    return (
      <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
        <Loader2 className="mr-2 size-4 animate-spin" /> Loading billing…
      </div>
    );
  }

  const connected =
    status.inBuild && status.granted && status.apiKeyConfigured && !!status.site;
  const paypalConnected =
    !!paypal?.inBuild &&
    !!paypal?.granted &&
    !!paypal?.clientIdConfigured &&
    !!paypal?.clientSecretConfigured;

  return (
    // `flex-1 overflow-y-auto` is load-bearing, not cosmetic: without it this
    // page is clipped at the viewport and the PayPal card below cannot be
    // scrolled to at all. Matches McpServersView, its nearest sibling.
    <div className="flex-1 overflow-y-auto" data-testid="billing-view">
      <div className="mx-auto w-full max-w-5xl space-y-6 px-4 py-6">
        <div>
          <h1 className="flex items-center gap-2 text-lg font-medium">
            <CreditCard className="size-5" /> Billing
          </h1>
          <p className="text-sm text-muted-foreground">
            Connect Chargebee so your teammates can raise invoices and answer
            questions about who has paid, and PayPal so they can report the
            wallet — without leaving this app.
          </p>
        </div>

        {/* The two problems this form cannot fix, said before the form so an
          operator does not fill it in and wonder why nothing happened. */}
        {!status.inBuild ? (
          <Alert data-testid="billing-not-in-build">
            <TriangleAlert className="size-4" />
            <AlertDescription>
              This host was built without Chargebee support, so these settings
              will be stored and have no effect. Rebuild with the{" "}
              <code>chargebee</code> feature.
            </AlertDescription>
          </Alert>
        ) : null}

        {status.inBuild && !status.granted ? (
          <Alert data-testid="billing-not-granted">
            <TriangleAlert className="size-4" />
            <AlertDescription>
              This company does not grant <code>chargebee</code>, so billing
              tools will not reach any teammate even once these credentials are
              saved. Add <code>chargebee</code> to <code>[tools].allow</code> in
              the company&rsquo;s manifest — it cannot be fixed from this page.
            </AlertDescription>
          </Alert>
        ) : null}

        <Card>
          <CardContent className="space-y-4 pt-6">
            <div className="flex items-center justify-between">
              <div className="space-y-1">
                <p className="text-sm font-medium">Chargebee</p>
                <p className="text-xs text-muted-foreground">
                  {connected
                    ? `Connected to ${status.site}.chargebee.com`
                    : "Not connected yet."}
                </p>
              </div>
              {connected ? (
                <Badge variant="secondary" data-testid="billing-connected">
                  <Check className="mr-1 size-3" /> Connected
                </Badge>
              ) : null}
            </div>

            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="cb-site">Site</Label>
                <Input
                  id="cb-site"
                  data-testid="billing-site"
                  placeholder="acme-test"
                  value={site}
                  onChange={(e) => setSite(e.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  The part before <code>.chargebee.com</code>. Pasting the full
                  URL is fine.
                </p>
              </div>

              <div className="space-y-2">
                <Label htmlFor="cb-key">API key</Label>
                <Input
                  id="cb-key"
                  data-testid="billing-api-key"
                  type="password"
                  autoComplete="off"
                  placeholder={
                    status.apiKeyConfigured
                      ? "Configured — type to replace"
                      : "From Chargebee → API Keys"
                  }
                  value={apiKey}
                  onChange={(e) => setApiKey(e.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  {status.apiKeyConfigured
                    ? "Stored. It is never shown again."
                    : "Stored write-only; it is never shown again."}
                </p>
              </div>
            </div>

            <div className="flex gap-2">
              <Button
                onClick={onSave}
                disabled={busy}
                data-testid="billing-save"
              >
                {busy ? <Loader2 className="mr-2 size-4 animate-spin" /> : null}
                Save
              </Button>
              {status.apiKeyConfigured || status.webhookConfigured ? (
                <Button
                  variant="ghost"
                  onClick={onClear}
                  disabled={busy}
                  data-testid="billing-clear"
                >
                  Disconnect
                </Button>
              ) : null}
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardContent className="space-y-4 pt-6">
            <div className="space-y-1">
              <p className="text-sm font-medium">Payment notifications</p>
              <p className="text-xs text-muted-foreground">
                Optional. Without this, invoicing still works — nobody is just
                told when a customer pays.
              </p>
            </div>

            <div className="space-y-2">
              <Label>Webhook URL</Label>
              {status.webhookUrl ? (
                <div className="flex gap-2">
                  <Input
                    readOnly
                    value={status.webhookUrl}
                    data-testid="billing-webhook-url"
                  />
                  <Button
                    variant="outline"
                    size="icon"
                    onClick={async () => {
                      // Await before claiming success: clipboard writes are
                      // refused in some browsers and over plain http, and a
                      // toast saying "copied" over an empty clipboard sends an
                      // operator to paste nothing into Chargebee.
                      try {
                        await navigator.clipboard.writeText(
                          status.webhookUrl ?? "",
                        );
                        toast.success("Webhook URL copied.");
                      } catch {
                        toast.error(
                          "Could not copy — select the URL and copy it manually.",
                        );
                      }
                    }}
                  >
                    <Copy className="size-4" />
                  </Button>
                </div>
              ) : (
                // Deliberately not a disabled box showing a loopback address:
                // Chargebee cannot deliver to one, and showing it would send an
                // operator to configure a webhook that silently never arrives.
                <p
                  className="text-xs text-muted-foreground"
                  data-testid="billing-no-webhook-url"
                >
                  This host has no public URL, so Chargebee cannot reach it. Set{" "}
                  <code>OPENCOMPANY_PUBLIC_URL</code> to an https address to get
                  a webhook URL.
                </p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="cb-hook">Webhook credential</Label>
              <Input
                id="cb-hook"
                data-testid="billing-webhook-secret"
                type="password"
                autoComplete="off"
                placeholder={
                  status.webhookConfigured
                    ? "Configured — type to replace"
                    : "username:password"
                }
                value={webhookSecret}
                onChange={(e) => setWebhookSecret(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                Invent a username and password, save them here as{" "}
                <code>username:password</code>, then set the same pair in
                Chargebee under{" "}
                <em>Protect webhook URL with basic authentication</em>.
                Subscribe to <code>payment_succeeded</code> and{" "}
                <code>payment_failed</code>.
              </p>
            </div>
          </CardContent>
        </Card>

        {/* PayPal (issue #789). A form, not a "Connect PayPal" button: these
          tools read the company's OWN wallet, so there is no third party for an
          OAuth popup to ask permission of. */}
        <Card>
          <CardContent className="space-y-4 pt-6">
            <div className="flex items-center justify-between">
              <div className="space-y-1">
                <p className="text-sm font-medium">PayPal</p>
                <p className="text-xs text-muted-foreground">
                  Lets your teammates report the wallet balance and recent
                  transactions. Invoice payments already route through PayPal
                  when you set it as a payment method inside Chargebee — that
                  needs nothing here.
                </p>
              </div>
              {paypalConnected ? (
                <Badge variant="secondary" data-testid="paypal-connected">
                  <Check className="mr-1 size-3" /> Connected
                </Badge>
              ) : null}
            </div>

            {/* PayPal failed to load while Chargebee did. Said here rather than
              as a page-level error: the Chargebee form above is unaffected and
              still usable, which is the whole reason the two load apart. The
              form below is hidden because saving against an unknown state would
              be guessing — there is no `environment` to compare against, so a
              Save would post fields the operator did not change. */}
            {paypalError ? (
              <Alert variant="destructive" data-testid="paypal-load-error">
                <TriangleAlert className="size-4" />
                <AlertDescription>
                  Could not load the PayPal connection: {paypalError}
                </AlertDescription>
              </Alert>
            ) : null}

            {paypal && !paypal.inBuild ? (
              <Alert data-testid="paypal-not-in-build">
                <TriangleAlert className="size-4" />
                <AlertDescription>
                  This host was built without PayPal support, so these settings
                  will be stored and have no effect.
                </AlertDescription>
              </Alert>
            ) : null}

            {paypal?.inBuild && !paypal.granted ? (
              <Alert data-testid="paypal-not-granted">
                <TriangleAlert className="size-4" />
                <AlertDescription>
                  This company does not grant <code>paypal</code>, so wallet
                  tools will not reach any teammate. Add <code>paypal</code> to{" "}
                  <code>[tools].allow</code> in the company&rsquo;s manifest.
                </AlertDescription>
              </Alert>
            ) : null}

            {paypalError ? null : (
              <>
                <div className="grid gap-4 sm:grid-cols-2">
                  <div className="space-y-2">
                    <Label htmlFor="pp-id">Client ID</Label>
                    <Input
                      id="pp-id"
                      data-testid="paypal-client-id"
                      type="password"
                      autoComplete="off"
                      placeholder={
                        paypal?.clientIdConfigured
                          ? "Configured — type to replace"
                          : "From developer.paypal.com → Apps & Credentials"
                      }
                      value={clientId}
                      onChange={(e) => setClientId(e.target.value)}
                    />
                  </div>
                  <div className="space-y-2">
                    <Label htmlFor="pp-secret">Client secret</Label>
                    <Input
                      id="pp-secret"
                      data-testid="paypal-client-secret"
                      type="password"
                      autoComplete="off"
                      placeholder={
                        paypal?.clientSecretConfigured
                          ? "Configured — type to replace"
                          : "Secret"
                      }
                      value={clientSecret}
                      onChange={(e) => setClientSecret(e.target.value)}
                    />
                  </div>
                </div>

                <div className="space-y-2">
                  <Label htmlFor="pp-env">Environment</Label>
                  <select
                    id="pp-env"
                    data-testid="paypal-environment"
                    className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm"
                    value={environment}
                    onChange={(e) => setEnvironment(e.target.value)}
                  >
                    <option value="sandbox">
                      Sandbox — developer.paypal.com test accounts
                    </option>
                    <option value="live">Live — real money</option>
                  </select>
                  <p className="text-xs text-muted-foreground">
                    Sandbox and live credentials are not interchangeable.
                    Picking the wrong one fails with &ldquo;invalid
                    client&rdquo;, which reads like a typo.
                  </p>
                </div>

                <div className="flex gap-2">
                  <Button
                    onClick={onSavePaypal}
                    disabled={busy}
                    data-testid="paypal-save"
                  >
                    {busy ? (
                      <Loader2 className="mr-2 size-4 animate-spin" />
                    ) : null}
                    Save
                  </Button>
                  {paypal?.clientIdConfigured ||
                  paypal?.clientSecretConfigured ? (
                    <Button
                      variant="ghost"
                      onClick={onClearPaypal}
                      disabled={busy}
                      data-testid="paypal-clear"
                    >
                      Disconnect
                    </Button>
                  ) : null}
                </div>
              </>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
