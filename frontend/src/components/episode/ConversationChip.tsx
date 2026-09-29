/**
 * A private exchange between seats of this desk: who is talking to whom, and
 * whether they still are.
 *
 * # Two seats, or more
 *
 * `ask` names one seat and `ask_teammates` names several into one room, so this
 * is not always a pair. The desk rows carry a single `askee` -- they were
 * written when an ask could only name one, and a group ask records the first it
 * asked -- but the channel key names every member, which is what this reads.
 * See `conversationAskees`.
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
  /** Roster id to display name, for every seat in the exchange. */
  agentNames?: Record<string, string>;
  className?: string;
}

/**
 * Every seat the asker is talking to, from the channel key.
 *
 * `conversation_channel` writes `dm:` and then the asker and all its askees,
 * sorted and deduped, joined by `+`. Dropping the asker leaves the askees --
 * one for `ask`, several for `ask_teammates`.
 *
 * Falls back to the row's own single `askee` when the key is not a channel this
 * understands, so an unfamiliar key shows one seat rather than none.
 */
export function conversationAskees(conversation: ConversationRecord): string[] {
  const bare = conversation.conversationId.startsWith("dm:")
    ? conversation.conversationId.slice("dm:".length)
    : null;
  const others = (bare?.split("+") ?? [])
    .filter(Boolean)
    .filter((seat) => seat !== conversation.asker);
  return others.length > 0 ? others : [conversation.askee];
}

/** `A`, `A and B`, `A, B and C` -- for the hover title. */
function listed(names: string[]): string {
  if (names.length <= 1) return names[0] ?? "";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

/** How many askee marks are drawn before the rest become a count. */
const SHOWN = 3;

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
  const askees = conversationAskees(conversation);
  const shown = askees.slice(0, SHOWN);
  const hidden = askees.length - shown.length;
  return (
    <span
      data-testid="conversation-chip"
      data-conversation-state={state}
      data-conversation-root={conversation.root}
      title={`${name(conversation.asker)} asked ${listed(askees.map(name))}`}
      data-conversation-askees={askees.length}
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
      {shown.map((askee) => (
        <TeammateAvatar key={askee} name={name(askee)} markOnly className="size-3.5 shrink-0" />
      ))}
      {hidden > 0 && <span className="shrink-0 tabular-nums">+{hidden}</span>}
      <span className="truncate">{LABEL[state]}</span>
    </span>
  );
}
