import { useCallback, useEffect, useRef, useState } from "react";
import { Plug } from "lucide-react";
import { toast } from "sonner";

import type { OpenCompanyClient } from "@/api/client";
import {
  copyAccountKeyToComposio,
  getComposioStatus,
  setComposioApiKey,
  setComposioToken,
  testComposioApiKey,
  type ComposioMutation,
  type ComposioStatus,
} from "@/api/composio";
import { getCompanyCredential } from "@/api/credential";
import { ApiError } from "@/api/types";
import { advisoryMessage, verdictMessage } from "@/composio/classify";
import type { ComposioSubmitOutcome } from "@/composio/classify";
import { ComposioRowList } from "@/composio/ComposioRowList";
import { guardedOutcome } from "@/composio/in-use";
import { ProbeAdvisory } from "@/composio/ProbeAdvisory";
// Shared with the LLM page (round-3b review, item 6) — moved out of
// `@/composio/**`, which held it alone until now.
import { ReuseAccountKeyBanner } from "@/inference/ReuseAccountKeyBanner";
import {
  readDismissed,
  reuseDismissKey,
  showsComposioReuseBanner,
  writeDismissed,
} from "@/inference/reuse-banner";
import { composioForm, composioRows, managedSourceOf, modeOf } from "@/composio/rows";
import type {
  ComposioPending,
  ComposioRow,
  ComposioRowId,
} from "@/composio/types";
import { grantStanding } from "@/lib/provider-grid";
import { classifyLoadFailure } from "@/lib/section-load";
import { ComposioCredentialDialog } from "@/views/connections/ComposioCredentialDialog";
import { SectionUnreachable } from "@/views/connections/SectionUnreachable";
import { GrantNamespace } from "@/components/grant-namespace";
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
import { Card, CardContent } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";

interface Props {
  client: OpenCompanyClient;
  company: string | null;
  /**
   * Whether this viewer may change what the company connects through (issue
   * #403) — the credential its agents present, and which provider accounts
   * they act through.
   *
   * **Courtesy, not enforcement.** The host refuses both writes with a 403
   * whatever this says. What it prevents is offering a credential field whose
   * Save is refused only after the operator has already pasted a live secret
   * into it.
   */
  canManage: boolean;
  /**
   * Called after the stored credential changes.
   *
   * The provider grid's status and routing are downstream of which credential
   * this company reaches Composio with — setting or clearing one here flips
   * `credentialSource`, and every tile's route with it. Without this the grid
   * would keep rendering the old answer while this section reported the new
   * one: the same two-surfaces-disagreeing failure #582 is about, arriving
   * through the credential rather than through the connection list.
   */
  onChanged: () => void;
}

/**
 * Which account this company reaches Composio through (issue #110, Cell D).
 *
 * # The shape, and why it changed
 *
 * A **Connected card of rows** — a mark, a name, one sub-line, controls on the
 * right — matching the reworked LLM page. What it replaces was two large tiles
 * under a four-branch paragraph that explained what each route meant, what
 * saving would change, and where the connected providers went; almost every
 * clause of it explained a control that was visible while it was being read.
 *
 * The one thing not carried over from the inference rows is the **per-row
 * toggle**. Those model providers that *coexist*; Composio's two modes are one
 * stored scalar and `resolve_access` reads exactly one branch, so a toggle per
 * row would make both-on and both-off reachable with nowhere to put them. The
 * rows are single-select instead: `composioRows` states the argument in full.
 *
 * # Where the decisions live
 *
 * Not here. `@/composio/rows` decides what each row says and which controls it
 * may offer; `@/composio/classify` decides what a failed check is called. This
 * component is layout and handlers, and everything it renders is a function of
 * those two.
 *
 * # The credential tiers, which the rows report rather than hide
 *
 * - `attested` (hosted) — the instance holds a platform identity, so there is
 *   nothing to paste and nothing stored.
 * - `company` (issue #586) — this company's own TinyHumans credential, set by
 *   its admin, brokering Composio.
 * - `static` — a Composio token this company pasted, or a static instance key.
 * - `none` — no credential can be obtained, so agents get no Composio tools.
 *
 * The managed row's sub-line is driven by `managedCredentialSource`, never by
 * "did somebody paste a token": that boolean is issue #886, it answers only
 * about the first of three tiers, and it is routinely false on a working hosted
 * tenant.
 *
 * Every credential here is WRITE-ONLY: stored and never shown again. A set or
 * clear takes effect on the agents' next turn, no restart. Hidden entirely when
 * the feature is not in the build.
 */
