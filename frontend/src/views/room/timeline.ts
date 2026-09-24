// How a channel's rows are grouped and rendered: senders, hydration, the
// timeline entries, the approval and round items interleaved among them, and
// reactions.
//
// Split out of the old `model.ts` (issue: room store / P2). Pure.
//
// `HISTORY_UNSTARTED` vs `HISTORY_UNTRACKED` is the subtlety worth reading
// before touching anything here — see their own docs.

// The chat workspace's data model: channels, direct messages, and the grouping
// rules the timeline reads. Everything here is pure — the view owns the state.

import type { ApprovalSummary, Verdict } from "@/api/types";
import {
  clearTaskCard,
  type ChatMessage,
  type Reaction,
} from "@/lib/chat";
import type { Episode, EpisodeRound } from "@/lib/episodes";
import { initials as nameInitials, type TeamMember } from "@/lib/team";
import type { Channel } from "./channels";
import { latestSettlePillIdByTaskId } from "./review";

/**
 * A host desk (`GET .../desks`), shaped into the console's `Desk`. The host
 * has no separate channel-slug or blurb field, so the slug is derived from
 * the desk's name and the blurb falls back to its description — the id is
 * the one field that must survive untouched, since it doubles as the chat
 * thread id `send` addresses.
 *
 * `members` / `overlayMembers` come through as the host sent them, order
 * included — `members[0]` is the desk's lead, and the rest is the hierarchy the
 * company declared. Dropping them here is what made every channel show the
 * whole company (issue #369).
 */
/**
 * Every message the open thread panel should show under `parent` — not just
 * its direct children.
 *
 * Review feedback sent from inside a thread is anchored on whichever reply
 * {@link reviewAnchorForThread} found (the card's settle pill or relay, when
 * that card was dispatched from inside this very thread) rather than on
 * `parent` itself, because that is what the backend needs to find the card
 * (`review_anchor_card` on the host walks a message's *direct* parent, not
 * its thread). That reply becomes the operator's own message's parent, so a
 * same-level filter (`m.parentId === parent.id`) never finds it — the
 * message the operator just typed disappears from the panel the moment it
 * sends, in both the optimistic bubble and the persisted echo. Walk each
 * message's parent chain back to `parent` instead, so a reply-to-a-reply
 * still renders.
 */
export function repliesInThread(
  parent: ChatMessage,
  messages: readonly ChatMessage[],
): ChatMessage[] {
  const byId = new Map(messages.map((m) => [m.id, m]));
  const descendsFromParent = (message: ChatMessage): boolean => {
    const seen = new Set<string>();
    let ancestorId = message.parentId;
    while (ancestorId !== undefined && !seen.has(ancestorId)) {
      if (ancestorId === parent.id) return true;
      seen.add(ancestorId);
      ancestorId = byId.get(ancestorId)?.parentId;
    }
    return false;
  };
  return messages.filter(descendsFromParent);
}

/**
 * Every channel's transcript, keyed by channel id. Owned by `AppShell`, not
 * `RoomView`, so a transcript survives `RoomView` unmounting when the operator
 * steps into Tasks, Settings, or any other view and comes back.
 */
export type Transcripts = Record<string, ChatMessage[]>;

/**
 * How far a channel's persisted history has got, per channel.
 *
 * {@link Transcripts} cannot answer this on its own: an absent key and a key
 * holding `[]` both read as "no messages", and the timeline coerces the two
 * together the moment it does `transcripts[id] ?? []`. So "nobody has asked the
 * host yet" is indistinguishable from "the host says this channel is empty",
 * and the timeline printed the second while the first was true — the reload
 * flash issue #934 describes.
 */
export type HistoryStatus = "loading" | "ready";

export interface HistoryHydration {
  /**
   * Whether the desks/roster pass has finished marking every channel it is
   * going to hydrate.
   *
   * Needed because `RoomView` resolves its own desk list independently of the
   * shell's, and can therefore render a channel before the shell's pass has
   * reached it. Without this, that window has no entry in `byChannel` and looks
   * exactly like a channel nothing will ever hydrate.
   */
  discovered: boolean;
  /** Channel id → whether its `chat/history` request has settled. */
  byChannel: Record<string, HistoryStatus>;
}

/** Before a company's rehydration pass has begun: everything is still pending. */
export const HISTORY_UNSTARTED: HistoryHydration = { discovered: false, byChannel: {} };

/**
 * No rehydration is happening or ever will — for a `RoomView` mounted without a
 * shell behind it. The distinction from {@link HISTORY_UNSTARTED} is the whole
 * point: this one resolves every channel to "ready", so a caller that does not
 * track hydration renders exactly as it did before, rather than spinning on a
 * pass that is never coming.
 */
export const HISTORY_UNTRACKED: HistoryHydration = { discovered: true, byChannel: {} };

/**
 * Whether we know enough about `channelId` to state that it is empty.
 *
 * The three cases, and why the last one is `discovered` rather than `false`: a
 * channel with a status answers for itself; a channel with none *after* the
 * pass has run is one nothing will hydrate (a console-only teammate, a host
 * with no `chat/history`), and holding a spinner on it forever is worse than
 * the wrong claim this exists to prevent.
 */
export function historyReady(hydration: HistoryHydration, channelId: string): boolean {
  const status = hydration.byChannel[channelId];
  if (status) return status === "ready";
  return hydration.discovered;
}

