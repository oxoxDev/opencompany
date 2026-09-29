import { useEffect, useRef } from "react";
import { AlertTriangle, ExternalLink, Loader2, Save } from "lucide-react";

import { offersSkipVerify } from "@/composio/classify";
import type { ComposioSubmitOutcome } from "@/composio/classify";
import { ProbeAdvisory } from "@/composio/ProbeAdvisory";
import { credentialDialogBlurb, credentialDialogTitle } from "@/composio/rows";
import type { ComposioForm } from "@/composio/types";
import { Button, buttonVariants } from "@/components/ui/button";
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
import { cn } from "@/lib/utils";

/**
 * Where a BYOK key comes from.
 *
 * The bare host the field's own copy names, not a deep link into the settings
 * page that mints the key: that path is the vendor's to move, and a stale one
 * strands the operator on a 404 *after* a sign-in that worked — which is worse
 * than the landing page they can navigate from themselves.
 */
export const COMPOSIO_DASHBOARD_URL = "https://app.composio.dev";

/**
 * The Composio credential form, as a component rather than as JSX inside the
 * Connections page.
 *
 * Lifted out unchanged so a second surface can mount it — the first-run
 * wizard's self-managed step asks the same question before a company exists,
 * and the alternative was a wizard-sized copy of a form whose Save writes the
 * credential every agent in the company presents.
 *
 * What it owns is what the form itself decides: the title and blurb for the
 * route, where the key comes from, when "add anyway" is on offer, the
 * managed → BYOK confirmation and the focus handoff into and out of it. What
 * it is given is the form to draw, the value being typed, and what its Save
 * does — because that is the part the two callers genuinely differ on: the
 * Connections page writes, and the wizard stages for an apply.
 */
