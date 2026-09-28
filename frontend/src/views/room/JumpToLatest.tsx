import { ArrowDown } from "lucide-react";

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

interface Props {
  /** Travels to the newest row and resumes following. */
  onClick: () => void;
  className?: string;
}

/**
 * The control offered while the reader has scrolled away from the newest row.
 *
 * Render it as a **sibling** of the scroller, inside a `relative` wrapper: a
 * child of the scroller scrolls with the transcript, which puts the offer to
 * reach the bottom at whatever height the reader has already left behind.
 *
 * It ships with the anchoring rules and never instead of them. On its own it
 * would either stay hidden in exactly the state the anchor is missing from, or
 * stand permanently on every open as an invitation to fix that by hand.
 */
export function JumpToLatest({ onClick, className }: Props) {
  return (
    <Button
      type="button"
      variant="outline"
      size="icon"
      aria-label="Jump to the end of this conversation."
      title="Jump to the end of this conversation."
      data-testid="jump-to-latest"
      onClick={onClick}
      className={cn("absolute right-4 bottom-4 z-10 rounded-full shadow-md", className)}
    >
      <ArrowDown className="size-4" />
    </Button>
  );
}
