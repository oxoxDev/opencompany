import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowLeft, Check, Loader2, Sparkles, Users } from "lucide-react";

import type { OpenCompanyClient } from "@/api/client";
import { proposeRoster, type ProposedAgent } from "@/api/company-setup";
import { Button } from "@/components/ui/button";
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
import { Textarea } from "@/components/ui/textarea";
import {
  SETUP_STEPS,
  buildOutLabel,
  draftIsSubmittable,
  emptySetupDraft,
  stepProblem,
  type SetupDraft,
} from "@/lib/company-setup";
import { cn } from "@/lib/utils";
import { initials, toneFor } from "@/lib/team";
import { TEAM_TONES } from "@/lib/team";

/**
 * How long each created agent stays on screen before the next write starts.
 *
 * The one place in this product where slower is better. The work can finish in
 * well under a second on a warm local host, and a build-out that flashes past
 * reads as a form submitting — which is exactly the feeling this screen exists
 * to replace. Paced, it reads as a company being assembled.
 *
 * Small enough that six agents still land inside four seconds, so nobody is
 * waiting on theatre.
 */
const REVEAL_MS = 450;

type Phase =
  | { kind: "asking"; step: number }
  | { kind: "thinking" }
  | { kind: "building"; agents: ProposedAgent[]; created: number; fallback: boolean }
  | { kind: "done"; agents: ProposedAgent[]; fallback: boolean }
  | { kind: "failed"; reason: string };

/**
 * First-run company setup: three questions, then a team built on the host
 * (docs/spec/runtime/company-setup.md).
 *
 * Owns the whole flow because the flow is one decision from the operator's point
 * of view — they answer, they watch, they land in a staffed company. Splitting
 * the questions from the build-out would put a route boundary in the middle of
 * the moment the feature exists to create.
 *
 * ## The build-out creates one agent at a time, deliberately
 *
 * Each `addTeamMember` is awaited in turn and revealed as it lands. The host has
 * no batch create and does not need one: sequential writes are what let this
 * screen narrate itself with no event plumbing, and they mean a browser closed
 * halfway leaves a company with three real teammates rather than a broken one.
 */
