import { useCallback, useEffect, useState } from "react";
import { BrainCircuit, Check, Loader2, RotateCcw, Save, Trash2, Zap } from "lucide-react";
import { toast } from "sonner";

import type { OpenCompanyClient } from "@/api/client";
import {
  getInferenceStatus,
  restartInference,
  revertInference,
  setInference,
  testInference,
  type InferenceMutation,
  type InferenceProvider,
  type InferenceStatus,
  type UsageMetering,
} from "@/api/inference";
import { ApiError } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";

/** The abstract cognition tiers the tenant model table maps. */
const TIERS = ["chat-v1", "reasoning-v1", "agentic-v1", "vision-v1"] as const;
type Tier = (typeof TIERS)[number];

/**
 * What each provider is, as data rather than as name comparisons scattered
 * through the view.
 *
 * `acceptsKey` / `requiresBaseUrl` used to be written inline as
 * `provider !== "ollama"` and `provider === "ollama" || provider ===
 * "openai_compatible"`. Both were deny-lists over a closed set: a future local
 * or credential-less provider would have inherited a key input nobody wanted
 * simply by not being named in them. Asking the descriptor means a new provider
 * declares what it needs when it is added here, and the view stops caring.
 *
 * `keyKind` is the other half of that. The credential a provider wants is
 * vendor-specific, and this field is not the only place in Connections that
 * accepts a TinyHumans key — so saying which vendor's key belongs here, per
 * provider, is what stops one being pasted into the other.
 */
const PROVIDERS: Record<
  InferenceProvider,
  {
    label: string;
    /** Whether this provider authenticates with a bearer at all. */
    acceptsKey: boolean;
    /** Whether the endpoint must be given explicitly (no useful default). */
    requiresBaseUrl: boolean;
    /** Which vendor's credential belongs in the key field, in the operator's words. */
    keyKind: string;
    /** Form defaults applied when the operator picks this provider. */
    preset: { baseUrl: string; models: Partial<Record<Tier, string>> };
  }
> = {
  managed: {
    label: "Managed (TinyHumans)",
    acceptsKey: true,
    requiresBaseUrl: false,
    keyKind: "a TinyHumans API key",
    preset: { baseUrl: "", models: {} },
  },
  openrouter: {
    label: "OpenRouter",
    acceptsKey: true,
    requiresBaseUrl: false,
    keyKind: "an OpenRouter key (`sk-or-…`)",
    // OpenRouter's recommended DeepSeek pairing, prefilled.
    preset: {
      baseUrl: "",
      models: { "chat-v1": "deepseek/deepseek-chat", "reasoning-v1": "deepseek/deepseek-r1" },
    },
  },
  ollama: {
    label: "Ollama (local)",
    acceptsKey: false,
    requiresBaseUrl: true,
    keyKind: "no key — Ollama takes no bearer",
    preset: { baseUrl: "http://localhost:11434/v1", models: { "chat-v1": "llama3.1" } },
  },
  openai_compatible: {
    label: "Custom (OpenAI-compatible)",
    acceptsKey: true,
    requiresBaseUrl: true,
    keyKind: "whatever key the endpoint below expects",
    preset: { baseUrl: "", models: {} },
  },
};

/**
 * The base-ui `Select` wants a plain id -> label map for its `items` prop, so
 * project one out of the descriptor rather than maintaining a second list.
 */
const PROVIDER_LABEL_ITEMS: Record<InferenceProvider, string> = Object.fromEntries(
  (Object.keys(PROVIDERS) as InferenceProvider[]).map((p) => [p, PROVIDERS[p].label]),
) as Record<InferenceProvider, string>;

/**
 * What the live cognition path's metering mode means for the Usage view — so a
 * zero token/cost reading is legible instead of alarming (issue #174).
 */
const METERING_NOTES: Record<UsageMetering, string> = {
  perTurn: "usage metered per turn",
  perCycle: "usage metered per cycle, from what the provider reports",
  none: "no model runs on this path, so Usage stays at zero",
};

/** Per-provider form defaults applied when the operator picks a provider. */
function presetFor(provider: InferenceProvider): {
  baseUrl: string;
  models: Partial<Record<Tier, string>>;
} {
  return PROVIDERS[provider].preset;
}