export function ComposioSection({
  client,
  company,
  canManage,
  onChanged,
}: Props) {
  const [load, setLoad] = useState<
    "loading" | "ready" | "unavailable" | "unconfigured" | "error"
  >("loading");
  const [status, setStatus] = useState<ComposioStatus | null>(null);
  const [busy, setBusy] = useState(false);
  // The credential form an operator asked for, as an intent rather than a
  // rendered state. `composioForm` re-checks it against the rows on every
  // render, so a form left standing by a status that moved underneath it — a
  // refresh, another admin — simply stops being returned.
  const [pending, setPending] = useState<ComposioPending | null>(null);
  const [secret, setSecret] = useState("");
  // What the last attempt at storing a credential came back as. Two shapes, not
  // a boolean: an advisory KEPT the key and a rejection stored nothing, and the
  // page must not colour a successful save red.
  const [outcome, setOutcome] = useState<ComposioSubmitOutcome | null>(null);
  // The managed → BYOK confirmation. It renders inside the credential dialog,
  // in place of that dialog's footer, rather than as a second modal over it:
  // what it warns about — every provider connected through the managed route
  // becoming invisible — is about the key in the field above it, and a second
  // overlay would hide the thing being decided about.
  const [confirmSwitch, setConfirmSwitch] = useState(false);
  // The check's verdict, kept apart from `outcome` on purpose. A check writes
  // nothing, so it must not reach `offersSkipVerify` — "add anyway" answers a
  // refused *write*, and offering it after a failed check would propose storing
  // a key that is already stored.
  const [testOutcome, setTestOutcome] = useState<ComposioSubmitOutcome | null>(
    null,
  );
  // Which row's check is in flight. Not folded into `busy`: `busy` disables the
  // controls that write, and a check changes nothing.
  const [testingRow, setTestingRow] = useState<ComposioRowId | null>(null);

  // ── In-use confirm dialogs (keys rework, issue #2306) ──────────────
  //
  // Two guarded, destructive row actions with no credential form of their
  // own to render a warning inside: clearing the managed token, and giving
  // the managed route back (the byok → managed switch — `useManaged`, which
  // used to write immediately with no confirmation at all). Each gets its
  // own `AlertDialog`, matching the pattern `ApiKeyView`'s account-key
  // removal already established.
  //
  // The managed → byok direction (`confirmSwitch` above) keeps its own
  // pre-existing inline confirmation inside the credential dialog rather than
  // being folded into this shape: it already gates that switch behind an
  // explicit click with fixed, load-bearing-accessible copy, so `submit`
  // below always sends `confirmInUse: true` for it — see `submit`'s comment.
  //
  // Each is `undefined` while the dialog is closed, `null` once open — showing
  // usage up front when `status.mode` already says this key is in use
  // (round-3 review, P1-2: re-read on open, see `requestClearManagedToken`/
  // `requestGiveBackManaged`) — and a string once a first, unconfirmed
  // attempt comes back `409 in_use` anyway (a stale read) — the host's own
  // sentence, shown in place of the generic question so a SECOND click can
  // resend with `confirmInUse: true` (`@/composio/in-use`, `guardedOutcome`).
  const [clearTokenPrompt, setClearTokenPrompt] = useState<
    string | null | undefined
  >(undefined);
  const [giveBackManagedPrompt, setGiveBackManagedPrompt] = useState<
    string | null | undefined
  >(undefined);

  // ── Reuse-the-account-key banner (keys rework, issue #2306, slice 4c) ──
  //
  // Whether this company has a TinyHumans account key at all
  // (`GET …/credential`'s existing `configured`) — read alongside the
  // Composio status in `refresh` below, best-effort: a failure here must not
  // affect the section's own load state, only hide the banner.
  const [accountConfigured, setAccountConfigured] = useState(false);
  // Whether the operator already said "Not now" for this company. Seeded from
  // `localStorage` and re-seeded whenever `company` changes, in the same
  // reset effect that clears every other per-company field below.
  const [reuseDismissed, setReuseDismissed] = useState(() =>
    readDismissed(reuseDismissKey("composio", company)),
  );
  const [reuseBusy, setReuseBusy] = useState(false);

  const requestGeneration = useRef(0);

  const refresh = useCallback(async () => {
    const generation = ++requestGeneration.current;
    try {
      const s = await getComposioStatus(client, company);
      if (generation !== requestGeneration.current) return;
      setStatus(s);
      // Hide the whole section when the feature is not compiled into this build.
      setLoad(s.inBuild ? "ready" : "unavailable");
    } catch (err) {
      if (generation !== requestGeneration.current) return;
      // A 404 is a host with no Composio surface — hide it. Anything else is a
      // host that could not answer; keep the section rather than vanishing
      // (issue #1470).
      setLoad(classifyLoadFailure(err));
    }
    // Best-effort, and deliberately its own try/catch: the reuse banner is a
    // courtesy, not core status, so a failed read here (or a host predating
    // `/credential`) must only hide the banner, never the section above.
    try {
      const credential = await getCompanyCredential(client, company);
      if (generation !== requestGeneration.current) return;
      setAccountConfigured(credential.configured);
    } catch {
      if (generation !== requestGeneration.current) return;
      setAccountConfigured(false);
    }
  }, [client, company]);

  useEffect(() => {
    setStatus(null);
    setPending(null);
    setSecret("");
    setOutcome(null);
    setConfirmSwitch(false);
    setClearTokenPrompt(undefined);
    setGiveBackManagedPrompt(undefined);
    setAccountConfigured(false);
    setReuseDismissed(readDismissed(reuseDismissKey("composio", company)));
    setLoad("loading");
    void refresh();
  }, [refresh, company]);

  const rows = composioRows(status);
  const form = composioForm(pending, rows);
  const persistedMode = modeOf(status);
  const showsReuseBanner = showsComposioReuseBanner({
    canManage,
    accountConfigured,
    mode: status?.mode,
    managedCredentialSource: managedSourceOf(status),
    dismissed: reuseDismissed,
  });

  /** "Yes" on the reuse banner: a single-slot copy, never the full fan-out. */
  async function reuseAccountKey() {
    setReuseBusy(true);
    try {
      const res = await copyAccountKeyToComposio(client, company);
      setStatus(res.status);
      toast.success(res.note);
      onChanged();
    } catch (err) {
      toast.error(
        err instanceof ApiError
          ? err.message
          : "Could not copy the account key to Composio.",
      );
    } finally {
      setReuseBusy(false);
    }
  }

  /** "Not now": remembered per company, so it stays hidden after a reload. */
  function dismissReuseBanner() {
    writeDismissed(reuseDismissKey("composio", company));
    setReuseDismissed(true);
  }

  /**
   * Land a mutation's answer.
   *
   * A response can carry an advisory even though it succeeded — the key was
   * stored and only the check failed — so "did it throw" is not enough to
   * decide what the page says next. The dialog closes either way, because the
   * credential is written.
   *
   * **The advisory is toasted, not merely set.** `onChanged()` at the bottom of
   * this function bumps the generation `ComposioView` keys this section on, and
   * a changed `key` is an unmount — so the `outcome` set three lines earlier is
   * thrown away before it can paint. That remount is deliberate (issue #586:
   * the tier this section reports is downstream of the key just written), and
   * the clean branch survived it only because a toast lives outside the tree
   * that remounts. The advisory branch had no toast, so the one case an
   * operator must not be left guessing about — the key IS stored, the check did
   * not pass — said nothing at all. The inline `outcome` is kept for the paths
   * that do not remount; the toast is what makes this one reach anybody.
   */
  function settle(res: ComposioMutation) {
    setStatus(res.status);
    setSecret("");
    setPending(null);
    setConfirmSwitch(false);
    if (res.probeClass || res.advisory) {
      const message = advisoryMessage(res.probeClass, res.advisory);
      setOutcome({ kind: "advisory", probeClass: res.probeClass, message });
      // Amber, not red: the write landed. `toast.error` here would report a
      // stored credential as a failure, which is the miscolouring the two
      // outcome shapes exist to prevent.
      toast.warning(message);
    } else {
      setOutcome(null);
      toast.success(res.note);
    }
    onChanged();
  }

  /** Land a refusal. The credential was not stored, so the form stays open. */
  function reject(err: unknown, fallback: string) {
    setOutcome({
      kind: "rejected",
      status: err instanceof ApiError ? err.status : undefined,
      // Carried so `offersSkipVerify` can tell the probe's own refusal apart
      // from every other 400-or-worse — see it for the cases that matter.
      code: err instanceof ApiError ? err.code : undefined,
      fromHost: err instanceof ApiError ? err.fromHost : false,
      message: err instanceof ApiError ? err.message : fallback,
    });
  }

  async function run(call: () => Promise<ComposioMutation>, fallback: string) {
    setBusy(true);
    // Cleared on every attempt, so an "add anyway" offered after one failure
    // does not survive a retry that failed for an unrelated reason.
    setOutcome(null);
    try {
      settle(await call());
    } catch (err) {
      reject(err, fallback);
    } finally {
      setBusy(false);
    }
  }

  /**
   * Run a row action the host may refuse `409 in_use` on a first,
   * uninformed attempt (in-use-guards.md §2) — clearing the managed token, or
   * giving the managed route back. One implementation shared by both, so the
   * "reopen with the host's reason, then resend confirmed" state machine
   * cannot drift between the two dialogs; the decisions themselves live in
   * `@/composio/in-use`, which is what is actually under test.
   *
   * `confirmInUse` (round-3 review, P1-2) is the caller's own computed value —
   * true once `status.mode` already said this key is in use (shown in the
   * dialog before any click), OR once a prior refusal on this same open
   * dialog already said so — never sent blind. `prompt` is the dialog's own
   * state at the moment of THIS click — `undefined`/`null` before any
   * refusal, the host's sentence on a retry — and `setPrompt` is how this
   * function reports what the dialog should show next: `undefined` closes it
   * (the write landed, or failed for an ordinary reason reported through
   * `reject` instead), a string reopens it.
   */
  async function runGuarded(
    call: (confirmInUse: boolean) => Promise<ComposioMutation>,
    fallback: string,
    confirmInUse: boolean,
    setPrompt: (next: string | null | undefined) => void,
  ) {
    setBusy(true);
    setOutcome(null);
    try {
      settle(await call(confirmInUse));
      setPrompt(undefined);
    } catch (err) {
      const outcome = guardedOutcome(err, confirmInUse);
      if (outcome.action === "reopen") {
        setPrompt(outcome.message);
        return;
      }
      setPrompt(undefined);
      reject(err, fallback);
    } finally {
      setBusy(false);
    }
  }

  /** Open the confirm dialog for clearing the token stored for the managed route, re-reading status so `status.mode` is fresh (round-3 review, P1-2). */
  function requestClearManagedToken() {
    setClearTokenPrompt(null);
    void refresh();
  }

  /**
   * Clear the Composio token stored for the managed route, falling back to
   * whatever remains.
   *
   * `composioUsesThisKey` (round-3 review, P1-2): `ComposioStatusDto` cannot
   * carry a structured `usedBy` (#886), but the host's own guard rule is
   * exactly `mode == slot` — so this token is in use whenever the persisted
   * mode currently reads `managed`, and that is on the wire already.
   */
  function confirmClearManagedToken() {
    const composioUsesThisKey = status?.mode === "managed";
    void runGuarded(
      (confirmInUse) => setComposioToken(client, company, "", confirmInUse),
      "Could not clear the Composio token.",
      clearTokenPrompt !== null && clearTokenPrompt !== undefined ? true : composioUsesThisKey,
      setClearTokenPrompt,
    );
  }

  /** Open the confirm dialog for giving the managed route back, re-reading status so `status.mode` is fresh (round-3 review, P1-2). */
  function requestGiveBackManaged() {
    setGiveBackManagedPrompt(null);
    void refresh();
  }

  /**
   * Move this company onto the managed route.
   *
   * One call, because on this host the mode is a consequence of the key rather
   * than a separate control: `setComposioApiKey("")` clears the company's own
   * Composio key and the route derived from it in the same write. That is also
   * why the own-account row offers no "Remove key" — it would be this exact
   * call under a second name.
   *
   * `composioUsesThisKey`: the byok key this clears is in use exactly when
   * `status.mode` currently reads `byok` (round-3 review, P1-2).
   */
  function confirmGiveBackManaged() {
    const composioUsesThisKey = status?.mode === "byok";
    void runGuarded(
      (confirmInUse) =>
        setComposioApiKey(client, company, "", false, confirmInUse),
      "Could not move this company to the TinyHumans-managed route.",
      giveBackManagedPrompt !== null && giveBackManagedPrompt !== undefined ? true : composioUsesThisKey,
      setGiveBackManagedPrompt,
    );
  }

  /**
   * Check the credential stored for `row`, in place.
   *
   * Writes nothing, on any path — the host does not either, and this is the
   * console half of the same rule: the page is not refreshed, no status is
   * replaced, and a rejected key is left exactly where it is. `auth` is the one
   * class shown as an error, because it is the one class that is a statement
   * about the key; the rest are amber, since the key is plausibly fine and only
   * the connection is in question.
   */
  async function runTest(row: ComposioRow) {
    setTestingRow(row.id);
    setTestOutcome(null);
    try {
      const verdict = await testComposioApiKey(client, company);
      if (verdict.ok) {
        toast.success(
          `Composio accepted the ${row.keyNoun} stored for ${row.label}.`,
        );
        return;
      }
      const message = verdictMessage(verdict.probeClass, verdict.message);
      setTestOutcome(
        verdict.probeClass === "auth"
          ? { kind: "rejected", message }
          : { kind: "advisory", probeClass: verdict.probeClass, message },
      );
    } catch (err) {
      setTestOutcome({
        kind: "rejected",
        status: err instanceof ApiError ? err.status : undefined,
        message:
          err instanceof ApiError
            ? err.message
            : "Could not check the Composio API key.",
      });
    } finally {
      setTestingRow(null);
    }
  }

  /**
   * Store what is in the field.
   *
   * `skipVerify` is passed only from the "add anyway" affordance, which is
   * offered only after a typed refusal — never as a standing option, and never
   * after an advisory, where the key already landed.
   *
   * The API-key branch always sends `confirmInUse: true`. That is not a
   * blanket opt-out of the guard: it is safe because this function's ONLY
   * caller for that branch is `requestSubmit`, which already routes every
   * switch-shaped save (the first move to BYOK) through `confirmSwitch`'s own
   * warning before this ever runs — so by the time `submit` fires for the
   * api-key credential, either the operator has just confirmed a switch, or
   * the write is not a switch at all (rotating a key on the row that is
   * already active), which the host never guards regardless of the flag. The
   * token branch never sends it: setting or rotating a non-empty token is
   * never guarded either.
   */
  function submit(skipVerify = false) {
    const value = secret.trim();
    if (!form || !value) return;
    if (form.credential === "composio-api-key") {
      void run(
        () => setComposioApiKey(client, company, value, skipVerify, true),
        "Could not save the Composio API key.",
      );
    } else {
      void run(
        () => setComposioToken(client, company, value),
        "Could not save the token.",
      );
    }
  }

  /**
   * A Save that would move this company off the managed route for the first
   * time.
   *
   * Gated on a confirmation because the consequence — the providers connected
   * through the TinyHumans-managed Composio account are in *that* account and
   * vanish from the grid until they are connected again here — is not readable
   * off a row. Rotating a key already in use, and switching back, are not
   * gated: neither strands anything the operator cannot immediately undo.
   */
  function requestSubmit() {
    if (
      form?.credential === "composio-api-key" &&
      persistedMode === "managed"
    ) {
      setConfirmSwitch(true);
      return;
    }
    submit();
  }

  function openForm(row: ComposioRow, action: ComposioPending["action"]) {
    setPending({ row: row.id, action });
    setSecret("");
    setOutcome(null);
    setConfirmSwitch(false);
  }

  /**
   * Close the credential dialog, discarding what was typed into it.
   *
   * Every exit the operator can take runs through here — Cancel, the X,
   * Escape, a click on the backdrop — so none of them leaves a secret in state
   * behind a closed modal, or `confirmSwitch` armed for the next opening.
   *
   * One exit does not, and cannot: the dialog is derived from `composioForm`,
   * so a status that moves underneath it closes the dialog by making that
   * function return `null` (which is the point — see its doc). That path leaves
   * `pending` and `secret` set. It is reachable only from a refresh raised
   * behind the overlay, and the next `openForm` clears both, but this is a
   * discipline the shape does not enforce rather than one it guarantees.
   */
  function closeForm() {
    setPending(null);
    setSecret("");
    setOutcome(null);
    setConfirmSwitch(false);
  }

  if (load === "unavailable") return null;

  // The composio-grant tri-state, narrowed the same way `ProvidersSection` does
  // (issue #1478): `undefined` reads as "unknown", never as "not granted", so
  // this section and the grid a few inches below it cannot disagree on the same
  // field.
  //
  // It no longer paints a badge — see the heading below — but the narrowing is
  // load-bearing all the same: the call to action underneath fires on
  // `not-granted` only, and collapsing "unknown" into it is exactly what #1478
  // is about.
  const grant = grantStanding(status?.granted);

  return (
    <section className="space-y-3">
      {/* The heading, and nothing beside it.

          A grant badge sat here reading "granted" / "not granted" / "grant
          unknown". Two of its three states say nothing an operator can act on
          — "granted" is the ordinary case, and "grant unknown" reports that a
          field was not read — so on almost every visit it was a chip of
          vocabulary ("grant") that belongs to the tool namespace rather than to
          the question this card answers, which is whose Composio account the
          company reaches.

          The third state is the one worth surfacing, and it already is, one
          element below: an explicit not-granted renders `GrantNamespace`, which
          says what is wrong in a sentence and offers the fix. The badge was the
          same fact with no verb. */}
      <div className="flex flex-wrap items-center gap-2">
        <Plug className="size-4 text-muted-foreground" />
        <h2 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          Connected
        </h2>
      </div>

      {load === "loading" ? (
        <Skeleton className="h-32 rounded-xl" />
      ) : load === "error" ? (
        <SectionUnreachable label="Couldn't read this company's Composio credential" />
      ) : (
        <>
          {/* Keys rework, issue #2306, slice 4c: offered only once the account
              key exists, the managed slot has no key of its own, and the
              operator has not already dismissed it for this company. */}
          {showsReuseBanner && (
            <ReuseAccountKeyBanner
              testId="composio-reuse-account-key-banner"
              text="Your TinyHumans account is connected. Use the same key for Composio?"
              busy={reuseBusy}
              onYes={() => void reuseAccountKey()}
              onNotNow={dismissReuseBanner}
            />
          )}

          {/* Fires only on an explicit not-granted, never on an unchecked grant
              (issue #1478): telling an operator to widen a grant that may
              already be set, off a field that was never read, is the same false
              confidence a status badge used to show. */}
          {grant === "not-granted" && (
            <GrantNamespace
              client={client}
              company={company}
              namespace="composio"
              explanation="Agents will not receive Composio tools even once connected."
              canManage={canManage}
              onGranted={async () => {
                await refresh();
                onChanged();
              }}
              testId="composio-not-granted"
            />
          )}

          <Card className="py-0">
            <CardContent className="px-0">
              <ComposioRowList
                rows={rows}
                canManage={canManage}
                busy={busy}
                onSelect={(row) => {
                  if (row.id === "managed") requestGiveBackManaged();
                  // The own-account route cannot be chosen without the key that
                  // makes it resolve, so choosing it opens the field rather than
                  // writing anything.
                  else openForm(row, "add");
                }}
                onAddKey={(row) => openForm(row, "add")}
                onReplaceKey={(row) => openForm(row, "replace")}
                onRemoveKey={(row) => {
                  if (row.id === "managed") requestClearManagedToken();
                }}
                onTest={(row) => void runTest(row)}
                testingRow={testingRow}
              />
            </CardContent>
          </Card>

          {/* The one explanation that survives, because no control on the page
              says it: a credential that has landed and a credential that is in
              effect look identical, and here they differ by one turn. */}
          <p className="text-xs text-muted-foreground">
            A change here takes effect on the agents&apos; next turn. No
            restart.
          </p>

          {/* The outcome of an action taken from a ROW rather than from the
              dialog — "Use this" on the managed route, "Remove token" — which
              have no field to sit beside and no dialog to sit in.

              `!form` rather than a check on the kind, because what decides
              where a message goes is whether a dialog is open, not what the
              message says: behind a modal overlay, a sentence on the page is a
              sentence nobody can read, so anything raised while the dialog is
              up renders inside it instead.

              Note what does NOT arrive here: an advisory from `settle`. That
              path remounts this section (see `settle`), so its message is
              carried by a toast. */}
          {outcome && !form && (
            <ProbeAdvisory
              outcome={outcome}
              skipOffered={false}
              busy={busy}
              onSkip={() => submit(true)}
              onDismiss={() => setOutcome(null)}
            />
          )}

          {/* The check's verdict, with its own test-id namespace: it and a
              write's outcome are separate state and can be on screen together.
              Never offers "add anyway" — that answers a refused write, and this
              route wrote nothing to refuse. */}
          {testOutcome && (
            <ProbeAdvisory
              outcome={testOutcome}
              skipOffered={false}
              busy={testingRow !== null}
              onSkip={() => {}}
              onDismiss={() => setTestOutcome(null)}
              testIdPrefix="composio-test"
            />
          )}

          {/* The credential surface is a MODAL, and that is the fix rather
              than the decoration.

              It was an inline card appended to the bottom of this section —
              after the rows, after the "takes effect next turn" line, after two
              advisory slots. Clicking "Add a token" on a row near the top of a
              scrolling page therefore rendered a form roughly a screenful below
              the fold, with nothing scrolling to it: the operator pressed the
              button, the page did not visibly move, and the honest reading of
              that is "the button is broken". It was reported as exactly that.

              A modal also matches what the action is. Pasting the credential
              every agent in the company presents is not an edit alongside the
              rows — it is one decision taken to the exclusion of the page
              behind it, and it either lands or is refused before anything else
              can be touched. Which is also why the host's answer is rendered in
              here (`outcome`) instead of on the page underneath. */}
          {form && canManage && (
            <ComposioCredentialDialog
              form={form}
              secret={secret}
              onSecretChange={(next) => {
                setSecret(next);
                // A refusal is a verdict on the key that was SUBMITTED, and
                // "add anyway" is only earned by that key. Leaving it standing
                // while the field changes would let the button store a
                // different, never-probed value with the check skipped.
                if (outcome?.kind === "rejected") setOutcome(null);
              }}
              outcome={outcome}
              onOutcomeChange={setOutcome}
              confirmSwitch={confirmSwitch}
              onConfirmSwitchChange={setConfirmSwitch}
              busy={busy}
              onSubmit={submit}
              onRequestSubmit={requestSubmit}
              onCancel={closeForm}
            />
          )}

          {/* Clearing the managed-route token. No credential form to render a
              warning inside — the row's own "Remove token" control opens this
              directly — so it is its own `AlertDialog`, the pattern
              `ApiKeyView`'s account-key removal already established. */}
          {canManage && (
            <AlertDialog
              open={clearTokenPrompt !== undefined}
              onOpenChange={(next) => {
                if (next || busy) return;
                setClearTokenPrompt(undefined);
              }}
            >
              <AlertDialogContent data-testid="composio-clear-token-dialog">
                <AlertDialogHeader>
                  <AlertDialogTitle>Disconnect Composio?</AlertDialogTitle>
                  <AlertDialogDescription>
                    {clearTokenPrompt ??
                      // Round-3 review, P1-2: named up front whenever
                      // `status.mode` already says this key is in use — never
                      // only after a refusal.
                      `${status?.mode === "managed" ? "Composio uses this key. " : ""}Clears the token stored for the managed route. Agents use whatever credential remains — a company key, the instance identity, or none — from their next turn.`}
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel disabled={busy}>
                    Keep the token
                  </AlertDialogCancel>
                  <AlertDialogAction
                    disabled={busy}
                    data-testid="composio-clear-token-confirm"
                    className="bg-destructive text-white hover:bg-destructive/90"
                    onClick={(event) => {
                      // Keep the dialog open on a stale-UI 409 so it can
                      // reopen with the host's own reason — see
                      // `AlertDialogAction`'s own docs. `runGuarded` closes it
                      // itself on success or on an unrelated failure.
                      event.preventBaseUIHandler();
                      confirmClearManagedToken();
                    }}
                  >
                    Disconnect Composio
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          )}

          {/* Giving the managed route back (byok → managed). `useManaged` used
              to write immediately with no confirmation at all; this is the
              gap the operator's mid-project ask closes for that direction —
              see the state's own comment for why managed → byok keeps its
              existing inline confirmation instead of moving here. */}
          {canManage && (
            <AlertDialog
              open={giveBackManagedPrompt !== undefined}
              onOpenChange={(next) => {
                if (next || busy) return;
                setGiveBackManagedPrompt(undefined);
              }}
            >
              <AlertDialogContent data-testid="composio-use-managed-dialog">
                <AlertDialogHeader>
                  <AlertDialogTitle>
                    Switch Composio to the TinyHumans-managed route?
                  </AlertDialogTitle>
                  <AlertDialogDescription>
                    {giveBackManagedPrompt ??
                      // Round-3 review, P1-2: named up front whenever
                      // `status.mode` already says this key is in use.
                      `${status?.mode === "byok" ? "Composio uses this key. " : ""}Clears this company's own Composio API key. Providers connected through that account stay there — connect them again here, or add the key back to switch to it.`}
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel disabled={busy}>Cancel</AlertDialogCancel>
                  <AlertDialogAction
                    disabled={busy}
                    data-testid="composio-use-managed-confirm"
                    onClick={(event) => {
                      event.preventBaseUIHandler();
                      confirmGiveBackManaged();
                    }}
                  >
                    Use TinyHumans-managed Composio
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          )}
        </>
      )}
    </section>
  );
}

// `showManagedTokenCard` lived here and is gone. It gated the legacy
// managed-route token card on the SELECTED tile and the PERSISTED route
// agreeing, because either alone put two credential surfaces on screen at
// exactly the moment an operator was switching between them — and one of those
// directions offered a Clear that silently destroyed a preserved token (#586)
// while leaving the company where it was.
//
// The invariant survives; the predicate does not need to. There is one
// `pending` form at a time and `composioForm` returns at most one
// `ComposioForm`, so two credential surfaces are now unrepresentable rather
// than merely tested against. `composio/rows.ts` owns the check that a pending
// form is still permitted by the row it belongs to, which is the half that
// used to be spread across `mode`, `onByok` and `showOverride`.
//
// `modeOf` also moved, unchanged, to `composio/rows.ts`. Nothing is re-exported
// from here on its way out: a pure function reachable only through a `.tsx` is
// the shape that made the old predicate testable but not the rows around it.