export type SenderKind = "you" | "company" | "agent" | "system";

export interface Sender {
  /** Stable identity, so consecutive lines from one voice group together. */
  key: string;
  name: string;
  kind: SenderKind;
  tone?: string;
  /**
   * The avatar reference (`TeamMember.avatar`) when the sender resolves to a
   * roster teammate, and your own when the sender is you.
   *
   * Undefined for "system", and for an agent voice `senderOf` could not match
   * against the roster — `TeammateAvatar` falls back to seeding on `name` in
   * both of those cases, same as before issue #1185.
   */
  avatar?: string;
  /**
   * The roster agent id behind this voice, when there is one — what a click on
   * the face opens the profile panel on (issue #1653).
   *
   * Set only on a voice that actually **matched** the roster, never on the
   * channel slug that seeded the face. A desk-originated cross-post carries a
   * desk id in that slot (see `api/types.ts` on `thread`), and opening a
   * teammate profile on a desk id would ask the host for an agent that does not
   * exist.
   */
  agentId?: string;
}

/** Channel names the host uses for its own voice rather than a named agent. */
const COMPANY_VOICE = new Set(["operator", "console", "chat", "owner", ""]);

/**
 * Who said a line, within a channel.
 *
 * The company side wears the channel's identity unless the reply names a
 * distinct originating channel — then it reads as that agent, which is how a
 * single endpoint produces a multi-voice transcript.
 *
 * `members` is the roster, so a named agent's face can be looked up rather
 * than left to fall back on its title-cased channel slug (issue #1185). The
 * host's own convention for that slug (`api/types.ts`'s note on `thread`) is
 * a desk id for a channel reply and a roster agent id for a direct message —
 * only the latter matches a `TeamMember.id`, so a miss here is expected for a
 * desk-originated cross-post and simply keeps today's name-seeded fallback,
 * never a wrong face.
 */
export function senderOf(
  m: ChatMessage,
  channel: Channel,
  members: TeamMember[],
  youAvatar?: string,
): Sender {
  // Still "You" rather than your name: in your own transcript the second person
  // is what identifies the line, and a name there would read as somebody else.
  // Only the face is yours — which is the half a reader scanning a busy channel
  // actually picks their own lines out by.
  if (m.from === "you") return { key: "you", name: "You", kind: "you", avatar: youAvatar };
  if (m.from === "system") return { key: "system", name: "System", kind: "system" };

  const named = m.channel?.trim().toLowerCase() ?? "";
  if (named && !COMPANY_VOICE.has(named)) {
    const agent = members.find((mem) => mem.id === named);
    return {
      key: `agent:${named}`,
      name: titleize(m.channel ?? ""),
      kind: "agent",
      tone: named,
      avatar: agent?.avatar,
      agentId: agent?.id,
    };
  }

  // A desk speaks as itself and wears its own tone; only the main line — the
  // one channel with no tone of its own — speaks as the company. A DM's
  // "channel" is the teammate on the other end, so its avatar is theirs.
  return {
    key: `channel:${channel.id}`,
    name: channel.voice ?? channel.name,
    kind: channel.kind === "dm" || channel.tone ? "agent" : "company",
    tone: channel.tone,
    avatar: channel.member?.avatar,
    // A DM's other end is a roster teammate; a desk channel's voice is the desk
    // itself, which has no profile of its own to open.
    agentId: channel.member?.id,
  };
}

function titleize(s: string): string {
  return s.replace(/[._-]+/g, " ").replace(/\w\S*/g, (w) => w.charAt(0).toUpperCase() + w.slice(1));
}

export const initials = nameInitials;

/* ---- timeline grouping ---- */

/** Consecutive lines from one sender inside this window collapse into a run. */
const GROUP_WINDOW_MS = 5 * 60 * 1000;

export interface TimelineEntry {
  message: ChatMessage;
  sender: Sender;
  /** True when this row continues the run above it — no avatar, no name. */
  continuation: boolean;
  /** Set on the first row of a new calendar day; the divider label. */
  dayLabel?: string;
  /** Replies hanging off this row, oldest first. */
  replies: ChatMessage[];
  /**
   * The distinct voices in those replies, in the order they first spoke
   * (issue #1324).
   *
   * Resolved here rather than in the row because resolving a sender needs the
   * channel and the roster, and neither reaches the renderer. Without it the
   * summary row could only seed a face on `message.channel` — one value shared
   * by every reply in a thread — so a three-face pile drew one colour three
   * times and said nothing at all.
   *
   * Deduped by `Sender.key`: a pile is a list of *people*, and someone who
   * replied four times is still one face.
   */
  replySenders: Sender[];
  /**
   * For a system settle pill, whether it is the most recent one carrying its
   * `taskId`. A card that has re-run since parks an older pill in history
   * with the same id; only the latest should offer Approve. Meaningless (and
   * left `undefined`) for any other row.
   */
  isLatestSettlePill?: boolean;
}

/**
 * The distinct voices in a run of messages, in first-spoken order.
 *
 * Goes through the same {@link senderOf} every rendered row does, so a face in
 * a thread's summary pile is the same face that thread shows when it is opened.
 * A system line is dropped: it has no voice to draw, and a pile that counted it
 * would claim one more participant than the thread has.
 */
