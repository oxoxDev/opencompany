/**
 * A private exchange between two seats: who is talking to whom, and whether
 * they still are.
 *
 * # Why this exists at all
 *
 * A conversation's own rows are not on this desk. A seat that `ask`s another
 * opens a thread in the pair's channel, so a fold over the desk's transcript
 * never sees the question or the answer. What the desk carries is a pair of
 * reference rows — opened and concluded — and this chip is what they are for:
 * an operator watching a room can see that two teammates stepped aside,
 * while they are still in it.
 *
 * # Ended is not the same as answered
 *
 * A conversation that runs out of turns ends `forced`, without an answer.
 * Both states are "no longer live", and an indicator that showed only
 * "answered" would be claiming something that did not happen — so the ended
 * chip says which it was.
 *
 * `data-conversation-state` is the contract a live spec reads.
 */

import { MessagesSquare } from "lucide-react";

import { teammateName } from "@/components/episode/teammate-name";
import { TeammateAvatar } from "@/components/teammate-avatar";
import type { ConversationRecord } from "@/lib/episodes";
import { cn } from "@/lib/utils";

interface Props {
  conversation: ConversationRecord;
  /** Roster id to display name, for the two seats. */
  agentNames?: Record<string, string>;
  className?: string;
}

/** `live` while nothing has ended it, then how it ended. */
function stateOf(conversation: ConversationRecord): "live" | "answered" | "unanswered" {
  if (conversation.endedAtMillis === undefined) return "live";
  return conversation.forced ? "unanswered" : "answered";
}

const LABEL: Record<"live" | "answered" | "unanswered", string> = {
  live: "talking",
  answered: "answered",
  unanswered: "no answer",
};

export function ConversationChip({ conversation, agentNames, className }: Props) {
  const state = stateOf(conversation);
  const name = (id: string) => teammateName(id, agentNames);
  return (
    <span
      data-testid="conversation-chip"
      data-conversation-state={state}
      data-conversation-root={conversation.root}
      title={`${name(conversation.asker)} asked ${name(conversation.askee)}`}
      className={cn(
        "inline-flex max-w-full items-center gap-1 rounded-full border px-2 py-0.5 text-2xs font-medium text-muted-foreground",
        state === "live" && "border-dashed",
        className,
      )}
    >
      <MessagesSquare
        className={cn("size-3 shrink-0", state === "live" && "animate-pulse")}
        aria-hidden
      />
      <TeammateAvatar name={name(conversation.asker)} markOnly className="size-3.5 shrink-0" />
      <TeammateAvatar name={name(conversation.askee)} markOnly className="size-3.5 shrink-0" />
      <span className="truncate">{LABEL[state]}</span>
    </span>
  );
}
