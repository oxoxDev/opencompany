// The empty state both skills tabs draw, so "nothing installed" and "nothing in
// the registry" are the same shape of answer.

import { Sparkles } from "lucide-react";

/** The centred placeholder a skills tab draws when it has no rows. */
export function SkillsEmpty({ label }: { label: string }) {
  return (
    <div className="mt-12 flex flex-col items-center gap-2 text-center text-muted-foreground">
      <Sparkles className="size-8" />
      <p className="text-sm">{label}</p>
    </div>
  );
}