function distinctSenders(
  messages: ChatMessage[],
  channel: Channel,
  members: TeamMember[],
  youAvatar?: string,
): Sender[] {
  const byKey = new Map<string, Sender>();
  for (const m of messages) {
    if (m.from === "system") continue;
    const sender = senderOf(m, channel, members, youAvatar);
    if (!byKey.has(sender.key)) byKey.set(sender.key, sender);
  }
  return [...byKey.values()];
}

/**
 * Which first replies render **inline** in the channel rather than folding into
 * their parent's summary row (issue #1890 D, part 2).
 *
 * # Why this exists at all
 *
 * Part 1 of #1890 D threads every answer under the message that opened it, so
 * that `parent` is uniform and a thread means a *topic*. Fold every parented
 * line, as this module did before, and the channel becomes a column of your own
 * questions each wearing a "1 reply" chip — every answer deleted from the view.
 *
 * # Flat when nothing overlaps, threaded when it does
 *
 * A question answered with nothing in between is not a thread anyone opened; it
 * is a normal exchange, and it reads as one. So its first reply is laid out
 * inline, in its own chronological place. Only when something *else* arrived
 * between the question and its answer does the pair collapse to the summary
 * row — which is the case the fold was always for: two conversations racing in
 * one channel, where inline rendering would interleave them into nonsense.
 *
 * # Decided here, never in the journal
 *
 * The tempting version stamps this at write time — "thread it only if another
 * question arrived while I was working". That makes `parent` a function of race
 * timing, and `parent` is permanent: two operators doing the identical thing
 * would get permanently different transcripts on microseconds, and the console
 * renders a reply as it streams, before the backend could know, so a bubble
 * would render inline and jump into a thread on reload. Re-deciding
 * presentation on every render costs nothing and writes nothing racy down.
 *
 * # What counts as "in between"
 *
 * Any message that is neither the root nor one of the root's own replies. That
 * is deliberately wider than "another root": a sibling thread's reply landing
 * between question and answer interleaves the two conversations on screen just
 * as visibly as a new question does, and the rule is about what a reader sees.
 *
 * Returns the reply ids to render inline, so the caller can lay each out in its
 * own place and leave the remainder on the parent's chip.
 */
function inlineFirstReplies(
  messages: ChatMessage[],
  replies: Map<string, ChatMessage[]>,
): Set<string> {
  const position = new Map<string, number>();
  const roots = new Set<string>();
  messages.forEach((m, i) => {
    position.set(m.id, i);
    if (!m.parentId) roots.add(m.id);
  });

  const inline = new Set<string>();
  for (const [rootId, bucket] of replies) {
    // **An orphan renders flat rather than not at all** (issue #1890 D).
    //
    // A reply whose parent is absent from this transcript used to be dropped,
    // which was safe while only hand-opened threads carried a `parentId`. Part
    // 1 gives *every* answer one, so the same rule silently deletes answers —
    // and two of them are ordinary: a reply to a message another client sent
    // (this console deliberately does not draw an operator line it did not
    // send), and a reply that arrives before `reconcileIds` has swapped a
    // locally-sent message's id for the host's, which a killed POST leaves
    // pending for good.
    //
    // There is no summary row to fold into, so the whole bucket renders. The
    // cost is a reply whose root fell outside the history window reading
    // without its question; the alternative is an answer that is simply gone,
    // and a lost answer is the failure this whole sub-issue exists to prevent.
    if (!position.has(rootId)) {
      for (const orphan of bucket) inline.add(orphan.id);
      continue;
    }
    // **Only a root's reply is ever promoted.** A reply-to-a-reply must render
    // nowhere, and promoting one would give the console a second fold level —
    // which is not a cosmetic difference: `cycle_conversation`
    // (`src/runtime/cycle.rs`) parents an approval continuation to the thread
    // *root* rather than to the message that raised it precisely because a
    // grandchild is unrenderable, and #435's routing choice would quietly stop
    // being necessary. Pinned by the one-level-deep test.
    //
    // A grandchild whose own parent IS present is therefore still dropped —
    // the orphan arm above is about a root this transcript never held, not
    // about relaxing the depth rule.
    if (!roots.has(rootId)) continue;
    // **One turn's output is not promoted apart.**
    //
    // Promotion is safe because it *empties* the chip — `own` below drops what
    // was promoted, so a lone answer renders inline, no chip appears, and the
    // thread is never opened. The message lives on exactly one surface. That is
    // the case #1890 D / #1972 / #2001 built this for, and it still holds when
    // the rest of the bucket is the operator writing again: their follow-up is
    // a separate act, and the answer they were waiting for belongs in the
    // channel.
    //
    // A capped turn is not that. It emits the agent's partial write-up and then
    // the host's `iteration_cap_pause_notice`, both parented to the same
    // operator message, and promoting only the first splits one turn's output
    // across two surfaces: the write-up renders inline *and* in the panel,
    // because `repliesInThread` walks the parent chain and knows nothing of
    // what was promoted. Dropping it from the panel instead is not open to us —
    // the notice under it opens "The reply above is a pause", and there has to
    // be a reply above.
    //
    // So promotion stops at the boundary it was always about: a lone answer.
    // When the runtime spoke more than once, the whole turn stays folded and
    // the chip says so.
    const runtimeReplies = bucket.filter(
      (r) => (r.from === "company" && !r.byPerson) || r.from === "system",
    );
    if (runtimeReplies.length > 1) continue;
    const root = position.get(rootId);
    const first = bucket[0];
    // **Only the runtime's own answer is ever promoted** (codex on #1972).
    //
    // `bucket[0]` is merely the earliest reply, and that is the *operator's*
    // own follow-up whenever they wrote again before the agent answered — a
    // thread they deliberately opened, flattened back into the channel, with
    // the answer they were waiting for still folded behind the root's chip. The
    // reader sees their own words twice and the reply not at all, which is the
    // failure this promotion exists to prevent, in the one case where a person
    // was demonstrably treating the exchange as a thread.
    //
    // A `system` line is excluded on the same terms: a settle marker is
    // runtime-generated but it is not an answer, and #1890 B put markers in the
    // thread that raised the card on purpose. Promoting one back into the
    // channel would undo that from the render side.
    //
    // **`from` alone does not say "the runtime wrote this".** `fromHistory`
    // projects `from` off `mine`, so *another signed-in person's* reply arrives
    // as `company` too, carrying `byPerson` to tell them apart — and without
    // that term a colleague answering first was promoted exactly as the
    // operator's own follow-up had been, reproducing this defect for everyone
    // except the viewer (codex + coderabbit on #2001).
    //
    // Only an explicit `true` blocks it. `undefined` means the host did not
    // say, and it is what *every* locally built company line carries — this
    // console's own POST, an `AgentReplyEvent` — so reading it as "might be a
    // person" would fold the live answer this promotion exists for.
    if (first === undefined || first.from !== "company" || first.byPerson) continue;
    const answer = position.get(first.id);
    if (root === undefined || answer === undefined) continue;
    const own = new Set(bucket.map((r) => r.id));
    let interleaved = false;
    for (let i = root + 1; i < answer; i += 1) {
      if (!own.has(messages[i].id)) {
        interleaved = true;
        break;
      }
    }
    if (!interleaved) inline.add(first.id);
  }
  return inline;
}

