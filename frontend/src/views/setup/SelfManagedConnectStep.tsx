import { useRef, useState } from "react";
import { Plus } from "lucide-react";

import type { OpenCompanyClient } from "@/api/client";
import type { AddProviderInput } from "@/api/inference";
import type { SetupInput } from "@/api/setup";
import { probeSetupDraft, testSetupComposioKey } from "@/api/setup";
import { ApiError } from "@/api/types";
import type { ComposioSubmitOutcome } from "@/composio/classify";
import { ComposioRowList } from "@/composio/ComposioRowList";
import { composioForm, composioRows } from "@/composio/rows";
import type { ComposioPending } from "@/composio/types";
import { Button } from "@/components/ui/button";
import { ComposioCredentialDialog } from "@/views/connections/ComposioCredentialDialog";
// The naming collision this file exists on top of: `@/search-providers` ships
// files called `AddProviderDialog` and `ProviderConnectDialog` too, and they
// are Search's own add flow. The wrong import type-checks.
// `scripts/ci/assert-setup-inference-imports.sh` fails the build on it.
import { AddProviderDialog } from "@/inference/AddProviderDialog";
import { ProviderConnectDialog } from "@/inference/ProviderConnectDialog";
import type { ConnectDraft, ModelAsk } from "@/inference/ProviderConnectDialog";
import { isAzureEndpoint } from "@/inference/catalogue";
import { addOptions, modelAskFromProbe, probeEndpoint } from "@/inference/connect";
import { stripEnvelopePrefix } from "@/inference/ProvidersTab";

/** The Composio credential staged for the apply. */
export type ComposioDraft = NonNullable<SetupInput["composio_draft"]>;

/**
 * The two rows, for a company that does not exist yet.
 *
 * `composioRows` tolerates a `null` status — `modeOf` reads it as `managed`,
 * which is where a company with nothing configured genuinely is — and it always
 * returns both rows. So the card the wizard draws is the card Connections
 * draws, with the controls a blank company actually has: a token to add on the
 * managed route, and the own-account route to switch to by supplying the key
 * that makes it resolve.
 */
const COMPOSIO_ROWS = composioRows(null);

/** The name to show a staged draft under, from the same catalogue the picker drew it from. */
export function draftLabel(draft: AddProviderInput): string {
  const options = addOptions([]);
  const match = [...options.cloud, ...options.local, ...options.cli].find(
    (option) => option.value === draft.kind,
  );
  return match?.label ?? draft.label?.trim() ?? "Custom provider";
}

/**
 * The self-managed branch's step 1: Connections → LLM's add-provider sequence
 * and nothing else.
 *
 * Both dialogs are the Connections ones, mounted — not a wizard-sized copy of
 * them. `AddProviderDialog` takes a list and a callback and makes no request of
 * its own; `ProviderConnectDialog` takes a client and a company it does not use
 * on the add path (its draft probe is gated on the edit step, and its model
 * field is in list mode from the probe's own answer), so both render before a
 * company exists. `inference-connect-dialog-offline.test.ts` pins that.
 *
 * What is **not** mounted is `ProvidersTab`. It is a controlled view over
 * `InferenceState`/`InferenceActions`, and `useInference` opens with
 * `GET {scope}/inference` — which answers `CompanyNotFound` here. So this owns
 * the orchestration between the two dialogs instead, in the shape
 * `ProvidersTab.submitConnect` already has: probe the drafted endpoint, stop on
 * an `auth` refusal at the field that can be fixed, otherwise open the model
 * step with that probe's own catalogue in hand.
 *
 * ## Staged, not written
 *
 * The add is deferred to the apply for the same reason the managed branch's key
 * is: `POST …/inference/providers` writes a company's rows and the company is
 * what the apply creates. So this collects a draft and proves its endpoint; the
 * host runs the real add and reports what it did.
 *
 * ## Skipping records nothing
 *
 * There is no "explicitly deferred" to store. `null` is the honest state of a
 * company minutes old, nothing downstream could act on the difference, and the
 * operator connects providers from Connections whenever they like. The
 * reassurance is shown while they are standing here and forgotten at Next —
 * which is what this component unmounting does for free.
 */
