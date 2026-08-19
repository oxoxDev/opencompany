// The first-run setup wizard.
//
// One flow that configures an instance: pick a company template, choose how
// people sign in, point the brain at a credential, review the tool surfaces
// this build has, and commit. Before this existed the same decisions were
// spread across a hand-edited `config.toml`, a `serve --company` flag and six
// Settings sub-pages, and a freshly spun-up harness with no company dead-ended
// on "No companies are running on this host".
//
// ## What it writes, and what it only stages
//
// Everything here lands in `config.toml`, which is the *second* precedence
// layer (`env ⟵ config.toml ⟵ manifest ⟵ default`). Two consequences the UI
// has to be honest about rather than hide:
//
//   - A field the environment owns cannot be written at all. The host reports
//     `editable: false` for those and refuses the write; we render them
//     read-only with the owning layer shown, so nobody submits a change that
//     silently does nothing.
//   - Host-level fields are read once, at boot, so a change to some of them is
//     *staged* rather than applied. The host applies what it can in place (it
//     rebuilds companies for a new sign-in mode) and reports what is genuinely
//     left; the completion screen shows that answer, not a guess, and never
//     implies its own button performed the restart.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AlertTriangle, Check, Loader2, Lock, RotateCw } from "lucide-react";

import { requestCode } from "@/api/auth";
import type { OpenCompanyClient } from "@/api/client";
import {
  changedFields,
  fieldsFor,
  getSetup,
  INFERENCE_PROVIDERS,
  proposeSetupRoster,
  testInference,
  submitSetup,
  type SetupApplied,
  type SetupField,
  type SetupRoster,
  type SetupStatus,
} from "@/api/setup";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { TEAM_TONES, initials, toneFor } from "@/lib/team";
import { fieldCopy, fieldPlaceholder } from "@/lib/setup-fields";
import type { Step } from "@/components/ui/stepper";
import {
  adminEmailProblem,
  emptySetupDraft,
  jobItems,
  type SetupDraft,
} from "@/lib/company-setup";
import { cn } from "@/lib/utils";

/** The steps, in order. `fields` names the config keys each one owns. */
/**
 * The flow, question-first.
 *
 * The order is the whole design. Three cheap questions about *them* come before
 * anything about the machine, because nobody abandons "what do you sell" and
 * plenty abandon `bind`. The two asks that cost something — an address and a
 * credential — arrive fourth and fifth, after enough investment that their
 * purpose is self-evident rather than a wall on screen one.
 *
 * Everything else that used to be a step is now behind Advanced: sign-in mode,
 * brain, host and tools all have defaults that work, and a knob with a working
 * default is not a decision worth a screen.
 */
/**
 * The flow, in order.
 *
 * **Model comes first, and it did not used to.** It sat third, after the three
 * questions, on the reasoning that cheap interesting questions earn the right to
 * ask for a credential. That reasoning was sound about *motivation* and wrong
 * about *consequence*: the design pass is silent when a credential is missing or
 * bad — it falls back to a curated team — so a wrong key produced a plausible
 * company and an operator found out two screens later, if at all. The one step
 * whose failure invalidates every answer after it belongs before them.
 *
 * It is a gate, not a wall: the step can be skipped outright, and skipping is
 * what the curated fallback is for.
 */
const STEPS: readonly (Step & { fields: readonly string[] })[] = [
  { id: "power", label: "Model", fields: ["tinyhumans_api_key"] },
  { id: "business", label: "Business", fields: [] },
  { id: "account", label: "You", fields: [] },
  { id: "advanced", label: "Advanced", fields: [] },
  { id: "review", label: "Review", fields: [] },
];

/**
 * Advanced, as its own stepped flow rather than a wall of fields.
 *
 * These were four screens before the merge, and collapsing them into one
 * scrolling accordion made "advanced settings" mean "everything we could not
 * place". They are four different subjects — who may sign in, what thinks, where
 * it runs, what it can reach — and each deserves the same one-subject-per-screen
 * treatment the main flow gets. The difference is that this is opt-in, and
 * leaving it never blocks anything.
 */
const ADVANCED_GROUPS: readonly {
  id: string;
  label: string;
  title: string;
  hint: string;
  fields: readonly string[];
}[] = [
  {
    id: "signin",
    label: "Sign-in",
    title: "How people sign in",
    hint: "This applies to every company this host serves.",
    fields: ["auth_mode"],
  },
  {
    id: "host",
    label: "Host",
    title: "How this host runs",
    hint: "The address it serves on and how much room its workspace gets. Defaults are fine for a laptop.",
    fields: ["bind", "public_url", "workspace.max_blob_mb", "workspace.storage_quota_gb"],
  },
  {
    id: "tools",
    label: "Tools",
    title: "Repository access",
    hint: "Lets agents read your code and open pull requests. You can add this later.",
    fields: ["github_token"],
  },
];

/** How each sign-in mode is described, in consequences rather than mode names. */
const AUTH_MODE_COPY: Record<string, { label: string; hint: string }> = {
  email: {
    label: "Email",
    hint: "People sign in with a magic link sent to an invited address.",
  },
  wallet: {
    label: "Wallet",
    hint: "People sign in by signing a challenge with an invited wallet.",
  },
  none: {
    label: "No sign-in",
    hint: "Anyone who can reach this host is the owner. Only offered because this host is loopback-only.",
  },
};


interface Props {
  client: OpenCompanyClient;
  /** Called once setup has been applied, so the caller can re-enter the console. */
  onDone: () => void;
  /**
   * Whether the operator can leave without finishing. False on a genuine first
   * run, where there is no console to go back to.
   */
  onCancel?: () => void;
}

