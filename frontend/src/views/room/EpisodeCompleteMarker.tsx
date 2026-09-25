/**
 * The line that says an episode is over, and how.
 *
 * A centred pill like the dispatch marker (issue #377), for the same reason:
 * the end of an episode is a structural fact the prose cannot carry. The seat
 * that called `complete_episode` wrote a closing line, and that line reads like
 * any other reply — this is what tells a reader the desk has stopped, after
 * how many rounds, and whether it stopped because a seat said so or because
 * the host cut it off.
 */

import { CheckCircle2, AlertTriangle } from "lucide-react";

import { deskRounds, type Episode } from "@/lib/episodes";
import { cn } from "@/lib/utils";

interface Props {
  episode: Episode;
  agentNames?: Readonly<Record<string, string>>;
}

/** Operator-facing words for each completion reason; unknown words pass through. */
export function describeReason(reason: string | undefined): string | null {
  switch (reason) {
    case undefined:
    case "complete_episode":
      return null;
    case "round_cap":
      return "round cap reached";
    case "timeout":
      return "timed out";
    case "failed":
      return "a turn failed";
    case "membership_changed":
      return "the desk changed";
    default:
      return reason;
  }
}

export function EpisodeCompleteMarker({ episode, agentNames }: Props) {
  const cut = describeReason(episode.reason);
  const by = episode.completedBy ? agentNames?.[episode.completedBy] ?? episode.completedBy : null;
  const rounds = deskRounds(episode);
  return (
    <div
      className="my-2 flex justify-center"
      data-testid="episode-complete"
      data-episode-id={episode.id}
      data-reason={episode.reason ?? "complete_episode"}
    >
      <span
        className={cn(
          "inline-flex items-center gap-1.5 rounded-full border px-3 py-1 text-2xs text-muted-foreground",
          cut ? "border-dashed" : "border-status-done/50",
        )}
      >
        {cut ? (
          <AlertTriangle className="size-3 text-status-failed-text" aria-hidden />
        ) : (
          <CheckCircle2 className="size-3 text-status-done-text" aria-hidden />
        )}
        <span>
          Episode complete · {rounds} round{rounds === 1 ? "" : "s"}
          {by ? ` · closed by ${by}` : ""}
          {cut ? ` · ${cut}` : ""}
        </span>
      </span>
    </div>
  );
}
