/**
 * The one speech act a seat ended its turn with, on the row it produced.
 *
 * Every turn inside an episode ends with exactly one of four calls on the
 * `opencompany` MCP server — `post`, `broadcast`, `dm`, `complete_episode` —
 * and the row is the utterance. Rendered verbatim it is prose like any other
 * reply, so a reader cannot tell a line the whole desk heard from one that
 * went to a single teammate, or the line that ended the episode from the one
 * before it. The chip carries that difference, and the prose carries the
 * argument.
 *
 * Told apart by icon and word, never by colour alone.
 */

import { CheckCircle2, MessageSquare, Radio, Send } from "lucide-react";

import type { MessageEpisodeDto, UtteranceKind } from "@/api/types";
import { RoutingPlanChip } from "@/components/episode/RoutingPlanChip";
import { teammateName } from "@/components/episode/teammate-name";
import { cn } from "@/lib/utils";

interface Props {
  episode: MessageEpisodeDto;
  /** Who may read the row, when the host narrowed it — drawn as `→ @x`. */
  audience?: string[];
  agentNames?: Readonly<Record<string, string>>;
  className?: string;
}

/** The chip's words for each kind, in the reader's terms rather than the tool's. */
export const UTTERANCE_LABEL: Record<UtteranceKind, string> = {
  post: "Posted",
  broadcast: "Shared with the desk",
  dm: "Private note",
  complete_episode: "Finished",
};

const UNKNOWN_LABEL = "Replied";

/** The words before the recipients, or the whole label when there are none. */
export function utteranceLead(kind: UtteranceKind, hasRecipients: boolean): string {
  const label = UTTERANCE_LABEL[kind] ?? UNKNOWN_LABEL;
  if (!hasRecipients) return label;
  return kind === "dm" ? "Sent to" : `${label} to`;
}

/**
 * Recipients by display name, never by roster id.
 *
 * Deduplicated by id, not by the name it resolves to — two distinct
 * teammates sharing a display name are two recipients, not one. Unnamed ids
 * still collapse to a single "a teammate" entry rather than repeating it.
 */
export function recipientNames(
  ids: readonly string[],
  agentNames?: Readonly<Record<string, string>>,
): string[] {
  const seenIds = new Set<string>();
  const names: string[] = [];
  let unnamedAdded = false;

  for (const id of ids) {
    if (seenIds.has(id)) continue;
    seenIds.add(id);

    if (agentNames?.[id] === undefined) {
      if (unnamedAdded) continue;
      unnamedAdded = true;
    }
    names.push(teammateName(id, agentNames));
  }

  return names;
}

export function roundTitle(revision: number): string {
  return `Round ${revision + 1}`;
}

const ICON: Record<UtteranceKind, typeof MessageSquare> = {
  post: MessageSquare,
  broadcast: Radio,
  dm: Send,
  complete_episode: CheckCircle2,
};

export function UtteranceChip({ episode, audience, agentNames, className }: Props) {
  const Icon = ICON[episode.kind] ?? MessageSquare;
  const to = episode.to?.length ? episode.to : episode.kind === "dm" ? audience : undefined;
  return (
    <span
      className={cn("mt-1 flex flex-wrap items-center gap-1.5", className)}
      data-testid="utterance-chip"
      data-kind={episode.kind}
      data-episode-id={episode.id}
      data-round-revision={episode.revision}
    >
      <span
        className={cn(
          "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-2xs font-medium",
          episode.kind === "complete_episode"
            ? "border-status-done/50 text-foreground"
            : "text-muted-foreground",
        )}
        title={roundTitle(episode.revision)}
      >
        <Icon className="size-3 shrink-0" aria-hidden />
        {utteranceLead(episode.kind, Boolean(to?.length))}
        {to?.length ? (
          <>
            {" "}
            <span className="font-normal" data-testid="utterance-audience">
              {recipientNames(to, agentNames).join(", ")}
            </span>
          </>
        ) : null}
      </span>
      {episode.routedBy && (
        <RoutingPlanChip
          plan={episode.routedBy.plan}
          router={episode.routedBy.router}
          agentNames={agentNames}
        />
      )}
    </span>
  );
}
