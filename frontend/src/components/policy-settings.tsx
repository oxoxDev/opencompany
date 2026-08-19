import { useCallback, useEffect, useState } from "react";
import { Loader2, RotateCcw, ShieldCheck } from "lucide-react";
import { toast } from "sonner";

import type { OpenCompanyClient } from "@/api/client";
import {
  getPolicy,
  type PolicyStatus,
  resetPolicy,
  setPolicy,
} from "@/api/policy";
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
import { cn } from "@/lib/utils";

interface Props {
  client: OpenCompanyClient;
  company: string | null;
}

/**
 * The autonomy tier and the always-ask list (issue #562).
 *
 * An operator drowning in approval cards previously had no way to stop it: the
 * tier lives in the company manifest, and nothing in the console read or wrote
 * it — so changing it meant editing a version-controlled file and redeploying,
 * or on a hosted tenant (where the manifest is a read-only boot snapshot) it
 * meant nothing at all.
 *
 * Two things this deliberately renders rather than hides:
 *
 * - **The tiers are described by consequence, not by name.** "Supervised" and
 *   "full" mean nothing to someone deciding between them; "asks before every
 *   change, including its own scratch files" does. The prose comes from the
 *   host, because it describes what that host's approval gate actually does.
 * - **When a change bites.** A tier change lands on the company's *next* turn,
 *   so a turn already running finishes under the old one. Since stopping the
 *   flood *now* is exactly why an operator is here, that gap is stated instead
 *   of being left to discover.
 * - **That version control outranks it.** The override is durable between seed
 *   edits, but editing `[policy]` in `company.toml` clears it. An operator who
 *   cannot see that would be surprised by a redeploy.
 */
export function PolicySettings({ client, company }: Props) {
  const [status, setStatus] = useState<PolicyStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  // Distinguishes "still loading" from "load finished and failed". Without it,
  // `loading || !status` renders the spinner forever on a failed load and the
  // operator has no way to retry.
  const [loadError, setLoadError] = useState<string | null>(null);
  // The always-ask list is edited as text and only committed on Save, so a
  // half-typed effect kind never reaches the gate.
  const [draftAlways, setDraftAlways] = useState("");
  const [dirty, setDirty] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const next = await getPolicy(client, company);
      setStatus(next);
      setDraftAlways(next.alwaysApprove.join(", "));
      setDirty(false);
    } catch (error) {
      const message =
        error instanceof Error ? error.message : "Could not load the policy.";
      setLoadError(message);
      toast.error(message);
    } finally {
      setLoading(false);
    }
  }, [client, company]);

  useEffect(() => {
    void load();
  }, [load]);

  /**
   * Applies a server response.
   *
   * `resyncDraft` is false when the operator has unsaved always-ask edits: the
   * server's list is authoritative for what the gate is enforcing, but
   * overwriting the box would silently discard what they were part-way through
   * typing. The tier request does not touch the list, so leaving the draft
   * alone keeps the two independent — the same separation the `PUT` body has.
   */
  const apply = (next: PolicyStatus, message: string, resyncDraft = true) => {
    setStatus(next);
    if (resyncDraft) {
      setDraftAlways(next.alwaysApprove.join(", "));
      setDirty(false);
    }
    toast.success(message, { description: next.takesEffect });
  };

  const chooseTier = async (mode: string) => {
    if (!status || saving || mode === status.mode) return;
    setSaving(true);
    try {
      // Only `mode` is sent: an omitted field leaves the always-ask list where
      // it is, so picking a tier cannot silently discard a list the operator
      // edited earlier.
      // `dirty` means the operator has unsaved list edits; keep them.
      apply(
        await setPolicy(client, company, { mode }),
        "Autonomy tier updated",
        !dirty,
      );
    } catch (error) {
      toast.error(
        error instanceof Error ? error.message : "Could not change the tier.",
      );
    } finally {
      setSaving(false);
    }
  };

  const saveAlways = async () => {
    if (!status || saving) return;
    setSaving(true);
    try {
      // An empty box means an empty list, not "leave it alone" — the host keeps
      // those apart and so must this.
      const kinds = draftAlways
        .split(",")
        .map((kind) => kind.trim())
        .filter(Boolean);
      apply(
        await setPolicy(client, company, { alwaysApprove: kinds }),
        "Always-ask list updated",
      );
    } catch (error) {
      toast.error(
        error instanceof Error ? error.message : "Could not save the list.",
      );
    } finally {
      setSaving(false);
    }
  };

  const reset = async () => {
    if (!status || saving) return;
    setSaving(true);
    try {
      apply(
        await resetPolicy(client, company),
        "Reverted to the manifest's policy",
      );
    } catch (error) {
      toast.error(
        error instanceof Error ? error.message : "Could not reset the policy.",
      );
    } finally {
      setSaving(false);
    }
  };

  return (
    <Card data-testid="policy-settings">
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <ShieldCheck className="h-4 w-4" />
          Approvals
        </CardTitle>
        <CardDescription>
          How much the teammates do on their own, and what they always ask about
          first.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        {loading ? (
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            Loading the current policy…
          </div>
        ) : !status ? (
          <div className="space-y-3">
            <p className="text-sm text-muted-foreground">
              {loadError ?? "Could not load the policy."}
            </p>
            <Button size="sm" variant="outline" onClick={() => void load()}>
              Try again
            </Button>
          </div>
        ) : (
          <>
            <div className="space-y-2">
              {status.tiers.map((tier) => {
                const active = tier.value === status.mode;
                return (
                  <button
                    key={tier.value}
                    type="button"
                    disabled={saving}
                    onClick={() => void chooseTier(tier.value)}
                    aria-pressed={active}
                    className={cn(
                      "w-full rounded-md border p-3 text-left transition-colors",
                      "disabled:cursor-not-allowed disabled:opacity-60",
                      active
                        ? "border-primary bg-primary/5"
                        : "hover:bg-muted/50",
                    )}
                  >
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium">{tier.label}</span>
                      {active && (
                        <Badge variant="secondary" className="text-xs">
                          Current
                        </Badge>
                      )}
                    </div>
                    <p className="mt-1 text-xs text-muted-foreground">
                      {tier.description}
                    </p>
                  </button>
                );
              })}
              <p className="text-xs text-muted-foreground">
                Takes effect {status.takesEffect}.
              </p>
            </div>

            <div className="space-y-2">
              <Label htmlFor="always-approve">Always ask first</Label>
              <p className="text-xs text-muted-foreground">
                Effect kinds that park for approval whatever the tier — these win
                even on Full. Comma-separated.
              </p>
              <Input
                id="always-approve"
                value={draftAlways}
                disabled={saving}
                placeholder="payment.send, filing.submit, external.publish"
                onChange={(event) => {
                  setDraftAlways(event.target.value);
                  setDirty(true);
                }}
              />
              {dirty && (
                <Button
                  size="sm"
                  disabled={saving}
                  onClick={() => void saveAlways()}
                >
                  Save list
                </Button>
              )}
            </div>

            {status.overridden && (
              <div className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-dashed p-3">
                <p className="text-xs text-muted-foreground">
                  Set here{status.setBy ? ` by ${status.setBy}` : ""}, overriding
                  the manifest ({status.manifestMode}). Editing{" "}
                  <code>[policy]</code> in <code>company.toml</code> clears it —
                  version control wins when it speaks.
                </p>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={saving}
                  onClick={() => void reset()}
                >
                  <RotateCcw className="mr-1 h-3 w-3" />
                  Use the manifest's policy
                </Button>
              </div>
            )}
          </>
        )}
      </CardContent>
    </Card>
  );
}
