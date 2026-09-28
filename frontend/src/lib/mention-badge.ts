import type { NotificationDto } from "@/api/types";
import { hostMessageId } from "@/lib/chat";

/**
 * The notification kinds that badge a channel on the rail: a mention (#65) and
 * a parked blocker (#1862). Both are "somebody wants you here" — a named
 * message, or a teammate blocked in this DM awaiting a verdict.
 */
export function isBadgingKind(kind: string): boolean {
  return kind === "mention" || kind === "blocker_parked";
}

/**
 * The rendered channel a mention's `context` badges, or `undefined` when it has
 * none. The context is the channel id the host recorded, `#general` included,
 * so it is the answer as it stands. Shared by the badge placement in
 * [`mentionCountsByChannel`], the shell's "re-read the thread a mention's
 * message is missing from" trigger and the notification links, so all three
 * resolve the same channel for the same row.
 */
export function renderedChannelIdForContext(
  context: string | null | undefined,
): string | undefined {
  return context ?? undefined;
}

/**
 * The mention badge: how many unread mentions of **you** sit in each channel.
 *
 * # Why this is not the unread count
 *
 * They answer different questions and come from different places, and the whole
 * value of the mention badge is that it does *not* inherit the unread badge's
 * caveat:
 *
 * | | Unread | Mentions |
 * |---|---|---|
 * | Derived | in this browser, from what this tab has seen | by the host, from who was named |
 * | Survives a reload | only via the read-state floor | yes, it is a stored row |
 * | Means | "you have not looked here" | "somebody asked *you* something" |
 *
 * Merging them would take the durable, per-person fact and give it the
 * best-effort one's meaning. So the rail renders two badges, and only one of
 * them carries the "this tab only" tooltip.
 */
export function mentionCountsByChannel(
  notifications: readonly NotificationDto[],
): Record<string, number> {
  const out: Record<string, number> = {};
  // Defensive against a caller handing us something that is not a list. The
  // types say it cannot happen; a host returning an unexpected shape says
  // otherwise, and the consequence of being wrong here is a render-time throw
  // that blanks the console rather than a missing badge.
  if (!Array.isArray(notifications)) return out;
  for (const n of notifications) {
    // Unread only. A mention you have already dealt with is not a summons.
    if (n.readAt !== undefined) continue;
    // `kind` rather than `subjectKind`: a future notification about a message
    // that is not a mention (a reply, a reaction) must not silently start
    // badging as one. A parked blocker (#1862) is the second summons that
    // belongs on the rail — a teammate is blocked in this DM and wants a
    // verdict — so it badges the same way a mention does.
    if (!isBadgingKind(n.kind)) continue;
    // A row with no channel cannot be placed on the rail. Counted nowhere
    // rather than counted somewhere arbitrary.
    if (n.context === undefined || n.context === null) continue;
    // The placement arm shared with the app shell's re-read trigger, so a
    // mention is badged and recovered from the same channel it is placed on.
    const channelId = renderedChannelIdForContext(n.context);
    if (channelId === undefined) continue;
    out[channelId] = (out[channelId] ?? 0) + 1;
  }
  return out;
}

/**
 * The ids to mark read when a channel is opened.
 *
 * Only that channel's, and only the unread ones — opening `#engineering` must
 * not silently clear a mention waiting in `#design`, which is exactly what a
 * bare "mark all" would do and exactly the summons somebody would then miss.
 */
