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
import { listWorkflowToolSlugs } from "@/api/workflows";
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

/**
 * Tools worth naming as an *example* of something to always ask about, most
 * consequential first (issue #1226).
 *
 * A placeholder is a suggestion, so it should suggest a gate an operator might
 * actually want. Taking the host's list in its own order put
 * `read_workspace_state` — a read — in the worked example, which is a valid
 * entry and a pointless one. This orders the candidates; the datalist under the
 * field still carries the deployment's full set in the host's order, because
 * that is a lookup rather than a recommendation.
 *
 * Not a validator and not a filter: anything wired here is offered, and a tool
 * absent from this list simply sorts after the ones on it.
 */
const WORTH_GATING = [
  "publish_artifact",
  "shell",
  "http_request",
  "curl",
  "git_operations",
  "apply_patch",
  "web_fetch",
];

/**
 * Up to three worked examples, drawn from what this deployment wired.
 *
 * Falls back to real tool names when the host served nothing — a host predating
 * `…/workflows/tool-slugs` still deserves an example that would work, and the
 * one this field used to give (`payment.send, filing.submit, external.publish`)
 * is the one issue #684 deleted for gating nothing.
 */
export function alwaysAskPlaceholder(wired: string[]): string {
  if (wired.length === 0) return "shell, http_request, publish_artifact";
  const rank = (slug: string) => {
    const at = WORTH_GATING.indexOf(slug);
    return at === -1 ? WORTH_GATING.length : at;
  };
  return [...wired]
    .sort((a, b) => rank(a) - rank(b) || wired.indexOf(a) - wired.indexOf(b))
    .slice(0, 3)
    .join(", ");
}

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
  /**
   * The tool names this deployment can actually gate (issue #1226).
   *
   * An `always_approve` entry IS a tool name on the harness path — see
   * `src/policy/always_approve.rs`, which explains that the two were never
   * separate namespaces. So the honest set of worked examples is the set of
   * tools wired here, and this is the same read the workflow copilot grounds on
   * (issues #783 / #874) for the same reason: so nothing suggests a tool this
   * deployment does not have.
   *
   * Empty on a host predating the route, which degrades to the plain field the
   * operator had before — the suggestions are help, never a constraint. The
   * namespace stays open on purpose (a hosted brain may emit a kind this
   * repository has never seen), so nothing here validates what is typed.
   */
  const [wiredTools, setWiredTools] = useState<string[]>([]);

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

  // Deliberately silent about its own failure, and deliberately not part of
  // `load`: these are suggestions under a free-text box. A host that cannot
  // serve them costs the operator a datalist, not the setting, and a second
  // error banner would report the policy card as broken when it is merely
  // plainer — the same reasoning `LedgersView.refreshTasks` gives.
  useEffect(() => {
    let live = true;
    void listWorkflowToolSlugs(client, company)
      .then((r) => {
        if (live) setWiredTools(r.slugs);
      })
      .catch(() => {
        if (live) setWiredTools([]);
      });
    return () => {
      live = false;
    };
  }, [client, company]);

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
              {/* Issue #1226: what an entry IS, said here rather than left to
                  the placeholder. `payment.send, filing.submit,
                  external.publish` used to be the only worked example this
                  field offered — the exact three strings issue #684 deleted
                  from the shipped default because, on the harness path, none of
                  them names a tool and so none of them gated anything. An
                  operator following the suggestion got a fence that was not
                  there, confirmed by a "list updated" toast.

                  A tool name and an effect kind were never two namespaces (see
                  `src/policy/always_approve.rs`), so naming the tool case first
                  is naming the case that applies to every company running the
                  openhuman toolbelt. The prefix rule is stated because it is
                  what `always_approve::matches` implements and nothing in the
                  console said it. */}
              <p className="text-xs text-muted-foreground">
                What the teammates always park for approval, whatever the tier —
                these win even on Full. Comma-separated. An entry is a tool name
                (<code>shell</code>, <code>http_request</code>), or a dotted
                effect kind a hosted brain emits; a leading segment matches the
                rest, so <code>invoice</code> covers{" "}
                <code>invoice.send</code>.
              </p>
              <Input
                id="always-approve"
                value={draftAlways}
                disabled={saving}
                list={wiredTools.length > 0 ? "always-approve-tools" : undefined}
                placeholder={alwaysAskPlaceholder(wiredTools)}
                onChange={(event) => {
                  setDraftAlways(event.target.value);
                  setDirty(true);
                }}
              />
              {/* Suggestions, never a constraint: the effect namespace is open
                  on purpose, because a hosted brain may emit a kind this
                  repository has never seen, and a `datalist` leaves free text
                  free. Rendered only when the host served the set, so a host
                  predating the route degrades to the plain box. */}
              {wiredTools.length > 0 && (
                <datalist id="always-approve-tools">
                  {wiredTools.map((slug) => (
                    <option key={slug} value={slug} />
                  ))}
                </datalist>
              )}
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
