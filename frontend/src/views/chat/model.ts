// The chat workspace's data model: channels, direct messages, and the grouping
// rules the timeline reads. Everything here is pure — the view owns the state.

import type { ApprovalSummary, DeskDto, Verdict } from "@/api/types";
import type { ChatMessage, Reaction } from "@/lib/chat";
import { defaultDesks, type Desk } from "@/lib/desks";
import { initials as nameInitials, type TeamMember } from "@/lib/team";

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
export function deskFromDto(d: DeskDto): Desk {
  const slug = d.name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
  return {
    id: d.id,
    channel: slug || d.id,
    name: d.name,
    blurb: d.description ?? "",
    members: d.members,
    overlayMembers: d.overlayMembers,
  };
}

/**
 * Every channel's transcript, keyed by channel id. Owned by `AppShell`, not
 * `ChatView`, so a transcript survives `ChatView` unmounting when the operator
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
   * Needed because `ChatView` resolves its own desk list independently of the
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
 * No rehydration is happening or ever will — for a `ChatView` mounted without a
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

export type ChannelKind = "channel" | "dm";

export interface Channel {
  id: string;
  /** The bare name — rendered after a `#` for channels, plain for DMs. */
  name: string;
  /**
   * How the company side signs its messages here. A channel's name is a slug
   * (`front-desk`); its voice is who is speaking (`Front desk`).
   */
  voice?: string;
  kind: ChannelKind;
  /** One line under the title, and the tooltip on it. */
  purpose: string;
  /** Private channels wear a lock instead of a hash. */
  private?: boolean;
  /** Avatar tone key; DMs and desk channels both carry one. */
  tone?: string;
  /** The roster entry behind a DM, when there is one. */
  member?: TeamMember;
  /**
   * Who is in this channel, as roster teammate **ids** in the desk's own order
   * (lead first). Ids rather than resolved `TeamMember`s so this model stays
   * pure and a channel never goes stale when the roster reloads — resolve with
   * {@link channelMembers}.
   *
   * Absent for DMs (a two-person line needs no list) and for the static
   * fallback desks, which have no membership concept; a consumer that finds it
   * absent falls back to the whole roster (issue #369).
   */
  memberIds?: string[];
}

export interface ChannelSection {
  id: string;
  label: string;
  channels: Channel[];
}

/**
 * The channel list.
 *
 * `desks` become the `#channels` — they are the standing lines you can
 * address, and each already carries a name, a blurb, and a tone. Defaults to
 * `lib/desks.ts`'s static set for a host that doesn't expose `.../desks` yet
 * (issue #53); the caller fetches the real ones and passes them in once they
 * land, so a company's own desks show up instead of the generic
 * strategy/creative/front-desk trio. Every roster teammate additionally gets
 * a DM, so a one-to-one line exists for anyone the company employs.
 *
 * Both kinds post to the same company endpoint. A channel scopes a transcript
 * and gives the company side a stable identity; it is not a separate backend.
 */
export function buildChannels(members: TeamMember[], desks: Desk[] = defaultDesks()): ChannelSection[] {
  const channels: Channel[] = desks.map((d) => ({
    id: d.id,
    name: d.channel,
    voice: d.name,
    kind: "channel" as const,
    purpose: d.blurb,
    tone: d.tone,
    memberIds: d.members,
  }));

  const dms: Channel[] = members.map((m) => ({
    id: dmChannelId(m),
    name: m.name,
    kind: "dm" as const,
    // The teammate's **description**, which is the field parallel to a desk's
    // `blurb` above — both answer "what is this line for", and neither repeats
    // what the title already said. This used to read `m.role`, an identity
    // field in a description slot, and that is precisely what made the header
    // say the same words twice (issue #1180): `fromDto` falls back
    // `dto.name?.trim() || dto.role`, so a company that names roles rather than
    // people has name === role, and the title and the slot after the divider
    // resolved to one string. The role is still the fallback — for a teammate
    // the host *did* name it is a real second fact — and {@link channelSubtitle}
    // is what declines to render even that when it just echoes the title.
    purpose: m.description.trim() || m.role,
    tone: m.tone,
    member: m,
  }));

  return [
    { id: "channels", label: "Channels", channels },
    { id: "dms", label: "Direct messages", channels: dms },
  ];
}