export function SetupWizard({ client, onDone, onCancel }: Props) {
  const [status, setStatus] = useState<SetupStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [step, setStep] = useState(0);
  const [values, setValues] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [applied, setApplied] = useState<SetupApplied | null>(null);

  /** The three answers. */
  const [draft, setDraft] = useState<SetupDraft>(emptySetupDraft);
  /** The address that will be able to sign in. */
  const [email, setEmail] = useState("");
  /** Whether the operator has been shown a problem on the current step yet. */
  const [touched, setTouched] = useState(false);
  /**
   * The model connection, as the first step leaves it.
   *
   * `provider` and `baseUrl` start from what the host already holds, so a hosted
   * operator — who has no key and cannot get one — arrives at a step that is
   * already answered and only needs testing.
   */
  // Seeded from the host once its status arrives — see the fetch effect. Not an
  // initialiser, because `status` is null until then.
  const [provider, setProvider] = useState<string>("managed");
  const [baseUrl, setBaseUrl] = useState<string>("");
  /**
   * The verdict on the credential, and the reason the step can gate on it.
   *
   * `"untested"` blocks Next; `"ok"` releases it; `"failed"` blocks with the
   * reason shown. `"skipped"` releases it too — see the skip link. There is no
   * state in which the operator cannot proceed at all: decision D3 says nobody
   * gets stuck, and a credential they cannot obtain must not be the one thing
   * that traps them.
   */
  const [tested, setTested] = useState<
    { kind: "untested" } | { kind: "testing" } | { kind: "ok"; baseUrl: string } | { kind: "failed"; error: string } | { kind: "skipped" }
  >({ kind: "untested" });
  /**
   * The team, once the host has designed one — and `null` until then.
   *
   * Held as state rather than refetched per render because the operator edits
   * it: what they approve on Review is exactly what gets built, and a second
   * pass could return a different team.
   */
  const [roster, setRoster] = useState<SetupRoster | null>(null);
  const [designing, setDesigning] = useState(false);
  const [designError, setDesignError] = useState<string | null>(null);
  /** How many teammates have landed, once the apply is building them. */
  const [built, setBuilt] = useState<number | null>(null);
  /**
   * The sign-in the wizard arranges on the operator's behalf, once the company
   * exists.
   *
   * Without this the flow forgot them at the finish line: they typed an address
   * on step four, and the console then handed them an **empty** email box with
   * no explanation — on a laptop with no mail configured, waiting for a link
   * that was never going to arrive. The wizard already knows who they are and
   * can ask the host itself.
   */
  /** Guards the hand-off against re-running; see the effect below. */
  const arranged = useRef(false);
  const [handoff, setHandoff] = useState<
    | { kind: "arranging" }
    | { kind: "link"; url: string }
    | { kind: "mailed" }
    | { kind: "open" }
    | null
  >(null);

  useEffect(() => {
    let cancelled = false;
    getSetup(client)
      .then((s) => {
        if (cancelled) return;
        setStatus(s);
        // Seed the form from what the file already holds, so an operator
        // re-running setup edits their configuration rather than a blank one.
        const seeded: Record<string, string> = {};
        for (const f of s.fields) if (f.value !== null) seeded[f.key] = f.value;
        setValues(seeded);
        // Pre-fill the model step from what the host already holds. A hosted
        // operator has a credential injected by the control plane, no key of
        // their own, and no way to get one — the step should arrive answered.
        if (s.inference.provider) setProvider(s.inference.provider);
        if (s.inference.base_url) setBaseUrl(s.inference.base_url);
      })
      .catch((err: unknown) => {
        if (!cancelled) setLoadError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  const set = useCallback((key: string, value: string) => {
    setValues((prev) => ({ ...prev, [key]: value }));
  }, []);

  /**
   * Arrange the operator's way in, the moment the company exists.
   *
   * Three outcomes, and each is said plainly rather than left to be discovered:
   *
   * - **No sign-in on this host** — nothing to arrange; the console is open.
   * - **A link we can hand over** — a loopback host with no mail transport
   *   returns the code in the response rather than mailing it, so the honest
   *   thing is to give them the link instead of pointing at an inbox that will
   *   stay empty. This is the laptop case, and it was the broken one.
   * - **Mailed** — say which address, so they know where to look and that we
   *   used the one they typed.
   *
   * Failure is not fatal: the sign-in form still works, and the button below
   * still opens the console.
   */
  useEffect(() => {
    // Guarded by a ref, not by `handoff`.
    //
    // Depending on the state this effect *sets* made it cancel itself: setting
    // `arranging` re-ran the effect, whose cleanup flipped `cancelled` on the
    // request still in flight, so the answer was always discarded and the screen
    // sat on "Getting you signed in…" forever. A ref settles before the next
    // render and is not a dependency.
    if (!applied || arranged.current) return;
    arranged.current = true;
    const company = applied.seeded_company;
    const address = email.trim();

    // No status means the read failed and we cannot tell what mode this host is
    // in; opening the console is the answer that cannot be wrong.
    if (!company || !address || !status || !requiresSignIn(status, values)) {
      setHandoff({ kind: "open" });
      return;
    }

    setHandoff({ kind: "arranging" });
    requestCode(client, company, address)
      .then((result) => {
        setHandoff(
          result.dev_code
            ? {
                kind: "link",
                url: `/login?company=${encodeURIComponent(company)}&code=${encodeURIComponent(result.dev_code)}`,
              }
            : { kind: "mailed" },
        );
      })
      .catch(() => {
        // The sign-in form still works; the button below still opens it.
        setHandoff({ kind: "open" });
      });
  }, [applied, email, client, status, values]);

  // See `changedFields`: unchanged fields are omitted, env-owned ones are never
  // sent (the host refuses them and an apply is all-or-nothing), and a secret
  // goes only when the operator typed one.
  const changed = useMemo(
    () => (status ? changedFields(status, values) : {}),
    [status, values],
  );

  const restartKeys = useMemo(() => {
    if (!status) return [];
    return Object.keys(changed).filter(
      (k) => status.fields.find((f) => f.key === k)?.requires_restart,
    );
  }, [status, changed]);

  /**
   * Ask the host to design a team, on the way into Review.
   *
   * Never throws upward: the host answers with its curated team rather than an
   * error when it cannot reach a model, so a rejection here is a genuine
   * transport failure — and even then the operator gets a roster to review,
   * because being stranded five screens in is the one outcome worse than an
   * imperfect team.
   */
  const design = useCallback(async () => {
    setDesigning(true);
    setDesignError(null);
    try {
      const proposed = await proposeSetupRoster(client, {
        industry: draft.industry,
        teamHint: draft.teamHint,
        automate: draft.automate,
        inferenceKey: values.tinyhumans_api_key || null,
      });
      // The host is contracted never to answer with an empty roster, so a
      // missing or empty one is a failure rather than a team of nobody — and
      // trusting the shape here crashed Review on `.map` of undefined.
      if (!Array.isArray(proposed?.agents) || proposed.agents.length === 0) {
        throw new Error("The host answered without a team to review.");
      }
      setRoster(proposed);
    } catch (err: unknown) {
      setDesignError(err instanceof Error ? err.message : String(err));
      setRoster(null);
    } finally {
      setDesigning(false);
    }
  }, [client, draft, values.tinyhumans_api_key]);

  const submit = useCallback(async () => {
    if (!status) return;
    // A host with no company and no designed roster would finish setup into
    // exactly the dead end this flow exists to remove: a configured instance
    // with nothing to sign in to and no way back into setup.
    if (status.companies.length === 0 && !roster?.agents.length) return;
    setSaving(true);
    setSaveError(null);
    setBuilt(roster?.agents.length ?? null);
    try {
      const result = await submitSetup(client, {
        fields: changed,
        company:
          status.companies.length === 0 && roster
            ? {
                industry: draft.industry,
                teamHint: draft.teamHint,
                automate: draft.automate,
                // As reviewed, not as proposed.
                agents: roster.agents,
                adminEmail: email.trim() || null,
              }
            : null,
      });
      setApplied(result);
    } catch (err: unknown) {
      setSaveError(err instanceof Error ? err.message : String(err));
      setBuilt(null);
    } finally {
      setSaving(false);
    }
  }, [client, status, changed, roster, draft, email]);

  if (loadError) {
    return (
      <Shell>
        <Alert variant="destructive">
          <AlertTitle>Can&apos;t read this instance&apos;s setup</AlertTitle>
          <AlertDescription>{loadError}</AlertDescription>
        </Alert>
      </Shell>
    );
  }

  if (!status) {
    return (
      <Shell>
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" /> Reading this instance…
        </div>
      </Shell>
    );
  }

  if (applied) {
    return (
      <Shell>
        <div className="space-y-4" data-testid="setup-done">
          <h1 className="text-xl font-semibold">You&apos;re set up</h1>
          <p className="text-sm text-muted-foreground">
            Written to <code className="font-mono text-xs">{applied.config_path}</code>.
          </p>
          {applied.seeded_company && (
            <p className="text-sm text-muted-foreground">
              {/* No template was chosen — the team was designed from the
                  answers, and saying otherwise would credit a menu the
                  operator never saw. */}
              Built <strong>{applied.seeded_company}</strong> with{" "}
              {roster?.agents.length ?? 0}{" "}
              {roster?.agents.length === 1 ? "teammate" : "teammates"}.
            </p>
          )}
          {/* The button below cannot restart the host — it only re-enters the
              console — so this must not read as something already handled.
              Naming the setting and the action keeps the two apart. */}
          {applied.restart_required.length > 0 && (
            <Alert>
              <RotateCw />
              <AlertTitle>
                You need to restart the host for {applied.restart_required.length} setting(s)
              </AlertTitle>
              <AlertDescription>
                <span className="block">
                  These are read once, when the host starts, so they are saved but{" "}
                  <strong>not yet in force</strong>:{" "}
                  <span className="font-mono text-xs">
                    {applied.restart_required.join(", ")}
                  </span>
                </span>
                <span className="mt-2 block">
                  Stop the <code className="font-mono text-xs">opencompany serve</code> process
                  and start it again. Opening the console now works, but with the previous
                  values for those settings.
                </span>
              </AlertDescription>
            </Alert>
          )}
          {/* What happens next, said before they have to guess it. */}
          {handoff?.kind === "arranging" && (
            <p className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" /> Getting you signed in…
            </p>
          )}

          {handoff?.kind === "mailed" && (
            <Alert data-testid="setup-handoff-mailed">
              <AlertTitle>Check your email</AlertTitle>
              <AlertDescription>
                We sent a sign-in link to{" "}
                <strong className="text-foreground">{email.trim()}</strong>. It is the only
                address that can administer this company.
              </AlertDescription>
            </Alert>
          )}

          {handoff?.kind === "link" && (
            <Alert data-testid="setup-handoff-link">
              <AlertTitle>You&apos;re ready to go in</AlertTitle>
              <AlertDescription>
                This host doesn&apos;t send mail, so there is no link to wait for — use the
                button below. You&apos;ll be signed in as{" "}
                <strong className="text-foreground">{email.trim()}</strong>.
              </AlertDescription>
            </Alert>
          )}

          {handoff?.kind === "link" ? (
            <Button
              data-testid="setup-signin"
              onClick={() => {
                window.location.href = handoff.url;
              }}
            >
              Sign in and open my company
            </Button>
          ) : (
            <Button onClick={onDone} data-testid="setup-open-console">
              {applied.restart_required.length > 0
                ? "Open the console anyway"
                : "Open the console"}
            </Button>
          )}
        </div>
      </Shell>
    );
  }

  const current = STEPS[step];
  const last = step === STEPS.length - 1;
  const needsCompany = status.companies.length === 0;
  // The one thing that must never be reachable: a configured instance with
  // nothing to sign in to and no way back into setup.
  const noRoster = needsCompany && !roster?.agents.length;

  /** Whether this step can be left, and why not when it cannot. */
  const problem = (): string | undefined => {
    // The gate. Untested is not "probably fine": the whole reason this step
    // moved to the front is that a bad credential is silent everywhere else.
    if (current.id === "power" && tested.kind !== "ok" && tested.kind !== "skipped") {
      return tested.kind === "failed"
        ? "That connection did not work. Fix it, or continue without a model."
        : "Test the connection first, or continue without a model.";
    }
    if (current.id === "business" && !draft.industry.trim()) {
      return "Tell us a little about the company first.";
    }
    if (current.id === "account" && needsCompany) {
      // Checked here rather than left to the manifest validator on the last
      // screen, which reported it as "`[users].admins` has an invalid entry"
      // after the roster had been designed — a configuration error about a
      // mistake made four steps earlier, in the language of a file the operator
      // has never seen.
      const problem = adminEmailProblem(email, requiresSignIn(status, values));
      if (problem) return problem;
    }
    return undefined;
  };

  const advance = () => {
    if (problem()) {
      setTouched(true);
      return;
    }
    setTouched(false);
    // Designing happens on the way into Review, so the wait sits between two
    // screens rather than in front of one.
    if (STEPS[step + 1]?.id === "review" && !roster && !designing) void design();
    setStep((n) => n + 1);
  };

  return (
    <Shell
      header={
        <div className="space-y-4">
          <div className="space-y-1">
            <h1 className="text-xl font-semibold tracking-tight">
              {status.complete ? "Reconfigure this instance" : "Let's build your company"}
            </h1>
            <p className="text-sm text-muted-foreground">
              {status.complete
                ? "Change what this host is configured with."
                : "A few questions, then we'll put a team together."}
            </p>
          </div>
          {/* A bar, not a numbered stepper.
              `1 Business — 2 You — 3 Model — 4 Advanced — 5 Review` is the
              language of enterprise configuration: it tells you that you are
              inside a multi-page form. This is sixty seconds long. Progress
              should be felt, and the one thing worth naming is where you are. */}
          <div className="space-y-2">
            <div className="flex gap-1" aria-hidden>
              {STEPS.map((s, i) => (
                <button
                  key={s.id}
                  type="button"
                  tabIndex={-1}
                  disabled={i >= step}
                  onClick={() => setStep(i)}
                  data-testid={`step-${s.id}`}
                  className={cn(
                    "h-1 flex-1 rounded-full transition-colors",
                    i < step && "bg-primary/70 hover:bg-primary",
                    i === step && "bg-primary",
                    i > step && "bg-border",
                  )}
                />
              ))}
            </div>
            <p className="text-xs text-muted-foreground">
              <span className="text-foreground">{current.label}</span> · step {step + 1} of{" "}
              {STEPS.length}
            </p>
          </div>
        </div>
      }
      footer={
        <div className="flex items-center justify-between gap-3">
          {onCancel ? (
            <Button variant="ghost" size="sm" onClick={onCancel}>
              Cancel
            </Button>
          ) : (
            <span />
          )}
          <div className="flex gap-2">
            <Button variant="outline" disabled={step === 0} onClick={() => setStep((n) => n - 1)}>
              Back
            </Button>
            {last ? (
              <Button
                onClick={() => void submit()}
                disabled={saving || noRoster || designing}
                data-testid="setup-finish"
              >
                {saving && <Loader2 className="animate-spin" />}
                Build my company
              </Button>
            ) : (
              <Button onClick={advance} data-testid="setup-next">
                {current.id === "advanced" ? "Looks good" : "Next"}
              </Button>
            )}
          </div>
        </div>
      }
    >
      <div className="space-y-6" data-testid="setup-wizard">
        {current.id === "business" && (
          <BusinessStep draft={draft} onChange={setDraft} onEnter={advance} />
        )}

        {current.id === "account" && (
          <AccountStep
            value={email}
            onChange={setEmail}
            onEnter={advance}
            required={needsCompany && requiresSignIn(status, values)}
          />
        )}

        {current.id === "power" && (
          <PowerStep
            status={status}
            client={client}
            provider={provider}
            onProvider={(p) => {
              setProvider(p);
              // A changed provider invalidates the verdict. Carrying a green
              // tick across a provider switch would be the worst kind of lie:
              // one the operator watched us earn.
              setTested({ kind: "untested" });
              setBaseUrl(p === status.inference.provider ? (status.inference.base_url ?? "") : "");
            }}
            baseUrl={baseUrl}
            onBaseUrl={(v) => {
              setBaseUrl(v);
              setTested({ kind: "untested" });
            }}
            value={values.tinyhumans_api_key ?? ""}
            onChange={(v) => {
              set("tinyhumans_api_key", v);
              setTested({ kind: "untested" });
            }}
            tested={tested}
            onTested={setTested}
          />
        )}

        {current.id === "advanced" && (
          <AdvancedStep status={status} values={values} set={set} />
        )}

        {current.id === "review" && (
          <ReviewStep
            designing={designing}
            designError={designError}
            roster={roster}
            onRoster={setRoster}
            onRetry={() => void design()}
            changed={changed}
            restartKeys={restartKeys}
            status={status}
            email={email}
            built={built}
          />
        )}

        {problem() && touched && (
          <p className="text-sm text-destructive" data-testid="setup-problem">
            {problem()}
          </p>
        )}

        {saveError && (
          <Alert variant="destructive">
            <AlertTriangle />
            <AlertTitle>That didn&apos;t apply</AlertTitle>
            <AlertDescription>{saveError}</AlertDescription>
          </Alert>
        )}
      </div>
    </Shell>
  );
}

/**
 * Whether this host will ask anyone to sign in.
 *
 * On `none` there is nobody to invite and an address would be a field with no
 * consequence; on every other mode the address is the only thing standing
 * between the operator and a company they cannot get into.
 */
function requiresSignIn(status: SetupStatus, values: Record<string, string>): boolean {
  const chosen =
    values.auth_mode ?? status.fields.find((f) => f.key === "auth_mode")?.value ?? "email";
  return chosen !== "none";
}

// ---------------------------------------------------------------------------
// Steps
// ---------------------------------------------------------------------------
function SignInStep({
  status,
  value,
  onChange,
}: {
  status: SetupStatus;
  value: string;
  onChange: (v: string) => void;
}) {
  const field = status.fields.find((f) => f.key === "auth_mode");
  const locked = field !== undefined && !field.editable;

  return (
    <div className="space-y-3">
      {/* No heading here: inside Advanced the group already carries one, and a
          second would read as a new section rather than the same one. */}
      {locked && <LayerLock />}

      <div className="space-y-2">
        {status.auth_modes.map((mode) => {
          const copy = AUTH_MODE_COPY[mode] ?? { label: mode, hint: "" };
          const active = (value || field?.value) === mode;
          return (
            <button
              key={mode}
              type="button"
              disabled={locked}
              onClick={() => onChange(mode)}
              data-testid={`auth-mode-${mode}`}
              aria-pressed={active}
              className={cn(
                "w-full rounded-lg border p-3 text-left transition-colors",
                !locked && "hover:bg-muted",
                active && "border-primary bg-muted",
                locked && "opacity-60",
              )}
            >
              <div className="text-sm font-medium">{copy.label}</div>
              <div className="mt-0.5 text-xs text-muted-foreground">{copy.hint}</div>
            </button>
          );
        })}
      </div>

      {!status.auth_modes.includes("none") && (
        <p className="text-xs text-muted-foreground">
          &ldquo;No sign-in&rdquo; isn&apos;t offered because this host binds a routable address,
          where it would serve an unauthenticated admin console to anyone who can reach it.
        </p>
      )}
    </div>
  );
}

function FieldRow({
  field,
  value,
  onChange,
}: {
  field: SetupField;
  value: string;
  onChange: (v: string) => void;
}) {
  const locked = !field.editable;
  const copy = fieldCopy(field.key);
  return (
    <div>
      {/* Words first, key second.
          The label used to *be* the key, which turned this screen into a
          `.toml` file with input boxes. The key is still here — small and
          monospaced — because whoever opened Advanced is often the person who
          will next edit that file by hand. */}
      <Label htmlFor={field.key} className="text-base font-medium leading-snug">
        {copy.label}
      </Label>
      {copy.hint && (
        <p className="mt-0.5 text-sm leading-snug text-muted-foreground">{copy.hint}</p>
      )}
      <Input
        id={field.key}
        data-testid={`field-${field.key}`}
        value={locked ? (field.value ?? "") : value}
        disabled={locked}
        type={field.secret ? "password" : "text"}
        placeholder={fieldPlaceholder(field)}
        className="mt-2.5"
        onChange={(e) => onChange(e.target.value)}
      />
      <div className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1">
        <code className="font-mono text-2xs text-muted-foreground/70">{field.key}</code>
        {/* Only where it is true *and* actionable: a locked field cannot be
            changed here at all, so telling its owner about a restart is noise
            about work they are not doing. */}
        {field.requires_restart && !locked && (
          <span className="text-2xs text-muted-foreground/70">· needs a restart</span>
        )}
      </div>
      {locked && <div className="mt-1.5">{<LayerLock />}</div>}
    </div>
  );
}

/**
 * Why a field can't be edited.
 *
 * Worth its own component because the reason is not obvious and the failure it
 * prevents is silent: `config.toml` sits *below* the environment in precedence,
 * so writing an env-owned field would produce a saved value that the next boot
 * ignores. Saying so beats disabling an input with no explanation.
 */
function LayerLock() {
  return (
    <p className="flex items-start gap-1.5 text-xs text-muted-foreground">
      <Lock className="mt-0.5 size-3 shrink-0" />
      <span>
        Set by an environment variable on this host, which outranks{" "}
        <code className="font-mono">config.toml</code>. Change it where the host is deployed.
      </span>
    </p>
  );
}

/**
 * A card, centred — not an application shell.
 *
 * The previous version put edge-to-edge header and footer rules across the
 * viewport, which is the chrome of an app you live in. This is a task you pass
 * through once, and dressing it as an admin panel made a five-field flow look
 * like a broken settings page: rules running off both edges, a narrow column
 * marooned in the middle of them, and — because the content band was `flex-1` —
 * most of a thousand pixels of nothing between the last field and the buttons.
 *
 * One bounded card instead, centred on both axes, with its own header and its
 * own actions. It grows with its content and stops at 88vh, after which the
 * middle scrolls and the header and actions stay put. Nothing floats, nothing
 * stretches, and the eye has one object to land on.
 */
function Shell({
  header,
  footer,
  children,
}: {
  header?: React.ReactNode;
  footer?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="flex min-h-screen items-center justify-center bg-background p-4 sm:p-6">
      <div className="flex max-h-[88vh] w-full max-w-[34rem] flex-col overflow-hidden rounded-2xl border bg-card shadow-xl">
        {header && <div className="shrink-0 border-b px-6 py-5">{header}</div>}
        <div className="min-h-0 flex-1 overflow-y-auto px-6 py-6">{children}</div>
        {footer && <div className="shrink-0 border-t px-6 py-4">{footer}</div>}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// The question screens
// ---------------------------------------------------------------------------

function AccountStep({
  value,
  onChange,
  onEnter,
  required,
}: {
  value: string;
  onChange: (v: string) => void;
  onEnter: () => void;
  required: boolean;
}) {
  return (
    <div>
      {/* Same rhythm as every other question: the heading and its hint are one
          sentence, and the gap belongs before the field. */}
      <Label
        htmlFor="setup-email"
        className="text-base font-medium leading-snug"
        data-testid="setup-question"
      >
        What&apos;s your email?
      </Label>
      <p className="mt-0.5 text-sm leading-snug text-muted-foreground">
        {required
          ? "This is how you sign back in, and the only address that can administer the company."
          : "Optional on this host — you chose no sign-in, so anyone who can reach it is the owner."}
      </p>
      <Input
        id="setup-email"
        autoFocus
        type="email"
        value={value}
        placeholder="you@example.com"
        data-testid="setup-field-email"
        className="mt-2.5"
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") onEnter();
        }}
      />
    </div>
  );
}

/**
 * The credential, framed as what it buys rather than as what it is.
 *
 * Fifth, not first. By now the operator has described their business and can
 * see what the key is *for*; an API-key field on screen one is a wall in front
 * of a product nobody has seen yet. Skipping is a first-class answer, and the
 * copy says exactly what it costs.
 */
function PowerStep({
  status,
  client,
  provider,
  onProvider,
  baseUrl,
  onBaseUrl,
  value,
  onChange,
  tested,
  onTested,
}: {
  status: SetupStatus;
  client: OpenCompanyClient;
  provider: string;
  onProvider: (p: string) => void;
  baseUrl: string;
  onBaseUrl: (v: string) => void;
  value: string;
  onChange: (v: string) => void;
  tested: TestState;
  onTested: (t: TestState) => void;
}) {
  const field = status.fields.find((f) => f.key === "tinyhumans_api_key");
  const locked = field !== undefined && !field.editable;
  const spec = INFERENCE_PROVIDERS.find((p) => p.id === provider) ?? INFERENCE_PROVIDERS[0];
  /** Whether the operator asked to supply their own key over the host's. */
  const [override, setOverride] = useState(false);
  // The house already holds one, and this operator may have no way to get their
  // own. The key box is then optional rather than the point of the screen.
  const onTheHouse = status.inference.ready && provider === status.inference.provider;

  const run = async () => {
    onTested({ kind: "testing" });
    try {
      const result = await testInference(client, {
        provider,
        key: value.trim() || null,
        baseUrl: baseUrl.trim() || null,
      });
      onTested(
        result.ok
          ? { kind: "ok", baseUrl: result.baseUrl }
          : { kind: "failed", error: result.error ?? "Could not reach the provider." },
      );
    } catch (err: unknown) {
      onTested({
        kind: "failed",
        error: err instanceof Error ? err.message : String(err),
      });
    }
  };

  return (
    <div className="space-y-7">
      <div>
        <Label className="text-base font-medium leading-snug" data-testid="setup-question">
          What should your team think with?
        </Label>
        <p className="mt-0.5 text-sm leading-snug text-muted-foreground">
          {onTheHouse
            ? "This host already has a model. Test it and carry on — you don't need a key of your own."
            : "Your teammates need a model to work. We'll check it reaches before going any further."}
        </p>

        {/* Cards, not a select. Four options with a sentence each is a choice
            someone can make without knowing the vocabulary first — a dropdown
            of slugs assumes they already do. */}
        <div className="mt-3 grid gap-2" role="radiogroup" aria-label="Model provider">
          {INFERENCE_PROVIDERS.map((option) => (
            <button
              key={option.id}
              type="button"
              role="radio"
              aria-checked={option.id === provider}
              data-testid={`setup-provider-${option.id}`}
              onClick={() => onProvider(option.id)}
              className={cn(
                "rounded-lg border p-3 text-left transition-colors",
                option.id === provider
                  ? "border-primary bg-primary/5"
                  : "hover:border-input hover:bg-muted/50",
              )}
            >
              <div className="flex items-center gap-2">
                <span className="text-sm font-medium">{option.label}</span>
                {status.inference.ready && option.id === status.inference.provider && (
                  <Badge variant="secondary">Already set up</Badge>
                )}
              </div>
              <p className="mt-0.5 text-sm leading-snug text-muted-foreground">{option.hint}</p>
            </button>
          ))}
        </div>
      </div>

      {spec.needsUrl && (
        <div>
          <Label htmlFor="setup-base-url" className="text-base font-medium leading-snug">
            Endpoint
          </Label>
          <p className="mt-0.5 text-sm leading-snug text-muted-foreground">
            The base URL of the chat API, ending in <code>/v1</code>.
          </p>
          <Input
            id="setup-base-url"
            value={baseUrl}
            placeholder={provider === "ollama" ? "http://127.0.0.1:11434/v1" : "https://…/v1"}
            data-testid="setup-field-base-url"
            className="mt-2.5"
            onChange={(e) => onBaseUrl(e.target.value)}
          />
        </div>
      )}

      {spec.needsKey && (
        <div>
          <Label htmlFor="setup-key" className="text-base font-medium leading-snug">
            API key
          </Label>

          {/* Already configured is a **resolved state**, not an empty field.
              This was an empty password box with "Using this host's key" as grey
              placeholder text, and it read exactly like an unanswered question —
              the one impression it must not give, because on a hosted tenant the
              operator has no key to put there and nothing is wrong.

              There is no value to pre-fill with, and that is deliberate: the host
              never sends a credential to a browser, not even masked. `GET
              /api/v1/setup` reports a secret's *presence*, never its bytes. So
              the honest fix is to stop drawing an input at all and state the
              fact, with a way out for someone who wants their own key. */}
          {onTheHouse && !override ? (
            <div className="mt-2 flex items-center justify-between gap-3 rounded-lg border bg-muted/40 p-3">
              <div className="flex items-center gap-2 text-sm">
                <Check className="size-4 text-status-done-text" />
                <span data-testid="setup-key-on-the-house">
                  Using this host&apos;s key
                </span>
              </div>
              <button
                type="button"
                onClick={() => setOverride(true)}
                data-testid="setup-key-override"
                className="shrink-0 text-sm text-muted-foreground underline underline-offset-4 hover:text-foreground"
              >
                Use my own
              </button>
            </div>
          ) : (
            <>
              <p className="mt-0.5 text-sm leading-snug text-muted-foreground">
                {onTheHouse
                  ? "This replaces the host's key for this company."
                  : "Used to test the connection now, and saved when you finish."}
              </p>

              {locked && (
                <div className="mt-2.5">
                  <LayerLock />
                </div>
              )}

              <Input
                id="setup-key"
                autoFocus
                type="password"
                value={value}
                disabled={locked}
                placeholder="sk-…"
                data-testid="setup-field-key"
                className="mt-2.5"
                onChange={(e) => onChange(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void run();
                }}
              />
              {onTheHouse && (
                <button
                  type="button"
                  onClick={() => {
                    setOverride(false);
                    onChange("");
                  }}
                  data-testid="setup-key-revert"
                  className="mt-2 text-sm text-muted-foreground underline underline-offset-4 hover:text-foreground"
                >
                  Go back to using this host&apos;s key
                </button>
              )}
            </>
          )}
        </div>
      )}

      <div className="space-y-2">
        <Button
          type="button"
          variant={tested.kind === "ok" ? "outline" : "default"}
          disabled={tested.kind === "testing"}
          onClick={() => void run()}
          data-testid="setup-test-connection"
        >
          {tested.kind === "testing" ? (
            <>
              <Loader2 className="size-4 animate-spin" />
              Testing…
            </>
          ) : tested.kind === "ok" ? (
            "Test again"
          ) : (
            "Test connection"
          )}
        </Button>

        {/* The verdict names the endpoint it reached. A tick earned against the
            default endpoint, on a host where the operator meant to point
            somewhere else, is worse than no tick — it is a wrong answer they
            watched us produce. */}
        {tested.kind === "ok" && (
          <p className="text-sm leading-snug text-status-done-text" data-testid="setup-test-ok">
            Reached {tested.baseUrl} and got a reply.
          </p>
        )}
        {tested.kind === "failed" && (
          <Alert variant="destructive" data-testid="setup-test-failed">
            <AlertTriangle />
            <AlertTitle>That didn&apos;t connect</AlertTitle>
            <AlertDescription>{tested.error}</AlertDescription>
          </Alert>
        )}
      </div>

      {/* Nobody gets stuck (decision D3). A hosted operator with no key must not
          be trapped behind a credential they cannot obtain — and the curated
          team exists precisely for this path. Stated plainly, as a consequence
          rather than a warning, so the choice is informed rather than scary. */}
      {tested.kind !== "ok" && (
        <button
          type="button"
          onClick={() => onTested({ kind: "skipped" })}
          data-testid="setup-skip-model"
          className="text-sm text-muted-foreground underline underline-offset-4 hover:text-foreground"
        >
          Continue without a model — you&apos;ll get a standard team for your
          industry, and can add a key later
        </button>
      )}
      {tested.kind === "skipped" && (
        <p className="text-sm leading-snug text-muted-foreground" data-testid="setup-skipped">
          Carrying on without a model. Your team will be a standard one for your
          industry rather than designed from your answers.
        </p>
      )}
    </div>
  );
}

/** The connection verdict. See `tested` on the wizard for why each state exists. */
type TestState =
  | { kind: "untested" }
  | { kind: "testing" }
  | { kind: "ok"; baseUrl: string }
  | { kind: "failed"; error: string }
  | { kind: "skipped" };

// ---------------------------------------------------------------------------
// Review, and the team as reviewed
// ---------------------------------------------------------------------------

/**
 * The team, before it exists.
 *
 * The screen that earns the ownership. People value what they had a hand in
 * shaping, and this is the honest place to catch a wrong guess — while it is
 * still four rows in a browser rather than six records on a host.
 *
 * It also says where the team came from, in a sentence. An operator shown a
 * curated roster with no indication assumes a model read their answers and
 * produced it, and judges the product on a team it never designed.
 */
function ReviewStep({
  designing,
  designError,
  roster,
  onRoster,
  onRetry,
  changed,
  restartKeys,
  status,
  email,
  built,
}: {
  designing: boolean;
  designError: string | null;
  roster: SetupRoster | null;
  onRoster: (roster: SetupRoster) => void;
  onRetry: () => void;
  changed: Record<string, string | null>;
  restartKeys: string[];
  status: SetupStatus;
  email: string;
  /** Non-null once the apply is building, so the button reads as progress. */
  built: number | null;
}) {
  if (designing) {
    return (
      <div className="flex flex-col items-center gap-3 py-12" data-testid="setup-designing">
        <Loader2 className="size-6 animate-spin text-primary" />
        {/* Not "Designing your team…". The console cannot know which path the
            host will take, and on a laptop with no credential nothing designs
            anything — the curated team comes back in milliseconds under a word
            that claimed a model had read their answers. This is true either
            way; the review screen then says which actually happened. */}
        <p className="text-sm text-muted-foreground">Putting your team together…</p>
      </div>
    );
  }

  if (designError || !roster) {
    return (
      <Alert variant="destructive" data-testid="setup-design-error">
        <AlertTriangle />
        <AlertTitle>We couldn&apos;t design a team</AlertTitle>
        <AlertDescription className="space-y-2">
          <p>{designError ?? "The host returned nothing to review."}</p>
          <Button size="sm" variant="outline" onClick={onRetry}>
            Try again
          </Button>
        </AlertDescription>
      </Alert>
    );
  }

  const drop = (index: number) =>
    onRoster({ ...roster, agents: roster.agents.filter((_, i) => i !== index) });

  const rename = (index: number, role: string) =>
    onRoster({
      ...roster,
      agents: roster.agents.map((a, i) => (i === index ? { ...a, role } : a)),
    });

  return (
    <div className="space-y-4" data-testid="setup-review">
      <div>
        <h2 className="text-base font-medium leading-snug">Your team</h2>
        <p className="mt-0.5 text-sm leading-snug text-muted-foreground">
          {roster.source === "model"
            ? "Built from what you told us. Rename or drop anyone — you can add more later."
            : roster.reason === "not_designable"
              ? "A standard team for your industry — there wasn't enough in your answers to design one around. Go back and say more about the business, or rename and drop anyone here."
              : "A solid standard team for your industry — we couldn't reach a model to tailor it. Rename or drop anyone, and add a key later to redesign."}
        </p>
      </div>

      {/* People, not form rows.
          This was five bare text inputs stacked in a column, which is what a
          settings page looks like — and this is the one screen in the product
          where someone meets their company for the first time. The field is
          still there and still the first thing focus lands on; it just stops
          announcing itself as a form until you go to use it. */}
      <ul className="divide-y rounded-xl border" data-testid="setup-review-list">
        {roster.agents.map((agent, i) => (
          <li
            key={`${agent.role}-${i}`}
            className="group flex items-center gap-3 p-3"
            data-testid="setup-review-agent"
          >
            <span
              aria-hidden
              className={cn(
                "flex size-9 shrink-0 items-center justify-center rounded-full text-xs font-medium",
                TEAM_TONES[toneFor(agent.role)] ?? TEAM_TONES.sky,
              )}
            >
              {initials(agent.name || agent.role)}
            </span>
            <div className="min-w-0 flex-1">
              <Input
                value={agent.role}
                aria-label={`Role for ${agent.role}`}
                data-testid="setup-review-role"
                onChange={(e) => rename(i, e.target.value)}
                className="h-7 border-transparent bg-transparent px-1 font-medium shadow-none hover:border-input focus-visible:border-input"
              />
              <p className="truncate px-1 text-xs text-muted-foreground">{agent.description}</p>
            </div>
            <Button
              size="sm"
              variant="ghost"
              onClick={() => drop(i)}
              aria-label={`Remove ${agent.role}`}
              data-testid="setup-review-remove"
              className="shrink-0 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
            >
              Remove
            </Button>
          </li>
        ))}
      </ul>

      {roster.agents.length === 0 && (
        <Alert>
          <AlertTriangle />
          <AlertTitle>That&apos;s everyone gone</AlertTitle>
          <AlertDescription>
            A company needs at least one teammate. Add one back, or start again.
          </AlertDescription>
        </Alert>
      )}

      {/* What the host checked, reported whichever way it came out.
          The checklist is the operator's own words, split by the host, and the
          verdict is set maths over that list — not the design pass's opinion of
          its own work. Reporting only the good case would make this decoration;
          the gap is the half worth showing, because it is the one they can do
          something about. */}
      {roster.source === "model" && (roster.jobs?.length ?? 0) > 0 && (
        <div
          className="rounded-lg border p-3 text-sm"
          data-testid="setup-coverage"
        >
          {(roster.uncovered?.length ?? 0) === 0 ? (
            <p className="text-muted-foreground">
              Every job you listed has an owner on this team.
            </p>
          ) : (
            <>
              <p className="font-medium">Nobody owns this yet</p>
              <ul className="mt-1.5 space-y-1 text-muted-foreground">
                {roster.uncovered?.map((job) => (
                  <li key={job} data-testid="setup-uncovered-job">
                    {job}
                  </li>
                ))}
              </ul>
              <p className="mt-2 text-sm leading-snug text-muted-foreground">
                You can still continue — add someone for it here, or later from
                the team page.
              </p>
            </>
          )}
        </div>
      )}

      {/* Stated, not asked. Nobody five screens in can answer a governance
          question; they can recognise a sentence and change it in Advanced. */}
      <div className="rounded-lg border border-dashed p-3 text-sm text-muted-foreground">
        {/* What they can do on day one, said before they find out by trying.
            A roster reads as a set of capabilities: "Social Media Manager —
            owns posting and engagement" is taken to mean it can post. It
            cannot, yet, and nothing on this screen used to say so. Every
            designed teammate starts with the workspace and nothing outward,
            because reaching a real account needs an account connected first —
            an act only a person can perform, and one there has been no
            opportunity to perform yet.

            This is the same failure as the twelve invented teammates that used
            to render here: offering something the host cannot honour. The fix
            is the sentence, not a wider tool grant. */}
        <p data-testid="setup-reach">
          Your team starts with its own workspace — reading, writing, drafting.
          Posting, emailing and anything else that touches an outside account
          needs that account connected first, from Settings.
        </p>
        <p className="mt-1">
          Anything that leaves the company — sending, publishing, spending — waits
          for you until you say otherwise.
        </p>
        {email.trim() && (
          <p className="mt-1">
            You&apos;ll sign in as <span className="font-medium text-foreground">{email.trim()}</span>.
          </p>
        )}
      </div>

      {built !== null && (
        <p className="text-sm text-muted-foreground" data-testid="setup-building">
          Building {built} {built === 1 ? "teammate" : "teammates"}…
        </p>
      )}

      {(Object.keys(changed).length > 0 || restartKeys.length > 0) && (
        <details className="rounded-lg border p-3">
          <summary className="cursor-pointer text-sm font-medium">
            Settings this will write ({Object.keys(changed).length})
          </summary>
          <ul className="mt-2 space-y-1 text-xs text-muted-foreground">
            {Object.keys(changed).map((key) => (
              <li key={key}>
                <code className="font-mono">{key}</code>
                {restartKeys.includes(key) && " — takes effect after a restart"}
              </li>
            ))}
          </ul>
        </details>
      )}

      {status.companies.length > 0 && (
        <p className="text-xs text-muted-foreground">
          This host already serves a company, so no new one will be created.
        </p>
      )}
    </div>
  );
}

/**
 * Advanced, as a step of its own.
 *
 * It was a disclosure hanging under the footer, which made the page appear to
 * end twice and made these four subjects feel like a cupboard rather than part
 * of setting up. They are a step now, in the same sequence as everything else,
 * and skippable by pressing on — which is what "advanced" should mean: present
 * and passed over, not hidden and hunted for.
 *
 * All four groups on one screen, for the same reason the three questions are:
 * they are short, related, and splitting them would be four more Next presses
 * for settings most people will never touch.
 */
function AdvancedStep({
  status,
  values,
  set,
}: {
  status: SetupStatus;
  values: Record<string, string>;
  set: (key: string, value: string) => void;
}) {
  return (
    <div className="space-y-7" data-testid="setup-advanced">
      <div>
        <h2 className="text-base font-medium leading-snug" data-testid="setup-question">
          Anything you want to change?
        </h2>
        <p className="mt-0.5 text-sm leading-snug text-muted-foreground">
          Every one of these already has a working default. Press on if none of
          it matters to you — written to{" "}
          <code className="font-mono text-xs">{status.config_path}</code>.
        </p>
      </div>

      {ADVANCED_GROUPS.map((group) => (
        // Each subject is its own bounded card. Four sections running together
        // down one scroll is what made this read as a dump — nothing told you
        // where "how people sign in" ended and "where the thinking runs" began.
        <section key={group.id} className="rounded-xl border">
          <div className="border-b px-4 py-3">
            <h3 className="text-base font-medium leading-snug">{group.title}</h3>
            <p className="mt-0.5 text-sm leading-snug text-muted-foreground">{group.hint}</p>
          </div>
          <div className="space-y-5 px-4 py-4">
            {group.id === "signin" && (
              <SignInStep
                status={status}
                value={values.auth_mode ?? ""}
                onChange={(v) => set("auth_mode", v)}
              />
            )}
            {fieldsFor(status, group.fields)
              // `auth_mode` is chosen with the three cards above. Rendering its
              // raw input underneath offered the same setting twice, and the
              // second one looked like the real control because it was a field.
              .filter((f) => !(group.id === "signin" && f.key === "auth_mode"))
              .map((f) => (
                <FieldRow
                  key={f.key}
                  field={f}
                  value={values[f.key] ?? ""}
                  onChange={(v) => set(f.key, v)}
                />
              ))}
          </div>
        </section>
      ))}
    </div>
  );
}

/**
 * Everything we ask about the business, on one screen.
 *
 * These were three screens, one field each. That is the right shape when a
 * question needs the operator's whole attention, and the wrong one here: the
 * three are a single thought — *what you do, who you want, what you want off
 * your plate* — and splitting them made two of the three feel like padding
 * between the interesting one and the end.
 *
 * Read together they also answer each other. Seeing "what do you want to
 * automate" while the industry answer is still on screen is what makes someone
 * write "order dispatch" rather than "operations".
 */
function BusinessStep({
  draft,
  onChange,
  onEnter,
}: {
  draft: SetupDraft;
  onChange: (update: (d: SetupDraft) => SetupDraft) => void;
  onEnter: () => void;
}) {
  const jobs = jobItems(draft.automate);

  return (
    <div className="space-y-7">
      <div>
        {/* Label and hint are one sentence, so they sit tight together; the
            breathing room belongs *before the field*, not inside the sentence.
            A uniform `space-y` gave all three the same gap and the question
            read as three unrelated lines. */}
        <Label htmlFor="setup-industry" className="text-base font-medium leading-snug">
          What kind of company are you setting up?
        </Label>
        <p className="mt-0.5 text-sm leading-snug text-muted-foreground">
          A sentence is plenty. What you sell, or what you do.
        </p>
        <Input
          id="setup-industry"
          autoFocus
          value={draft.industry}
          placeholder="e.g. E-commerce — I sell homeware online"
          data-testid="setup-field-industry"
          className="mt-2.5"
          onChange={(e) => onChange((d) => ({ ...d, industry: e.target.value }))}
          onKeyDown={(e) => {
            if (e.key === "Enter") onEnter();
          }}
        />
      </div>

      <div>
        <Label htmlFor="setup-automate" className="text-base font-medium leading-snug">
          What are you trying to automate?
        </Label>
        <p className="mt-0.5 text-sm leading-snug text-muted-foreground">
          List whatever comes to mind. This is what your team gets built around.
        </p>
        <Textarea
          id="setup-automate"
          className="mt-2.5 min-h-0"
          rows={2}
          value={draft.automate}
          placeholder="e.g. Meta ads, order dispatch, daily sales reports"
          data-testid="setup-field-automate"
          onChange={(e) => onChange((d) => ({ ...d, automate: e.target.value }))}
        />
        {/* Their own words, split the way the host splits them — not a guess at
            what they meant. This is the checklist the roster is judged against,
            so showing it here is what makes a bad split fixable by the person
            who typed it rather than a silent input to a prompt. */}
        {jobs.length > 1 && (
          <p className="mt-2 text-sm leading-snug text-muted-foreground" data-testid="setup-jobs">
            {jobs.length} jobs — each one needs an owner on your team.
          </p>
        )}
      </div>

      <div>
        <Label htmlFor="setup-teamHint" className="text-base font-medium leading-snug">
          Anyone in particular you need on the team?
          <span className="ml-1.5 text-sm font-normal text-muted-foreground">Optional</span>
        </Label>
        <p className="mt-0.5 text-sm leading-snug text-muted-foreground">
          We&apos;ll suggest a team either way — this just adds to it.
        </p>
        <Textarea
          id="setup-teamHint"
          className="mt-2.5 min-h-0"
          rows={2}
          value={draft.teamHint}
          placeholder="e.g. someone chasing the customers who go quiet"
          data-testid="setup-field-teamHint"
          onChange={(e) => onChange((d) => ({ ...d, teamHint: e.target.value }))}
        />
      </div>
    </div>
  );
}
