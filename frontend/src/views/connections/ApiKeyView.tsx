import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { CreditCard, EllipsisVertical, ExternalLink, KeyRound, Wallet } from "lucide-react";
import { toast } from "sonner";

import { me as fetchMe } from "@/api/auth";
import type { OpenCompanyClient } from "@/api/client";
import {
  getCompanyBilling,
  getCompanyCredential,
  setCompanyCredential,
  setCompanyCredentialModel,
  type CompanyBilling,
  type CompanyCredentialMutation,
  type CompanyCredentialStatus,
} from "@/api/credential";
import { restartInference } from "@/api/inference";
import { accountFills } from "@/views/connections/account-fill";
import {
  accountKeyUsedByMessage,
  confirmInUseFor,
  guardedOutcome,
} from "@/views/connections/account-in-use";
import { ApiError } from "@/api/types";
import { PageHeader } from "@/components/page-header";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import {
  ACCOUNT_LABEL,
  REMOVAL_CONSEQUENCE,
  REMOVAL_AND_THINKING,
  accountShape,
  accountSubline,
  balanceLine,
  canRemoveKey,
  headerActions,
  keyVerdict,
  type AccountLoad,
} from "@/views/connections/account";
import { AccountKeyDialog, type AccountKeyModelStep } from "@/views/connections/AccountKeyDialog";
import { useRedeemKeyGrant } from "@/views/connections/use-redeem-key-grant";
import { useStartKeyGrant } from "@/views/connections/use-start-key-grant";

interface Props {
  client: OpenCompanyClient;
  company: string | null;
}

/**
 * Account — the one page about the account this company spends through.
 *
 * ## Why it is its own page rather than a card on Apps
 *
 * The key was reachable from two places and explained by neither. On
 * Connections → Apps it sat under a heading about third-party accounts, framed
 * as the thing that makes Gmail connectable; on Inference it appeared as one
 * option in a provider picker. Both are true and both are consequences. What
 * neither page could say, because neither is about it, is the plain thing: this
 * key is the company's account with TinyHumans, every teammate's thinking and
 * every connected app is billed to it, and when it runs out the company stops
 * working.
 *
 * A page whose subject is the account can say that once, show what is left on
 * it, and put the two actions — connect, top up — where somebody looking for
 * them would look.
 *
 * ## What it shows and what it refuses to
 *
 * Balance and plan, read through the host with the key it already holds
 * (`GET …/credential/billing`). Never the key itself: it is write-only on the
 * host and is not returned by any route, which is what makes "the console leaked
 * it" not a thing that can happen here.
 *
 * No checkout, either. Topping up and changing a plan move money and belong to
 * a person signed in to their own TinyHumans account — so those are links out,
 * to whichever hub this host is pointed at.
 *
 * ## The shape, and why it is the LLM page's
 *
 * Two cards: one carrying the action, one carrying the state as rows. It is the
 * same furniture `inference/ProvidersTab` uses, for the same reason — an
 * operator crossing from that page should recognise the language rather than
 * relearn it.
 *
 * It is **not** a list, and nothing here pretends otherwise. There is one
 * credential, so there is no toggle (nothing to enable against), no default
 * marker (there is one of these), and no add-a-provider modal. What the row
 * shape buys on a page with one thing on it is the sub-line: a fixed place to
 * say which tier actually answers, which is the fact this page most often gets
 * asked for and the one the old prose buried.
 *
 * The balance is the second row rather than a card of its own. It is another
 * fact about the same account, with its own two controls, and a card to hold
 * one number was a card explaining itself.
 *
 * ## Four states, and the third and fourth are the ones that were wrong
 *
 * Nothing set; this company's own key; **no company key but a live instance
 * identity** — the hosted case, where the server's account is quietly paying
 * and "not configured" is simply false; and **a store the host could not read**,
 * which `company_key::resolve` deliberately propagates rather than degrading,
 * because a connection made under a silently-borrowed identity belongs to the
 * wrong account invisibly and permanently. All four are decided in
 * `./account.ts`, with a unit test each, rather than in the JSX below.
 */