/**
 * Which of `messages` render **inline** in the channel rather than folding into
 * a parent's summary row (issue #1890 D).
 *
 * The public form of {@link inlineFirstReplies}, for the surfaces that must
 * agree with the timeline about what is on screen. Today that is the mention
 * badge: a summons inside a *folded* reply must stay unread until its thread is
 * opened, and one inside an *inline* reply is visible the moment the channel
 * is, so deferring it would leave a badge nobody can clear.
 *
 * Two surfaces, one definition — the discipline `owns` enforces on the host
 * side, and the reason this is exported rather than reimplemented.
 */
export function inlineReplyIds(messages: ChatMessage[]): ReadonlySet<string> {
  const replies = new Map<string, ChatMessage[]>();
  for (const m of messages) {
    if (!m.parentId) continue;
    const bucket = replies.get(m.parentId);
    if (bucket) bucket.push(m);
    else replies.set(m.parentId, [m]);
  }
  return inlineFirstReplies(messages, replies);
}

/**
 * Flatten a channel's messages into rows the timeline can render directly.
 *
 * A thread's **first reply renders inline** when nothing interleaved between
 * the question and it; everything else folds into the parent's summary row. See
 * {@link inlineFirstReplies} for the rule and why it is the renderer's to make.
 */
export function buildTimeline(
  messages: ChatMessage[],
  channel: Channel,
  members: TeamMember[],
  /** Your own face, so your lines in a busy channel are yours at a glance. */
  youAvatar?: string,
): TimelineEntry[] {
  const replies = new Map<string, ChatMessage[]>();
  for (const m of messages) {
    if (!m.parentId) continue;
    const bucket = replies.get(m.parentId);
    if (bucket) bucket.push(m);
    else replies.set(m.parentId, [m]);
  }
  const inline = inlineFirstReplies(messages, replies);

  const latestPillIdByTaskId = latestSettlePillIdByTaskId(messages);

  const entries: TimelineEntry[] = [];
  let prev: TimelineEntry | undefined;

  for (const m of messages) {
    if (m.parentId && !inline.has(m.id)) continue;
    const sender = senderOf(m, channel, members, youAvatar);
    const newDay = !prev || !sameDay(prev.message.at, m.at);
    const continuation =
      !newDay &&
      !!prev &&
      prev.sender.key === sender.key &&
      sender.kind !== "system" &&
      m.at - prev.message.at < GROUP_WINDOW_MS &&
      // A row with replies ends its run — the summary row below it would
      // otherwise sit between two lines that read as one utterance.
      prev.replies.length === 0 &&
      // Who *typed* it ends a run too (issue #1734, codex review of #1740).
      //
      // A collaborator's message and an agent reply share a sender key: both
      // are `from: "company"`, and the offline echo brain names its outbound
      // channel `operator` exactly as an operator message does, so `senderOf`
      // resolves both to the channel's own voice. Grouped, the second row is a
      // continuation — no author line, and therefore no Placeholder marker —
      // and it reads as part of the first one's utterance. That runs both ways
      // and is wrong both ways: an echo reply hides inside a colleague's run
      // unmarked, and a colleague's own words sit under an author line the
      // marker has already labelled as the echo brain's.
      //
      // Breaking the run is the honest rendering rather than a workaround: two
      // consecutive lines with different authorship are not one utterance, and
      // a run is a claim that they are.
      !!prev.message.byPerson === !!m.byPerson;

    // **Only a root carries a chip.** An inline reply is a rendered row, so
    // hanging its own bucket off it would put the second fold level back on
    // screen through the summary instead of through a row — the same
    // one-level-deep invariant `inlineFirstReplies` guards, and just as
    // invisible when it breaks.
    //
    // And the inline first reply is a row of its own, so it must not also count
    // on its parent's chip: a reader would see the answer and be told there is
    // one more thing to open, which there is not.
    const own = m.parentId
      ? []
      : (replies.get(m.id) ?? []).filter((r) => !inline.has(r.id));
    const entry: TimelineEntry = {
      message: m,
      sender,
      continuation,
      dayLabel: newDay ? formatDay(m.at) : undefined,
      replies: own,
      replySenders: distinctSenders(own, channel, members, youAvatar),
      isLatestSettlePill:
        m.from === "system" && m.taskId !== undefined
          ? latestPillIdByTaskId.get(m.taskId) === m.id
          : undefined,
    };
    entries.push(entry);
    prev = entry;
  }

  return entries;
}

