// A horizontal step indicator for multi-step flows.
//
// Added for the first-run setup wizard (`views/setup/`), which is the console's
// first genuinely stepped flow — every other form here is a single page, so
// there was no precedent to reuse.
//
// Presentational only: it renders the position it is given and never owns it.
// The wizard holds the current step because it also owns whether a step may be
// left, and a component that tracked its own index would have to be told the
// answer anyway.

import { Check } from "lucide-react";

import { cn } from "@/lib/utils";

export interface Step {
  /** Stable key, used for React identity and as the step's test id. */
  id: string;
  /** The operator-facing label. */
  label: string;
}

interface StepperProps {
  steps: readonly Step[];
  /** Index of the step being shown. */
  current: number;
  /**
   * Jump to an already-completed step. Omit to make the stepper read-only.
   *
   * Only backwards: a forward jump would skip whatever validation the steps in
   * between perform, and the wizard's own Next button is the only thing that
   * knows a step is complete.
   */
  onSelect?: (index: number) => void;
  className?: string;
}

export function Stepper({ steps, current, onSelect, className }: StepperProps) {
  return (
    <ol
      // Wraps rather than stretches. Every step used to take an equal `flex-1`
      // share of the row, which on a six-step flow left each label ~90px and
      // `truncate` turned "Business" into "Busine…" — a progress indicator you
      // cannot read is worse than none.
      className={cn("flex w-full flex-wrap items-center gap-x-1 gap-y-2", className)}
      aria-label="Setup progress"
      data-slot="stepper"
    >
      {steps.map((step, index) => {
        const done = index < current;
        const active = index === current;
        // Only a completed step is reachable — see `onSelect`.
        const clickable = done && onSelect !== undefined;
        return (
          <li key={step.id} className="flex shrink-0 items-center gap-1">
            <button
              type="button"
              disabled={!clickable}
              onClick={clickable ? () => onSelect(index) : undefined}
              // `step` is the a11y idiom for a stepper's current position;
              // `aria-current="true"` would say "current item" without saying
              // current *what*.
              aria-current={active ? "step" : undefined}
              data-testid={`step-${step.id}`}
              data-state={done ? "done" : active ? "active" : "todo"}
              className={cn(
                "flex shrink-0 items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs transition-colors",
                clickable && "hover:bg-muted",
                !clickable && "cursor-default",
                active ? "text-foreground" : "text-muted-foreground",
              )}
            >
              <span
                aria-hidden
                className={cn(
                  "flex size-5 shrink-0 items-center justify-center rounded-full border text-3xs font-medium",
                  done && "border-primary bg-primary text-primary-foreground",
                  active && "border-primary text-primary",
                  !done && !active && "border-border",
                )}
              >
                {done ? <Check className="size-3" /> : index + 1}
              </span>
              <span className={cn("whitespace-nowrap", active && "font-medium")}>
                {step.label}
              </span>
            </button>
            {index < steps.length - 1 && (
              <span
                aria-hidden
                className={cn("h-px w-4 shrink-0", done ? "bg-primary" : "bg-border")}
              />
            )}
          </li>
        );
      })}
    </ol>
  );
}
