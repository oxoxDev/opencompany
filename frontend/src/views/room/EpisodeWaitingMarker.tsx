/**
 * The line that says a seat of an open episode is parked on the operator.
 *
 * One pill per waiting seat, shaped like the completion marker beside it. It
 * names the teammate, never an id, and jumps to the approval card it waits on
 * — in this channel when the card is on screen, the Approvals page otherwise.
 */

import { Hourglass } from "lucide-react";

import type { Episode } from "@/lib/episodes";
import type { WaitingSeat } from "./timeline";

interface Props {
  episode: Episode;
  seats: WaitingSeat[];
  agentNames?: Readonly<Record<string, string>>;
}

export function waitingLabel(agentId: string, agentNames?: Readonly<Record<string, string>>): string {
  return `Waiting for approval — ${agentNames?.[agentId] || "a teammate"}`;
}

function openApproval(approvalIds: string[]) {
  for (const id of approvalIds) {
    const card = document.querySelector(`[data-approval-id="${CSS.escape(id)}"]`);
    if (card) {
      card.scrollIntoView({ block: "center", behavior: "smooth" });
      return;
    }
  }
  window.location.hash = "/approvals";
}

export function EpisodeWaitingMarker({ episode, seats, agentNames }: Props) {
  return (
    <div className="my-2 flex flex-col items-center gap-1" data-testid="episode-waiting" data-episode-id={episode.id}>
      {seats.map((seat) => (
        <button
          key={seat.agentId}
          type="button"
          data-testid="episode-waiting-seat"
          data-agent-id={seat.agentId}
          onClick={() => openApproval(seat.approvalIds)}
          className="inline-flex items-center gap-1.5 rounded-full border border-dashed border-status-running/50 px-3 py-1 text-2xs text-muted-foreground hover:text-foreground"
        >
          <Hourglass className="size-3 text-status-running-text" aria-hidden />
          <span>{waitingLabel(seat.agentId, agentNames)}</span>
        </button>
      ))}
    </div>
  );
}
