import type { ReactNode } from "react";
import type { TooltipRenderProps } from "react-joyride";

import { Button } from "@/components/ui/button";

/**
 * The tour's spotlight card — a design-system-native replacement for
 * react-joyride's default tooltip: title, a step counter, the body copy, a
 * progress bar, and Skip / Back / Next controls (Base UI buttons).
 */
export function TourTooltip({
  index,
  size,
  step,
  backProps,
  primaryProps,
  skipProps,
  tooltipProps,
  isLastStep,
}: TooltipRenderProps) {
  const pct = Math.round(((index + 1) / size) * 100);
  return (
    <div
      {...tooltipProps}
      className="w-80 max-w-[calc(100vw-2rem)] rounded-xl bg-popover p-4 text-popover-foreground shadow-lg ring-1 ring-foreground/10"
    >
      <div className="flex items-center justify-between gap-2">
        <span className="font-heading text-sm font-semibold">{step.title as ReactNode}</span>
        <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
          {index + 1} / {size}
        </span>
      </div>

      <p className="mt-2 text-sm leading-snug text-muted-foreground">
        {step.content as ReactNode}
      </p>

      <div className="mt-3 h-1 w-full overflow-hidden rounded-full bg-muted">
        <div
          className="h-full rounded-full bg-primary transition-all"
          style={{ width: `${pct}%` }}
        />
      </div>

      <div className="mt-3 flex items-center justify-between">
        <Button variant="ghost" size="sm" {...skipProps}>
          Skip
        </Button>
        <div className="flex items-center gap-2">
          {index > 0 && (
            <Button variant="outline" size="sm" {...backProps}>
              Back
            </Button>
          )}
          <Button size="sm" {...primaryProps}>
            {isLastStep ? "Finish" : "Next"}
          </Button>
        </div>
      </div>
    </div>
  );
}