/**
 * An approval this console has watched being decided (#379).
 *
 * The **summary is kept, not just the verdict**, and that is the whole point:
 * the host drops a resolved approval from `GET …/approvals` immediately, so a
 * console holding only the verdict has nothing left to draw and the card blinks
 * out of the thread the moment it is decided — which reads as the request
 * having been lost, not answered. Keeping the last-seen summary is what lets it
 * settle in place instead.
 */
export interface DecidedApproval {
  verdict: Verdict;
  approval: ApprovalSummary;
}

/**
 * One row of a channel, which is no longer only a message (#379).
 *
 * A parked approval is a **distinct kind**, not a synthetic `ChatMessage`. It
 * has to be: a card is decidable, carries live server state, and settles into a
 * terminal state — none of which a message row can represent, and faking one
 * would mean inventing an id, a sender and a body for something that is not an
 * utterance. Keeping it separate is also what lets the card *derive* from
 * `feed.approvals` rather than being appended once and then going stale.
 *
 * Both kinds carry `at` so the two streams interleave on real time, which is
 * the only ordering that reads correctly: the request appears where the
 * conversation was when it was raised.
 */
export type TimelineItem =
  | { kind: "message"; key: string; at: number; entry: TimelineEntry }
  | {
      kind: "approval";
      key: string;
      at: number;
      /**
       * Every gated call the same turn parked, oldest first (#842).
       *
       * Usually one. A research turn that reaches three sites parks three, and
       * the conversation asks about them **once** — one card listing three
       * hosts — rather than interrupting the operator three times for one piece
       * of work. Each entry stays its own approval underneath: its own id, its
       * own decision, its own host-scoped grant on approve.
       *
       * Never empty. {@link buildTimelineItems} only mints an item when it has
       * an approval to put in it, so a renderer can read `approvals[0]` for the
       * facts the whole batch shares (the asker, the thread, the tool).
       */
      approvals: ApprovalSummary[];
      /**
       * The verdicts this console has witnessed, keyed by approval id (#842).
       *
       * A decided approval leaves `feed.approvals` on the next refresh, so
       * without this the card would simply vanish mid-glance — an abrupt
       * unmount that reads as the request having been lost. Holding the
       * witnessed verdict lets it settle into a terminal state instead.
       *
       * Per item rather than per card, because a batch settles **item by
       * item**: the Approvals page decides one row at a time, and a card that
       * kept claiming three things were pending after one was approved there
       * would be the two surfaces drifting. A batch is fully settled only when
       * every id in {@link approvals} has an entry here.
       */
      decided: Record<string, Verdict>;
    }
  | {
      /**
       * One round of a desk answering as a room.
       *
       * The rows a round committed collapse into a single item so the band can
       * draw what a flat list cannot: the seats that ran together, which of
       * them is still working, and the plan the episode opened with.
       *
       * The operator message that opened the episode is deliberately **not**
       * inside it. The question is the operator's and the answer is the room's;
       * nesting the former inside the latter reads as though the desk asked
       * itself.
       */
      kind: "round";
      key: string;
      at: number;
      episode: Episode;
      round: EpisodeRound;
      /** The rows this round produced, in transcript order. */
      items: TimelineItem[];
    }
  | {
      /** The line that says an episode is over, after its last round. */
      kind: "episode_complete";
      key: string;
      at: number;
      episode: Episode;
    }
  | {
      /** The seats of an open episode parked on an operator decision, after its last row. */
      kind: "episode_waiting";
      key: string;
      at: number;
      episode: Episode;
      seats: WaitingSeat[];
    };

/** A seat waiting on the operator, and the approvals it is waiting on. */
export interface WaitingSeat {
  agentId: string;
  approvalIds: string[];
}

/** The episode an approval item was raised in, when a seat raised it. */
function approvalEpisodeId(item: Extract<TimelineItem, { kind: "approval" }>): string | undefined {
  return item.approvals.find((approval) => approval.episode?.id)?.episode?.id;
}