export function SelfManagedConnectStep({
  client,
  draft,
  onDraft,
  composio,
  onComposio,
}: {
  client: OpenCompanyClient;
  /** The provider staged for the apply, or `null` while none is. */
  draft: AddProviderInput | null;
  onDraft: (next: AddProviderInput | null) => void;
  /** The Composio credential staged for the apply, or `null` while none is. */
  composio: ComposioDraft | null;
  onComposio: (next: ComposioDraft | null) => void;
}) {
  const [adding, setAdding] = useState(false);
  const [connecting, setConnecting] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [probeFailed, setProbeFailed] = useState(false);
  const [modelAsk, setModelAsk] = useState<ModelAsk | null>(null);
  /** Whether the operator said they would do this later. Local, and gone at Next. */
  const [deferred, setDeferred] = useState(false);
  /** Which Composio form the operator asked for, as an intent rather than a rendered state. */
  const [composioPending, setComposioPending] = useState<ComposioPending | null>(null);
  const [composioSecret, setComposioSecret] = useState("");
  const [composioOutcome, setComposioOutcome] = useState<ComposioSubmitOutcome | null>(null);
  const [composioBusy, setComposioBusy] = useState(false);
  const [composioDeferred, setComposioDeferred] = useState(false);
  /**
   * What the check said about a key that was staged anyway.
   *
   * Amber, not red, and beside the staged row rather than in the dialog: the
   * key landed. Only a rejected credential holds the form open.
   */
  const [composioAdvisory, setComposioAdvisory] = useState<string | null>(null);
  /**
   * Invalidates a superseded connect attempt, exactly as `ProvidersTab` does:
   * a probe started under one provider must not land its catalogue in the
   * dialog of another the operator opened while it was in flight.
   */
  const attempt = useRef(0);

  const reset = () => {
    attempt.current += 1;
    setBusy(false);
    setError(null);
    setProbeFailed(false);
    setModelAsk(null);
  };

  const close = () => {
    reset();
    setConnecting(null);
  };

  const open = (optionSlug: string) => {
    reset();
    setAdding(false);
    setConnecting(optionSlug);
  };

  async function submitConnect(connect: ConnectDraft) {
    const myAttempt = ++attempt.current;
    setBusy(true);
    setError(null);
    setProbeFailed(false);
    try {
      if (!modelAsk) {
        const url = probeEndpoint(connect.kind, connect.baseUrl);
        const probe = url
          ? await probeSetupDraft(client, {
              baseUrl: url,
              key: connect.key,
              kind: connect.kind,
            })
          : null;
        if (myAttempt !== attempt.current) return;
        // A rejected key must not reach the model step. Stopping here leaves
        // the operator on the field they can actually correct, rather than
        // discovering the same rejection again at the finish — where the
        // company has already been built.
        if (probe && !probe.ok && probe.class === "auth") {
          setError(
            probe.message ? stripEnvelopePrefix(probe.message) : "The credential was rejected.",
          );
          setProbeFailed(true);
          return;
        }
        setModelAsk(modelAskFromProbe(url, probe, isAzureEndpoint));
        return;
      }
      onDraft({
        kind: connect.kind,
        label: connect.label,
        baseUrl: connect.baseUrl,
        key: connect.key,
        model: connect.model ?? "",
        addAnyway: connect.addAnyway,
      });
      setDeferred(false);
      close();
    } catch (err) {
      if (myAttempt !== attempt.current) return;
      setError(
        err instanceof ApiError ? stripEnvelopePrefix(err.message) : "That did not work.",
      );
    } finally {
      if (myAttempt === attempt.current) setBusy(false);
    }
  }

  const composioFormValue = composioForm(composioPending, COMPOSIO_ROWS);

  /**
   * Stage what is in the Composio field, after checking it where a check is
   * possible.
   *
   * Only the own-account key can be checked: the host's probe asks Composio
   * whether it recognises an API key, and the managed route's token is a bearer
   * the TinyHumans backend issues, which no cheap call can tell apart from a
   * backend that is down. The rows already say that — the managed row offers no
   * Test — so a token is staged as typed.
   *
   * Only a **rejected** credential holds the form open. Every other class is
   * non-destructive — Composio was unreachable, the check is not compiled into
   * this build — and the Connections page stores the key on those and reports
   * the reason beside it. Refusing here instead would leave an operator on a
   * default build unable to stage a key at all, because the check can only ever
   * answer "did not complete" there.
   *
   * `skipVerify` is the "add anyway" path and stages without asking. The
   * `outcome` this check produces never carries `fromHost`/`status`/`code`,
   * so `offersSkipVerify` (keyed on that shape) never fires here — that
   * escape only reaches the Connections form's own write failure.
   */
  async function submitComposio(skipVerify = false) {
    if (!composioFormValue) return;
    const value = composioSecret.trim();
    if (!value) return;
    const stage = (advisory: string | null) => {
      onComposio({ credential: composioFormValue.credential, value });
      setComposioAdvisory(advisory);
      setComposioDeferred(false);
      closeComposio();
    };
    if (skipVerify || composioFormValue.credential !== "composio-api-key") {
      stage(null);
      return;
    }
    setComposioBusy(true);
    setComposioOutcome(null);
    try {
      const verdict = await testSetupComposioKey(client, value);
      if (verdict.ok) {
        stage(null);
        return;
      }
      const message = verdict.message ?? "Composio did not recognise that key.";
      if (verdict.probeClass === "auth") {
        setComposioOutcome({ kind: "rejected", message });
        return;
      }
      stage(message);
    } catch (err) {
      setComposioOutcome({
        kind: "rejected",
        status: err instanceof ApiError ? err.status : undefined,
        message:
          err instanceof ApiError
            ? stripEnvelopePrefix(err.message)
            : "Could not check the Composio API key.",
      });
    } finally {
      setComposioBusy(false);
    }
  }

  const openComposio = (row: { id: ComposioPending["row"] }) => {
    setComposioPending({ row: row.id, action: "add" });
    setComposioSecret("");
    setComposioOutcome(null);
    setComposioAdvisory(null);
  };

  const closeComposio = () => {
    setComposioPending(null);
    setComposioSecret("");
    setComposioOutcome(null);
  };

  return (
    <div className="space-y-7">
      <div>
        <h2
          className="text-base font-medium leading-snug"
          data-testid="setup-question"
        >
          Connect what your team thinks with
        </h2>
        <p
          className="text-xs leading-snug text-muted-foreground"
          data-testid="setup-model-prompt"
        >
          Your own provider and your own key. Nothing here is brokered for you.
        </p>

        <div className="mt-3 space-y-2">
          {draft ? (
            <div
              className="flex flex-wrap items-center justify-between gap-2 rounded-md border px-3 py-2"
              data-testid="setup-provider-staged"
            >
              <span className="min-w-0 text-sm">
                <span className="font-medium">{draftLabel(draft)}</span>
                <span className="text-muted-foreground"> · {draft.model}</span>
              </span>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                data-testid="setup-provider-remove"
                onClick={() => onDraft(null)}
              >
                Remove
              </Button>
            </div>
          ) : (
            <Button
              type="button"
              variant="outline"
              data-testid="setup-add-provider"
              onClick={() => setAdding(true)}
            >
              <Plus className="size-4" />
              Add a provider
            </Button>
          )}

          {!draft &&
            (deferred ? (
              <p
                className="text-xs leading-snug text-muted-foreground"
                data-testid="setup-provider-later-note"
              >
                You can add this later under Connections → API Keys → LLM. Until then your
                team gets a standard roster rather than a designed one.
              </p>
            ) : (
              <Button
                type="button"
                variant="ghost"
                size="sm"
                className="px-0 text-muted-foreground"
                data-testid="setup-provider-later"
                onClick={() => setDeferred(true)}
              >
                Set this up later
              </Button>
            ))}
        </div>
      </div>

      <div>
        <h2
          className="text-base font-medium leading-snug"
          data-testid="setup-composio-question"
        >
          Connect the account your tools run through
        </h2>
        <p className="text-xs leading-snug text-muted-foreground">
          Gmail, Slack and the rest are reached through a Composio account. Bring your own,
          or paste a token for the TinyHumans-managed route.
        </p>

        <div className="mt-3 space-y-2">
          {composio ? (
            <div
              className="flex flex-wrap items-center justify-between gap-2 rounded-md border px-3 py-2"
              data-testid="setup-composio-staged"
            >
              <span className="min-w-0 text-sm">
                {composio.credential === "composio-api-key"
                  ? "This company's own Composio account"
                  : "A token for the TinyHumans-managed route"}
              </span>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                data-testid="setup-composio-remove"
                onClick={() => {
                  onComposio(null);
                  setComposioAdvisory(null);
                }}
              >
                Remove
              </Button>
            </div>
          ) : (
            // The Connections card, as it stands for a company with nothing
            // configured: one single-select pair, the managed route active with
            // a token to add, and the own-account route offering to be chosen
            // by supplying the key that makes it resolve.
            <ComposioRowList
              rows={COMPOSIO_ROWS}
              canManage
              busy={composioBusy}
              onSelect={openComposio}
              onAddKey={openComposio}
              onReplaceKey={openComposio}
              onRemoveKey={() => {}}
              onTest={() => {}}
              testingRow={null}
            />
          )}

          {composio && composioAdvisory && (
            <p
              className="text-xs leading-snug text-status-blocked-text"
              data-testid="setup-composio-advisory"
            >
              {composioAdvisory}
            </p>
          )}

          {!composio &&
            (composioDeferred ? (
              <p
                className="text-xs leading-snug text-muted-foreground"
                data-testid="setup-composio-later-note"
              >
                You can add this later under Connections → Composio. Until then your team
                has no app connections.
              </p>
            ) : (
              <Button
                type="button"
                variant="ghost"
                size="sm"
                className="px-0 text-muted-foreground"
                data-testid="setup-composio-later"
                onClick={() => setComposioDeferred(true)}
              >
                Set this up later
              </Button>
            ))}
        </div>
      </div>

      <AddProviderDialog
        open={adding}
        onOpenChange={setAdding}
        // No company exists yet, so nothing is connected and every option is on
        // offer. This is the honest list rather than a stand-in for one.
        providers={[]}
        onChoose={open}
      />
      {/* Keyed so the dialog seeds fresh per open, exactly as the LLM page keys
          it — its fields initialise at mount rather than in an effect. */}
      <ProviderConnectDialog
        key={connecting ?? "closed"}
        client={client}
        // Never read on the add path: the dialog's only company-scoped call is
        // its edit-step draft probe, and `editing` is `null` throughout here.
        company={null}
        optionSlug={connecting}
        providers={[]}
        editing={null}
        busy={busy}
        error={error}
        offerAddAnyway={probeFailed}
        modelAsk={modelAsk}
        // The first provider this company will ever have, by construction.
        noDefaultYet
        onCancel={close}
        onBack={reset}
        onSubmit={(connect) => void submitConnect(connect)}
      />
      {composioFormValue && (
        <ComposioCredentialDialog
          form={composioFormValue}
          secret={composioSecret}
          onSecretChange={(next) => {
            setComposioSecret(next);
            // A refusal is a verdict on the key that was submitted, and "add
            // anyway" is only earned by that key — so editing retires both.
            if (composioOutcome?.kind === "rejected") setComposioOutcome(null);
          }}
          outcome={composioOutcome}
          onOutcomeChange={setComposioOutcome}
          // The switch confirmation warns that providers connected before this
          // stay in the account this company is leaving. A company that does
          // not exist yet has connected none, so there is nothing to warn
          // about and Save goes straight through.
          confirmSwitch={false}
          onConfirmSwitchChange={() => {}}
          busy={composioBusy}
          onSubmit={(skipVerify) => void submitComposio(skipVerify)}
          onRequestSubmit={() => void submitComposio()}
          onCancel={closeComposio}
        />
      )}
    </div>
  );
}