export function ApiKeyView({ client, company }: Props) {
  // Resolved here rather than taken as a prop, the same way `OAuthView` does
  // it: the section is a dispatcher and has no user plane of its own, and a
  // page that asked its parent for authority would be trusting a value nothing
  // on this rail is responsible for keeping true.
  //
  // Courtesy, not enforcement — the host refuses a non-admin's write whatever
  // this says. What it prevents is offering somebody a credential field whose
  // submit could only ever 403.
  const [canManage, setCanManage] = useState(false);
  const [status, setStatus] = useState<CompanyCredentialStatus | null>(null);
  const [billing, setBilling] = useState<CompanyBilling | null>(null);
  const [load, setLoad] = useState<AccountLoad>("loading");
  const [generation, setGeneration] = useState(0);
  const [editing, setEditing] = useState(false);
  /** Why the last key save failed — shown inside the dialog, not as a toast. */
  const [keyError, setKeyError] = useState<string | null>(null);
  /** Whether the Remove-key confirmation is open. */
  const [removing, setRemoving] = useState(false);
  /**
   * The Remove-key dialog's own reason for showing more than the generic
   * question — `null` before anything has said the key is in use, and the
   * host's sentence once either the status read at open time or a refused
   * attempt has (KR-L3-01; `@/views/connections/account-in-use`). Reopening
   * the dialog after a `409` sets this rather than closing it, which is
   * exactly the flow the old fixed-text dialog never had.
   */
  const [removeReason, setRemoveReason] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /**
   * The key already saved by step one, waiting on step two's model
   * (keys rework #2306, slice 4b). A ref, not state: it must survive step one
   * closing its own form but never outlive the dialog or land in
   * `localStorage` — see `AccountKeyDialog`'s gotchas.
   */
  const pendingKey = useRef<string | null>(null);
  /** Non-null once the host has answered `needsModel` — the dialog's step two. */
  const [modelStep, setModelStep] = useState<AccountKeyModelStep | null>(null);
  /**
   * Invalidates a stale save attempt (round-3b review, P2-4). Bumped whenever
   * the key dialog actually closes or is opened fresh — mirrors
   * `ProvidersTab`'s connect-dialog `attempt` ref exactly. Without it, a Save
   * that is still in flight when Cancel is pressed could have its late
   * `needsModel` answer land after the dialog already closed and silently
   * reopen it on step two the next time it is opened, with a key the operator
   * never chose to see through to step two. Belt-and-suspenders alongside
   * `closeKeyDialog`'s own busy guard below, which is what stops the race from
   * `Cancel` in the first place — this still protects against a stale result
   * from any other close path.
   */
  const attempt = useRef(0);

  // Discards the result of a request that is no longer the latest one asked
  // for — a monotonic counter rather than "is this still the wanted company",
  // because a company can stay the same while `client` is reseated to another
  // host (the retired company-credential card carried the same guard):
  // comparing only `company` would let the old host's slower response land
  // last and overwrite the new host's credential status and balance.
  const requestGeneration = useRef(0);

  const refresh = useCallback(async () => {
    setLoad("loading");
    const asked = ++requestGeneration.current;
    try {
      // Together: the page draws one story out of both, and sequencing them
      // would show a connected company an empty wallet for a frame.
      const [credential, money] = await Promise.all([
        getCompanyCredential(client, company),
        getCompanyBilling(client, company).catch(
          (): CompanyBilling => ({
            // A rejected billing request is not the same fact as "no key". The
            // credential result (just resolved above, in the same batch) is
            // what actually says whether a key exists; a company that has one
            // must keep seeing the unavailable explanation rather than have
            // the balance row silently vanish. `configured` is corrected
            // against the credential result once both have settled below.
            configured: false,
            unavailable: "The balance could not be read just now.",
            // The request never reached the hub's verdict, so nothing here
            // knows the key's standing — and a thrown error carries whatever
            // text some layer chose to put in it, which is not a sentence this
            // page may put under somebody's balance.
            unavailableReason: "unknown",
          }),
        ),
      ]);
      if (asked !== requestGeneration.current) return;
      setStatus(credential);
      setBilling(
        money.unavailable !== undefined
          ? { ...money, configured: credential.configured }
          : money,
      );
      setLoad("ready");
    } catch {
      if (asked !== requestGeneration.current) return;
      // Not "no key". The credential route surfaces an unreadable secret store
      // as a 5xx precisely so this case exists to be told apart, and the row
      // below says so in words rather than showing an empty state that would
      // send an admin to set a key they may already have set.
      //
      // Both are dropped, not left standing. A balance from the last good read
      // under a row that has just admitted it does not know whose account this
      // is would be the most confident thing on the page and the least earned.
      setStatus(null);
      setBilling(null);
      setLoad("error");
    }
  }, [client, company]);

  useEffect(() => {
    void refresh();
  }, [refresh, generation]);

  // Redeems a returning key grant (`POST …/credential/link/finish`). Called
  // **unconditionally**, before anything the credential read decides: the
  // grant arrives on a fresh boot with the code already stripped from the
  // address bar and held in a module-local box that a reload empties, so
  // gating this on state that is null while the read is in flight — or stays
  // null when it fails — would drop the credential with no way back to it.
  // The page no longer shows a sign-in button (operator request, 2026-09-14);
  // a grant started elsewhere still lands here.
  //
  // A grant stores a key this page never saw, so when the host answers
  // `needsModel` the model step opens with NO pending key: `writeModel` then
  // completes the row through `PUT …/credential/model`, off the stored key.
  // Without this the toast said "choose a model" and every path on from it
  // asked for the key again.
  const onGrantConnected = useCallback((result: CompanyCredentialMutation) => {
    setGeneration((n) => n + 1);
    if (result.needsModel === true) {
      pendingKey.current = null;
      setModelStep({
        models: result.models ?? [],
        setsDefault: result.setsDefault ?? false,
        note: result.note,
      });
      setEditing(true);
    }
  }, []);
  useRedeemKeyGrant(client, company, onGrantConnected);
  // The other end of the same flow: the dialog's "Connect with TinyHumans".
  // In a browser it navigates away and the hook above redeems on return; in
  // the desktop the host redeems on its own route and this resolves when the
  // key lands, re-reading the page and closing the dialog.
  const onGrantLanded = useCallback(() => {
    setGeneration((n) => n + 1);
    setEditing(false);
  }, []);
  const grant = useStartKeyGrant(client, company, onGrantLanded);

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

  /**
   * KR-ACCT-01: performs the same restart the LLM page's own "Restart now"
   * button does (`POST …/inference/restart`, `@/api/inference`'s
   * `restartInference`) — a save here can create or complete the
   * `tinyhumans` row for a company that already booted, and only a restart
   * puts the new config to work. Offered as a toast action rather than a
   * standing banner: the Account page has no persistent "restart required"
   * state of its own to render one from (unlike `ProvidersTab`'s
   * `state.status.restartRequired`) — `restartRequired` here is a one-shot
   * fact about the write that just landed, not a fact this page keeps
   * polling for.
   */
  const doRestart = useCallback(async () => {
    try {
      await restartInference(client, company);
      toast.success("Restarted.");
      setGeneration((n) => n + 1);
    } catch (err) {
      toast.error(err instanceof ApiError ? err.message : "Couldn't restart.");
    }
  }, [client, company]);

  /**
   * Set, rotate or (with an empty value) clear the company's key.
   *
   * `confirmInUse` matters only on a clear (`mode === "clear"`): it is what
   * `confirmInUseFor(removeReason)` decided at the moment of THIS click —
   * `true` once the dialog has actually shown a reason, from the status read
   * at open time or from a prior refusal, `false` on an uninformed first
   * attempt. A save/rotate never sends it (KR-L3-01; the fix does not touch
   * the never-guarded set/rotate path).
   */
  const write = useCallback(
    async (key: string, mode: "save" | "clear", confirmInUse = false) => {
      const myAttempt = attempt.current;
      setBusy(true);
      setKeyError(null);
      try {
        const result = await setCompanyCredential(client, company, key, undefined, confirmInUse);
        // The fan-out (keys rework #2306, slice 4a) could not create a
        // `tinyhumans` row for want of a model. The key itself already saved
        // — `setGeneration` reflects that on the page underneath — but the
        // dialog stays open on step two rather than closing on a save that is
        // only half done.
        if (mode === "save" && result.needsModel === true) {
          // Round-3b review, P2-4: if Cancel landed while this request was in
          // flight (bumping `attempt`), this answer is stale — the row still
          // refreshes (the key really did save), but nothing here may reopen
          // the dialog on step two behind a Cancel the operator already
          // pressed.
          if (myAttempt !== attempt.current) {
            setGeneration((n) => n + 1);
            return;
          }
          pendingKey.current = key;
          setModelStep({
            models: result.models ?? [],
            setsDefault: result.setsDefault ?? false,
            note: result.note,
          });
          setGeneration((n) => n + 1);
          return;
        }
        // Unconditional: the write really did land, and an admin who navigated
        // away mid-request is still owed that fact.
        //
        // KR-ACCT-01: `result.note` can say things like "…is now the default
        // for new work", true of the *saved configuration* but not of what
        // agents are currently running on — a bare "Key saved." beside that
        // note reads as "and it's live now." `restartRequired` is what tells
        // the two apart, so the headline names the restart explicitly and the
        // toast carries the same "Restart now" action the LLM page offers,
        // rather than leaving the operator to notice the gap on their own.
        const wantsRestart = mode === "save" && result.restartRequired === true;
        toast.success(
          mode === "save"
            ? wantsRestart
              ? "Key saved — restart required to use it."
              : "Key saved."
            : "Key removed.",
          {
            description: result.note,
            action: wantsRestart
              ? { label: "Restart now", onClick: () => void doRestart() }
              : undefined,
          },
        );
        setEditing(false);
        setRemoving(false);
        setRemoveReason(null);
        setGeneration((n) => n + 1);
      } catch (err) {
        // The host's own reason where it sent one — an admin-only refusal or a
        // store failure says something specific, and a generic "couldn't save"
        // throws away the only actionable part. A failed save stays in the
        // dialog, beside the key that was refused; a failed removal has no
        // dialog left open to hold it, UNLESS it is a stale-UI `409 in_use`
        // that this attempt had not yet confirmed — that reopens the dialog
        // with the host's own reason instead (KR-L3-01), the same recovery
        // Composio's and Search's own guarded dialogs already give every
        // other in-use mutation on this host.
        if (mode === "save") {
          // Same staleness guard as the needsModel branch above: a Cancel that
          // landed first must not have a late refusal paint an error into a
          // dialog that has since closed (and been reopened fresh — its own
          // `openKeyDialog` already clears `keyError`, but only at the moment
          // it opens, not for whatever lands after).
          if (myAttempt === attempt.current) {
            setKeyError(err instanceof ApiError ? err.message : "Couldn't save the key.");
          }
        } else {
          const outcome = guardedOutcome(err, confirmInUse);
          if (outcome.action === "reopen") {
            setRemoveReason(outcome.message);
          } else {
            setRemoving(false);
            setRemoveReason(null);
            toast.error(err instanceof ApiError ? err.message : "Couldn't remove the key.");
          }
        }
      } finally {
        setBusy(false);
      }
    },
    [client, company, doRestart],
  );

  /**
   * Step two: save the model against the key step one already stored — or,
   * with no pending key (a redeemed grant, whose key this page never held),
   * against the key the host stores, through `PUT …/credential/model`. A
   * stray call once the dialog has closed and forgotten both its pending key
   * and its model step must not post anything at all.
   */
  const writeModel = useCallback(
    async (model: string) => {
      const key = pendingKey.current;
      if (!key && !modelStep) {
        setModelStep(null);
        setEditing(false);
        return;
      }
      setBusy(true);
      setKeyError(null);
      try {
        const result = key
          ? await setCompanyCredential(client, company, key, model)
          : await setCompanyCredentialModel(client, company, model);
        const modelWriteFailed = result.slots?.some(
          (slot) =>
            (slot.slot === "provider" || slot.slot === "default") &&
            (slot.outcome === "failed" || slot.detail === "inferenceRejected"),
        );
        if (modelWriteFailed) {
          setKeyError("The key was saved, but its model could not be applied. Please try again.");
          return;
        }
        // KR-ACCT-01: the same restart-honesty fix as `write`'s own success
        // toast, above — step two is the save that actually completes the
        // `tinyhumans` row with a model, so it is at least as likely as step
        // one to need a restart before anything runs on it.
        const wantsRestart = result.restartRequired === true;
        toast.success(wantsRestart ? "Key saved — restart required to use it." : "Key saved.", {
          description: result.note,
          action: wantsRestart
            ? { label: "Restart now", onClick: () => void doRestart() }
            : undefined,
        });
        pendingKey.current = null;
        setModelStep(null);
        setEditing(false);
        setGeneration((n) => n + 1);
      } catch (err) {
        // Shown in the dialog's step two, not as a toast — the operator is
        // still looking at the model they picked when this fails.
        setKeyError(err instanceof ApiError ? err.message : "Couldn't save the model.");
      } finally {
        setBusy(false);
      }
    },
    [client, company, doRestart, modelStep],
  );

  /**
   * The dialog's own `onOpenChange`. Any close forgets the pending key and
   * step two's state — reopening must start over at step one, never resume a
   * half-finished model save against a key nobody can see any more.
   *
   * Ignores a close while `busy` (round-3b review, P2-4) — the same guard
   * `ProviderConnectDialog` uses on the LLM page: `AccountKeyDialog`'s Dialog
   * only ever calls `onOpenChange(false)` from Escape, a backdrop click, or
   * its own Cancel button (which this page also disables while busy, but
   * Escape and the backdrop go straight through this callback regardless of
   * any button's `disabled`). Refusing the close here is what actually stops
   * a save in flight from being cancelled out from under itself — the
   * `attempt` counter above is the backstop for whatever this guard does not
   * catch, not the primary fix.
   */
  const closeKeyDialog = useCallback(
    (next: boolean) => {
      if (next || busy) return;
      attempt.current += 1;
      setEditing(false);
      pendingKey.current = null;
      setModelStep(null);
    },
    [busy],
  );

  /**
   * Opens the Remove-key dialog, seeded with whatever the page already knows
   * about who depends on the key — `status.usedBy`, read alongside the row
   * this dialog opens from, at no extra request (KR-L3-01, choice (a) in the
   * dispatch: the field is a small, clean addition to the existing status DTO
   * rather than a reliance on the 409-only recovery every other guarded
   * dialog on this host uses). A stale answer (something starts depending on
   * the key between this open and the confirm click) is still caught by
   * `write`'s own reopen-on-409 handling below.
   */
  const openRemoveDialog = useCallback(() => {
    setRemoveReason(accountKeyUsedByMessage(status?.usedBy));
    setRemoving(true);
  }, [status]);

  /** The dialog's own `onOpenChange` — closing (never while busy) forgets the reason. */
  const closeRemoveDialog = useCallback(
    (next: boolean) => {
      if (next || busy) return;
      setRemoving(false);
      setRemoveReason(null);
    },
    [busy],
  );

  /** Confirm click: sends `confirmInUse` exactly when the dialog is already showing a reason. */
  const confirmRemoveKey = useCallback(() => {
    void write("", "clear", confirmInUseFor(removeReason));
  }, [write, removeReason]);

  const shape = accountShape(load, status, billing);
  const verdict = keyVerdict(status, billing);
  const removable = canRemoveKey(status);
  const actions = headerActions(status, canManage);
  const openKeyDialog = () => {
    // Also bumps `attempt`: opening fresh (the row menu and header button are
    // both disabled while `busy`, so this only ever runs once any earlier
    // save has actually settled) must not let that earlier attempt's result
    // land in this new, unrelated open.
    attempt.current += 1;
    setKeyError(null);
    setEditing(true);
  };
  const balance = balanceLine(billing);
  const account = status?.account;
  // The hub's own top-up address where it sent one; the host-derived link only
  // while nothing has said the key is refused. A refused key does not start
  // working because the account behind it has more money in it, and a Top up
  // beside "replace it to reconnect" offers the wrong repair.
  const topUpUrl =
    billing?.summary?.topUpUrl ?? (verdict === "rejected" ? undefined : account?.topUpUrl);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* The console's one page header (#1763) rather than a hand-rolled `h1`:
          a routed view that titles itself is how twelve heading styles happened
          the first time, and `page-header-adoption` is the test that says so. */}
      <PageHeader
        title="Account"
        width="full"
        description="The TinyHumans account this company acts and spends through."
      />

      <div className="min-h-0 w-full flex-1 space-y-6 overflow-y-auto px-4 py-6">
        {/* The action, and the one sentence the action does not itself say: that
            a single key covers both halves. Everything else the old page opened
            with — what write-only means, what Clear does, what happens at zero —
            described a control that was visible while it was being read. */}
        <Card>
          <CardContent className="flex flex-wrap items-center justify-between gap-3">
            <div className="grid gap-0.5">
              <h2 className="text-sm font-medium">{ACCOUNT_LABEL}</h2>
              {/* The billing consequence, on the card carrying the button it is
                  true of, and visible before anything is saved. Connecting
                  stores the identity, and managed turns resolve through this
                  same key (#2266) — so it moves the thinking bill as well.

                  Q7 (keys rework #2306): saving never overwrites a key set on
                  the LLM or Composio page's own — the fan-out fills only the
                  slots that are empty or still equal to this account key.
                  Where either page already holds a key of its own it keeps
                  answering, and saving moves the other slots without moving
                  that one. The dialog's own conditional line
                  (`account-fill.ts`) says exactly which slots this save would
                  fill; this sentence states the general rule. */}
              <p className="text-xs text-muted-foreground">
                One key for the apps your agents act through and the models they think with.
                Saving copies it to the LLM, Composio, and Search pages wherever they hold no key of their
                own.
              </p>
            </div>
            {/* One way to connect: the API-key dialog. The "Sign in with
                TinyHumans" option was removed at the operator's request
                (2026-09-14). Not shown once this company has a key of its own —
                the row below carries Replace and Remove. Decided in
                `headerActions`. A returning grant is still redeemed by the
                unconditional `useRedeemKeyGrant` call above. */}
            <div className="flex flex-wrap items-center gap-2">
              {actions.key && (
                <Button type="button" onClick={openKeyDialog} data-testid="account-add-key">
                  <KeyRound className="size-4" />
                  Connect to TinyHumans
                </Button>
              )}
            </div>
          </CardContent>
        </Card>

        {/* The state. One row for the account, one for what is left on it. */}
        <Card>
          <CardContent className="px-0">
            {/* Named for the state the row is actually in, never for one it
                might be in: a heading reading "Connected" over "No account
                connected yet.", over "The host could not say", or over a key
                the hub has refused contradicts the only line under it. */}
            <h3
              className={cn(
                "px-4 pb-2 text-xs font-medium tracking-wide uppercase",
                shape === "rejected" ? "text-status-blocked-text" : "text-muted-foreground",
              )}
              data-testid="account-card-state"
            >
              {shape === "connected"
                ? "Connected"
                : shape === "rejected"
                  ? "Action needed"
                  : "Account"}
            </h3>

            {load === "loading" ? (
              <div className="px-4 pb-2">
                <Skeleton className="h-10 rounded-md" />
              </div>
            ) : shape === "empty" ? (
              // Nothing resolves anywhere. Not a row: there is no account to
              // describe, and a row saying so would be a heading over blank space.
              <div
                className="flex flex-col items-start gap-3 px-4 py-6"
                data-testid="account-empty"
              >
                {/* Scoped to what this credential actually governs. The old page
                    said "agents cannot think and no provider can be connected"
                    here, which is false on a company whose LLM page holds a
                    provider key of its own — `inference/key` resolves without
                    this one, so such a company thinks perfectly well and would
                    be sent to fix something that is not broken. The exception is
                    named rather than denied. */}
                <p className="text-sm">
                  <span className="font-medium">No account connected yet.</span>{" "}
                  <span className="text-muted-foreground">
                    Apps cannot be connected, and there is no TinyHumans balance to think
                    against — though a provider key set on the LLM page still works.
                  </span>
                </p>
                {/* No button here, deliberately, though the list this borrows its
                    shape from has one. The page it replaces carried the warning
                    in its own source: two identical primary buttons on one screen
                    leave a reader working out whether they do the same thing. On
                    a list of providers the header action and the empty-state
                    action are inches apart in a long card; on a page with one
                    credential they are adjacent and identical, and the header
                    card's action is already in view directly above this. The
                    sentence stays — it is what the empty state is for. */}
              </div>
            ) : (
              <ul className="divide-y divide-border" data-testid="account-rows">
                <li className="flex items-center gap-3 px-4 py-3" data-testid="account-row">
                  <Mark label={ACCOUNT_LABEL} />
                  <span className="grid min-w-0 flex-1 leading-tight">
                    <span className="truncate text-sm font-medium">{ACCOUNT_LABEL}</span>
                    <span
                      className="truncate text-xs text-muted-foreground"
                      data-testid="account-row-subline"
                    >
                      {accountSubline(load, status, billing)}
                    </span>
                  </span>

                  {/* Revoking a key is not something this console can do — it
                      ends an instance's access and lives behind that person's own
                      sign-in — so it is a link out, and it sits on the row it is
                      about rather than in a footer. */}
                  {account && (
                    <a
                      className="inline-flex items-center gap-1 text-xs font-medium underline underline-offset-4"
                      href={account.manageKeysUrl}
                      target="_blank"
                      rel="noreferrer"
                      data-testid="hub-manage-keys"
                    >
                      Manage keys <ExternalLink className="size-3" />
                    </a>
                  )}

                  <DropdownMenu>
                    <DropdownMenuTrigger
                      render={
                        <Button
                          variant="ghost"
                          size="icon"
                          disabled={!canManage || busy || shape === "unknown"}
                          aria-label={`${ACCOUNT_LABEL} actions`}
                          data-testid="account-row-menu"
                        />
                      }
                    >
                      <EllipsisVertical className="size-4" />
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem onClick={openKeyDialog}>
                        {removable ? "Replace key" : "Add a key"}
                      </DropdownMenuItem>
                      {/* Offered only when there is a key of this row's own to
                          remove. The instance's identity is not this row's to
                          take away, and a Remove that clears nothing is the
                          control-that-cannot-act the LLM page deleted a toggle
                          over. */}
                      {/* Opens the confirmation rather than clearing on the
                          press. Clearing is destructive, irreversible from this
                          console — the hub emits a key's plaintext once — and
                          what it costs depends on state the menu item cannot
                          show. A menu item that silently revokes a company's
                          identity is the shape of mistake that cost this repo a
                          live key today. */}
                      {removable && (
                        <DropdownMenuItem
                          variant="destructive"
                          onClick={openRemoveDialog}
                          data-testid="account-remove-key"
                        >
                          Remove key
                        </DropdownMenuItem>
                      )}
                    </DropdownMenuContent>
                  </DropdownMenu>
                </li>

                {/* Only once there is an account of this company's own: a row
                    reading "$0.00" for a company with no wallet would be a
                    made-up fact. */}
                {balance && (
                  <li className="flex items-center gap-3 px-4 py-3" data-testid="account-balance">
                    <Mark icon={<Wallet className="size-4" />} label="Balance" />
                    <span className="grid min-w-0 flex-1 leading-tight">
                      <span
                        className={cn(
                          "truncate text-sm font-medium tabular-nums",
                          balance.low && "text-status-blocked-text",
                        )}
                        data-testid="billing-balance"
                      >
                        {balance.amount ?? "Balance unknown"}
                      </span>
                      <span className="truncate text-xs text-muted-foreground">
                        {balance.detail}
                      </span>
                    </span>

                    {billing?.summary?.manageUrl && (
                      <a
                        className="inline-flex items-center gap-1 text-xs font-medium underline underline-offset-4"
                        href={billing.summary.manageUrl}
                        target="_blank"
                        rel="noreferrer"
                        data-testid="billing-manage-plan"
                      >
                        Manage plan <ExternalLink className="size-3" />
                      </a>
                    )}

                    {/* Moving money is a decision made signed in on the hub, so
                        this is a link and never a route here. */}
                    {topUpUrl && (
                      <a
                        className="inline-flex items-center gap-1 text-xs font-medium underline underline-offset-4"
                        href={topUpUrl}
                        target="_blank"
                        rel="noreferrer"
                        data-testid="billing-top-up"
                      >
                        <CreditCard className="size-3" /> Top up <ExternalLink className="size-3" />
                      </a>
                    )}
                  </li>
                )}
              </ul>
            )}
          </CardContent>
        </Card>

        <AccountKeyDialog
          open={editing}
          onOpenChange={closeKeyDialog}
          replacing={removable}
          busy={busy}
          error={keyError}
          onSubmit={(key) => void write(key, "save")}
          fills={accountFills(status)}
          modelStep={modelStep}
          onSubmitModel={(model) => void writeModel(model)}
          client={client}
          company={company}
          onConnect={status?.hubLink && canManage ? grant.start : null}
          connecting={grant.starting}
          keysUrl={status?.account?.manageKeysUrl ?? null}
        />

        {/* Names what actually depends on the key, and what happens next rather
            than only what is lost. The generic two sentences live in
            `account.ts` with a test each: they are the page's one
            irreversible claim, and the reasoning behind each half — why both
            fallbacks are offered rather than one guessed at, and why the
            removal is not allowed to promise that the billing stops —
            belongs next to the assertion that holds it.

            `removeReason`, above them, is KR-L3-01's fix: the one sentence
            naming who actually depends on the key right now — Composio, the
            LLM page's TinyHumans row, or both — from `status.usedBy` at open
            time, or from the host's own `409 in_use` message if a stale
            attempt gets refused. Nothing before this dispatch ever told the
            operator that; the dialog only ever showed the two generic
            sentences below, whatever actually used the key. */}
        <AlertDialog open={removing} onOpenChange={closeRemoveDialog}>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Remove this company&apos;s account key?</AlertDialogTitle>
              {removeReason && (
                <AlertDialogDescription data-testid="account-remove-key-reason">
                  {removeReason}
                </AlertDialogDescription>
              )}
              <AlertDialogDescription>{REMOVAL_CONSEQUENCE}</AlertDialogDescription>
              <AlertDialogDescription>{REMOVAL_AND_THINKING}</AlertDialogDescription>
              <AlertDialogDescription>
                The key itself cannot be recovered from here — TinyHumans shows a key&apos;s value
                once, when it is created. You would have to connect again or paste a new one.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel disabled={busy}>Keep the key</AlertDialogCancel>
              <AlertDialogAction
                disabled={busy}
                onClick={(event) => {
                  // Keep the dialog open on a stale-UI 409 so it can reopen
                  // with the host's own reason — see `AlertDialogAction`'s own
                  // docs, and `@/composio/in-use`'s identical use of this
                  // escape hatch. `write` closes the dialog itself on success
                  // or on an unrelated failure.
                  event.preventBaseUIHandler();
                  confirmRemoveKey();
                }}
                className="bg-destructive text-white hover:bg-destructive/90"
                data-testid="account-remove-key-confirm"
              >
                Remove key
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      </div>
    </div>
  );
}

/**
 * The row's mark: two letters, or an icon where the row is a figure rather than
 * a name.
 *
 * Local and deliberately plain. It is here to give the rows a consistent left
 * edge — the thing that makes two rows read as one list — not to carry
 * information, so it takes no colour of its own and says nothing a screen
 * reader needs to hear.
 */
function Mark({ label, icon }: { label: string; icon?: ReactNode }) {
  return (
    <span
      aria-hidden="true"
      className="flex size-8 shrink-0 items-center justify-center rounded-md bg-muted text-xs font-medium text-muted-foreground"
    >
      {icon ?? label.slice(0, 2).toUpperCase()}
    </span>
  );
}