/**
 * A DM's channel id, and so its URL.
 *
 * Keyed on the teammate's **id**, which is now the one stable thing about them
 * (issue #364). A host roster entry has always had a stable agent id; what was
 * missing was a stable id for a console-invented teammate, and `lib/team.ts`'s
 * counter was why this used to key on the name instead.
 *
 * Keying on the name had a cost that only shows up later: renaming a teammate
 * silently moved their DM to a new URL and orphaned every message already
 * journaled under the old one. An id does not change when a person's name does,
 * which is the entire reason to prefer it.
 */
export function dmChannelId(member: TeamMember): string {
  return `dm:${member.id}`;
}

/**
 * The name-derived DM id this console minted before issue #364.
 *
 * Kept for one release, and for one purpose: a `#/chat/dm:ada-1f3k` link that
 * somebody bookmarked or pasted into a ticket still has to land on Ada's DM.
 * Only {@link resolveDmChannelId} calls it — nothing addresses a channel by it,
 * so no new state is ever written under a legacy id.
 */
export function legacyDmChannelId(member: TeamMember): string {
  const name = member.name.trim();
  const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
  return `dm:${slug ? `${slug}-` : ""}${nameHash(name)}`;
}

/**
 * The current DM channel id a URL segment names, or `null` when it names no
 * DM this company has.
 *
 * Resolves the current id first, then the pre-#364 name-derived form, so an old
 * link keeps working without the old id ever becoming addressable again.
 */
export function resolveDmChannelId(id: string, members: TeamMember[]): string | null {
  if (!id.startsWith("dm:")) return null;
  const match = members.find(
    (m) => dmChannelId(m) === id || legacyDmChannelId(m) === id,
  );
  return match ? dmChannelId(match) : null;
}

function nameHash(name: string): string {
  let hash = 0;
  for (let i = 0; i < name.length; i++) hash = (hash * 31 + name.charCodeAt(i)) | 0;
  return (hash >>> 0).toString(36);
}

/**
 * The chat channel a **host thread id** belongs to, or `null` when this
 * company has no channel that owns it.
 *
 * This is the one subtle rule in the chat's addressing, kept in one place so a
 * caller cannot get it wrong twice. A desk's channel id *is* its thread id —
 * {@link deskFromDto} leaves `DeskDto.id` untouched precisely so addressing the
 * channel routes to that desk. A DM's is not: its channel id is the
 * console-local {@link dmChannelId}, while the thread id the host journals
 * under (and `chat` / `chat/history` take) is the roster teammate's agent id.
 *
 * Anything routing a host-side event — which always names a *thread* — into
 * {@link Transcripts} has to make that distinction. Issue #367 is what the
 * console looks like when it doesn't.
 */
export function channelIdForThread(
  threadId: string,
  desks: Desk[],
  members: TeamMember[],
): string | null {
  if (desks.some((d) => d.id === threadId)) return threadId;
  const member = members.find((m) => m.id === threadId);
  return member ? dmChannelId(member) : null;
}

export function findChannel(sections: ChannelSection[], id: string | null): Channel | null {
  if (!id) return null;
  for (const s of sections) {
    const hit = s.channels.find((c) => c.id === id);
    if (hit) return hit;
  }
  return null;
}

/**
 * The first channel across all sections, or `null` when there are none.
 *
 * The last-resort selection, so the chat never renders blank while any channel
 * exists (issue #366), and the address of last resort for a line with no
 * channel of its own (issue #368). Both used to reach for a literal `"main"`
 * instead — an id carried only by the first *fallback* desk, so it matched
 * nothing at all on a company with desks of its own.
 */
export function firstChannel(sections: ChannelSection[]): Channel | null {
  for (const s of sections) {
    if (s.channels.length > 0) return s.channels[0];
  }
  return null;
}

/**
 * A channel's own members, resolved against the roster — `null` when the
 * channel names no membership (a DM, or a fallback desk), which is the caller's
 * cue to fall back to the whole roster rather than draw an empty pane.
 *
 * Maps over the **ids**, not over the roster: the desk's order is meaningful —
 * `memberIds[0]` is the lead — and filtering the roster would silently reorder
 * everyone into roster order and lose that. An id with no roster row (a
 * teammate removed since the desks were fetched) drops out rather than
 * rendering a placeholder for somebody who isn't there.
 */
export function channelMembers(channel: Channel, roster: TeamMember[]): TeamMember[] | null {
  if (!channel.memberIds) return null;
  const byId = new Map(roster.map((m) => [m.id, m]));
  return channel.memberIds
    .map((id) => byId.get(id))
    .filter((m): m is TeamMember => m !== undefined);
}