type Load = "loading" | "ready" | "unavailable";
type TestState =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "ok"; note: string }
  | { kind: "error"; message: string };

/**
 * Bring-Your-Own-Key inference (issue #56). Shows the company's effective
 * provider (with a source badge + tier→model rows + a "key set" indicator), a
 * live "Test" probe, and a switch form with per-provider presets. The key input
 * is **write-only** — it is sent on Save, stored server-side, and never read
 * back.
 *
 * A switch takes effect on the agents' next turn with no restart — *except* the
 * not-configured → configured transition, where the running brain was already
 * chosen without one. The host reports that as `restartRequired`, and this
 * section says so in the toast and keeps saying it in the status card until the
 * restart happens (issue #266).
 */
export function InferenceSection({
  client,
  company,
  canManage,
}: {
  client: OpenCompanyClient;
  company: string | null;
  /**
   * Whether this viewer may change where the company's model calls go (issue
   * #403). Courtesy only — the host answers 403 regardless; this stops the
   * console offering a form whose Save cannot land. `Test` stays available: it
   * probes the config as already stored, and the host leaves it open.
   */
  canManage: boolean;
}) {
  const [load, setLoad] = useState<Load>("loading");
  const [status, setStatus] = useState<InferenceStatus | null>(null);
  const [busy, setBusy] = useState<
    "save" | "reset" | "test" | "removeKey" | "restart" | null
  >(null);
  const [test, setTest] = useState<TestState>({ kind: "idle" });

  // Switch form.
  const [provider, setProvider] = useState<InferenceProvider>("managed");
  const [baseUrl, setBaseUrl] = useState("");
  const [models, setModels] = useState<Partial<Record<Tier, string>>>({});
  const [key, setKey] = useState("");

  const refresh = useCallback(async () => {
    try {
      setStatus(await getInferenceStatus(client, company));
      setLoad("ready");
    } catch {
      setLoad("unavailable");
    }
  }, [client, company]);

  useEffect(() => {
    setLoad("loading");
    void refresh();
  }, [refresh]);

  function pickProvider(next: InferenceProvider) {
    setProvider(next);
    const preset = presetFor(next);
    setBaseUrl(preset.baseUrl);
    setModels(preset.models);
    setTest({ kind: "idle" });
  }

  function setModel(tier: Tier, value: string) {
    setModels((m) => ({ ...m, [tier]: value }));
  }

  async function save() {
    if (busy) return;
    setBusy("save");
    try {
      // "Managed" with no company key — neither typed now nor already stored —
      // means "use the platform default", which is a revert rather than a
      // runtime override. With a key it is a real override (issue #585): the
      // company pays for its own agents on the platform brain, so the override
      // has to be written or the key would be stored and then ignored.
      //
      // This is what retires issue #265's refusal guard. That guard existed
      // because a managed save was *only* ever a revert, and a revert cannot
      // carry a credential, so a typed key would have been dropped while the
      // toast claimed success. The invariant it protected — a save that reports
      // success never discarded what the operator typed — is what this branch
      // now upholds directly, by storing the key instead of refusing it.
      let result: InferenceMutation;
      if (provider === "managed" && !key.trim() && !status?.keyConfigured) {
        result = await revertInference(client, company);
      } else {
        const cleanModels = Object.fromEntries(
          Object.entries(models)
            .map(([t, v]) => [t, (v ?? "").trim()])
            .filter(([, v]) => v.length > 0),
        );
        result = await setInference(client, company, {
          provider,
          baseUrl: baseUrl.trim() || undefined,
          models: Object.keys(cleanModels).length ? cleanModels : undefined,
          key: key.trim() || undefined,
        });
      }
      // Issue #266: only the host knows whether the *running* brain can act on
      // what was just saved. Which brain a company runs is fixed when it is
      // built, so a company that started with no inference source keeps echoing
      // no matter what lands here — "teammates use it on their next turn" was a
      // promise the runtime could not keep for exactly the transition an
      // operator makes first. Follow the response instead of asserting.
      if (result.status.restartRequired) {
        toast.warning("Inference saved — restart the company for teammates to use it.", {
          description: result.note,
        });
      } else {
        toast.success("Inference updated. Teammates use it on their next turn.");
      }
      setKey("");
      setTest({ kind: "idle" });
      await refresh();
    } catch (err) {
      toast.error(err instanceof ApiError ? err.message : "Couldn't update inference.");
    } finally {
      setBusy(null);
    }
  }

  /** Clears the stored company key without changing the effective provider. */
  async function removeKey() {
    if (busy) return;
    setBusy("removeKey");
    const effective = (status?.provider as InferenceProvider) ?? provider;
    try {
      await setInference(client, company, {
        provider: effective,
        // Only echo the resolved base URL back for the providers that require
        // one. `managed` and `openrouter` resolve theirs from the environment or
        // a well-known default, and persisting the *displayed* value would pin
        // the company to it — silently overriding `OPENCOMPANY_INFERENCE_URL`.
        baseUrl: PROVIDERS[effective]?.requiresBaseUrl ? status?.baseUrl || undefined : undefined,
        models: status && Object.keys(status.models).length ? status.models : undefined,
        key: "",
      });
      toast.success("Removed the company key. Teammates fall back on their next turn.");
      setKey("");
      setTest({ kind: "idle" });
      await refresh();
    } catch (err) {
      toast.error(err instanceof ApiError ? err.message : "Couldn't remove the key.");
    } finally {
      setBusy(null);
    }
  }

  /**
   * Rebuild the company's runtime now, clearing the restart-required state.
   *
   * The banner used to name a restart and offer no way to perform one, which is
   * a dead end for exactly the operator most likely to hit it: a hosted tenant
   * cannot restart its own container, and the control plane has no button for
   * it either.
   *
   * The resulting status is read from the response rather than assumed. A host
   * that wired no rebuilder genuinely cannot do this and says so, and reporting
   * success there would replace a visible dead end with an invisible one.
   */
  async function restartNow() {
    if (busy) return;
    setBusy("restart");
    try {
      const result = await restartInference(client, company);
      if (result.status.restartRequired) {
        // The rebuild ran and the company is still on the old brain. Follow the
        // host's note, which names the process restart that would work.
        toast.warning("Still needs a restart.", { description: result.note });
      } else {
        toast.success("Restarted. Teammates think with the new provider from their next turn.");
      }
      setStatus(result.status);
    } catch (err) {
      toast.error(err instanceof ApiError ? err.message : "Couldn't restart the company.");
    } finally {
      setBusy(null);
    }
  }

  async function reset() {
    if (busy) return;
    setBusy("reset");
    try {
      await revertInference(client, company);
      toast.success("Reverted to the managed configuration.");
      pickProvider("managed");
      setKey("");
      await refresh();
    } catch (err) {
      toast.error(err instanceof ApiError ? err.message : "Couldn't revert inference.");
    } finally {
      setBusy(null);
    }
  }

  async function probe() {
    if (busy) return;
    setBusy("test");
    setTest({ kind: "loading" });
    try {
      const result = await testInference(client, company);
      if (result.ok) {
        setTest({ kind: "ok", note: result.note ?? "Reached the provider." });
      } else {
        setTest({ kind: "error", message: result.error ?? "The provider did not respond." });
      }
    } catch (err) {
      setTest({
        kind: "error",
        message: err instanceof ApiError ? err.message : "The probe failed.",
      });
    } finally {
      setBusy(null);
    }
  }

  if (load === "unavailable") return null;

  const modelRows = status ? Object.entries(status.models) : [];
  return (
    <section className="space-y-3">
      <div className="flex items-center gap-2">
        <BrainCircuit className="size-4 text-muted-foreground" />
        <h3 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          Inference (BYOK)
        </h3>
        {/* Mirrors the Company credential card's own subtitle. Two cards in
            Connections accept a key and one of them accepts a TinyHumans key,
            so each says in one line which it is — the distinction is otherwise
            only in a module doc no operator reads (#637). */}
        <span className="text-xs text-muted-foreground">
          the key your teammates think with — not the company account key
        </span>
      </div>
      <p className="text-sm text-muted-foreground">
        Choose which model provider your teammates think with. Bring your own key for OpenRouter, a
        custom OpenAI-compatible endpoint, or a local Ollama server — the key is stored securely and
        never shown again. Switching provider or model takes effect on the teammates' next turn.
        Giving inference to a company that started without any does not: the brain is chosen at
        startup, so that first setup needs a restart.
      </p>

      {load === "loading" ? (
        <Skeleton className="h-40 rounded-xl" />
      ) : (
        <Card>
          <CardContent className="space-y-4 py-4">
            {/* Status card. */}
            {status && (
              <div className="space-y-2">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-medium">{PROVIDERS[status.provider as InferenceProvider]?.label ?? status.provider}</span>
                  <Badge variant={status.source === "runtime" ? "outline" : "secondary"}>
                    {status.source}
                  </Badge>
                  {status.keyConfigured && (
                    <span className="inline-flex items-center gap-1 text-xs text-status-done-text">
                      <Check className="size-3" /> key set
                    </span>
                  )}
                  <Button
                    variant="ghost"
                    size="sm"
                    className="ml-auto"
                    disabled={busy !== null}
                    onClick={() => void probe()}
                  >
                    {busy === "test" ? <Loader2 className="size-4 animate-spin" /> : <Zap className="size-4" />}
                    Test
                  </Button>
                </div>
                <p className="truncate text-xs text-muted-foreground">{status.baseUrl}</p>
                {/* Issue #174: config resolving to a provider does not mean the
                    company booted onto it. Say which cognition path is live and
                    whether its usage is metered, so a zero Usage reading reads as
                    "nothing was spent" rather than "accounting is broken". */}
                <p className="text-xs text-muted-foreground">
                  Cognition: <span className="font-mono">{status.cognition}</span> ·{" "}
                  {METERING_NOTES[status.usageMetering] ?? "usage metering unknown"}
                </p>
                {/* Issue #266: a saved config the running brain cannot act on.
                    The toast that says so is gone in seconds — and an operator
                    who reloads the page, or comes back tomorrow, sees only a
                    correct-looking provider next to agents that still echo. This
                    is the surface that stays until the restart happens. */}
                {status.restartRequired && (
                  <div
                    className="flex items-start gap-1.5 rounded-lg border border-status-blocked/40 bg-status-blocked-soft px-3 py-2 text-xs text-status-blocked-text"
                    data-testid="inference-restart-required"
                  >
                    <RotateCcw className="mt-px size-3.5 shrink-0" />
                    <div className="space-y-2">
                      <span>
                        <span className="font-medium">Restart required.</span> This company started
                        with no inference source, so it is running the offline echo brain and its
                        scheduled workflows cannot fire. The brain is chosen at startup — this
                        configuration is saved, but teammates keep echoing until the company is
                        restarted.
                      </span>
                      {/* The action, not just the diagnosis. Telling a hosted
                          operator to "restart the company" names something they
                          have no way to do — the container is the unit of
                          restart and the control plane has no button for it —
                          so this rebuilds the runtime in place instead (#290).
                          Only rendered for an admin, matching the route's own
                          authority check, so a member is not shown a control
                          that can only 403. */}
                      {canManage && (
                        <Button
                          size="sm"
                          variant="outline"
                          disabled={busy !== null}
                          data-testid="inference-restart-now"
                          onClick={() => void restartNow()}
                        >
                          {busy === "restart" ? (
                            <Loader2 className="size-3.5 animate-spin" />
                          ) : (
                            <RotateCcw className="size-3.5" />
                          )}
                          Restart now
                        </Button>
                      )}
                    </div>
                  </div>
                )}
                {modelRows.length > 0 && (
                  <ul className="space-y-1 rounded-md bg-muted/40 p-2">
                    {modelRows.map(([tier, model]) => (
                      <li key={tier} className="text-xs">
                        <span className="font-mono font-medium">{tier}</span>
                        <span className="text-muted-foreground"> → {model}</span>
                      </li>
                    ))}
                  </ul>
                )}
                {test.kind === "ok" && (
                  <p className="flex items-center gap-1 text-xs text-status-done-text">
                    <Check className="size-3" /> {test.note}
                  </p>
                )}
                {test.kind === "error" && <p className="text-xs text-destructive">{test.message}</p>}
                {test.kind === "loading" && (
                  <p className="flex items-center gap-1 text-xs text-muted-foreground">
                    <Loader2 className="size-3 animate-spin" /> Probing the provider…
                  </p>
                )}
              </div>
            )}

            {/* Switch form. */}
            {/* The switch form is an admin's: it decides the base URL every
                agent's prompts travel to and the key they are billed against
                (issue #403). */}
            {canManage && (
              <div className="space-y-3 border-t border-border pt-3">
                <div className="grid gap-2 sm:grid-cols-2 sm:items-end">
                  <div className="space-y-1">
                    <Label htmlFor="inference-provider" className="text-xs">
                      Provider
                    </Label>
                    <Select
                      value={provider}
                      onValueChange={(v) => pickProvider(v as InferenceProvider)}
                      items={PROVIDER_LABEL_ITEMS}
                    >
                      <SelectTrigger id="inference-provider" className="w-full">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {(Object.keys(PROVIDERS) as InferenceProvider[]).map((p) => (
                          <SelectItem key={p} value={p}>
                            {PROVIDERS[p].label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  {PROVIDERS[provider].requiresBaseUrl && (
                    <div className="space-y-1">
                      <Label htmlFor="inference-base-url" className="text-xs">
                        Base URL
                      </Label>
                      <Input
                        id="inference-base-url"
                        value={baseUrl}
                        placeholder="https://host/v1"
                        onChange={(e) => setBaseUrl(e.target.value)}
                      />
                    </div>
                  )}
                </div>

                {provider !== "managed" && (
                  <div className="grid gap-2 sm:grid-cols-2">
                    {TIERS.map((tier) => (
                      <div key={tier} className="space-y-1">
                        <Label htmlFor={`inference-model-${tier}`} className="text-xs">
                          {tier}
                        </Label>
                        <Input
                          id={`inference-model-${tier}`}
                          value={models[tier] ?? ""}
                          placeholder="provider model id"
                          onChange={(e) => setModel(tier, e.target.value)}
                        />
                      </div>
                    ))}
                  </div>
                )}

                {/*
                  The key field is offered for `managed` too (issue #585). The
                  company's TinyHumans key is the admin's to set, and hiding the
                  input here was the whole reason it could only arrive as a
                  deploy-time environment variable — `resolve_endpoint` has
                  always preferred a stored key over the env default on this
                  provider. Ollama is the one provider that takes no bearer.
                */}
                {PROVIDERS[provider].acceptsKey && (
                  <div className="space-y-1">
                    <Label htmlFor="inference-key" className="text-xs">
                      API key {status?.keyConfigured ? "(leave blank to keep)" : ""}
                    </Label>
                    <Input
                      id="inference-key"
                      type="password"
                      value={key}
                      placeholder="write-only"
                      autoComplete="off"
                      onChange={(e) => setKey(e.target.value)}
                    />
                    <p className="text-xs text-muted-foreground" data-testid="inference-key-note">
                      Paste {PROVIDERS[provider].keyKind}. This field is scoped to the provider
                      selected above and is stored against it — a key for any other vendor will
                      fail at the first turn, so it is not the place for one.
                      {provider === "managed" && " Leave it blank to keep running on the platform credential."}
                    </p>
                    <p className="text-xs text-muted-foreground">
                      Whatever you set here is what the company spends against: everyone you invite
                      spends on it — inference, embeddings, tools and capabilities all bill to this
                      one account, and spend cannot be attributed back to individual members.
                      Removing someone from the roster stops their future access; it does not
                      separate what they already spent.
                    </p>
                  </div>
                )}

                <div className="flex items-center gap-2">
                  <Button
                    data-testid="inference-save"
                    disabled={busy !== null}
                    onClick={() => void save()}
                  >
                    {busy === "save" ? <Loader2 className="size-4 animate-spin" /> : <Save className="size-4" />}
                    Save
                  </Button>
                  <Button variant="outline" disabled={busy !== null} onClick={() => void reset()}>
                    {busy === "reset" ? <Loader2 className="size-4 animate-spin" /> : <RotateCcw className="size-4" />}
                    Reset to managed
                  </Button>
                  {status?.keyConfigured && (
                    <Button
                      variant="ghost"
                      className="text-destructive"
                      data-testid="inference-remove-key"
                      disabled={busy !== null}
                      onClick={() => void removeKey()}
                    >
                      {busy === "removeKey" ? (
                        <Loader2 className="size-4 animate-spin" />
                      ) : (
                        <Trash2 className="size-4" />
                      )}
                      Remove key
                    </Button>
                  )}
                </div>
              </div>
            )}
          </CardContent>
        </Card>
      )}
    </section>
  );
}