export function mentionsToClear(
  notifications: readonly NotificationDto[],
  channelId: string,
  /**
   * The loaded transcript's thread replies, keyed by the console id
   * (`h<seq>`) of the reply to the id of the parent it is folded under.
   * A mention inside a thread reply must not clear on channel-open alone —
   * see the gate at the end of the filter.
   */
  replyParents: ReadonlyMap<string, string> = new Map(),
  /** The thread panel currently open, or `null` when none is. */
  openThreadId: string | null = null,
  /**
   * The set of all message ids (`h<seq>`) in the currently loaded transcript
   * for this channel. When provided, a mention whose subject message is absent
   * from this set — outside the history window, or history hydration failed —
   * is not cleared, because the person was never shown the text that mentioned
   * them. Without this, the function cannot distinguish "a top-level message
   * rendered on screen" from "a message that was never loaded" (Codex P1).
   */
  loadedMessageIds: ReadonlySet<string> | undefined = undefined,
): string[] {
  return notifications
    .filter((n) => {
      if (n.readAt !== undefined || !isBadgingKind(n.kind) || n.context === undefined) {
        return false;
      }
      if (n.context !== channelId) return false;
      // A parked blocker has no summoning chat message — its card renders from
      // the approvals feed, not the transcript — so opening the DM clears it
      // outright, without the message-loaded gates the mention path needs.
      if (n.kind === "blocker_parked") return true;
      // A mention inside a thread reply stays until that reply is actually on
      // screen. The main timeline folds replies into their parent
      // (`buildTimeline`), so a collapsed thread hides the text even while the
      // channel is open — clearing it would lose the summons without the
      // person ever seeing it. The notification names the message by its host
      // sequence (`subjectId`); the loaded transcript's reply map keys by the
      // console's `h<seq>` id, so the two meet through `hostMessageId`.
      const consoleId = hostMessageId(n.subjectId);
      const replyParent = replyParents.get(consoleId);
      if (replyParent !== undefined && replyParent !== openThreadId) return false;
      // When the loaded transcript's message set is known, require the subject
      // to be present — a message outside the history window (or one that
      // hydration failed to fetch) was never displayed, and clearing its
      // mention would lose the summons with nothing left to notice it by.
      if (loadedMessageIds !== undefined && !loadedMessageIds.has(consoleId)) return false;
      return true;
    })
    .map((n) => n.id);
}

/**
 * Which host threads need their history re-read because a newly polled
 * mention's message is absent from the loaded transcript.
 *
 * A mention posted by another operator never reaches an open console through
 * SSE — `OperatorMessage` is deliberately dropped from the stream projection —
 * and the transcripts are otherwise re-read only when a turn *this tab is
 * watching* settles, which a turn another operator's message opened is not. So
 * a mention that lands while the tab is already open has no later delivery
 * path, and opening its channel cannot satisfy the `loadedMessageIds` gate in
 * [`mentionsToClear`]: the operator can neither see the summons nor clear it
 * until reloading. Re-read those threads so the mentioned message lands.
 *
 * `seenSubjects` is the set of subject ids already scheduled this session —
 * the caller adds each returned `subjects` entry to it, so one mention
 * triggers at most one re-read rather than one per poll. The fold that
 * rebuilds the transcript dedupes by message id, so re-reading a thread whose
 * message arrived some other way is a no-op.
 */
export function threadsToReReadForMentions(
  notifications: readonly NotificationDto[],
  /**
   * The message ids currently loaded per rendered channel, keyed by channel id
   * (the same ids `mentionsToClear`'s `loadedMessageIds` holds).
   */
  loadedByChannel: Readonly<Record<string, ReadonlySet<string>>>,
  /** The console's thread-id → channel-id map, as `AppShell` keeps it. */
  chatChannelByThread: Readonly<Record<string, string>>,
  seenSubjects: ReadonlySet<string>,
): { threadIds: string[]; subjects: string[] } {
  const threadIds = new Set<string>();
  const subjects = new Set<string>();
  const renderedChannelIds = new Set(Object.values(chatChannelByThread));
  for (const n of notifications) {
    if (n.readAt !== undefined || n.kind !== "mention") continue;
    const subject = hostMessageId(n.subjectId);
    if (seenSubjects.has(subject)) continue;
    const context = n.context;
    if (context === undefined || context === null) continue;
    const channelId = renderedChannelIdForContext(context);
    if (channelId === undefined) continue;
    // The message is already on screen for this channel — nothing to recover.
    if (loadedByChannel[channelId]?.has(subject)) continue;
    // A desk's channel id doubles as its thread id; a DM's channel is keyed by
    // the teammate's thread id, so that one is found by reverse lookup.
    const threadId = !renderedChannelIds.has(context)
      ? undefined
      : chatChannelByThread[context] === context
        ? context
        : Object.entries(chatChannelByThread).find(([, c]) => c === context)?.[0];
    if (threadId !== undefined) {
      threadIds.add(threadId);
      subjects.add(subject);
    }
  }
  return { threadIds: [...threadIds], subjects: [...subjects] };
}