/** How a channel is titled in the header and the rail. */
export function channelTitle(channel: Channel): string {
  return channel.kind === "dm" ? channel.name : `#${channel.name}`;
}

/**
 * The line that goes *beside* the title — the muted slot after the header's
 * divider, the rail row's tooltip, the conversation intro's clause — or `null`
 * when there is nothing to say that the title has not already said.
 *
 * `null` rather than the empty string, and a rule rather than a DM special
 * case. A subtitle exists to add a second fact; one that repeats the first is
 * not a hierarchy, it is the same word twice with two type styles, and the
 * honest render of "nothing more to say" is nothing. Issue #1180 is what that
 * looks like when it ships: every agent in a company that declares roles and no
 * names read `Backend Engineer │ Backend Engineer` across the top of its DM.
 *
 * The comparison is case- and whitespace-insensitive because the duplicate is a
 * duplicate to a reader either way — a manifest whose description restates the
 * role in sentence case is the same non-fact as one that restates it verbatim.
 *
 * Kind-agnostic on purpose. A DM is where this bites today, but a desk whose
 * blurb is just its own name is the identical duplicate under `#`, and a rule
 * that only fires for DMs would let that one through. Channels are otherwise
 * untouched: a blurb that says something the slug does not — which is every
 * desk that bothered to write one — comes back exactly as before.
 */
export function channelSubtitle(channel: Channel): string | null {
  const purpose = channel.purpose.trim();
  if (!purpose) return null;
  // Collapse runs of internal whitespace too, not just the outer trim — a
  // manifest description copy-pasted from the role with a doubled space or a
  // stray newline in the middle is still the same duplicate to a reader, and
  // the doc comment above promises "whitespace-insensitive" without
  // qualification.
  const normalize = (s: string) => s.trim().replace(/\s+/g, " ").toLowerCase();
  if (normalize(purpose) === normalize(channel.name)) return null;
  return purpose;
}

/**
 * The line under the identity block at the top of an empty transcript.
 *
 * Pure, exported and here rather than inline in `MessageTimeline` because it is
 * four branches of prose over two nullable inputs, and prose defects are
 * invisible to a type checker. The one this arrived with: the DM branch used to
 * append a full stop by hand, which was right while the subtitle was a role
 * (`Backend Engineer`) and produced "…and services.." the moment issue #1180
 * made it a description that brings its own punctuation. `sentence` is the rule
 * for that, so every branch goes through it.
 *
 * `loading` renders the subtitle alone: both finished sentences are positive
 * claims that the channel has no history, and neither may be made before the
 * host has answered (issue #934).
 */
export function channelIntroSentence(channel: Channel, loading: boolean): string {
  const subtitle = channelSubtitle(channel);
  if (loading) return subtitle ? sentence(subtitle) : "";
  if (channel.kind === "dm") {
    // No subtitle drops the clause, not the sentence: where you are is still
    // worth saying, the tautology after it is not.
    return subtitle
      ? `This is the start of your direct message with ${channel.name} — ${sentence(lower(subtitle))}`
      : `This is the start of your direct message with ${channel.name}.`;
  }
  return `This is the very beginning of ${channelTitle(channel)}.${subtitle ? ` ${sentence(subtitle)}` : ""}`;
}

/** Lowercases the first character only, for a clause continuing a sentence. */
function lower(s: string): string {
  return s.charAt(0).toLowerCase() + s.slice(1);
}

/** Terminates `s` with a full stop unless it already ends in punctuation. */
function sentence(s: string): string {
  const t = s.trim();
  return /[.!?]$/.test(t) ? t : `${t}.`;
}

/**
 * The face a DM wears — the `Avatar` seed for the teammate on the other end —
 * or `null` for anything that has no face: a channel, and a DM with no roster
 * entry behind it (both of those wear a glyph instead).
 *
 * One function rather than the same two props written out at each call site,
 * because the rail row and the header sit on screen together and `Avatar`
 * derives its mascot from the `name` it is handed. A call site that seeded on
 * anything else — the teammate's id, its role — would draw a *different* face
 * for the same person a few pixels away, which is worse than the generic glyph
 * the header used to show (issue #1170). Deriving both from here is what makes
 * that drift impossible rather than merely unlikely.
 */
export function dmFace(channel: Channel): { name: string; tone?: string } | null {
  if (channel.kind !== "dm" || !channel.member) return null;
  return { name: channel.name, tone: channel.tone };
}

