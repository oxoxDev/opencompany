import { useCallback, useEffect, useState } from "react";
import {
  Check,
  Copy,
  Globe,
  Info,
  Loader2,
  Mail,
  ShieldAlert,
  TriangleAlert,
  X,
} from "lucide-react";
import { toast } from "sonner";

import { me as fetchMe } from "@/api/auth";
import { ApiError } from "@/api/types";
import type { OpenCompanyClient } from "@/api/client";
import {
  clearDomain,
  type DnsRecord,
  type DomainStatus,
  getDomain,
  type RecordCheck,
  saveDomain,
  verifyDomain,
} from "@/api/domain";
import { getSmtp, saveSmtp, type SmtpSecurity, type SmtpStatus, testSmtp } from "@/api/smtp";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { isValidDomain } from "@/lib/domain";
import { cn } from "@/lib/utils";

interface Props {
  client: OpenCompanyClient;
  company: string | null;
}

const SECURITY_LABELS: Record<SmtpSecurity, string> = {
  none: "None",
  starttls: "STARTTLS",
  ssl: "SSL / TLS",
};

/** The message out of a rejected request, whatever it was rejected with. */
function reason(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Whether a rejection means "this build has no such feature".
 *
 * The `code`, never the status. Both of these routes can also answer a plain
 * 404 or a 400 for ordinary reasons — verify refuses with 400 when no domain is
 * configured — and a status check would read a real refusal as a missing
 * feature and hide the operator's actual problem behind a build notice.
 */
function isUnwired(err: unknown): boolean {
  return err instanceof ApiError && err.code === "not_wired";
}

/**
 * Settings → General: the company's custom domain and its outbound mail server.
 *
 * # This was a mock until #1460
 *
 * Both cards were browser-local. The domain card hashed the domain into a fake
 * verification token, pasted it into a hardcoded `opencompany.host` target, and
 * rendered five DNS records the host had never heard of; its "Pending" badge
 * pulsed forever because nothing checked anything. "Verify DNS" and "Test
 * connection" each fired a `toast.info` saying the real thing would happen once
 * connected. The host had implemented all of it and the console had never
 * called it.
 *
 * Now each card owns one route pair and renders only what the host reported.
 * They are deliberately independent components rather than two halves of one
 * `MailSettings` object: different routes, different authority, different
 * failure modes. A domain read that fails must not blank the SMTP form.
 *
 * # The password
 *
 * Write-only three ways, exactly as in `HostingView`: the host has no field to
 * return it, nothing persists it to browser storage, and a successful save
 * clears the input. See `src/api/smtp.ts` and `src/lib/domain.ts`.
 *
 * # Both cards are gated as coming soon (#2131)
 *
 * Neither feature is ready, so `DomainSettings` renders `ComingSoon` previews
 * and mounts neither card. `DomainCard` and `SmtpCard` below are exported and
 * otherwise unchanged except for the admin gate on their own write controls
 * (`PUT …/domain`, `PUT …/smtp` and `POST …/smtp/test` all take
 * `AdminScopedCompany` on the host, `POST …/domain/verify` stays open to a
 * member on purpose): their host-backed guarantees, gate included, are still
 * covered by `test/unit/domain-settings-host-backed.test.ts`, which renders
 * them directly, so switching the feature on is putting them back into the
 * two slots below rather than rebuilding them out of the git history. Each
 * card resolves its own `canManage`, the way `HostingView` and `SearchView`
 * resolve the same question, since neither has a mounted parent to resolve it
 * once and pass down while this gate stands.
 */
export function DomainSettings(_props: Props) {
  return (
    <>
      <ComingSoon
        testid="domain-card"
        icon={Globe}
        title="Custom domain"
        description="Sending and receiving on your own domain is on the way. It is not switched on yet, so there is nothing to configure here."
      >
        <DomainPreview />
      </ComingSoon>
      <ComingSoon
        testid="smtp-card"
        icon={Mail}
        title="Email (SMTP)"
        description="Pointing this company at your own outbound mail server is on the way. It is not switched on yet, so there is nothing to configure here."
      >
        <SmtpPreview />
      </ComingSoon>
    </>
  );
}

/**
 * A card that shows the shape of a surface without letting anyone use it.
 *
 * # Why the controls are absent rather than disabled
 *
 * The brief for #2131 is that these two are *genuinely* inert — not reachable
 * by mouse, not in the tab order, and with nothing that can be submitted.
 * `disabled` and `inert` deliver the first two and neither delivers the third:
 * an inert `<button>` still runs its handler when something calls `.click()` on
 * it, and a disabled `<input>` is still an input whose value a script or an
 * autofill can set. The SMTP card's fields include a password and a Save that
 * puts it in the host's secret store, which is exactly the pair that must not
 * be one `querySelector` away from firing.
 *
 * So the gate is that `children` is a static picture: `div`s carrying the real
 * field labels, with no `input`, `button`, `select` or handler anywhere in it.
 * There is no control to enable, no state to fill and no request to send —
 * these cards do not even read the host any more. The blur is decoration over
 * a surface that was already empty rather than the thing standing between an
 * operator and a save. `settings-coming-soon.test.ts` pins that property.
 *
 * `inert` is set anyway, imperatively — React 18's types predate the boolean
 * prop, which is why `Overview.tsx` sets it the same way — so the guarantee
 * survives someone later dropping a real control into a preview. `aria-hidden`
 * goes with it: the header and the description carry the whole meaning for a
 * screen reader, which is why they say "not switched on yet" in words instead
 * of leaving it to a blur nobody can hear.
 */
function ComingSoon({
  testid,
  icon: Icon,
  title,
  description,
  children,
}: {
  testid: string;
  icon: typeof Globe;
  title: string;
  description: string;
  children: React.ReactNode;
}) {
  return (
    <Card data-testid={testid}>
      <CardHeader>
        <CardTitle className="flex flex-wrap items-center gap-2 text-base">
          <Icon className="size-4" /> {title}
          <Badge variant="secondary" className="font-normal" data-testid={`${testid}-coming-soon`}>
            Coming soon
          </Badge>
        </CardTitle>
        <CardDescription>{description}</CardDescription>
      </CardHeader>
      <CardContent>
        <div
          data-testid={`${testid}-preview`}
          aria-hidden="true"
          ref={(el) => el?.setAttribute("inert", "")}
          className="pointer-events-none max-w-4xl select-none opacity-60 blur-xs"
        >
          {children}
        </div>
      </CardContent>
    </Card>
  );
}

/** One labelled bar standing in for a field. Not a control — see `ComingSoon`. */
function PreviewField({ label, className }: { label: string; className?: string }) {
  return (
    <div className={cn("grid gap-2", className)}>
      <span className="text-sm font-medium">{label}</span>
      <div className="h-9 rounded-md border bg-muted/40" />
    </div>
  );
}

/** Stands in for a button, so the blurred card keeps the rhythm of the real one. */
function PreviewButton({ label, className }: { label: string; className?: string }) {
  return (
    <div
      className={cn(
        "flex h-9 items-center justify-center rounded-md border bg-muted px-4 text-sm font-medium",
        className,
      )}
    >
      {label}
    </div>
  );
}

/**
 * The shape of {@link DomainCard} with nothing in it that works.
 *
 * It mirrors the real card's three parts — the domain entry row, the "Add
 * these DNS records" heading, and the TXT/CNAME table — because a preview that
 * does not resemble what is coming tells the operator nothing about what
 * switching it on would get them. The record values are bare bars rather than
 * plausible-looking hostnames: an invented `_oc-verify.example.com` sitting
 * under a blur is the kind of thing someone squints at and copies into their
 * DNS.
 *
 * Every node here is a `div` or a `span`. See {@link ComingSoon} for why that
 * is the gate rather than `disabled`.
 */
function DomainPreview() {
  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row">
        <div className="h-9 flex-1 rounded-md border bg-muted/40" />
        <PreviewButton label="Add domain" className="shrink-0" />
      </div>
      <div className="space-y-2">
        <p className="text-sm font-medium">Add these DNS records</p>
        <div className="rounded-lg border">
          <div className="flex gap-6 border-b bg-muted/50 px-3 py-2 text-xs font-medium">
            <span className="w-16">Type</span>
            <span className="w-40">Name</span>
            <span className="w-40">Value</span>
          </div>
          {["TXT", "CNAME"].map((type) => (
            <div key={type} className="flex items-center gap-6 px-3 py-2 text-xs">
              <span className="w-16 font-mono">{type}</span>
              <div className="h-3 w-40 rounded bg-muted" />
              <div className="h-3 w-40 rounded bg-muted" />
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

/**
 * The shape of {@link SmtpCard} with nothing in it that works.
 *
 * The six field labels are the real ones in the real order, so an operator can
 * tell at a glance whether they will have what the form is going to ask for.
 * The "Password" row is a {@link PreviewField} like the other five — a
 * labelled bar, not an `<input type="password">` — which is the whole point of
 * {@link ComingSoon}: there is no field here for a password manager to fill,
 * and no Save to put what it filled into the host's secret store.
 */
function SmtpPreview() {
  return (
    <div className="space-y-4">
      <div className="grid gap-4 sm:grid-cols-2">
        <PreviewField label="SMTP host" />
        <div className="grid grid-cols-2 gap-3">
          <PreviewField label="Port" />
          <PreviewField label="Security" />
        </div>
        <PreviewField label="Username" />
        <PreviewField label="Password" />
        <PreviewField label="From name" />
        <PreviewField label="From email" />
      </div>
      <div className="flex gap-2">
        <PreviewButton label="Save" />
        <PreviewButton label="Test connection" />
      </div>
    </div>
  );
}

/**
 * The real custom-domain card: add a domain, read back the DNS records the
 * host wants, and ask it to verify them.
 *
 * Not mounted anywhere while #2131's gate stands — {@link DomainSettings}
 * renders {@link DomainPreview} in its place. It is exported rather than
 * deleted so that switching the feature on is putting one element back, and so
 * that `test/unit/domain-settings-host-backed.test.ts` can go on rendering it
 * directly and pinning the guarantee that matters here: every field is read
 * from and written to the host, and none of it is cached in the browser.
 */
export function DomainCard({ client, company }: Props) {
  const [canManage, setCanManage] = useState(false);
  const [status, setStatus] = useState<DomainStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [draft, setDraft] = useState("");
  // A build fact, so it is sticky for the life of the card rather than a toast:
  // the operator is looking at the Verify button when they learn this, and a
  // notice that vanishes leaves them clicking a button that cannot work.
  const [verifyUnwired, setVerifyUnwired] = useState(false);

  useEffect(() => {
    let live = true;
    void (async () => {
      let admin = false;
      try {
        admin = (await fetchMe(client, company)).role === "admin";
      } catch {
        // `resolve_principal` on the host tries a human session first and
        // only falls back to the platform/tenant bearer when none is
        // present — a hub console can carry both, and `PUT …/domain` is
        // AdminScopedCompany, which admits that machine principal
        // unconditionally once it has addressed this company. So `/auth/me`
        // failing outright (no session at all) means the bearer is what the
        // host will actually authorize on; `/auth/me` succeeding with a
        // member role — even with a bearer also present — means the host
        // resolved that member and will 403 the same as with no bearer,
        // which the `try` above already handles correctly.
        admin = client.carriesPlatformBearer;
      }
      if (live) setCanManage(admin);
    })();
    return () => {
      live = false;
    };
  }, [client, company]);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const next = await getDomain(client, company);
      setStatus(next);
      setLoadError(null);
    } catch (err) {
      setLoadError(reason(err));
    } finally {
      setLoading(false);
    }
  }, [client, company]);

  useEffect(() => {
    void load();
  }, [load]);

  const configured = Boolean(status?.domain);

  async function connect() {
    const domain = draft.trim().toLowerCase();
    // Pre-flight only — the host does not validate, so this exists to turn a
    // typo into a sentence instead of a stored value that can never verify.
    if (!isValidDomain(domain)) {
      toast.error("Enter a valid domain, e.g. mail.acme.com");
      return;
    }
    setBusy(true);
    try {
      setStatus(await saveDomain(client, company, domain));
      toast.success("Domain saved — add the DNS records below.");
    } catch (err) {
      toast.error(reason(err));
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    setBusy(true);
    try {
      setStatus(await clearDomain(client, company));
      setDraft("");
      // A fresh domain deserves a fresh verdict on whether verification works.
      setVerifyUnwired(false);
      toast.success("Domain removed.");
    } catch (err) {
      toast.error(reason(err));
    } finally {
      setBusy(false);
    }
  }

  async function verify() {
    // The host answers 400 when nothing is configured; never ask it.
    if (!configured) return;
    setBusy(true);
    try {
      const next = await verifyDomain(client, company);
      setStatus(next);
      if (next.verified) toast.success("Domain verified.");
      else toast.message("Records not found yet — DNS can take up to 48h to propagate.");
    } catch (err) {
      // No success toast and no badge change on this path: the card must never
      // render a state the host did not report.
      if (isUnwired(err)) setVerifyUnwired(true);
      else toast.error(reason(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card data-testid="domain-card">
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <Globe className="size-4" /> Custom domain
        </CardTitle>
        <CardDescription>Send and receive on your own domain instead of the default.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {loadError ? (
          <Alert variant="destructive" data-testid="domain-load-error">
            <TriangleAlert className="size-4" />
            <AlertDescription>Could not load the domain settings: {loadError}</AlertDescription>
          </Alert>
        ) : loading ? (
          <p className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" /> Loading domain…
          </p>
        ) : !configured ? (
          canManage ? (
            <div className="flex flex-col gap-2 sm:flex-row">
              <Input
                value={draft}
                data-testid="domain-input"
                // The card title is the only thing naming this field, and a
                // screen reader does not read it as the input's name. The
                // placeholder is an example, not a label — it disappears on the
                // first keystroke, which is exactly when it would be needed.
                aria-label="Custom domain"
                onChange={(e) => setDraft(e.target.value)}
                placeholder="mail.acme.com"
                onKeyDown={(e) => e.key === "Enter" && void connect()}
              />
              <Button
                className="shrink-0"
                disabled={busy}
                onClick={() => void connect()}
                data-testid="domain-add"
              >
                Add domain
              </Button>
            </div>
          ) : (
            <>
              <Alert data-testid="domain-read-only">
                <Info className="size-4" />
                <AlertTitle>Only an admin can add this company&apos;s domain</AlertTitle>
                <AlertDescription>
                  A domain is the company&rsquo;s mail identity, so an admin sets it.
                </AlertDescription>
              </Alert>
              <p className="text-sm text-muted-foreground">No custom domain configured.</p>
            </>
          )
        ) : (
          <>
            {!canManage && (
              <Alert data-testid="domain-read-only">
                <Info className="size-4" />
                <AlertTitle>Only an admin can change this company&apos;s domain</AlertTitle>
                <AlertDescription>
                  A domain is the company&rsquo;s mail identity, so an admin sets and removes it.
                  You can see what is configured, and verifying its DNS records is open to
                  everyone.
                </AlertDescription>
              </Alert>
            )}

            <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border p-3">
              <span className="inline-flex items-center gap-2 font-mono text-sm">
                <Globe className="size-4 text-muted-foreground" />
                {status?.domain}
              </span>
              <div className="flex items-center gap-2">
                {status?.verified ? (
                  <Badge
                    className="gap-1 bg-status-done-soft text-status-done-text"
                    data-testid="domain-verified"
                  >
                    <Check className="size-3" /> Verified
                  </Badge>
                ) : (
                  <Badge variant="secondary" className="gap-1" data-testid="domain-pending">
                    <span className="size-1.5 rounded-full bg-status-blocked" /> Pending
                  </Badge>
                )}
                {canManage && (
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={busy}
                    onClick={() => void remove()}
                    data-testid="domain-remove"
                  >
                    Remove
                  </Button>
                )}
              </div>
            </div>

            {!status?.verified ? (
              <p className="text-xs text-muted-foreground" data-testid="domain-check-summary">
                {checkSummary(status?.checks, status?.records ?? [])}
              </p>
            ) : null}

            <div className="space-y-2">
              <p className="text-sm font-medium">Add these DNS records</p>
              {/* Straight off the status. Never re-derived here — see the
                  module header of `src/api/domain.ts`. */}
              <DnsTable records={status?.records ?? []} checks={status?.checks} />

              {verifyUnwired ? (
                <Alert data-testid="domain-verify-unwired">
                  <TriangleAlert className="size-4" />
                  <AlertDescription>
                    This host was built without DNS lookups, so it can&rsquo;t check these
                    records for you. Add them at your registrar; a build with the{" "}
                    <code>dns</code> feature will verify them.
                  </AlertDescription>
                </Alert>
              ) : null}

              <div className="flex items-center gap-2 pt-1">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy || verifyUnwired}
                  onClick={() => void verify()}
                  data-testid="domain-verify"
                >
                  {busy ? <Loader2 className="mr-2 size-4 animate-spin" /> : null}
                  Verify DNS
                </Button>
                <p className="text-xs text-muted-foreground">
                  Changes can take up to 48h to propagate.
                </p>
              </div>
            </div>
          </>
        )}
      </CardContent>
    </Card>
  );
}

/**
 * What the subtext under a Pending badge says.
 *
 * "Not checked yet" and "0 of 5 records found" mean different things to the
 * operator — the first is on them to press the button, the second is on their
 * registrar or on propagation — and telling them apart is the entire reason the
 * host returns `checks` at all rather than just `verified`.
 */
function checkSummary(checks: RecordCheck[] | undefined, records: DnsRecord[]): string {
  if (checks === undefined) return "Not checked yet.";
  const found = checks.filter((c) => c.found).length;
  const total = records.length || checks.length;
  return `${found} of ${total} records found.`;
}

function DnsTable({ records, checks }: { records: DnsRecord[]; checks?: RecordCheck[] }) {
  return (
    <div className="overflow-x-auto rounded-lg border">
      <table className="w-full text-left text-xs">
        <thead className="bg-muted/50 text-muted-foreground">
          <tr>
            <th className="px-3 py-2 font-medium">Type</th>
            <th className="px-3 py-2 font-medium">Name</th>
            <th className="px-3 py-2 font-medium">Value</th>
            <th className="px-3 py-2 font-medium">TTL</th>
            {checks ? <th className="px-3 py-2 font-medium">Found</th> : null}
          </tr>
        </thead>
        <tbody className="divide-y">
          {records.map((r) => {
            // Matched by (name, type), never by index. The host is free to
            // return checks in another order or to check a subset, and an
            // index-matched join would put a tick on the wrong row without
            // anything on screen looking wrong.
            const check = checks?.find((c) => c.name === r.name && c.type === r.type);
            return (
              <tr key={`${r.type}:${r.name}`} className="align-top" data-testid="dns-record-row">
                <td className="px-3 py-2">
                  <Badge variant="outline" className="font-mono">
                    {r.type}
                  </Badge>
                </td>
                <td className="px-3 py-2">
                  <CopyCell value={r.name} />
                </td>
                <td className="px-3 py-2">
                  <CopyCell value={r.value} />
                </td>
                <td className="px-3 py-2 font-mono text-muted-foreground">{r.ttl}</td>
                {checks ? (
                  <td className="px-3 py-2" data-testid="dns-record-check">
                    {check === undefined ? (
                      <span className="text-muted-foreground">—</span>
                    ) : check.found ? (
                      <Check className="size-3.5 text-status-done-text" aria-label="found" />
                    ) : (
                      <X className="size-3.5 text-status-blocked" aria-label="not found" />
                    )}
                  </td>
                ) : null}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function CopyCell({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);
  function copy() {
    void navigator.clipboard?.writeText(value);
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  }
  return (
    <button
      onClick={copy}
      className="group flex max-w-[28ch] items-center gap-1.5 text-left sm:max-w-[40ch]"
      title="Copy"
    >
      <span className="truncate font-mono">{value}</span>
      {copied ? (
        <Check className="size-3 shrink-0 text-status-done-text" />
      ) : (
        <Copy className="size-3 shrink-0 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100" />
      )}
    </button>
  );
}

/**
 * The real outbound-mail card: point the company at an SMTP server, save the
 * credentials into the host's secret store, and send a test message.
 *
 * Not mounted anywhere while #2131's gate stands — {@link DomainSettings}
 * renders {@link SmtpPreview} in its place. Exported for the same two reasons
 * as {@link DomainCard}, and one more that is specific to it: the password is
 * write-only three ways, and `test/unit/domain-settings-host-backed.test.ts`
 * is what holds that property while nothing renders the card.
 */
export function SmtpCard({ client, company }: Props) {
  const [canManage, setCanManage] = useState(false);
  const [status, setStatus] = useState<SmtpStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [testUnwired, setTestUnwired] = useState(false);

  useEffect(() => {
    let live = true;
    void (async () => {
      let admin = false;
      try {
        admin = (await fetchMe(client, company)).role === "admin";
      } catch {
        // `resolve_principal` on the host tries a human session first and
        // only falls back to the platform/tenant bearer when none is
        // present — a hub console can carry both, and `PUT …/smtp` and
        // `POST …/smtp/test` are AdminScopedCompany, which admits that
        // machine principal unconditionally once it has addressed this
        // company. So `/auth/me` failing outright (no session at all) means
        // the bearer is what the host will actually authorize on;
        // `/auth/me` succeeding with a member role — even with a bearer
        // also present — means the host resolved that member and will 403
        // the same as with no bearer, which the `try` above already
        // handles correctly.
        admin = client.carriesPlatformBearer;
      }
      if (live) setCanManage(admin);
    })();
    return () => {
      live = false;
    };
  }, [client, company]);

  const [host, setHost] = useState("");
  const [port, setPort] = useState("587");
  const [security, setSecurity] = useState<SmtpSecurity>("starttls");
  const [username, setUsername] = useState("");
  const [fromName, setFromName] = useState("");
  const [fromEmail, setFromEmail] = useState("");
  // Always seeded empty and never prefilled with dots: the host has no field to
  // return it from, and an input full of placeholder characters invites an
  // operator to "correct" a value they cannot see — submitting the dots would
  // store the dots.
  const [password, setPassword] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const next = await getSmtp(client, company);
      setStatus(next);
      setLoadError(null);
      setHost(next.host ?? "");
      setPort(next.port === undefined ? "587" : String(next.port));
      setSecurity(next.security ?? "starttls");
      setUsername(next.username ?? "");
      setFromName(next.from_name ?? "");
      setFromEmail(next.from_email ?? "");
      setPassword("");
    } catch (err) {
      setLoadError(reason(err));
    } finally {
      setLoading(false);
    }
  }, [client, company]);

  useEffect(() => {
    void load();
  }, [load]);

  async function save() {
    // The host takes a `u16`, so anything outside this range comes back as a
    // serde deserialize error an operator cannot act on. Caught here instead.
    const portNumber = Number(port.trim());
    if (!Number.isInteger(portNumber) || portNumber < 1 || portNumber > 65535) {
      toast.error("Port must be a whole number between 1 and 65535.");
      return;
    }
    setBusy(true);
    try {
      const next = await saveSmtp(client, company, {
        host: host.trim(),
        port: portNumber,
        // Always sent explicitly: it has no safe default on the host, and a
        // silently-omitted `security` is the difference between STARTTLS and
        // plaintext on the wire.
        security,
        username: username.trim(),
        from_email: fromEmail.trim(),
        from_name: fromName.trim(),
        // Omitted when blank, which is how "leave the stored one alone" is
        // expressed. Sending "" would clear it.
        ...(password ? { password } : {}),
      });
      setStatus(next);
      // Same reason `HostingView` clears its API key: a credential left sitting
      // in a form field is one screen-share from a leak.
      setPassword("");
      toast.success("Email settings saved.");
    } catch (err) {
      toast.error(reason(err));
    } finally {
      setBusy(false);
    }
  }

  async function test() {
    setBusy(true);
    try {
      const res = await testSmtp(client, company);
      // The host's own sentence, verbatim, on both branches — it knows whether
      // the server refused the credentials, timed out, or rejected the From
      // address, and a generic replacement throws that away.
      if (res.ok) toast.success(res.message);
      else toast.error(res.message);
    } catch (err) {
      if (isUnwired(err)) setTestUnwired(true);
      else toast.error(reason(err));
    } finally {
      setBusy(false);
    }
  }

  if (loadError) {
    return (
      <Card data-testid="smtp-card">
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <Mail className="size-4" /> Email (SMTP)
          </CardTitle>
        </CardHeader>
        <CardContent>
          <Alert variant="destructive" data-testid="smtp-load-error">
            <TriangleAlert className="size-4" />
            <AlertDescription>Could not load the email settings: {loadError}</AlertDescription>
          </Alert>
        </CardContent>
      </Card>
    );
  }

  // Whether the host has a *stored* configuration to test — not whether the
  // form on screen looks filled in. The test send goes through what the host
  // holds, so gating on the form would enable the button on a typed-but-unsaved
  // password and report a verdict about something else entirely. That is the
  // same class of lie the card had before #1460, one button along.
  const testable = Boolean(status?.configured);

  return (
    <Card data-testid="smtp-card">
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <Mail className="size-4" /> Email (SMTP)
        </CardTitle>
        <CardDescription>The outbound mail server your company sends through.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {loading ? (
          <p className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" /> Loading email settings…
          </p>
        ) : !canManage ? (
          <>
            <Alert data-testid="smtp-read-only">
              <Info className="size-4" />
              <AlertTitle>Only an admin can change this company&apos;s email settings</AlertTitle>
              <AlertDescription>
                These credentials are the address the company sends mail as, so an admin sets
                them. You can see what is configured.
              </AlertDescription>
            </Alert>
            {status?.configured ? (
              <dl className="grid gap-2 text-sm sm:grid-cols-2" data-testid="smtp-summary">
                <SummaryRow label="SMTP host" value={status.host} />
                <SummaryRow label="Port" value={status.port !== undefined ? String(status.port) : undefined} />
                <SummaryRow
                  label="Security"
                  value={status.security ? SECURITY_LABELS[status.security] : undefined}
                />
                <SummaryRow label="Username" value={status.username} />
                <SummaryRow label="From name" value={status.from_name} />
                <SummaryRow label="From email" value={status.from_email} />
              </dl>
            ) : (
              <p className="text-sm text-muted-foreground">No email settings configured.</p>
            )}
          </>
        ) : (
          <>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label="SMTP host" id="smtp-host">
                <Input
                  id="smtp-host"
                  data-testid="smtp-host"
                  value={host}
                  onChange={(e) => setHost(e.target.value)}
                  placeholder="smtp.postmarkapp.com"
                />
              </Field>
              <div className="grid grid-cols-2 gap-3">
                <Field label="Port" id="smtp-port">
                  <Input
                    id="smtp-port"
                    data-testid="smtp-port"
                    value={port}
                    onChange={(e) => setPort(e.target.value)}
                    placeholder="587"
                    inputMode="numeric"
                  />
                </Field>
                <Field label="Security" id="smtp-security">
                  <Select
                    value={security}
                    onValueChange={(v) => v && setSecurity(v as SmtpSecurity)}
                    items={SECURITY_LABELS}
                  >
                    <SelectTrigger id="smtp-security" className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {(Object.keys(SECURITY_LABELS) as SmtpSecurity[]).map((k) => (
                        <SelectItem key={k} value={k}>
                          {SECURITY_LABELS[k]}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </Field>
              </div>
              <Field label="Username" id="smtp-user">
                <Input
                  id="smtp-user"
                  data-testid="smtp-username"
                  value={username}
                  onChange={(e) => setUsername(e.target.value)}
                  placeholder="apikey"
                  autoComplete="off"
                />
              </Field>
              <Field
                label={status?.configured ? "Password (stored — leave blank to keep)" : "Password"}
                id="smtp-pass"
                hint="Sent to the host's secret store on Save, and never returned. Never written to browser storage."
              >
                <Input
                  id="smtp-pass"
                  data-testid="smtp-password"
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  autoComplete="off"
                />
              </Field>
              <Field label="From name" id="smtp-fromname">
                <Input
                  id="smtp-fromname"
                  data-testid="smtp-from-name"
                  value={fromName}
                  onChange={(e) => setFromName(e.target.value)}
                  placeholder="Agentic Marketing Agency"
                />
              </Field>
              <Field label="From email" id="smtp-fromemail">
                <Input
                  id="smtp-fromemail"
                  data-testid="smtp-from-email"
                  value={fromEmail}
                  onChange={(e) => setFromEmail(e.target.value)}
                  placeholder="hello@mail.acme.com"
                />
              </Field>
            </div>

            {testUnwired ? (
              <Alert data-testid="smtp-test-unwired">
                <TriangleAlert className="size-4" />
                <AlertDescription>
                  This host was built without the <code>smtp</code> feature, so it can&rsquo;t
                  send mail — the credentials above are stored and will be used by a build
                  that has it.
                </AlertDescription>
              </Alert>
            ) : null}

            <div className="flex items-center gap-2">
              <Button onClick={() => void save()} disabled={busy} data-testid="smtp-save">
                {busy ? <Loader2 className="mr-2 size-4 animate-spin" /> : null}
                Save
              </Button>
              <Button
                variant="outline"
                disabled={busy || !testable || testUnwired}
                onClick={() => void test()}
                data-testid="smtp-test"
              >
                <ShieldAlert className="size-4" /> Test connection
              </Button>
              {testable ? null : (
                <span className="text-xs text-muted-foreground" data-testid="smtp-test-hint">
                  Save a complete configuration, password included, to test it.
                </span>
              )}
            </div>
          </>
        )}
      </CardContent>
    </Card>
  );
}

function Field({
  label,
  id,
  hint,
  children,
}: {
  label: string;
  id: string;
  /** Rendered under the control. Use it to state what happens to what is typed. */
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className={cn("grid gap-2")}>
      <Label htmlFor={id}>{label}</Label>
      {children}
      {hint ? <p className="text-xs text-muted-foreground">{hint}</p> : null}
    </div>
  );
}

/** One read-only field in the SMTP summary a non-admin sees instead of the form. */
function SummaryRow({ label, value }: { label: string; value: string | undefined }) {
  return (
    <div>
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="font-mono">{value || "—"}</dd>
    </div>
  );
}