/**
 * The seats of `episode` still waiting on the operator.
 *
 * A seat the frames parked counts until it resumes, unless every approval it
 * named has been decided here. A pending approval a seat raised counts on its
 * own, which is what survives a reload that dropped the frames.
 */
export function waitingSeats(
  episode: Episode,
  approvals: ApprovalSummary[],
  decided: Record<string, DecidedApproval> = {},
): WaitingSeat[] {
  if (episode.status === "completed") return [];
  const seats = new Map<string, WaitingSeat>();
  const add = (agentId: string, approvalIds: string[]) => {
    const held = seats.get(agentId);
    if (!held) {
      seats.set(agentId, { agentId, approvalIds: [...approvalIds] });
      return;
    }
    for (const id of approvalIds) if (!held.approvalIds.includes(id)) held.approvalIds.push(id);
  };
  for (const seat of episode.waiting ?? []) {
    const settled = seat.approvalIds.length > 0 && seat.approvalIds.every((id) => decided[id]);
    if (!settled) add(seat.agentId, seat.approvalIds.filter((id) => !decided[id]));
  }
  for (const approval of approvals) {
    if (approval.episode?.id !== episode.id || decided[approval.id]) continue;
    add(approval.episode.seat, [approval.id]);
  }
  return [...seats.values()];
}

/**
 * Interleave a channel's messages and the approvals raised in it, oldest first.
 *
 * `approvals` is expected to be pre-filtered to this channel by the caller —
 * the thread→channel mapping lives in the shell, which owns the desk list and
 * the roster, and this module stays pure.
 *
 * A `decided` card is kept even once the feed has dropped it, so the operator
 * sees their own decision land rather than the card disappearing.
 *
 * ## One card per turn (#842)
 *
 * Approvals sharing a `batch` — the host's key for the turn that parked them —
 * collapse into a single item. The conversation is interrupted once for one
 * piece of work, which is the whole of the issue; the grouping is presentation
 * only, and every approval inside the item is still decided on its own id.
 *
 * Approvals with **no** batch are never grouped, not even with each other. An
 * absent key means "the host did not say which turn this came from" — a
 * workflow node, a scheduler tick, an older host — and folding those together
 * would invent a batch out of two facts that are only alike in being unknown,
 * which is how an operator ends up approving something they were never shown.
 * Each gets its own card, exactly as before this existed.
 */
/**
 * The key that decides which approvals share one card (#842, #1891).
 *
 * The turn's `batch` when the host named one, and the approval's **own id**
 * otherwise — which is what makes "ungrouped" the safe default: an id is
 * unique, so a batchless approval can only ever group with itself. An absent
 * key means "the host did not say which turn this came from" (a workflow node,
 * a scheduler tick, an older host), and folding those together would invent a
 * batch out of two facts that are only alike in being unknown, which is how an
 * operator ends up approving something they were never shown.
 *
 * Extracted so the board card groups exactly as the transcript does (#1895
 * review). It rendered a paused card's whole queue as one `ApprovalRow`, so a
 * card holding two turns' parks — or several batchless ones — offered a single
 * Approve that authorised across them. The rule was already written down here;
 * the second surface just wasn't reading it.
 */
export function approvalBatchKey(approval: ApprovalSummary): string {
  // A blocker folds by its root cause (#1862): every card stalled on one
  // broken integration is one question, even across turns a batch would keep
  // apart. Falls back to the turn batch, then to the id — so an ordinary
  // approval groups exactly as before.
  if (approval.group_key) return `group:${approval.group_key}`;
  return approval.batch ?? `solo:${approval.id}`;
}