export function SetupDialog({
  open,
  client,
  company,
  onSkip,
  onDone,
}: {
  open: boolean;
  client: OpenCompanyClient;
  company: string | null;
  /** "I'll do this later" — records the skip and closes. */
  onSkip: () => void;
  /** Setup finished; the caller refreshes the roster and hands off to the tour. */
  onDone: () => void;
}) {
  const [draft, setDraft] = useState<SetupDraft>(emptySetupDraft);
  const [phase, setPhase] = useState<Phase>({ kind: "asking", step: 0 });
  const [touched, setTouched] = useState(false);
  /**
   * Guards the build-out against a second run.
   *
   * StrictMode double-invokes effects and the build-out effect creates
   * teammates, so without this a development build would staff every company
   * twice. A ref rather than state: it must be set before the first await, not
   * on the next render.
   */
  const building = useRef(false);

  const step = phase.kind === "asking" ? SETUP_STEPS[phase.step] : undefined;
  const problem = useMemo(
    () => (step && touched ? stepProblem(step, draft) : undefined),
    [step, touched, draft],
  );

  const set = useCallback((key: keyof SetupDraft, value: string) => {
    setDraft((prev) => ({ ...prev, [key]: value }));
  }, []);

  const submit = useCallback(async () => {
    setPhase({ kind: "thinking" });
    try {
      const proposal = await proposeRoster(client, company, draft);
      if (!proposal.agents.length) {
        // The host is contracted never to return an empty roster; treat it as a
        // failure rather than showing a build-out with nothing in it.
        setPhase({
          kind: "failed",
          reason: "Your company came back without a team. Try again in a moment.",
        });
        return;
      }
      setPhase({
        kind: "building",
        agents: proposal.agents,
        created: 0,
        fallback: proposal.source === "fallback",
      });
    } catch {
      // A real transport or auth failure — the host answers with its reference
      // team rather than an error for anything less.
      setPhase({
        kind: "failed",
        reason: "We couldn't reach your company. Check the connection and try again.",
      });
    }
  }, [client, company, draft]);

  const next = useCallback(() => {
    if (phase.kind !== "asking") return;
    const current = SETUP_STEPS[phase.step];
    if (stepProblem(current, draft)) {
      setTouched(true);
      return;
    }
    setTouched(false);
    if (phase.step + 1 < SETUP_STEPS.length) {
      setPhase({ kind: "asking", step: phase.step + 1 });
    } else if (draftIsSubmittable(draft)) {
      void submit();
    }
  }, [phase, draft, submit]);

  const back = useCallback(() => {
    if (phase.kind !== "asking" || phase.step === 0) return;
    setTouched(false);
    setPhase({ kind: "asking", step: phase.step - 1 });
  }, [phase]);

  // The build-out: create each proposed agent in turn, revealing as we go.
  useEffect(() => {
    if (phase.kind !== "building" || building.current) return;
    building.current = true;
    let cancelled = false;
    const agents = phase.agents;

    (async () => {
      const fallback = phase.fallback;
      for (let i = 0; i < agents.length; i++) {
        const agent = agents[i];
        try {
          await client.addTeamMember(
            { name: agent.name, role: agent.role, description: agent.description },
            company,
          );
        } catch {
          // One refused write must not abandon the rest: a company with five of
          // six teammates is a working company, and the operator can add the
          // last by hand. Silent by design — a toast per failure would turn a
          // celebration screen into an error list.
        }
        if (cancelled) return;
        setPhase({ kind: "building", agents, created: i + 1, fallback });
        if (i + 1 < agents.length) {
          await new Promise((resolve) => setTimeout(resolve, REVEAL_MS));
          if (cancelled) return;
        }
      }
      await new Promise((resolve) => setTimeout(resolve, REVEAL_MS * 1.5));
      if (!cancelled) setPhase({ kind: "done", agents, fallback });
    })();

    return () => {
      cancelled = true;
    };
    // Keyed on the phase *kind* rather than the whole phase: this effect sets
    // `created` on the same phase, and depending on it would re-enter the loop
    // on every reveal.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [phase.kind, client, company]);

  return (
    // Blocking: the no-op `onOpenChange` is what makes it so. Base UI drives
    // every dismissal — Esc, the backdrop, the close button — through this
    // callback, so ignoring it leaves the only way out the explicit "I'll do
    // this later" below, which records the skip. A silent dismiss would leave
    // the dialog reopening on every load.
    <Dialog open={open} onOpenChange={() => {}}>
      <DialogContent
        showCloseButton={false}
        className="sm:max-w-lg"
        data-testid="setup-dialog"
      >
        {phase.kind === "asking" && step && (
          <>
            <DialogHeader>
              <StepDots total={SETUP_STEPS.length} at={phase.step} />
              <DialogTitle data-testid="setup-question">{step.question}</DialogTitle>
              <DialogDescription>{step.hint}</DialogDescription>
            </DialogHeader>
            <div className="grid gap-2 py-2">
              <Label htmlFor={`setup-${step.key}`} className="sr-only">
                {step.question}
              </Label>
              {step.key === "industry" ? (
                <Input
                  id={`setup-${step.key}`}
                  autoFocus
                  value={draft[step.key]}
                  placeholder={step.placeholder}
                  data-testid={`setup-field-${step.key}`}
                  onChange={(e) => set(step.key, e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") next();
                  }}
                />
              ) : (
                <Textarea
                  id={`setup-${step.key}`}
                  autoFocus
                  rows={3}
                  value={draft[step.key]}
                  placeholder={step.placeholder}
                  data-testid={`setup-field-${step.key}`}
                  onChange={(e) => set(step.key, e.target.value)}
                />
              )}
              {problem && (
                <p className="text-sm text-destructive" data-testid="setup-problem">
                  {problem}
                </p>
              )}
            </div>
            <DialogFooter className="sm:justify-between">
              <div className="flex gap-2">
                {phase.step > 0 && (
                  <Button variant="ghost" onClick={back} data-testid="setup-back">
                    <ArrowLeft className="size-4" />
                    Back
                  </Button>
                )}
                <Button variant="ghost" onClick={onSkip} data-testid="setup-skip">
                  I'll do this later
                </Button>
              </div>
              <Button onClick={next} data-testid="setup-next">
                {phase.step + 1 === SETUP_STEPS.length ? "Build my company" : "Next"}
              </Button>
            </DialogFooter>
          </>
        )}

        {phase.kind === "thinking" && (
          <div className="flex flex-col items-center gap-3 py-10" data-testid="setup-thinking">
            <Loader2 className="size-6 animate-spin text-primary" />
            <p className="text-sm text-muted-foreground">Designing your team…</p>
          </div>
        )}

        {(phase.kind === "building" || phase.kind === "done") && (
          <BuildOut
            agents={phase.agents}
            created={phase.kind === "building" ? phase.created : phase.agents.length}
            finished={phase.kind === "done"}
            fallback={phase.fallback}
            onDone={onDone}
          />
        )}

        {phase.kind === "failed" && (
          <>
            <DialogHeader>
              <DialogTitle>That didn't work</DialogTitle>
              <DialogDescription data-testid="setup-failed">{phase.reason}</DialogDescription>
            </DialogHeader>
            <DialogFooter className="sm:justify-between">
              <Button variant="ghost" onClick={onSkip}>
                I'll do this later
              </Button>
              <Button onClick={() => setPhase({ kind: "asking", step: 0 })}>Try again</Button>
            </DialogFooter>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

/** The build-out: named teammates appearing one after another. */
function BuildOut({
  agents,
  created,
  finished,
  fallback,
  onDone,
}: {
  agents: ProposedAgent[];
  created: number;
  finished: boolean;
  /** The curated team shipped instead of a designed one — said out loud below. */
  fallback: boolean;
  onDone: () => void;
}) {
  return (
    <>
      <DialogHeader>
        <div className="mb-1 flex size-11 items-center justify-center rounded-xl bg-primary/10 text-primary">
          {finished ? <Check className="size-5" /> : <Users className="size-5" />}
        </div>
        <DialogTitle data-testid="setup-buildout-title">
          {finished ? "Your team is ready" : "Creating your team…"}
        </DialogTitle>
        <DialogDescription>
          {finished
            ? fallback
              ? "A general starting team for your industry — we couldn't reach a model to tailor it to your answers. Rename, retire, or add anyone from the Team page."
              : "Built from your answers. A starting point — rename, retire, or add anyone from the Team page."
            : buildOutLabel(created, agents.length)}
        </DialogDescription>
      </DialogHeader>
      <ul className="grid gap-2 py-2" data-testid="setup-buildout-list">
        {agents.map((agent, i) => {
          const landed = i < created;
          const tone = TEAM_TONES[toneFor(agent.role)] ?? TEAM_TONES.sky;
          return (
            <li
              key={agent.role}
              data-testid={landed ? "setup-agent-created" : "setup-agent-pending"}
              className={cn(
                "flex items-center gap-3 rounded-lg border px-3 py-2 transition-opacity duration-300",
                landed ? "opacity-100" : "opacity-40",
              )}
            >
              <span
                className={cn(
                  "flex size-8 shrink-0 items-center justify-center rounded-full text-xs font-medium",
                  tone,
                )}
              >
                {landed ? initials(agent.name) : ""}
              </span>
              <span className="min-w-0">
                <span className="block truncate text-sm font-medium">{agent.role}</span>
                <span className="block truncate text-xs text-muted-foreground">
                  {agent.description}
                </span>
              </span>
              {landed ? (
                <Check className="ml-auto size-4 shrink-0 text-primary" />
              ) : (
                <Loader2 className="ml-auto size-4 shrink-0 animate-spin text-muted-foreground/40" />
              )}
            </li>
          );
        })}
      </ul>
      {finished && (
        <DialogFooter>
          <Button onClick={onDone} data-testid="setup-finish">
            <Sparkles className="size-4" />
            Show me my company
          </Button>
        </DialogFooter>
      )}
    </>
  );
}

/** Which of the three questions we are on. */
function StepDots({ total, at }: { total: number; at: number }) {
  return (
    <div className="mb-2 flex items-center gap-1.5" aria-hidden>
      {Array.from({ length: total }, (_, i) => (
        <span
          key={i}
          className={cn(
            "h-1.5 rounded-full transition-all",
            i === at ? "w-6 bg-primary" : i < at ? "w-1.5 bg-primary/60" : "w-1.5 bg-muted",
          )}
        />
      ))}
    </div>
  );
}
