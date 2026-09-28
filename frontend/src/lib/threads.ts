// Conversation threads: WhatsApp-style "chats" with the company. Every thread
// talks to the same company chat endpoint; a thread just scopes a transcript
// and gives the company side a consistent identity (a "desk" you're talking to).

import type { DeskDto, TeamMemberDto } from "../api/types";
import { GENERAL_CHANNEL_ID, type ChatMessage } from "./chat";
import { toneFor } from "./team";

export interface ThreadContact {
  name: string;
  kind: "company" | "agent";
  /** Tailwind avatar tone key for agent desks; company uses the brand mark. */
  tone?: string;
}

export interface Thread {
  id: string;
  contact: ThreadContact;
  /** Short blurb shown under the name when the thread has no messages yet. */
  blurb: string;
  messages: ChatMessage[];
  /** Whether the composer for this thread is disabled. */
  readOnly?: boolean;
}

/** Avatar tones rotated across desk threads. */
const DESK_TONES = ["sky", "violet", "amber", "emerald", "rose", "cyan"];

/** The company-wide `#general` line — the orchestrator you talk to for anything. */
function generalThread(): Thread {
  return {
    id: GENERAL_CHANNEL_ID,
    contact: { name: "Your company", kind: "company" },
    blurb: "The whole company — ask for anything",
    messages: [],
  };
}

/** The default chat list: `#general` plus a few focused desks. */
export function defaultThreads(): Thread[] {
  return [
    generalThread(),
    {
      id: "strategy",
      contact: { name: "Strategy desk", kind: "agent", tone: "sky" },
      blurb: "Plans, priorities, and direction",
      messages: [],
    },
    {
      id: "creative",
      contact: { name: "Creative studio", kind: "agent", tone: "violet" },
      blurb: "Copy, design, and campaigns",
      messages: [],
    },
    {
      id: "frontdesk",
      contact: { name: "Front desk", kind: "agent", tone: "amber" },
      blurb: "Scheduling, inbox, and errands",
      messages: [],
    },
  ];
}

/**
 * Build the chat list from the company's real desks (issue #53), `#general`
 * first. A host that lists `#general` among its desks supplies it; for one that
 * does not, the line is still addressable and is added here.
 *
 * {@link defaultThreads} is only for a host that never answered at all (no
 * `/desks` route, or a failed read). An empty answer is an answer.
 */
export function threadsFromDesks(desks: DeskDto[]): Thread[] {
  const deskThreads: Thread[] = desks.map((desk, i) => ({
    id: desk.id,
    contact: {
      name: desk.name,
      kind: "agent",
      tone: DESK_TONES[i % DESK_TONES.length],
    },
    blurb: desk.description ?? "A desk of your company",
    messages: [],
  }));
  const general = deskThreads.filter((t) => t.id === GENERAL_CHANNEL_ID);
  const rest = deskThreads.filter((t) => t.id !== GENERAL_CHANNEL_ID);
  return [...(general.length > 0 ? general : [generalThread()]), ...rest];
}

/**
 * One DM thread per roster teammate (issue #151 §3.3): the agent's own console,
 * so an operator can follow up with the teammate who did the work instead of
 * going back through the orchestrator.
 *
 * Keyed by the **agent id**, which the host resolves straight to that teammate.
 * Desks are listed first and win any id collision — `existingIds` is what keeps
 * a teammate who is already reachable as a desk from appearing twice.
 *
 * Kept separate from {@link threadsFromDesks} so a host that exposes desks but
 * not `/team` (or fails that fetch) simply gets no DMs, rather than losing its
 * desk list too.
 */
export function agentDmThreads(
  team: TeamMemberDto[],
  existingIds: Iterable<string>,
): Thread[] {
  const taken = new Set(existingIds);
  const seen = new Set<string>();
  const threads: Thread[] = [];
  for (const member of team) {
    const id = member.id?.trim();
    // A teammate with no id has nothing the host could route to, and a
    // duplicate id would collide with the thread already added for it.
    if (!id || taken.has(id) || seen.has(id)) continue;
    seen.add(id);
    const name = member.name?.trim() || member.role;
    threads.push({
      id,
      contact: { name, kind: "agent", tone: toneFor(id) },
      blurb: member.description?.trim() || member.role,
      messages: [],
    });
  }
  return threads;
}