export function buildTimelineItems(
  entries: TimelineEntry[],
  approvals: ApprovalSummary[],
  decided: Record<string, DecidedApproval> = {},
  /**
   * The episodes this channel ran, if any.
   *
   * Optional and defaulted, so every existing call site and every test written
   * before episodes keeps its exact behaviour: with no episodes this returns
   * precisely what it always did.
   */
  episodes: Episode[] = [],
): TimelineItem[] {
  const items: TimelineItem[] = entries.map((entry) => ({
    kind: "message" as const,
    key: entry.message.id,
    at: entry.message.at,
    entry,
  }));

  // Insertion-ordered, so a batch lands where its **first** approval did rather
  // than wherever the last one happened to arrive. The caller hands us the
  // pending feed followed by the settled ones, so an item decided on the
  // Approvals page rejoins the card it was raised in instead of opening a
  // second one below it.
  const batches = new Map<string, ApprovalSummary[]>();
  for (const approval of approvals) {
    const key = approvalBatchKey(approval);
    const bucket = batches.get(key);
    if (bucket) bucket.push(approval);
    else batches.set(key, [approval]);
  }

  for (const [key, batch] of batches) {
    batch.sort((a, b) => a.at_millis - b.at_millis || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    const verdicts: Record<string, Verdict> = {};
    for (const approval of batch) {
      const verdict = decided[approval.id]?.verdict;
      if (verdict) verdicts[approval.id] = verdict;
    }
    items.push({
      kind: "approval",
      key: `approval:${key}`,
      // The turn asked once, at the moment its first call was gated. Placing
      // the card at the earliest of the batch is what keeps it beside the
      // message that provoked it.
      at: batch[0].at_millis,
      approvals: batch,
      decided: verdicts,
    });
  }

  // Stable within a millisecond: a card raised by the very turn whose reply
  // shares its timestamp should sit after that reply, not shuffle between
  // renders. `sort` is stable in every engine this ships to, so equal `at`
  // keeps insertion order — messages first, then cards.
  const ordered = items.sort((a, b) => a.at - b.at);
  const all = [...episodes, ...parkedOnlyEpisodes(episodes, approvals, decided)];
  return all.length === 0 ? ordered : groupEpisodes(ordered, all, decided);
}

/**
 * An open episode for each seat approval whose episode the rows and frames do
 * not know, so a seat parked before any reply still gets its band and waiting
 * marker after a reload.
 */
function parkedOnlyEpisodes(
  episodes: Episode[],
  approvals: ApprovalSummary[],
  decided: Record<string, DecidedApproval>,
): Episode[] {
  const known = new Set(episodes.map((episode) => episode.id));
  const minted = new Map<string, Episode>();
  for (const approval of approvals) {
    const ref = approval.episode;
    if (!ref || known.has(ref.id) || decided[approval.id]) continue;
    let episode = minted.get(ref.id);
    if (!episode) {
      episode = {
        id: ref.id,
        participants: [],
        status: "open",
        rounds: [{ episodeId: ref.id, revision: 0, status: "open", seats: [], messageIds: [] }],
        messageIds: [],
        openedAt: approval.at_millis,
        roundCount: 0,
        referrals: [],
        conversations: [],
        live: false,
      };
      minted.set(ref.id, episode);
    }
    if (!episode.participants.includes(ref.seat)) episode.participants.push(ref.seat);
    episode.openedAt = Math.min(episode.openedAt ?? approval.at_millis, approval.at_millis);
  }
  return [...minted.values()];
}

/**
 * Collapse each round's rows into one item, leaving everything else alone.
 *
 * A round takes the position of its **first** row, so it stays where the
 * conversation put it. A round with no rows yet — one that just opened, whose
 * seats are all still working — takes the moment it opened, which is after
 * every row of the round before it. A completed episode gets its marker after
 * its last round. Rows an episode claims that are not in this window (history
 * that has not loaded) are simply absent: the band renders what it has.
 *
 * An approval a seat of the episode raised sits inside its band, since the
 * episode is waiting on it; any other approval stays in the channel at its own
 * time. An open episode with a seat waiting on the operator gets a waiting
 * marker after its last row.
 */
function groupEpisodes(
  items: TimelineItem[],
  episodes: Episode[],
  decided: Record<string, DecidedApproval>,
): TimelineItem[] {
  const owner = new Map<string, { episode: Episode; round: EpisodeRound }>();
  const latest = new Map<string, { episode: Episode; round: EpisodeRound }>();
  for (const episode of episodes) {
    for (const round of episode.rounds) {
      for (const id of round.messageIds) owner.set(id, { episode, round });
    }
    const round = episode.rounds[episode.rounds.length - 1];
    if (round) latest.set(episode.id, { episode, round });
  }
  const approvals: ApprovalSummary[] = [];
  for (const item of items) if (item.kind === "approval") approvals.push(...item.approvals);

  const out: TimelineItem[] = [];
  const blocks = new Map<string, Extract<TimelineItem, { kind: "round" }>>();
  /**
   * One band per **episode**, not per revision.
   *
   * It used to key on the revision too, so every wave opened another band and
   * an episode that ran nine of them stacked nine. They also carried the raw
   * revision number, which is not a count: conversation waves take revisions
   * of their own, so a desk that ran nine rounds showed a band labelled
   * "Round 17". One band holding every row is what an episode actually is.
   */
  const blockKey = (round: EpisodeRound) => `round:${round.episodeId}`;

  for (const item of items) {
    const episodeId = item.kind === "approval" ? approvalEpisodeId(item) : undefined;
    const owned =
      item.kind === "message"
        ? owner.get(item.entry.message.id)
        : episodeId
          ? latest.get(episodeId)
          : undefined;
    if (!owned) {
      out.push(item);
      continue;
    }
    const key = blockKey(owned.round);
    let block = blocks.get(key);
    if (!block) {
      block = {
        kind: "round",
        key,
        at: item.at,
        episode: owned.episode,
        round: owned.round,
        items: [],
      };
      blocks.set(key, block);
      out.push(block);
    }
    // The live state is the newest wave's, so a band that outlives several
    // shows the seats working now rather than the ones that finished first.
    if (owned.round.revision >= block.round.revision) block.round = owned.round;
    block.items.push(item);
  }

  // Rounds no row has reached yet, and the completion markers. Each is placed
  // just after the last thing its episode put on screen, so a live round with
  // no rows sits below the previous round's replies rather than at the top.
  for (const episode of episodes) {
    let last = -Infinity;
    for (const round of episode.rounds) {
      const held = blocks.get(blockKey(round));
      if (held) {
        last = Math.max(last, ...held.items.map((row) => row.at));
        // A wave that has opened but committed nothing yet is still the live
        // one, and it owns no rows to carry it in above — without this the
        // band freezes on the last wave that spoke and shows seats as
        // finished while they are working.
        if (round.revision >= held.round.revision) held.round = round;
        continue;
      }
      const at = Math.max(round.startedAt ?? -Infinity, last === -Infinity ? -Infinity : last + 1);
      if (at === -Infinity) continue;
      const block: Extract<TimelineItem, { kind: "round" }> = {
        kind: "round",
        key: blockKey(round),
        at,
        episode,
        round,
        items: [],
      };
      blocks.set(block.key, block);
      out.push(block);
      last = at;
    }
    const waiting = waitingSeats(episode, approvals, decided);
    if (waiting.length > 0) {
      const raised = approvals
        .filter((approval) => approval.episode?.id === episode.id)
        .map((approval) => approval.at_millis);
      let at = Math.max(last, ...raised);
      if (at === -Infinity) at = episode.openedAt ?? -Infinity;
      if (at !== -Infinity) {
        out.push({ kind: "episode_waiting", key: `episode_waiting:${episode.id}`, at, episode, seats: waiting });
      }
    }
    if (episode.status === "completed") {
      const at = Math.max(episode.completedAt ?? -Infinity, last === -Infinity ? -Infinity : last + 1);
      if (at === -Infinity) continue;
      out.push({ kind: "episode_complete", key: `episode_complete:${episode.id}`, at, episode });
    }
  }

  // Stable, like `buildTimelineItems`'s own sort: a block minted after the
  // rows keeps its place among equal timestamps.
  return out.sort((a, b) => a.at - b.at);
}

/* ---- formatting ---- */

export function formatTime(at: number): string {
  return new Date(at).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
}

export function sameDay(a: number, b: number): boolean {
  return new Date(a).toDateString() === new Date(b).toDateString();
}

export function formatDay(at: number): string {
  const d = new Date(at);
  const today = new Date();
  const yesterday = new Date(today);
  yesterday.setDate(today.getDate() - 1);
  if (d.toDateString() === today.toDateString()) return "Today";
  if (d.toDateString() === yesterday.toDateString()) return "Yesterday";
  return d.toLocaleDateString(undefined, { weekday: "long", month: "long", day: "numeric" });
}

/* ---- reactions ---- */

/** The palette the hover bar offers, in the order it offers them. */
export const QUICK_REACTIONS = ["👍", "🎉", "👀", "✅", "❤️"] as const;

/**
 * Toggle the reader's own reaction, leaving everyone else's alone (issue #364).
 *
 * Reactions are per-person rows now, so a toggle adds or removes exactly one —
 * the reader's. It used to replace a count, which meant tapping an emoji
 * somebody else had already used silently wiped their reaction.
 *
 * `label` is how the reader will be shown in the chip's tooltip until the host
 * says otherwise. Returns `undefined` for an empty result so a message with no
 * reactions carries no key at all.
 */
export function toggleReaction(
  reactions: Reaction[] | undefined,
  emoji: string,
  label: string,
): Reaction[] | undefined {
  const rows = reactions ?? [];
  const mine = rows.some((r) => r.emoji === emoji && r.mine);
  const next = mine
    ? rows.filter((r) => !(r.emoji === emoji && r.mine))
    : [...rows, { emoji, by: label, mine: true }];
  return next.length ? next : undefined;
}

/** Whether the reader has already reacted to a message with this emoji. */
export function hasReacted(reactions: Reaction[] | undefined, emoji: string): boolean {
  return !!reactions?.some((r) => r.emoji === emoji && r.mine);
}

/** One emoji's chip: its rows collapsed into a count and a who-list. */
export interface ReactionChip {
  emoji: string;
  count: number;
  /** Whether one of the rows is the reader's. */
  mine: boolean;
  /** Everyone who reacted with it, in the order the host listed them. */
  by: string[];
}

/**
 * Group per-person reaction rows into the chips the row renders.
 *
 * Chips keep first-reacted order rather than sorting by count, so a message's
 * reactions do not reshuffle under the reader as others react.
 */
export function reactionChips(reactions: Reaction[] | undefined): ReactionChip[] {
  const chips: ReactionChip[] = [];
  const byEmoji = new Map<string, ReactionChip>();
  for (const row of reactions ?? []) {
    let chip = byEmoji.get(row.emoji);
    if (!chip) {
      chip = { emoji: row.emoji, count: 0, mine: false, by: [] };
      byEmoji.set(row.emoji, chip);
      chips.push(chip);
    }
    chip.count += 1;
    chip.mine ||= row.mine;
    chip.by.push(row.by);
  }
  return chips;
}

/**
 * Drop a dismissed card from **every** channel's transcript (issue #984).
 *
 * The channel-level counterpart of {@link clearTaskCard}, and it exists for the
 * same reason one level up. That helper keys on the card rather than the clicked
 * row because one card can be named by several lines; this one keys on the card
 * rather than the active channel because those lines can sit in several
 * *channels* — a dispatch marker lands in the origin thread's channel, not
 * necessarily the one the operator is looking at. Clearing only the active
 * channel leaves the rest linking to a card the host no longer has.
 *
 * Returns the same object when nothing changed, so React sees no new state.
 */
export function clearTaskCardEverywhere(transcripts: Transcripts, taskId: string): Transcripts {
  let changed = false;
  const next: Transcripts = {};
  for (const [channelId, messages] of Object.entries(transcripts)) {
    const cleared = clearTaskCard(messages, taskId);
    if (cleared !== messages) changed = true;
    next[channelId] = cleared;
  }
  return changed ? next : transcripts;
}