/**
 * Whether this chat target's composer offers "Do it once" / "Build me the
 * workflow" (issues #580, #845).
 *
 * True for every real chat target — a channel line and a DM can each open a
 * board card, and the card is what routes a `workflow` request to the builder
 * pass. It is deliberately a total function over [`ChannelKind`] rather than an
 * inline `kind === "channel"`, so adding a kind is a decision someone makes here
 * instead of a control that silently fails to appear.
 *
 * Not every composer asks: the thread and copilot composers pass nothing and
 * keep their plain `(text)` `onSend`. A thread reply continues a message that
 * already made this choice, and a copilot line is about one graph rather than a
 * request to the company.
 *
 * #580 shipped the control on channels only. Nothing downstream was ever scoped
 * to channels — the chat route reads `deliverable` off the payload whatever
 * thread it came from — so the asymmetry lived entirely in the caller, and a DM
 * asking for a workflow had no way to say so. It went as a `once` card, was
 * dispatched to a desk agent holding no authoring tool, and came back a refusal.
 */
export function offersDeliverableChoice(kind: ChannelKind): boolean {
  switch (kind) {
    case "channel":
    case "dm":
      return true;
  }
}

/* ---- senders ---- */

export type SenderKind = "you" | "company" | "agent" | "system";

export interface Sender {
  /** Stable identity, so consecutive lines from one voice group together. */
  key: string;
  name: string;
  kind: SenderKind;
  tone?: string;
}

/** Channel names the host uses for its own voice rather than a named agent. */
const COMPANY_VOICE = new Set(["operator", "console", "chat", "owner", ""]);

/**
 * Who said a line, within a channel.
 *
 * The company side wears the channel's identity unless the reply names a
 * distinct originating channel — then it reads as that agent, which is how a
 * single endpoint produces a multi-voice transcript.
 */
export function senderOf(m: ChatMessage, channel: Channel): Sender {
  if (m.from === "you") return { key: "you", name: "You", kind: "you" };
  if (m.from === "system") return { key: "system", name: "System", kind: "system" };

  const named = m.channel?.trim().toLowerCase() ?? "";
  if (named && !COMPANY_VOICE.has(named)) {
    return { key: `agent:${named}`, name: titleize(m.channel ?? ""), kind: "agent", tone: named };
  }

  // A desk speaks as itself and wears its own tone; only the main line — the
  // one channel with no tone of its own — speaks as the company.
  return {
    key: `channel:${channel.id}`,
    name: channel.voice ?? channel.name,
    kind: channel.kind === "dm" || channel.tone ? "agent" : "company",
    tone: channel.tone,
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
}

/**
 * Flatten a channel's messages into rows the timeline can render directly.
 *
 * Replies are folded into their parent rather than laid out inline: a parent
 * carries its own replies and renders a summary row, matching how a threaded
 * chat keeps the main channel readable.
 */
export function buildTimeline(messages: ChatMessage[], channel: Channel): TimelineEntry[] {
  const replies = new Map<string, ChatMessage[]>();
  for (const m of messages) {
    if (!m.parentId) continue;
    const bucket = replies.get(m.parentId);
    if (bucket) bucket.push(m);
    else replies.set(m.parentId, [m]);
  }

  const entries: TimelineEntry[] = [];
  let prev: TimelineEntry | undefined;

  for (const m of messages) {
    if (m.parentId) continue;
    const sender = senderOf(m, channel);
    const newDay = !prev || !sameDay(prev.message.at, m.at);
    const continuation =
      !newDay &&
      !!prev &&
      prev.sender.key === sender.key &&
      sender.kind !== "system" &&
      m.at - prev.message.at < GROUP_WINDOW_MS &&
      // A row with replies ends its run — the summary row below it would
      // otherwise sit between two lines that read as one utterance.
      prev.replies.length === 0;

    const entry: TimelineEntry = {
      message: m,
      sender,
      continuation,
      dayLabel: newDay ? formatDay(m.at) : undefined,
      replies: replies.get(m.id) ?? [],
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
    };

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
export function buildTimelineItems(
  entries: TimelineEntry[],
  approvals: ApprovalSummary[],
  decided: Record<string, DecidedApproval> = {},
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
    // The id is the fallback key, which is what makes "ungrouped" the safe
    // default: an id is unique, so a batchless approval can only ever group
    // with itself.
    const key = approval.batch ?? `solo:${approval.id}`;
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
  return items.sort((a, b) => a.at - b.at);
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