export function ComposioCredentialDialog({
  form,
  secret,
  onSecretChange,
  outcome,
  onOutcomeChange,
  confirmSwitch,
  onConfirmSwitchChange,
  busy,
  onSubmit,
  onRequestSubmit,
  onCancel,
}: {
  /** The form to draw, from `composioForm`. */
  form: ComposioForm;
  secret: string;
  onSecretChange: (next: string) => void;
  /** What the last attempt came back as — an advisory kept the key, a rejection stored nothing. */
  outcome: ComposioSubmitOutcome | null;
  onOutcomeChange: (next: ComposioSubmitOutcome | null) => void;
  /** Whether the managed -> BYOK warning has the floor in place of the footer. */
  confirmSwitch: boolean;
  onConfirmSwitchChange: (next: boolean) => void;
  busy: boolean;
  /** Store what is in the field. `true` skips verification — the "add anyway" path. */
  onSubmit: (skipVerify?: boolean) => void;
  /** Save pressed: the caller decides whether this save needs confirming first. */
  onRequestSubmit: () => void;
  onCancel: () => void;
}) {
  // Focus in and back out of the switch confirmation. It is a labelled group
  // inside this dialog rather than a popup of its own, so nothing moves focus
  // for free: showing it unmounts the footer's Save button, which is where
  // focus was, and the dialog's trap then leaves focus on the popup itself —
  // invisible to a mouse user, but a screen-reader or keyboard user loses
  // their place entirely.
  //
  // Both directions are by REF to a currently-rendered button, not by
  // recording the node that had focus. Recording it was the first shape and it
  // cannot work here: the node focus came from is the footer's Save button,
  // and showing the confirmation is exactly what unmounts it — so by the time
  // Cancel puts it back, the recorded node is detached and a restore onto it
  // is a no-op. The footer's Save button re-registers this ref on the way
  // back, which is the same button by role even though it is a different node.
  const confirmPrimaryActionRef = useRef<HTMLButtonElement | null>(null);
  const saveButtonRef = useRef<HTMLButtonElement | null>(null);
  // Whether the confirmation has been on screen during this dialog. Without it,
  // the first render of every dialog would count as a close and yank focus onto
  // Save before the operator has touched the field.
  const confirmWasOpen = useRef(false);

  // Opening moves focus onto the confirmation's primary action; cancelling
  // hands it back to the Save button the confirmation replaced.
  //
  // On a save that SUCCEEDED there is nothing to hand back to — the whole
  // dialog unmounts — and `saveButtonRef` is null by then, so this does
  // nothing and Base UI returns focus to the row control that opened the
  // dialog. That is the right destination, and it is why this does not need a
  // guard for the difference.
  useEffect(() => {
    if (confirmSwitch) {
      confirmWasOpen.current = true;
      confirmPrimaryActionRef.current?.focus();
      return;
    }
    if (!confirmWasOpen.current) return;
    confirmWasOpen.current = false;
    saveButtonRef.current?.focus();
  }, [confirmSwitch]);

  // "Add anyway" answers a refused API-KEY write, and only that. `skipVerify`
  // is a parameter of `setComposioApiKey` alone — `submit(true)` on the managed
  // row's token drops it and re-sends a byte-identical request, so the button
  // there could only ever earn the same refusal again. The classifier cannot
  // see which credential is in the form, so the form says.
  const skipOffered = offersSkipVerify(outcome) && form.credential === "composio-api-key";

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        // A write in flight holds the dialog open: dismissing it now
        // would take away the only place its answer is reported, while
        // the credential lands anyway.
        if (next || busy) return;
        onCancel();
      }}
    >
      <DialogContent
        className="sm:max-w-md"
        showCloseButton={!busy}
        data-testid="composio-form-dialog"
      >
        <DialogHeader>
          <DialogTitle>{credentialDialogTitle(form)}</DialogTitle>
          <DialogDescription>
            {credentialDialogBlurb(form)}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-1.5">
          <Label
            htmlFor={form.credential}
            className="text-xs"
            data-testid="composio-form-label"
          >
            {form.row === "byok" ? "Composio API key" : "Composio token"}
            {form.rotating
              ? " — stored; paste a new value to rotate"
              : ""}
          </Label>
          <Input
            id={form.credential}
            type="password"
            autoComplete="off"
            disabled={busy}
            placeholder={
              form.row === "byok"
                ? "ak_…"
                : "paste the company's Composio token"
            }
            value={secret}
            onChange={(e) => {
              onSecretChange(e.target.value);
              // A refusal is a verdict on the key that was SUBMITTED,
              // and "add anyway" is only earned by that key. Leaving it
              // standing while the field changes would let the button
              // store a different, never-probed value with the check
              // skipped — a key nobody tried, handed the escape hatch
              // the flow reserves for one that was. So editing retires
              // the refusal, and with it the offer; the next Save
              // probes the new value like any other.
              if (outcome?.kind === "rejected") onOutcomeChange(null);
            }}
            // Enter submits, the idiom the console's other credential
            // field already uses (`McpServersSection`). Not while the
            // confirmation is up: there the keyboard belongs to the
            // choice being put. Tab is NOT taken, so the field behind
            // the confirmation is still reachable — deliberately, since
            // the value it holds is what the confirmation is about.
            onKeyDown={(e) => {
              if (e.key !== "Enter") return;
              if (busy || confirmSwitch || !secret.trim()) return;
              e.preventDefault();
              onRequestSubmit();
            }}
          />
          {/* Where to get it, which is the one thing the field cannot
              say for itself. What storing it *does* is the line under
              the title, so it is not repeated here. */}
          <p className="text-xs text-muted-foreground">
            {form.row === "byok"
              ? "From your Composio dashboard at app.composio.dev. Stored on this host, never shown again."
              : "Stored on this host, never shown again."}
          </p>
          {/* The dashboard the line above names, as somewhere to go
              rather than an address to retype. Deliberately the bare
              host from that copy and not a guessed deep link: a
              settings path that moves leaves the operator on a 404
              after a sign-in that worked.

              Only on the own-account row. The managed route's token
              does not come from app.composio.dev at all — it is a
              bearer the TinyHumans backend issues — so offering the
              same errand there would send an operator to the wrong
              vendor for the credential they were asked for. */}
          {form.row === "byok" && (
            <a
              href={COMPOSIO_DASHBOARD_URL}
              target="_blank"
              rel="noreferrer"
              data-testid="composio-open-dashboard"
              className={cn(
                buttonVariants({ variant: "outline", size: "sm" }),
                "mt-1",
              )}
            >
              Open Composio dashboard
              <ExternalLink className="size-3.5" />
            </a>
          )}
        </div>

        {/* The refusal, where the operator is looking — and "add
            anyway", which answers a refused write and so can only be
            offered next to the field that was refused. */}
        {outcome && (
          <ProbeAdvisory
            outcome={outcome}
            skipOffered={skipOffered}
            busy={busy}
            onSkip={() => onSubmit(true)}
            onDismiss={() => onOutcomeChange(null)}
          />
        )}

        {/* Said before the switch, not after: what it costs is not
            readable off a row.

            `role="group"`, NOT `role="alertdialog"`, which is what this
            carried while it was a block on the page. It is inside a
            `DialogContent` now — an element already announced as
            `role="dialog" aria-modal="true"` — and a second dialog role
            nested in a modal's own subtree is not a composition ARIA
            defines: two elements claim one modal context and the inner
            one has no modality, no focus containment and no boundary of
            its own. A labelled, described group is the honest shape for
            what this actually is — a titled block of the dialog it
            lives in, whose text belongs to the button beneath it. */}
        {confirmSwitch ? (
          <div
            role="group"
            aria-labelledby="composio-switch-warning"
            aria-describedby="composio-switch-consequence"
            className="space-y-3 rounded-md border border-status-blocked/40 bg-status-blocked-soft p-3"
          >
            <p
              id="composio-switch-warning"
              className="inline-flex items-center gap-2 text-xs font-medium"
            >
              <AlertTriangle className="size-3.5 shrink-0" />
              Providers connected before this stay where they are
            </p>
            <p
              id="composio-switch-consequence"
              className="text-xs text-muted-foreground"
            >
              They live in the Composio account this company reached
              before, not in this one, so the grid will look empty until
              you connect them again here. Choosing TinyHumans-managed
              again puts this company back where it is now.
            </p>
            <div className="flex flex-wrap gap-2">
              <Button
                ref={confirmPrimaryActionRef}
                size="sm"
                disabled={busy}
                data-testid="composio-confirm-switch"
                onClick={() => onSubmit()}
              >
                {busy ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <Save className="size-4" />
                )}
                Use this company&apos;s account
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={busy}
                onClick={() => onConfirmSwitchChange(false)}
              >
                Cancel
              </Button>
            </div>
          </div>
        ) : (
          <DialogFooter>
            <Button
              variant="outline"
              disabled={busy}
              data-testid="composio-form-cancel"
              onClick={onCancel}
            >
              Cancel
            </Button>
            <Button
              ref={saveButtonRef}
              disabled={busy || !secret.trim()}
              data-testid="composio-form-save"
              onClick={onRequestSubmit}
            >
              {busy ? (
                <Loader2 className="size-4 animate-spin" />
              ) : (
                <Save className="size-4" />
              )}
              {form.rotating
                ? `Rotate ${form.keyNoun}`
                : `Save ${form.keyNoun}`}
            </Button>
          </DialogFooter>
        )}
      </DialogContent>
    </Dialog>
  );
}
